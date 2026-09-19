//! On-disk `reindex.lock`, `reindex/active`, and the catch-up queue.
//!
//! `{vault}/.scheda/reindex.lock` is flock, kernel-released on process death,
//! same as `write.lock`. Presence of `reindex/active` means writers must
//! enqueue. Queue files are a set keyed by SHA-256 of `{collection}\0{id}`.

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;
use crate::schema::{CollectionDecl, Decls, Kind};
use crate::error::{Code, ContextAs, ContextIo, io_code};
use crate::id;
use crate::store;

/// Retry spacing while waiting for `write.lock` at publish.
pub(crate) const PUBLISH_RETRY: Duration = Duration::from_millis(25);
/// Bound on that wait. A per-call writer never exhausts it; an open
/// transaction always does.
pub(crate) const PUBLISH_WINDOW: Duration = Duration::from_secs(30);

#[cfg(test)]
thread_local! {
    static PUBLISH_WINDOW_OVERRIDE: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// Bound on waiting for `write.lock` at publish. Production is
/// [`PUBLISH_WINDOW`]; a test on this thread may shorten it.
pub(crate) fn publish_window() -> Duration {
    #[cfg(test)]
    {
        PUBLISH_WINDOW_OVERRIDE.with(|slot| slot.get().unwrap_or(PUBLISH_WINDOW))
    }
    #[cfg(not(test))]
    {
        PUBLISH_WINDOW
    }
}

/// Restores [`publish_window`] for this thread when dropped.
#[cfg(test)]
pub(crate) struct PublishWindowGuard {
    previous: Option<Duration>,
}

#[cfg(test)]
impl Drop for PublishWindowGuard {
    fn drop(&mut self) {
        PUBLISH_WINDOW_OVERRIDE.with(|slot| slot.set(self.previous));
    }
}

/// Shorten [`publish_window`] for this thread until the guard drops.
#[cfg(test)]
#[must_use]
pub(crate) fn set_publish_window(window: Duration) -> PublishWindowGuard {
    let previous = PUBLISH_WINDOW_OVERRIDE.with(|slot| slot.replace(Some(window)));
    PublishWindowGuard { previous }
}

const LOCK_FILE: &str = "reindex.lock";
const REINDEX_DIR: &str = "reindex";
const ACTIVE_FILE: &str = "active";
const QUEUE_DIR: &str = "queue";

#[derive(Debug, Deserialize, Serialize)]
struct QueueEntry {
    collection: String,
    id: String,
}

/// `{vault}/.scheda/reindex.lock` as a `try_lock` flock. Not a pid-file.
///
/// A few short retries absorb a stale `WouldBlock` after the previous
/// holder closed the fd (overlay / WSL). A second live `reindex` still
/// holds the lock for the whole walk, so these retries still end `locked`.
pub(crate) fn acquire_lock(vault: &Path) -> Result<File, Error> {
    const ATTEMPTS: u32 = 8;
    const RETRY: Duration = Duration::from_millis(10);
    for attempt in 0..ATTEMPTS {
        match try_lock(vault)? {
            Some(file) => return Ok(file),
            None if attempt + 1 < ATTEMPTS => thread::sleep(RETRY),
            None => break,
        }
    }
    Err(Error::locked("vault reindex lock is held")
        .context("[Vault::reindex] acquire reindex lock"))
}

/// `try_lock` of `reindex.lock`. `None` if another `reindex` holds it.
pub(crate) fn try_lock(vault: &Path) -> Result<Option<File>, Error> {
    let scheda = vault.join(".scheda");
    fs::create_dir_all(&scheda).context("[Reindex] create .scheda")?;
    store::restrict_dir(&scheda)?;
    let lock_path = scheda.join(LOCK_FILE);
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .context("[Reindex] open reindex lock")?;
    store::restrict_file(&lock_path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => {
            Err(Error::new(io_code(&err), err.to_string()).context("[Reindex] lock reindex lock"))
        }
    }
}

/// Whether `{vault}/.scheda/reindex/active` exists.
pub(crate) fn active_exists(vault: &Path) -> bool {
    active_path(vault).is_file()
}

/// Write `reindex/active` with the generation `N+1` will use.
pub(crate) fn write_active(vault: &Path, generation: u64) -> Result<(), Error> {
    let dir = reindex_dir(vault);
    fs::create_dir_all(&dir).context("[Reindex] create reindex dir")?;
    store::restrict_dir(&dir)?;
    let queue = queue_dir(vault);
    fs::create_dir_all(&queue).context("[Reindex] create queue dir")?;
    store::restrict_dir(&queue)?;
    let path = active_path(vault);
    store::atomic_write(&path, format!("{generation}\n").as_bytes())?;
    store::restrict_file(&path)
}

/// Delete `reindex/active` after publish or on failure cleanup.
pub(crate) fn clear_active(vault: &Path) -> Result<(), Error> {
    match fs::remove_file(active_path(vault)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("[Reindex] remove active"),
    }
}

/// Delete a stale `active` marker and every queue file. Live `N` already
/// holds those writes; do not apply the queue.
pub(crate) fn clear_stale(vault: &Path) -> Result<(), Error> {
    clear_active(vault)?;
    let queue = queue_dir(vault);
    match fs::read_dir(&queue) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.context("[Reindex] read stale queue entry")?;
                let path = entry.path();
                let result = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
                match result {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err).context("[Reindex] remove stale queue file"),
                }
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("[Reindex] read queue dir"),
    }
}

