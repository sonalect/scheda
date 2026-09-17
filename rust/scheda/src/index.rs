//! Disposable Index under `{vault}/.scheda/index`.
//!
//! Live layout is generational: `{vault}/.scheda/index/current` names the live
//! generation directory, and `{vault}/.scheda/index/<generation>/` holds the
//! `format` file, `lexical/`, `vector/`, edges, ord, and `journal/`. A handle
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

#[cfg(feature = "find")]
use crate::error::Context;
use crate::error::{Code, ContextAs, ContextIo, bail};
use crate::journal;
use crate::declaration::{self, Kind, Decls};
use crate::store;
use crate::{Error, ReindexResult};

/// On-disk Index format. Bumped to 4 when the Index became generational.
/// An older `format` file (version 3, files directly in `index/`) is
/// `index_version`; the caller runs `reindex`.
pub(crate) const FORMAT_VERSION: u32 = 4;
const FORMAT_FILE: &str = "format";
const CURRENT_FILE: &str = "current";
const STALE_FILE: &str = "stale";
const JOURNAL_DIR: &str = "journal";
const IDS_FILE: &str = "ids";
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

    /// Take another map's collections. Used to merge the search-engine walk
    /// into the maps `reindex` already built for journals that are not indexed.
    #[cfg(feature = "find")]
    pub(crate) fn absorb(&mut self, other: Self) {
        self.collections.extend(other.collections);
        self.segment_lines.extend(other.segment_lines);
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

    pub(crate) fn forget_segment_line(&mut self, path: &Path) {
        if let Some(n) = self.segment_lines.get_mut(path) {
            *n = n.saturating_sub(1);
        }
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

fn next_generation(root: &Path) -> Result<u64, Error> {
    Ok(read_current(root)?.unwrap_or(0).saturating_add(1).max(1))
}

/// Build a new generation directory without publishing `current`.
///
/// Used by `reindex`. A caller that stops before [`publish_generation`]
/// leaves the previous generation live; the unfinished directory is an
/// orphan removed by the next `reindex` or by `open` when the write lock
/// and `reindex.lock` are free.
pub(crate) fn start_generation(root: &Path) -> Result<PathBuf, Error> {
    prune_generations(root)?;
    let n = next_generation(root)?;
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
    let live = read_current(root)?;
    let mut keep = BTreeSet::new();
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
        let path = ids_path(index_dir, &decl.name);
        if !path.is_file() {
            continue;
        }
        if let Some(mut ids) = read_ids_file(&path)? {
            ids.retain(|id, rel| id_map_line_ok(decl, id, rel));
            maps.replace_collection(decl.name.clone(), ids);
        }
    }
    Ok(maps)
}

/// Fill `index_dir` with journal maps and the format file. Does not swap
/// `current`; the caller publishes after the search engines are rebuilt.
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
                #[cfg(feature = "find")]
                if decl.any_indexed() {
                    // The search-engine walk builds this map in the same pass.
                    continue;
                }
                let (records, invalid, segments, ids) = journal::reindex_map(vault, decl)?;
                records_read += records;
                skipped_invalid += invalid;
                segments_seen += segments;
                write_ids_file(index_dir, &decl.name, &ids)?;
                maps.replace_collection(decl.name.clone(), ids);
            }
            Kind::Document | Kind::Singleton => {
                #[cfg(feature = "find")]
                continue;
                #[cfg(not(feature = "find"))]
                {
                    if !decl.any_indexed() {
                        continue;
                    }
                    let (records, invalid) = store::reindex_stats(vault, decl)?;
                    records_read += records;
                    skipped_invalid += invalid;
                }
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

fn remove_legacy_isolated_index(vault: &Path) -> Result<(), Error> {
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

pub(crate) fn remember_id(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    id: &str,
    rel: &str,
) -> Result<(), Error> {
    let previous = match maps.lookup(collection, id) {
        MapLookup::Path(old) => Some(old.to_owned()),
        MapLookup::UnknownId | MapLookup::MissingMap => None,
    };
    if let Some(old) = previous {
        maps.put(collection.to_owned(), id.to_owned(), rel.to_owned());
        if old != rel {
            let ids = maps
                .collections
                .get(collection)
                .cloned()
                .unwrap_or_default();
            write_ids_file(index_dir, collection, &ids)?;
        }
        return Ok(());
    }
    append_ids_line(index_dir, collection, id, rel)?;
    maps.put(collection.to_owned(), id.to_owned(), rel.to_owned());
    Ok(())
}

/// Look up `id` in `{index_dir}/journal/{collection}/ids`.
///
/// A missing or unreadable file is [`OwnedLookup::MissingMap`] (caller may
/// scan). A readable file that does not name `id` is [`OwnedLookup::UnknownId`].
pub(crate) fn lookup_on_disk(
    index_dir: &Path,
    collection: &str,
    id: &str,
) -> Result<OwnedLookup, Error> {
    let path = ids_path(index_dir, collection);
    if !path.is_file() {
        return Ok(OwnedLookup::MissingMap);
    }
    match read_ids_file(&path)? {
        None => Ok(OwnedLookup::MissingMap),
        Some(ids) => match ids.get(id) {
            Some(rel) if id_map_rel_ok_for_name(collection, rel) => {
                Ok(OwnedLookup::Path(rel.clone()))
            }
            Some(_) | None => Ok(OwnedLookup::UnknownId),
        },
    }
}

/// Whether `rel` is a Vault-relative segment under this collection directory.
///
/// Rejects absolute paths, `..`, and a prefix that is not this collection
/// (plain or `.isolated/`). Used when reading the journal `id` map so a
/// hand-edited line cannot name a file outside the collection.
pub(crate) fn id_map_rel_ok(decl: &declaration::CollectionDecl, rel: &str) -> bool {
    let dir = declaration::collection_dir(&decl.name);
    let prefix = if decl.isolated {
        format!(".isolated/{dir}/")
    } else {
        format!("{dir}/")
    };
    id_map_rel_has_prefix(rel, &prefix)
}

fn id_map_line_ok(decl: &declaration::CollectionDecl, id: &str, rel: &str) -> bool {
    crate::id::require_slug("id", id).is_ok() && id_map_rel_ok(decl, rel)
}

fn id_map_rel_ok_for_name(collection: &str, rel: &str) -> bool {
    let dir = declaration::collection_dir(collection);
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

/// [`MapLookup`] with an owned segment path.
#[derive(Debug)]
pub(crate) enum OwnedLookup {
    /// No ids file, or the file could not be parsed.
    MissingMap,
    /// Ids file exists and does not name this `id`.
    UnknownId,
    /// Vault-relative POSIX path of the segment that holds `id`.
    Path(String),
}

pub(crate) fn forget_remembered(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    id: &str,
) -> Result<(), Error> {
    maps.forget_id(collection, id);
    let ids = maps
        .collections
        .get(collection)
        .cloned()
        .unwrap_or_default();
    write_ids_file(index_dir, collection, &ids)
}

pub(crate) fn rewrite_collection(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    ids: BTreeMap<String, String>,
) -> Result<(), Error> {
    write_ids_file(index_dir, collection, &ids)?;
    maps.replace_collection(collection.to_owned(), ids);
    Ok(())
}

fn ids_path(index_dir: &Path, collection: &str) -> PathBuf {
    index_dir
        .join(JOURNAL_DIR)
        .join(declaration::collection_dir(collection))
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
        let line = IdMapLine {
            id: id.clone(),
            path: rel.clone(),
        };
        let mut encoded = serde_json::to_vec(&line)
            .context(Code::Invalid, "[Index::write_ids_file] encode line")?;
        encoded.push(b'\n');
        bytes.extend_from_slice(&encoded);
    }
    store::atomic_write(&path, &bytes)?;
    restrict_index_tree(index_dir)
}

fn append_ids_line(index_dir: &Path, collection: &str, id: &str, rel: &str) -> Result<(), Error> {
    let path = ids_path(index_dir, collection);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("[Index::append_ids_line] create dir")?;
    }
    let line = IdMapLine {
        id: id.to_owned(),
        path: rel.to_owned(),
    };
    let mut encoded =
        serde_json::to_vec(&line).context(Code::Invalid, "[Index::append_ids_line] encode line")?;
    encoded.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .context("[Index::append_ids_line] open ids")?;
    file.write_all(&encoded)
        .context("[Index::append_ids_line] write ids")?;
    file.sync_all()
        .context("[Index::append_ids_line] fsync ids")?;
    restrict_index_tree(index_dir)
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

pub(crate) fn write_format(index_dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(index_dir).context("[Index::write_format] create dir")?;
    let path = index_dir.join(FORMAT_FILE);
    fs::write(&path, format!("{FORMAT_VERSION}\n"))
        .context("[Index::write_format] write format")?;
    restrict_index_tree(index_dir)
}

fn stale_path(index_dir: &Path) -> PathBuf {
    index_dir.join(STALE_FILE)
}

/// Whether a build that cannot update Tantivy segments wrote Records here.
#[cfg(feature = "find")]
#[must_use]
pub(crate) fn is_stale(index_dir: &Path) -> bool {
    stale_path(index_dir).is_file()
}

/// Mark lexical segments out of date after a write that did not update them.
#[cfg(not(feature = "find"))]
pub(crate) fn mark_stale(index_dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(index_dir).context("[Index::mark_stale] create dir")?;
    fs::write(stale_path(index_dir), []).context("[Index::mark_stale] write stale")?;
    restrict_index_tree(index_dir)
}

/// Fail closed when lexical segments were aged without a Tantivy update.
#[cfg(feature = "find")]
pub(crate) fn require_fresh(index_dir: &Path) -> Result<(), Error> {
    if is_stale(index_dir) {
        return Err(Error::index_lexical("index is stale"))
            .context("[Index::require_fresh] lexical segments");
    }
    Ok(())
}

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
    fn lookup_on_disk_unknown_id_is_not_missing_map() {
        let dir = scratch_dir();
        let mut maps = JournalIdMaps::default();
        maps.replace_collection("log".into(), BTreeMap::new());
        remember_id(&dir, &mut maps, "log", "a", "log/open.jsonl").unwrap();
        match lookup_on_disk(&dir, "log", "missing").unwrap() {
            OwnedLookup::UnknownId => {}
            other => panic!("expected UnknownId, got {other:?}"),
        }
        match lookup_on_disk(&dir, "log", "a").unwrap() {
            OwnedLookup::Path(rel) => assert_eq!(rel, "log/open.jsonl"),
            other => panic!("expected Path, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    fn journal_decl(name: &str, isolated: bool) -> declaration::CollectionDecl {
        declaration::CollectionDecl {
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

    #[test]
    fn lookup_on_disk_skips_path_outside_the_collection() {
        let dir = scratch_dir();
        let mut maps = JournalIdMaps::default();
        maps.replace_collection("log".into(), BTreeMap::new());
        remember_id(&dir, &mut maps, "log", "a", "log/open.jsonl").unwrap();
        let path = ids_path(&dir, "log");
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("{\"id\":\"stolen\",\"path\":\"/etc/passwd\"}\n");
        fs::write(&path, text).unwrap();
        match lookup_on_disk(&dir, "log", "stolen").unwrap() {
            OwnedLookup::UnknownId => {}
            other => panic!("expected UnknownId, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
