//! Disposable Index under `{vault}/.scheda/index`.
//!
//! Live layout is generational: `{vault}/.scheda/index/current` names the live
//! generation directory, and `{vault}/.scheda/index/<generation>/` holds the
//! `format` file, `marks/`, `lexical/`, `vector/`, edges, ord, and `journal/`. A handle
//! resolves `current` once at open and keeps that generation. A reader older
//! than one `reindex` cycle may find its files gone; `find` / `search` then
//! fail with `index_missing` (no `format` file) or `index_lexical` (Tantivy
//! directory gone). Incremental writes stay in the live generation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::Error;
#[cfg(not(feature = "find"))]
use crate::ReindexResult;
use crate::error::{Code, Context, ContextAs, ContextIo, bail};
#[cfg(not(feature = "find"))]
use crate::journal;
use crate::schema::{self, Decls, Kind};
use crate::store;

/// On-disk Index format. Bumped to 4 when the Index became generational,
/// to 5 when each generation recorded the commit every leg has reached
/// (`<gen>/marks/`), and to 6 when `integer` / `number` columns got the
/// order-preserving `num` field that `range` reads. An older `format` file
/// is `index_version`; the background indexer rebuilds it into a new
/// generation.
pub(crate) const FORMAT_VERSION: u32 = 6;
const FORMAT_FILE: &str = "format";
const CURRENT_FILE: &str = "current";
const JOURNAL_DIR: &str = "journal";
const IDS_FILE: &str = "ids";
const IDS_LOCK_FILE: &str = "ids.lock";
const LEGACY_FILES: &[&str] = &["format", "stale", "edges.jsonl", "ord.json", "txn"];
const LEGACY_DIRS: &[&str] = &["lexical", "vector", "journal"];

/// `{vault}/.scheda/index` plus the generation this handle is bound to.
pub(crate) struct IndexHandle {
    /// `{vault}/.scheda/index` (the pointer directory).
    pub(crate) root: PathBuf,
    dir: Mutex<PathBuf>,
}

impl IndexHandle {
    /// Empty handle for unit stubs.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            root: PathBuf::new(),
            dir: Mutex::new(PathBuf::new()),
        }
    }

    /// Generation directory this handle uses.
    pub(crate) fn dir(&self) -> Result<PathBuf, Error> {
        Ok(crate::error::mutex(&self.dir, "[IndexHandle] lock generation")?.clone())
    }

    /// Bind this handle to `dir` after a `reindex` swap.
    pub(crate) fn set_dir(&self, dir: PathBuf) -> Result<(), Error> {
        *crate::error::mutex(&self.dir, "[IndexHandle] lock generation")? = dir;
        Ok(())
    }
}

/// In-memory journal `(id → segment path)` maps, keyed by collection name.
///
/// A collection is absent until a map file has been written (`reindex` or the
/// first journal write). Absent means lookup must scan files.
#[derive(Clone, Debug, Default)]
pub(crate) struct JournalIdMaps {
    collections: BTreeMap<String, BTreeMap<String, String>>,
    segment_lines: BTreeMap<PathBuf, u64>,
    /// Length and mtime of each `ids` file when this map last read or
    /// wrote it. Another handle or process changes the file; a different
    /// stamp means re-read before trusting "unknown id".
    stamps: BTreeMap<String, Stamp>,
}

type Stamp = (u64, Option<std::time::SystemTime>);

fn stamp_of(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

#[derive(Debug)]
pub(crate) enum MapLookup<'a> {
    MissingMap,
    UnknownId,
    Path(&'a str),
}

#[derive(Debug, Deserialize, Serialize)]
struct IdMapLine {
    id: String,
    path: String,
}

impl JournalIdMaps {
    pub(crate) fn lookup(&self, collection: &str, id: &str) -> MapLookup<'_> {
        match self.collections.get(collection) {
            None => MapLookup::MissingMap,
            Some(ids) => match ids.get(id) {
                Some(path) => MapLookup::Path(path.as_str()),
                None => MapLookup::UnknownId,
            },
        }
    }

    pub(crate) fn contains(&self, collection: &str, id: &str) -> Option<bool> {
        self.collections
            .get(collection)
            .map(|ids| ids.contains_key(id))
    }

    fn put(&mut self, collection: String, id: String, path: String) {
        self.collections
            .entry(collection)
            .or_default()
            .insert(id, path);
    }

    fn replace_collection(&mut self, collection: String, ids: BTreeMap<String, String>) {
        self.collections.insert(collection, ids);
    }

    pub(crate) fn forget_id(&mut self, collection: &str, id: &str) {
        if let Some(ids) = self.collections.get_mut(collection) {
            ids.remove(id);
        }
    }

    pub(crate) fn add_segment_line(&mut self, path: &Path) {
        if let Some(n) = self.segment_lines.get_mut(path) {
            *n = n.saturating_add(1);
        }
    }

    /// Forget every cached segment line count; each is recounted on its
    /// next use. Needed after a restore truncated or deleted segments.
    pub(crate) fn clear_segment_lines(&mut self) {
        self.segment_lines.clear();
    }

    pub(crate) fn ensure_segment_lines(
        &mut self,
        path: &Path,
        count: impl FnOnce(&Path) -> Result<u64, Error>,
    ) -> Result<u64, Error> {
        if let Some(n) = self.segment_lines.get(path) {
            return Ok(*n);
        }
        let n = count(path)?;
        self.segment_lines.insert(path.to_path_buf(), n);
        Ok(n)
    }
}