/// Whether this collection's writes must enqueue while `active` exists.
#[must_use]
pub(crate) fn should_enqueue(decl: &CollectionDecl) -> bool {
    !decl.encrypted && (decl.any_indexed() || decl.kind == Kind::Journal)
}

/// Write one queue file for `(collection, id)` when `active` exists.
pub(crate) fn enqueue(vault: &Path, decl: &CollectionDecl, id: &str) -> Result<(), Error> {
    if !active_exists(vault) || !should_enqueue(decl) {
        return Ok(());
    }
    write_queue_file(vault, &decl.name, id)
}

/// Journal `id`s a pending restore may move, read before it runs. Empty
/// when no rebuild is active; `document` / `singleton` ids come from the
/// file name and need no snapshot.
pub(crate) struct RestoreSnapshot {
    ids: BTreeSet<(String, String)>,
}

/// Journal `id`s currently in recorded `*.jsonl` segments. Empty when
/// `reindex/active` is absent, so the hot path does not read those files.
pub(crate) fn snapshot_restored(
    vault: &Path,
    decls: &Decls,
    rels: &[String],
) -> Result<RestoreSnapshot, Error> {
    if !active_exists(vault) {
        return Ok(RestoreSnapshot {
            ids: BTreeSet::new(),
        });
    }
    let mut ids = BTreeSet::new();
    for rel in rels {
        for (collection, id) in ids_from_restored(vault, decls, rel)? {
            let Some(decl) = decls.get(&collection) else {
                continue;
            };
            if decl.kind == Kind::Journal {
                ids.insert((collection, id));
            }
        }
    }
    Ok(RestoreSnapshot { ids })
}

/// Enqueue one file per restored `(collection, id)` after discarding crash
/// leftovers, so `N+1` sees the restore.
///
/// The union of [`RestoreSnapshot`] and what the files hold after restore.
/// Catch-up re-reads each id from disk, so a redundant queue file is
/// harmless; a missing one publishes a generation that still holds lines
/// the restore deleted. This costs one queue file per line of a restored
/// open segment, and only runs on crash leftovers while a rebuild is active.
pub(crate) fn enqueue_restored(
    vault: &Path,
    decls: &Decls,
    rels: &[String],
    snapshot: &RestoreSnapshot,
) -> Result<(), Error> {
    if !active_exists(vault) {
        return Ok(());
    }
    let mut seen = BTreeSet::new();
    for (collection, id) in &snapshot.ids {
        enqueue_restored_id(vault, decls, collection, id, &mut seen)?;
    }
    for rel in rels {
        for (collection, id) in ids_from_restored(vault, decls, rel)? {
            enqueue_restored_id(vault, decls, &collection, &id, &mut seen)?;
        }
    }
    Ok(())
}

fn enqueue_restored_id(
    vault: &Path,
    decls: &Decls,
    collection: &str,
    id: &str,
    seen: &mut BTreeSet<(String, String)>,
) -> Result<(), Error> {
    if !seen.insert((collection.to_owned(), id.to_owned())) {
        return Ok(());
    }
    let Some(decl) = decls.get(collection) else {
        return Ok(());
    };
    enqueue(vault, decl, id)
}

