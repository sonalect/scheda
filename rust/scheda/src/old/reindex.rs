//! `{vault}/.scheda/reindex.lock`: one generation build at a time.
//!
//! The lock is a flock, kernel-released on process death, same as
//! `write.lock`. `reindex` and the background indexer's rebuild hold it
//! while they fill a new generation, so `open` does not prune that
//! directory as an orphan and two builds do not race.

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::Error;
use crate::error::{ContextIo, io_code};
use crate::schema::{Decls, Kind};
use crate::store;

const LOCK_FILE: &str = "reindex.lock";
/// Catch-up queue and `active` marker of releases before Index format 5.
const LEGACY_DIR: &str = "reindex";

/// `{vault}/.scheda/reindex.lock` as a `try_lock` flock. Not a pid-file.
///
/// A few short retries absorb a stale `WouldBlock` after the previous
/// holder closed the fd (overlay / WSL). A second live `reindex` still
/// holds the lock for the whole walk, so these retries still end `locked`.
pub(crate) fn acquire_lock(vault: &Path) -> Result<store::Flock, Error> {
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
pub(crate) fn try_lock(vault: &Path) -> Result<Option<store::Flock>, Error> {
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
        Ok(()) => Ok(Some(store::Flock::held(file))),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => {
            Err(Error::new(io_code(&err), err.to_string()).context("[Reindex] lock reindex lock"))
        }
    }
}

#[cfg(not(feature = "find"))]
/// `true` when `err` means the Record file (or journal line) is gone.
#[must_use]
pub(crate) fn is_absent(err: &Error) -> bool {
    matches!(
        err.known_code(),
        Some(crate::error::Code::IoNotFound | crate::error::Code::NotFound)
    )
}

/// Remove the catch-up queue an older release left under
/// `{vault}/.scheda/reindex/`. Marks replaced it: nothing reads it.
pub(crate) fn remove_leftovers(vault: &Path) -> Result<(), Error> {
    match fs::remove_dir_all(vault.join(".scheda").join(LEGACY_DIR)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("[Reindex] remove leftover queue"),
    }
}

/// `(collection, id)` of every journal line in the `*.jsonl` segments among
/// `rels` (vault-relative paths), as the files hold them now.
pub(crate) fn journal_ids_in(
    vault: &Path,
    decls: &Decls,
    rels: &[String],
) -> Result<BTreeSet<(String, String)>, Error> {
    let mut ids = BTreeSet::new();
    for rel in rels {
        for (collection, id) in journal_ids_in_file(vault, decls, rel)? {
            ids.insert((collection, id));
        }
    }
    Ok(ids)
}

/// The journal ids in one restored path; empty when the path is not a
/// `*.jsonl` segment of a journal collection or no longer exists.
fn journal_ids_in_file(
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
    if decl.kind != Kind::Journal || !file.ends_with(".jsonl") {
        return Ok(Vec::new());
    }
    let path = vault.join(rel);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    crate::journal::ids_in_segment(&path)
        .map(|ids| ids.into_iter().map(|id| (decl.name.clone(), id)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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
    fn leftover_queue_of_an_older_release_is_removed() {
        let scratch = Scratch::new();
        let vault = scratch.vault();
        let queue = vault.join(".scheda").join("reindex").join("queue");
        fs::create_dir_all(&queue).unwrap();
        fs::write(queue.join("entry"), b"{}").unwrap();
        fs::write(vault.join(".scheda").join("reindex").join("active"), b"2").unwrap();
        remove_leftovers(vault).unwrap();
        assert!(!vault.join(".scheda").join("reindex").exists());
        remove_leftovers(vault).unwrap();
    }
}
