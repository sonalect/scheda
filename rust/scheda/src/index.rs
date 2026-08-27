//! Disposable Index under `{vault}/.scheda/index`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Code, Context, ContextAs, ContextIo};
use crate::journal;
use crate::schema::{self, Kind, Schema};
use crate::store;
use crate::{Error, ReindexResult};

pub(crate) const FORMAT_VERSION: u32 = 3;
const FORMAT_FILE: &str = "format";
const STALE_FILE: &str = "stale";
const JOURNAL_DIR: &str = "journal";
const IDS_FILE: &str = "ids";

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

    pub(crate) fn extend_maps(&mut self, other: Self) {
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

pub(crate) fn load_journal_ids(index_dir: &Path, schema: &Schema) -> Result<JournalIdMaps, Error> {
    load_journal_ids_matching(index_dir, schema, false)
}

pub(crate) fn load_all_journal_ids(
    vault: &Path,
    main_index: &Path,
    schema: &Schema,
) -> Result<JournalIdMaps, Error> {
    let mut maps = load_journal_ids(main_index, schema)?;
    let nested = crate::isolated::index_dir(vault);
    if nested.is_dir() {
        maps.extend_maps(load_journal_ids_matching(&nested, schema, true)?);
    }
    Ok(maps)
}

fn load_journal_ids_matching(
    index_dir: &Path,
    schema: &Schema,
    isolated: bool,
) -> Result<JournalIdMaps, Error> {
    let mut maps = JournalIdMaps::default();
    for decl in schema.collections() {
        if decl.isolated != isolated || decl.kind != Kind::Journal {
            continue;
        }
        let path = ids_path(index_dir, &decl.name);
        if !path.is_file() {
            continue;
        }
        if let Some(ids) = read_ids_file(&path)? {
            maps.replace_collection(decl.name.clone(), ids);
        }
    }
    Ok(maps)
}

pub(crate) fn rebuild(
    vault: &Path,
    schema: &Schema,
    index_dir: &Path,
) -> Result<(ReindexResult, JournalIdMaps), Error> {
    let (mut counts, mut maps) = rebuild_matching(vault, schema, index_dir, false)?;
    if crate::isolated::root(vault).is_dir() {
        let nested = crate::isolated::index_dir(vault);
        fs::create_dir_all(&nested).context("[Index::rebuild] create isolated index")?;
        let isolated_counts;
        let isolated_maps;
        (isolated_counts, isolated_maps) = rebuild_matching(vault, schema, &nested, true)?;
        counts.records_read =
            Some(counts.records_read.unwrap_or(0) + isolated_counts.records_read.unwrap_or(0));
        counts.skipped_invalid = Some(
            counts.skipped_invalid.unwrap_or(0) + isolated_counts.skipped_invalid.unwrap_or(0),
        );
        counts.segments_seen =
            Some(counts.segments_seen.unwrap_or(0) + isolated_counts.segments_seen.unwrap_or(0));
        maps.extend_maps(isolated_maps);
    }
    Ok((counts, maps))
}

fn rebuild_matching(
    vault: &Path,
    schema: &Schema,
    index_dir: &Path,
    isolated: bool,
) -> Result<(ReindexResult, JournalIdMaps), Error> {
    clear_index_dir(index_dir)?;
    let mut records_read = 0u64;
    let mut skipped_invalid = 0u64;
    let mut segments_seen = 0u64;
    let mut maps = JournalIdMaps::default();
    for decl in schema
        .collections()
        .filter(|decl| decl.isolated == isolated)
    {
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
                let (records, invalid) = store::reindex_stats(vault, decl)?;
                records_read += records;
                skipped_invalid += invalid;
            }
        }
    }
    write_format(index_dir)?;
    Ok((
        ReindexResult::default()
            .with_records_read(records_read)
            .with_skipped_invalid(skipped_invalid)
            .with_segments_seen(segments_seen),
        maps,
    ))
}

pub(crate) fn remember_id(
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    collection: &str,
    id: &str,
    rel: &str,
) -> Result<(), Error> {
    append_ids_line(index_dir, collection, id, rel)?;
    maps.put(collection.to_owned(), id.to_owned(), rel.to_owned());
    Ok(())
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
        let line = IdMapLine {
            id: id.clone(),
            path: rel.clone(),
        };
        let mut encoded = serde_json::to_vec(&line)
            .context(Code::Invalid, "[Index::write_ids_file] encode line")?;
        encoded.push(b'\n');
        bytes.extend_from_slice(&encoded);
    }
    store::atomic_write(&path, &bytes)
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

pub(crate) fn write_format(index_dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(index_dir).context("[Index::write_format] create dir")?;
    let path = index_dir.join(FORMAT_FILE);
    fs::write(&path, format!("{FORMAT_VERSION}\n")).context("[Index::write_format] write format")
}

fn stale_path(index_dir: &Path) -> PathBuf {
    index_dir.join(STALE_FILE)
}

/// Whether a build that cannot update Tantivy segments wrote Records here.
#[must_use]
pub(crate) fn is_stale(index_dir: &Path) -> bool {
    stale_path(index_dir).is_file()
}

/// Mark lexical segments out of date after a write that did not update them.
#[cfg(not(feature = "find"))]
pub(crate) fn mark_stale(index_dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(index_dir).context("[Index::mark_stale] create dir")?;
    fs::write(stale_path(index_dir), []).context("[Index::mark_stale] write stale")
}

/// Fail closed when lexical segments were aged without a Tantivy update.
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
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.context("[Index::clear] remove entry")?;
    }
    Ok(())
}