/// Resolve or create this Vault's Index generation and bind a handle to it.
pub(crate) fn bind(root: &Path) -> Result<IndexHandle, Error> {
    let dir = bind_generation(root)?;
    Ok(IndexHandle {
        root: root.to_path_buf(),
        dir: Mutex::new(dir),
    })
}

fn bind_generation(root: &Path) -> Result<PathBuf, Error> {
    if is_legacy_layout(root) {
        return Ok(root.to_path_buf());
    }
    match read_current(root)? {
        Some(n) => Ok(root.join(n.to_string())),
        None => {
            let dir = create_generation(root, 1)?;
            write_current(root, 1)?;
            Ok(dir)
        }
    }
}

fn is_legacy_layout(root: &Path) -> bool {
    if root.join(CURRENT_FILE).is_file() {
        return false;
    }
    root.join(FORMAT_FILE).is_file()
        || root.join("lexical").is_dir()
        || root.join(JOURNAL_DIR).is_dir()
        || root.join("vector").is_dir()
}

/// Generation directory `current` names now, or `None` without a pointer.
///
/// Flag file a handle leaves in a generation whose Tantivy or vector files
/// failed to read; the lexical-role indexer, in whatever process, then
/// checks the generation and rebuilds it if it is damaged.
#[cfg(feature = "find")]
const DAMAGED_FILE: &str = "damaged";

/// Flag generation `dir` as possibly damaged. Best effort: the error that
/// prompted it is what the caller reports.
#[cfg(feature = "find")]
pub(crate) fn flag_damaged(dir: &Path) {
    let path = dir.join(DAMAGED_FILE);
    if dir.is_dir() && store::atomic_write(&path, b"").is_ok() {
        let _ = store::restrict_file(&path);
    }
}

/// Whether generation `dir` carries the flag of [`flag_damaged`].
#[cfg(feature = "find")]
pub(crate) fn flagged_damaged(dir: &Path) -> bool {
    dir.join(DAMAGED_FILE).is_file()
}

/// Drop the flag of [`flag_damaged`]: a check found nothing wrong.
#[cfg(feature = "find")]
pub(crate) fn clear_damaged(dir: &Path) -> Result<(), Error> {
    match fs::remove_file(dir.join(DAMAGED_FILE)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("[Index] clear damaged flag"),
    }
}

/// Whether `err` says Index files could not be read as written: Tantivy,
/// the vector dump, or a decode of either. Not a format change, a missing
/// Index, or a caller callback's failure.
#[cfg(feature = "find")]
pub(crate) fn is_damage(err: &Error) -> bool {
    matches!(
        err.known_code(),
        Some(Code::IndexLexical | Code::IndexCorrupt | Code::IndexVector)
    )
}

/// A handle reads this to follow a generation another handle or process
/// published.
#[cfg(feature = "find")]
pub(crate) fn current_dir(root: &Path) -> Result<Option<PathBuf>, Error> {
    Ok(read_current(root)?.map(|n| root.join(n.to_string())))
}

fn read_current(root: &Path) -> Result<Option<u64>, Error> {
    let path = root.join(CURRENT_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).context("[Index] read current")?;
    match parse_generation(text.trim()) {
        Some(n) => Ok(Some(n)),
        None => Ok(None),
    }
}

fn parse_generation(name: &str) -> Option<u64> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if name.len() > 1 && name.starts_with('0') {
        return None;
    }
    let n: u64 = name.parse().ok()?;
    (n >= 1).then_some(n)
}

fn write_current(root: &Path, n: u64) -> Result<(), Error> {
    let path = root.join(CURRENT_FILE);
    store::atomic_write(&path, format!("{n}\n").as_bytes())?;
    store::restrict_file(&path)
}

fn create_generation(root: &Path, n: u64) -> Result<PathBuf, Error> {
    let dir = root.join(n.to_string());
    fs::create_dir_all(&dir).context("[Index] create generation")?;
    store::restrict_dir(&dir)?;
    Ok(dir)
}

/// A number above `current`, every generation directory present, and
/// `previous`: a new generation never reuses a directory some handle may
/// still be writing, even after `{vault}/.scheda/` was deleted under it.
fn next_generation(root: &Path, previous: Option<u64>) -> Result<u64, Error> {
    let mut top = read_current(root)?.unwrap_or(0).max(previous.unwrap_or(0));
    match fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.context("[Index] read generation entry")?;
                if let Some(n) = entry.file_name().to_str().and_then(parse_generation) {
                    top = top.max(n);
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).context("[Index] read generations"),
    }
    Ok(top.saturating_add(1))
}