/// Queue file names currently in `reindex/queue/`.
pub(crate) fn list_queue(vault: &Path) -> Result<Vec<PathBuf>, Error> {
    let queue = queue_dir(vault);
    let entries = match fs::read_dir(&queue) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("[Reindex] read queue dir"),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.context("[Reindex] read queue entry")?;
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| !name.starts_with('.'))
        {
            out.push(path);
        }
    }
    Ok(out)
}

/// Unlink `path`, then parse its body. Skip (return `None`) when the JSON
/// or slugs are illegal — never treat `collection` / `id` as a filesystem
/// path.
pub(crate) fn take_queue_file(path: &Path) -> Result<Option<(String, String)>, Error> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("[Reindex] read queue file"),
    };
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).context("[Reindex] unlink queue file"),
    }
    Ok(parse_queue_body(&raw))
}

/// `true` when `err` means the Record file (or journal line) is gone.
#[must_use]
pub(crate) fn is_absent(err: &Error) -> bool {
    matches!(err.known_code(), Some(Code::IoNotFound | Code::NotFound))
}

fn write_queue_file(vault: &Path, collection: &str, id: &str) -> Result<(), Error> {
    let queue = queue_dir(vault);
    fs::create_dir_all(&queue).context("[Reindex] create queue dir")?;
    store::restrict_dir(&queue)?;
    let name = queue_file_name(collection, id);
    let path = queue.join(name);
    let body = QueueEntry {
        collection: collection.to_owned(),
        id: id.to_owned(),
    };
    let bytes = serde_json::to_vec(&body).context(Code::Invalid, "[Reindex] encode queue file")?;
    store::atomic_write(&path, &bytes)?;
    store::restrict_file(&path)
}

fn parse_queue_body(raw: &[u8]) -> Option<(String, String)> {
    let parsed: QueueEntry = serde_json::from_slice(raw).ok()?;
    if id::require_slug("collection", &parsed.collection).is_err() {
        return None;
    }
    if id::require_document_id(&parsed.id).is_err() {
        return None;
    }
    Some((parsed.collection, parsed.id))
}

fn queue_file_name(collection: &str, id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(collection.as_bytes());
    hasher.update([0u8]);
    hasher.update(id.as_bytes());
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 0x0f)] as char);
    }
    out
}

fn reindex_dir(vault: &Path) -> PathBuf {
    vault.join(".scheda").join(REINDEX_DIR)
}

fn active_path(vault: &Path) -> PathBuf {
    reindex_dir(vault).join(ACTIVE_FILE)
}

fn queue_dir(vault: &Path) -> PathBuf {
    reindex_dir(vault).join(QUEUE_DIR)
}