/// Build a new generation directory without publishing `current`.
///
/// Used by `reindex` and the indexer's rebuild. `previous` is the
/// generation the caller's handle answers from: it is neither pruned nor
/// reused. A caller that stops before [`publish_generation`] leaves the
/// previous generation live; the unfinished directory is an orphan removed
/// by the next build or by `open` when the write lock and `reindex.lock`
/// are free.
pub(crate) fn start_generation(root: &Path, previous: &Path) -> Result<PathBuf, Error> {
    let previous = if previous.parent() == Some(root) {
        generation_number(previous).ok()
    } else {
        None
    };
    prune_generations_keeping(root, previous)?;
    let n = next_generation(root, previous)?;
    create_generation(root, n)
}

/// Generation number named by `dir`'s last component.
pub(crate) fn generation_number(dir: &Path) -> Result<u64, Error> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(parse_generation);
    match name {
        Some(n) => Ok(n),
        None => Err(
            Error::index_corrupt("index generation name is not a decimal integer")
                .context("[Index] generation name"),
        ),
    }
}

/// Atomically point `current` at `dir`, keep one previous generation, and
/// drop anything older plus leftover files of a pre-generational Index.
pub(crate) fn publish_generation(root: &Path, dir: &Path) -> Result<(), Error> {
    let n = generation_number(dir)?;
    write_current(root, n)?;
    prune_generations(root)?;
    remove_legacy_layout_files(root)?;
    restrict_index_tree(dir)
}

/// Delete generation directories other than the live one and, when it
/// exists, the immediately previous one.
pub(crate) fn prune_generations(root: &Path) -> Result<(), Error> {
    prune_generations_keeping(root, None)
}

/// [`prune_generations`], also keeping generation `also`.
fn prune_generations_keeping(root: &Path, also: Option<u64>) -> Result<(), Error> {
    let live = read_current(root)?;
    let mut keep: BTreeSet<u64> = also.into_iter().collect();
    if let Some(n) = live {
        keep.insert(n);
        if n > 1 {
            keep.insert(n - 1);
        }
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).context("[Index] read generations"),
    };
    for entry in entries {
        let entry = entry.context("[Index] read generation entry")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(n) = parse_generation(name) else {
            continue;
        };
        if keep.contains(&n) {
            continue;
        }
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.context("[Index] remove orphan generation")?;
    }
    Ok(())
}

fn remove_legacy_layout_files(root: &Path) -> Result<(), Error> {
    for name in LEGACY_FILES {
        let path = root.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("[Index] remove legacy index file"),
        }
    }
    for name in LEGACY_DIRS {
        let path = root.join(name);
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("[Index] remove legacy index dir"),
        }
    }
    Ok(())
}

pub(crate) fn load_journal_ids(index_dir: &Path, decls: &Decls) -> Result<JournalIdMaps, Error> {
    let mut maps = JournalIdMaps::default();
    for decl in decls.collections() {
        if decl.encrypted || decl.kind != Kind::Journal {
            continue;
        }
        reload_ids(index_dir, &mut maps, decl)?;
    }
    Ok(maps)
}

/// Re-read `decl`'s `ids` file into `maps`, or drop the collection from
/// `maps` when the file is gone or unreadable (lookups then scan).
fn reload_ids(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    decl: &schema::CollectionDecl,
) -> Result<(), Error> {
    let path = ids_path(index_dir, &decl.name);
    let stamp = stamp_of(&path);
    let ids = if path.is_file() {
        read_ids_file(&path)?
    } else {
        None
    };
    match ids {
        Some(mut ids) => {
            ids.retain(|id, rel| id_map_line_ok(decl, id, rel));
            maps.replace_collection(decl.name.clone(), ids);
        }
        None => {
            maps.collections.remove(&decl.name);
        }
    }
    match stamp {
        Some(stamp) => maps.stamps.insert(decl.name.clone(), stamp),
        None => maps.stamps.remove(&decl.name),
    };
    Ok(())
}

/// Bring `decl`'s map up to date with its `ids` file when another handle
/// or process changed the file since this map last saw it. Costs one
/// `stat` when nothing changed.
pub(crate) fn refresh_ids(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    decl: &schema::CollectionDecl,
) -> Result<(), Error> {
    let path = ids_path(index_dir, &decl.name);
    if stamp_of(&path) == maps.stamps.get(&decl.name).copied() {
        return Ok(());
    }
    reload_ids(index_dir, maps, decl)
}

/// Hold `{index_dir}/journal/{collection}/ids.lock` while one change to
/// that `ids` file is made. Every writer of the file — a write on any
/// handle, the indexer in any process — takes it, never while waiting for
/// another lock, so the read-change-write of one cannot drop the line
/// another appended.
fn lock_ids(index_dir: &Path, collection: &str) -> Result<fs::File, Error> {
    let path = ids_path(index_dir, collection).with_file_name(IDS_LOCK_FILE);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("[Index::lock_ids] create dir")?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .context("[Index::lock_ids] open ids lock")?;
    store::restrict_file_below(index_dir, &path)?;
    file.lock().context("[Index::lock_ids] lock ids")?;
    Ok(file)
}

/// Re-read the `ids` file of `collection` into `maps` when its stamp
/// changed, for callers that hold [`lock_ids`] and know only the name.
fn refresh_named(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
) -> Result<(), Error> {
    let path = ids_path(index_dir, collection);
    let stamp = stamp_of(&path);
    if stamp == maps.stamps.get(collection).copied() {
        return Ok(());
    }
    if let Some(ids) = if path.is_file() {
        read_ids_file(&path)?
    } else {
        None
    } {
        let ids = ids
            .into_iter()
            .filter(|(id, rel)| {
                crate::id::require_slug("id", id).is_ok() && id_map_rel_ok_for_name(collection, rel)
            })
            .collect();
        maps.replace_collection(collection.to_owned(), ids);
    }
    if let Some(stamp) = stamp {
        maps.stamps.insert(collection.to_owned(), stamp);
    }
    Ok(())
}

fn note_stamp(index_dir: &Path, maps: &mut JournalIdMaps, collection: &str) {
    match stamp_of(&ids_path(index_dir, collection)) {
        Some(stamp) => maps.stamps.insert(collection.to_owned(), stamp),
        None => maps.stamps.remove(collection),
    };
}

/// Fill `index_dir` with journal maps and the format file. Does not swap
/// `current`; the caller publishes after the search engines are rebuilt.
#[cfg(not(feature = "find"))]
pub(crate) fn rebuild(
    vault: &Path,
    decls: &Decls,
    index_dir: &Path,
) -> Result<(ReindexResult, JournalIdMaps), Error> {
    clear_index_dir(index_dir)?;
    let mut records_read = 0u64;
    let mut skipped_invalid = 0u64;
    let mut segments_seen = 0u64;
    let mut maps = JournalIdMaps::default();
    for decl in decls.collections().filter(|decl| !decl.encrypted) {
        match decl.kind {
            Kind::Journal => {
                let (records, invalid, segments, ids) = journal::reindex_map(vault, decl)?;
                records_read += records;
                skipped_invalid += invalid;
                segments_seen += segments;
                write_ids_file(index_dir, &decl.name, &ids)?;
                maps.replace_collection(decl.name.clone(), ids);
            }
            Kind::Document | Kind::Singleton => {
                if !decl.any_indexed() {
                    continue;
                }
                let (records, invalid) = store::reindex_stats(vault, decl)?;
                records_read += records;
                skipped_invalid += invalid;
            }
        }
    }
    write_format(index_dir)?;
    remove_legacy_isolated_index(vault)?;
    restrict_index_tree(index_dir)?;
    Ok((
        ReindexResult::default()
            .with_records_read(records_read)
            .with_skipped_invalid(skipped_invalid)
            .with_segments_seen(segments_seen),
        maps,
    ))
}

pub(crate) fn remove_legacy_isolated_index(vault: &Path) -> Result<(), Error> {
    let leftover = crate::isolated::root(vault).join(".scheda");
    match fs::remove_dir_all(&leftover) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("[Index::rebuild] remove leftover isolated index"),
    }
}

/// Owner-only modes on `{vault}/.scheda/` after Index writes, including files
/// Tantivy created that this crate did not open. `index_dir` may be the Index
/// root or a generation subdirectory; the walk always starts at `.scheda/`.
pub(crate) fn restrict_index_tree(index_dir: &Path) -> Result<(), Error> {
    match scheda_dir(index_dir) {
        Some(scheda) if !scheda.as_os_str().is_empty() => store::restrict_tree(scheda),
        _ => Ok(()),
    }
}

fn scheda_dir(index_dir: &Path) -> Option<&Path> {
    let parent = index_dir.parent()?;
    if parent.file_name().is_some_and(|name| name == "index") {
        parent.parent()
    } else {
        Some(parent)
    }
}

/// Remember in `into` (and its `ids` files under `index_dir`) every line
/// `from` maps that `into` does not, for collections `into` knows. One
/// write per collection.
#[cfg(feature = "find")]
pub(crate) fn carry_over(
    index_dir: &Path,
    from: &JournalIdMaps,
    into: &mut JournalIdMaps,
) -> Result<(), Error> {
    for (collection, ids) in &from.collections {
        let missing: Vec<(String, String)> = ids
            .iter()
            .filter(|(id, _)| into.contains(collection, id) == Some(false))
            .map(|(id, rel)| (id.clone(), rel.clone()))
            .collect();
        if !missing.is_empty() {
            apply_id_changes(index_dir, into, collection, &missing, &[])?;
        }
    }
    Ok(())
}

/// Remember one line of `collection`'s `ids` map.
pub(crate) fn remember_id(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    id: &str,
    rel: &str,
) -> Result<(), Error> {
    apply_id_changes(
        index_dir,
        maps,
        collection,
        &[(id.to_owned(), rel.to_owned())],
        &[],
    )
}