fn ids_from_restored(
    vault: &Path,
    decls: &Decls,
    rel: &str,
) -> Result<Vec<(String, String)>, Error> {
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() || rel.starts_with('.') && !rel.starts_with(".isolated/") {
        return Ok(Vec::new());
    }
    let rest = rel.strip_prefix(".isolated/").unwrap_or(rel);
    let Some((name, file)) = rest.split_once('/') else {
        return Ok(Vec::new());
    };
    let Some(decl) = decls.get(name) else {
        return Ok(Vec::new());
    };
    match decl.kind {
        Kind::Document => {
            if file.starts_with('.') {
                return Ok(Vec::new());
            }
            if id::require_document_id(file).is_err() {
                return Ok(Vec::new());
            }
            Ok(vec![(decl.name.clone(), file.to_owned())])
        }
        Kind::Singleton => {
            let Some(id) = decl.singleton_id.as_deref() else {
                return Ok(Vec::new());
            };
            if file != id {
                return Ok(Vec::new());
            }
            Ok(vec![(decl.name.clone(), id.to_owned())])
        }
        Kind::Journal => {
            if !file.ends_with(".jsonl") {
                return Ok(Vec::new());
            }
            let path = vault.join(rel);
            if !path.is_file() {
                return Ok(Vec::new());
            }
            crate::journal::ids_in_segment(&path)
                .map(|ids| ids.into_iter().map(|id| (decl.name.clone(), id)).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            crate::git::isolate_test_git_env();
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let base = std::env::var_os("TEST_TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            let tid = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
            let root = base.join(format!("scheda-reindex-{}-{n}-{tid}", std::process::id()));
            fs::create_dir_all(root.join(".scheda")).unwrap();
            Self { root }
        }

        fn vault(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn decl(name: &str, kind: Kind) -> CollectionDecl {
        CollectionDecl {
            name: name.to_owned(),
            kind,
            version: 1,
            segment_max_bytes: None,
            segment_max_lines: None,
            segment_rotate: None,
            singleton_id: None,
            isolated: false,
            encrypted: false,
            exclude_lexical_index: false,
            exclude_vector_index: false,
        }
    }

    #[test]
    fn queue_file_round_trips_and_collapses_duplicates() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        write_active(vault, 2).unwrap();
        let notes = decl("c", Kind::Document);
        enqueue(vault, &notes, "alpha").unwrap();
        enqueue(vault, &notes, "alpha").unwrap();
        let files = list_queue(vault).unwrap();
        assert_eq!(files.len(), 1);
        let taken = take_queue_file(&files[0]).unwrap();
        assert_eq!(
            taken.as_ref().map(|(c, i)| (c.as_str(), i.as_str())),
            Some(("c", "alpha"))
        );
        assert!(list_queue(vault).unwrap().is_empty());
    }

    #[test]
    fn queue_file_accepts_hierarchical_document_id() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        write_active(vault, 2).unwrap();
        let notes = decl("c", Kind::Document);
        enqueue(vault, &notes, "work/meeting").unwrap();
        let files = list_queue(vault).unwrap();
        assert_eq!(files.len(), 1);
        let taken = take_queue_file(&files[0]).unwrap();
        assert_eq!(
            taken.as_ref().map(|(c, i)| (c.as_str(), i.as_str())),
            Some(("c", "work/meeting"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn queue_and_active_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let vault = scratch.vault();
        write_active(vault, 3).unwrap();
        let notes = decl("c", Kind::Document);
        enqueue(vault, &notes, "id").unwrap();
        let reindex = vault.join(".scheda").join("reindex");
        let mode = fs::metadata(&reindex).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let active_mode = fs::metadata(active_path(vault))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(active_mode, 0o600);
        let queue_file = &list_queue(vault).unwrap()[0];
        let queue_mode = fs::metadata(queue_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(queue_mode, 0o600);
        let name = queue_file.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, queue_file_name("c", "id"));
        assert_eq!(name.len(), 64);
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    #[test]
    fn illegal_queue_json_is_skipped_not_a_path() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        write_active(vault, 2).unwrap();
        let path = queue_dir(vault).join("deadbeef");
        fs::write(&path, br#"{"collection":"../etc","id":"passwd"}"#).unwrap();
        let taken = take_queue_file(&path).unwrap();
        assert!(taken.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn no_active_means_no_queue_file() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        let notes = decl("c", Kind::Document);
        enqueue(vault, &notes, "alpha").unwrap();
        assert!(list_queue(vault).unwrap().is_empty());
    }

    #[test]
    fn encrypted_collection_does_not_enqueue() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        write_active(vault, 2).unwrap();
        let mut sealed = decl("c", Kind::Document);
        sealed.encrypted = true;
        enqueue(vault, &sealed, "alpha").unwrap();
        assert!(list_queue(vault).unwrap().is_empty());
    }

    #[test]
    fn journal_with_both_legs_excluded_still_enqueues() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        write_active(vault, 2).unwrap();
        let mut log = decl("j", Kind::Journal);
        log.exclude_lexical_index = true;
        log.exclude_vector_index = true;
        enqueue(vault, &log, "line-1").unwrap();
        assert_eq!(list_queue(vault).unwrap().len(), 1);
    }

    #[test]
    fn reindex_lock_is_flock_not_under_index() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        let held = acquire_lock(vault).unwrap();
        assert!(vault.join(".scheda").join("reindex.lock").is_file());
        assert!(
            !vault
                .join(".scheda")
                .join("index")
                .join("reindex.lock")
                .exists()
        );
        assert!(try_lock(vault).unwrap().is_none());
        drop(held);
        // `try_lock` can still see WouldBlock on overlay/WSL after close;
        // `acquire_lock` is what `reindex` uses.
        acquire_lock(vault).unwrap();
    }

    #[test]
    fn publish_window_override_restores_on_drop() {
        assert_eq!(publish_window(), PUBLISH_WINDOW);
        {
            let _guard = set_publish_window(Duration::from_millis(10));
            assert_eq!(publish_window(), Duration::from_millis(10));
        }
        assert_eq!(publish_window(), PUBLISH_WINDOW);
    }
}