/// Make every change of `remember` (id and segment path) and `forget` (ids)
/// to `collection`'s `ids` map and file under one `ids.lock`.
///
/// Lines the map did not know are appended in one write with one `fsync`;
/// a line that moved to another segment, or any id to forget, rewrites the
/// file once. A change the map already holds writes nothing, so applying
/// the same batch twice is harmless. The files written get owner-only modes
/// at once, without a walk of the Index tree. A change to a generation a
/// `reindex` has since replaced is repeated on the live one.
pub(crate) fn apply_id_changes(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    remember: &[(String, String)],
    forget: &[String],
) -> Result<(), Error> {
    let live = {
        let _lock = lock_ids(index_dir, collection)?;
        write_id_changes(index_dir, maps, collection, remember, forget)?;
        live_after(index_dir)?
    };
    match live {
        Some(live) => forward_id_changes(&live, collection, remember, forget),
        None => Ok(()),
    }
}

/// The body of [`apply_id_changes`], under `ids.lock`.
fn write_id_changes(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    remember: &[(String, String)],
    forget: &[String],
) -> Result<(), Error> {
    refresh_named(index_dir, maps, collection)?;
    let mut appended: Vec<(&str, &str)> = Vec::new();
    let mut rewrite = !forget.is_empty();
    for (id, rel) in remember {
        match maps.lookup(collection, id) {
            MapLookup::Path(old) => rewrite = rewrite || old != rel.as_str(),
            MapLookup::UnknownId | MapLookup::MissingMap => appended.push((id, rel)),
        }
        maps.put(collection.to_owned(), id.clone(), rel.clone());
    }
    for id in forget {
        maps.forget_id(collection, id);
    }
    if rewrite {
        let ids = maps
            .collections
            .get(collection)
            .cloned()
            .unwrap_or_default();
        write_ids_file(index_dir, collection, &ids)?;
    } else if !appended.is_empty() {
        append_ids_lines(index_dir, collection, &appended)?;
    }
    note_stamp(index_dir, maps, collection);
    Ok(())
}

/// Whether `rel` is a Vault-relative segment under this collection directory.
///
/// Rejects absolute paths, `..`, and a prefix that is not this collection
/// (plain or `.isolated/`). Used when reading the journal `id` map so a
/// hand-edited line cannot name a file outside the collection.
pub(crate) fn id_map_rel_ok(decl: &schema::CollectionDecl, rel: &str) -> bool {
    let dir = schema::collection_dir(&decl.name);
    let prefix = if decl.isolated {
        format!(".isolated/{dir}/")
    } else {
        format!("{dir}/")
    };
    id_map_rel_has_prefix(rel, &prefix)
}

fn id_map_line_ok(decl: &schema::CollectionDecl, id: &str, rel: &str) -> bool {
    crate::id::require_slug("id", id).is_ok() && id_map_rel_ok(decl, rel)
}

fn id_map_rel_ok_for_name(collection: &str, rel: &str) -> bool {
    let dir = schema::collection_dir(collection);
    let plain = format!("{dir}/");
    let isolated = format!(".isolated/{dir}/");
    id_map_rel_has_prefix(rel, &plain) || id_map_rel_has_prefix(rel, &isolated)
}

fn id_map_rel_has_prefix(rel: &str, prefix: &str) -> bool {
    if rel.is_empty() || !rel.starts_with(prefix) || rel.len() <= prefix.len() {
        return false;
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// Fail if a journal `ids` file names a path outside its collection.
pub(crate) fn check_journal_id_maps(index_dir: &Path, decls: &Decls) -> Result<(), Error> {
    for decl in decls.collections() {
        if decl.encrypted || decl.kind != Kind::Journal {
            continue;
        }
        let path = ids_path(index_dir, &decl.name);
        if !path.is_file() {
            continue;
        }
        let raw = fs::read(&path).context("[Index::check_journal_id_maps] read ids")?;
        let text = str::from_utf8(&raw).context(
            Code::IndexCorrupt,
            "[Index::check_journal_id_maps] decode UTF-8",
        )?;
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let parsed: IdMapLine = serde_json::from_str(line).context(
                Code::IndexCorrupt,
                "[Index::check_journal_id_maps] parse line",
            )?;
            if !id_map_line_ok(decl, &parsed.id, &parsed.path) {
                bail!(
                    Error::index_corrupt(format!(
                        "journal id map for {} has a path outside the collection",
                        decl.name
                    ))
                    .context("[Index::check_journal_id_maps] path")
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn rewrite_collection(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    ids: BTreeMap<String, String>,
) -> Result<(), Error> {
    let (live, remember) = {
        let _lock = lock_ids(index_dir, collection)?;
        write_ids_file(index_dir, collection, &ids)?;
        let live = live_after(index_dir)?;
        let remember: Vec<(String, String)> = if live.is_some() {
            ids.iter()
                .map(|(id, rel)| (id.clone(), rel.clone()))
                .collect()
        } else {
            Vec::new()
        };
        maps.replace_collection(collection.to_owned(), ids);
        note_stamp(index_dir, maps, collection);
        (live, remember)
    };
    match live {
        // The live generation keeps its own forgets; only lines are added.
        Some(live) => forward_id_changes(&live, collection, &remember, &[]),
        None => Ok(()),
    }
}

/// The live generation when `current` names one newer than `index_dir`,
/// so `index_dir` is retired and an `ids` change made there must reach the
/// live one too. Read under `index_dir`'s `ids.lock`: a `reindex` that
/// publishes later copies the retired `ids` file under that lock and finds
/// the change; one that published earlier is seen here. `None` for the live
/// generation, one not yet published, and the legacy layout.
fn live_after(index_dir: &Path) -> Result<Option<PathBuf>, Error> {
    let generation = index_dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(parse_generation);
    let (Some(root), Some(n)) = (index_dir.parent(), generation) else {
        return Ok(None);
    };
    Ok(read_current(root)?
        .filter(|live| *live > n)
        .map(|live| root.join(live.to_string())))
}

/// Repeat an `ids` change on the live generation for a handle still bound
/// to a retired one. A live generation without this collection's `ids`
/// file has no map to keep current: its lookups scan.
fn forward_id_changes(
    live: &Path,
    collection: &str,
    remember: &[(String, String)],
    forget: &[String],
) -> Result<(), Error> {
    if !ids_path(live, collection).is_file() {
        return Ok(());
    }
    apply_id_changes(
        live,
        &mut JournalIdMaps::default(),
        collection,
        remember,
        forget,
    )
}

/// Add to `maps`, the journal maps of generation `into`, every line the
/// `ids` files of generation `from` hold and `maps` lacks. `into` was built
/// from commits; another handle may have appended to `from` meanwhile. Each
/// file of `from` is read under its `ids.lock`.
#[cfg(feature = "find")]
pub(crate) fn carry_retired(
    from: &Path,
    into: &Path,
    maps: &mut JournalIdMaps,
) -> Result<(), Error> {
    let collections: Vec<String> = maps.collections.keys().cloned().collect();
    for collection in collections {
        let path = ids_path(from, &collection);
        if !path.is_file() {
            continue;
        }
        let ids = {
            let _lock = lock_ids(from, &collection)?;
            read_ids_file(&path)?
        };
        let Some(ids) = ids else {
            continue;
        };
        let missing: Vec<(String, String)> = ids
            .into_iter()
            .filter(|(id, rel)| {
                maps.contains(&collection, id) == Some(false)
                    && crate::id::require_slug("id", id).is_ok()
                    && id_map_rel_ok_for_name(&collection, rel)
            })
            .collect();
        if !missing.is_empty() {
            apply_id_changes(into, maps, &collection, &missing, &[])?;
        }
    }
    Ok(())
}

fn ids_path(index_dir: &Path, collection: &str) -> PathBuf {
    index_dir
        .join(JOURNAL_DIR)
        .join(schema::collection_dir(collection))
        .join(IDS_FILE)
}

fn read_ids_file(path: &Path) -> Result<Option<BTreeMap<String, String>>, Error> {
    let raw = fs::read(path).context("[Index::read_ids_file] read ids")?;
    let Ok(text) = str::from_utf8(&raw) else {
        return Ok(None);
    };
    let mut ids = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<IdMapLine>(line) else {
            return Ok(None);
        };
        if parsed.id.is_empty() || parsed.path.is_empty() {
            return Ok(None);
        }
        ids.insert(parsed.id, parsed.path);
    }
    Ok(Some(ids))
}

fn write_ids_file(
    index_dir: &Path,
    collection: &str,
    ids: &BTreeMap<String, String>,
) -> Result<(), Error> {
    let path = ids_path(index_dir, collection);
    let mut bytes = Vec::new();
    for (id, rel) in ids {
        encode_id_line(&mut bytes, id, rel, "[Index::write_ids_file] encode line")?;
    }
    store::atomic_write(&path, &bytes)?;
    store::restrict_file_below(index_dir, &path)
}

/// Append `lines` (id, segment path) to `collection`'s `ids` file: one
/// write, one `fsync`.
fn append_ids_lines(
    index_dir: &Path,
    collection: &str,
    lines: &[(&str, &str)],
) -> Result<(), Error> {
    let path = ids_path(index_dir, collection);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("[Index::append_ids_lines] create dir")?;
    }
    let mut encoded = Vec::new();
    for (id, rel) in lines {
        encode_id_line(
            &mut encoded,
            id,
            rel,
            "[Index::append_ids_lines] encode line",
        )?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .context("[Index::append_ids_lines] open ids")?;
    file.write_all(&encoded)
        .context("[Index::append_ids_lines] write ids")?;
    file.sync_all()
        .context("[Index::append_ids_lines] fsync ids")?;
    store::restrict_file_below(index_dir, &path)
}

fn encode_id_line(
    out: &mut Vec<u8>,
    id: &str,
    rel: &str,
    frame: &'static str,
) -> Result<(), Error> {
    let line = IdMapLine {
        id: id.to_owned(),
        path: rel.to_owned(),
    };
    serde_json::to_writer(&mut *out, &line).context(Code::Invalid, frame)?;
    out.push(b'\n');
    Ok(())
}

#[cfg(feature = "find")]
pub(crate) fn format_version(index_dir: &Path) -> Result<Option<u32>, Error> {
    let path = index_dir.join(FORMAT_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).context("[Index::format_version] read format")?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.parse::<u32>() {
        Ok(version) => Ok(Some(version)),
        Err(_) => Err(Error::index_corrupt(format!(
            "index format is not a decimal integer: {}",
            path.display()
        ))
        .context("[Index::format_version] parse format")),
    }
}

#[cfg(feature = "find")]
pub(crate) fn require_current_format(index_dir: &Path) -> Result<(), Error> {
    match format_version(index_dir)? {
        Some(FORMAT_VERSION) => Ok(()),
        Some(version) => Err(Error::index_version(format!(
            "index format {version} is not {FORMAT_VERSION}"
        ))
        .context("[Index::require_current_format] check version")),
        None => Err(Error::index_missing("index format file is missing")
            .context("[Index::require_current_format] missing format")),
    }
}

#[cfg(feature = "find")]
pub(crate) fn ensure_format(index_dir: &Path) -> Result<(), Error> {
    match format_version(index_dir)? {
        Some(FORMAT_VERSION) => Ok(()),
        Some(version) => Err(Error::index_version(format!(
            "index format {version} is not {FORMAT_VERSION}"
        ))
        .context("[Index::ensure_format] check version")),
        None => write_format(index_dir),
    }
}

/// Write the `format` file whole: a reader in another handle or process
/// sees no file or the full version, never an empty one.
pub(crate) fn write_format(index_dir: &Path) -> Result<(), Error> {
    let path = index_dir.join(FORMAT_FILE);
    store::atomic_write(&path, format!("{FORMAT_VERSION}\n").as_bytes())
        .with_context(|| "[Index::write_format] write format")?;
    restrict_index_tree(index_dir)
}

#[cfg(not(feature = "find"))]
fn clear_index_dir(index_dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(index_dir).context("[Index::clear] create dir")?;
    let entries = fs::read_dir(index_dir).context("[Index::clear] read dir")?;
    for entry in entries {
        let entry = entry.context("[Index::clear] read entry")?;
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "vector") {
            // An inherited vector dump may already sit here from the start
            // of `reindex` and must survive this clear.
            continue;
        }
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.context("[Index::clear] remove entry")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir() -> PathBuf {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::var_os("TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let tid = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
        let root = base.join(format!("scheda-index-{}-{n}-{tid}", std::process::id()));
        let generation = root.join(".scheda").join("index").join("1");
        fs::create_dir_all(&generation).unwrap();
        generation
    }

    /// `{vault}/.scheda/` deleted under a live handle: its indexer may have
    /// recreated generation `1` with no `current`. A new build must neither
    /// prune nor reuse it.
    #[test]
    fn a_new_generation_never_prunes_or_reuses_the_callers() {
        let previous = scratch_dir();
        let root = previous.parent().unwrap().to_path_buf();
        fs::write(previous.join("marker"), b"in use").unwrap();
        let next = start_generation(&root, &previous).unwrap();
        assert_eq!(next, root.join("2"));
        assert!(
            previous.join("marker").is_file(),
            "the caller's generation survives"
        );
        // Orphans go first; the caller's generation still stays.
        fs::create_dir_all(root.join("7")).unwrap();
        assert_eq!(start_generation(&root, &previous).unwrap(), root.join("2"));
        assert!(!root.join("7").exists());
        assert!(previous.join("marker").is_file());
        let _ = fs::remove_dir_all(root.parent().unwrap().parent().unwrap());
    }

    fn ids_of(index_dir: &Path, collection: &str) -> BTreeMap<String, String> {
        read_ids_file(&ids_path(index_dir, collection))
            .unwrap()
            .unwrap()
    }

    fn one(id: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(id.to_owned(), "log/s.jsonl".to_owned())])
    }

    /// A handle still bound to generation 1 after a `reindex` published 2
    /// appends a journal line: generation 2 gets it too, or a handle on 2
    /// answers `not_found` for a committed Record.
    #[test]
    fn a_line_written_to_a_retired_generation_reaches_the_live_one() {
        let retired = scratch_dir();
        let root = retired.parent().unwrap().to_path_buf();
        let mut old = JournalIdMaps::default();
        rewrite_collection(&retired, &mut old, "log", one("a")).unwrap();
        let live = root.join("2");
        let mut new = JournalIdMaps::default();
        rewrite_collection(&live, &mut new, "log", one("a")).unwrap();
        // Not published yet: nothing is forwarded into a generation in build.
        remember_id(&retired, &mut old, "log", "early", "log/s.jsonl").unwrap();
        assert!(!ids_of(&live, "log").contains_key("early"));

        write_current(&root, 2).unwrap();
        remember_id(&retired, &mut old, "log", "late", "log/s.jsonl").unwrap();
        assert!(ids_of(&retired, "log").contains_key("late"));
        assert!(ids_of(&live, "log").contains_key("late"));
        // The live generation itself forwards nowhere.
        remember_id(&live, &mut new, "log", "own", "log/s.jsonl").unwrap();
        assert!(!ids_of(&retired, "log").contains_key("own"));
        let _ = fs::remove_dir_all(root.parent().unwrap().parent().unwrap());
    }

    /// `reindex` builds from commits; a line another handle appended to the
    /// live generation meanwhile is copied into the new one.
    #[cfg(feature = "find")]
    #[test]
    fn carry_retired_copies_the_lines_the_new_generation_lacks() {
        let retired = scratch_dir();
        let root = retired.parent().unwrap().to_path_buf();
        let mut old = JournalIdMaps::default();
        let mut both = one("a");
        both.extend(one("queued"));
        rewrite_collection(&retired, &mut old, "log", both).unwrap();
        let built = root.join("2");
        let mut maps = JournalIdMaps::default();
        rewrite_collection(&built, &mut maps, "log", one("a")).unwrap();
        carry_retired(&retired, &built, &mut maps).unwrap();
        assert_eq!(maps.contains("log", "queued"), Some(true));
        assert!(ids_of(&built, "log").contains_key("queued"));
        let _ = fs::remove_dir_all(root.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn remember_id_twice_does_not_duplicate_ids_file() {
        let dir = scratch_dir();
        let mut maps = JournalIdMaps::default();
        maps.replace_collection("log".into(), BTreeMap::new());
        remember_id(&dir, &mut maps, "log", "a", "log/2024-01-01.jsonl").unwrap();
        remember_id(&dir, &mut maps, "log", "a", "log/2024-01-01.jsonl").unwrap();
        let path = ids_path(&dir, "log");
        let text = fs::read_to_string(&path).unwrap();
        let hits = text.lines().filter(|line| line.contains("\"a\"")).count();
        assert_eq!(hits, 1, "{text}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn id_changes_of_one_collection_are_one_write_with_owner_only_modes() {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::var_os("TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("scheda-ids-batch-{}-{n}", std::process::id()));
        let dir = root.join(".scheda").join("index").join("1");
        fs::create_dir_all(&dir).unwrap();
        let mut maps = JournalIdMaps::default();
        maps.replace_collection("log".into(), BTreeMap::new());
        let lines: Vec<(String, String)> = (0..50)
            .map(|i| (format!("id-{i:02}"), "log/2024-01-01.jsonl".to_owned()))
            .collect();
        apply_id_changes(&dir, &mut maps, "log", &lines, &[]).unwrap();
        let path = ids_path(&dir, "log");
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 50);
        // The same batch again writes nothing new.
        apply_id_changes(&dir, &mut maps, "log", &lines, &[]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 50);
        // Forgets and a moved line are one rewrite.
        let moved = [("id-00".to_owned(), "log/2024-01-02.jsonl".to_owned())];
        let gone: Vec<String> = (1..=10).map(|i| format!("id-{i:02}")).collect();
        apply_id_changes(&dir, &mut maps, "log", &moved, &gone).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 40, "{text}");
        assert!(text.contains("2024-01-02"));
        assert_eq!(maps.contains("log", "id-05"), Some(false));
        assert_eq!(maps.contains("log", "id-20"), Some(true));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
            assert_eq!(mode(&dir.join(JOURNAL_DIR)), 0o700);
        }
        let _ = fs::remove_dir_all(&root);
    }

    fn journal_decl(name: &str, isolated: bool) -> schema::CollectionDecl {
        schema::CollectionDecl {
            name: name.to_owned(),
            kind: Kind::Journal,
            version: 1,
            segment_max_bytes: None,
            segment_max_lines: None,
            segment_rotate: None,
            singleton_id: None,
            isolated,
            encrypted: false,
            exclude_lexical_index: false,
            exclude_vector_index: false,
        }
    }

    #[test]
    fn id_map_rel_ok_rejects_absolute_parent_and_other_collection() {
        let decl = journal_decl("log", false);
        assert!(id_map_rel_ok(&decl, "log/open.jsonl"));
        assert!(!id_map_rel_ok(&decl, "/etc/passwd"));
        assert!(!id_map_rel_ok(&decl, "../secret"));
        assert!(!id_map_rel_ok(&decl, "log/../secret"));
        assert!(!id_map_rel_ok(&decl, "other/open.jsonl"));
        let isolated = journal_decl("log", true);
        assert!(id_map_rel_ok(&isolated, ".isolated/log/open.jsonl"));
        assert!(!id_map_rel_ok(&isolated, "log/open.jsonl"));
    }
}
