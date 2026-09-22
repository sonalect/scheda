//! Hybrid lexical + vector search.

use std::cmp::Reverse;
use std::collections::BTreeMap;
#[cfg(feature = "search")]
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

#[cfg(feature = "search")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "search")]
use sha1::{Digest, Sha1};
#[cfg(feature = "search")]
use tantivy::collector::DocSetCollector;
use tantivy::collector::{Count, TopDocs};
#[cfg(feature = "search")]
use tantivy::query::QueryParser;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery, TermSetQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema as TantivySchema, TEXT,
    Value as TantivyValue,
};
use tantivy::{
    Index, IndexReader, IndexWriter, Order, ReloadPolicy, Score, SegmentReader, TantivyDocument,
    Term,
};

use crate::error::{Code, ContextAs, ContextIo, bail};
use crate::index::{self, JournalIdMaps};
use crate::projector::{self, IndexDoc, IndexValue, ProjectorDump};
use crate::query::{self, FindFields};
use crate::schema::{CollectionDecl, Decls, Kind};
use crate::store;
use crate::{Embedder, Error, Filter, IndexSchema, Record};
#[cfg(feature = "search")]
use crate::{SearchHits, SearchQuery, search_hits};

#[cfg(feature = "search")]
use crate::ann::AnnIndex;

const LEXICAL_DIR: &str = "lexical";
const PROJECTOR_FILE: &str = "projector";
#[cfg(feature = "search")]
const VECTOR_DIR: &str = "vector";
#[cfg(feature = "search")]
const VECTOR_INDEX: &str = "index.usearch";
#[cfg(feature = "search")]
const VECTOR_META: &str = "meta.json";
#[cfg(feature = "search")]
const VECTOR_KEYS: &str = "keys.json";
const ORD_FILE: &str = "ord.json";
/// Heap budget for one Tantivy `IndexWriter`. Opening a writer allocates
/// this and starts a merge thread, so the engine keeps a writer only while
/// a transaction (or one autocommit) is in progress and drops it when that
/// work ends. Holding it for the handle lifetime would block every other
/// writer on the directory lock.
const WRITER_HEAP: usize = 16_000_000;
#[cfg(feature = "search")]
const RRF_K: f64 = 60.0;
#[cfg(feature = "search")]
const DEFAULT_K: usize = 10;
/// Minimum fused-candidate depth. Each Index leg retrieves this many hits
/// (or `k` if larger) so RRF can promote a document that a single leg
/// would have ranked below `k`.
#[cfg(feature = "search")]
const FUSION_POOL_FLOOR: usize = 50;

pub(crate) struct SearchEngine {
    index_dir: RwLock<PathBuf>,
    inner: RwLock<Inner>,
    projector: Option<Arc<dyn crate::Projector>>,
}

/// Counts and journal maps from one file walk that also fills this engine.
#[derive(Debug, Default)]
pub(crate) struct RebuildStats {
    /// Records opened during the walk, including invalid ones.
    pub records_read: u64,
    /// Opened Records that failed current Schema.
    pub skipped_invalid: u64,
    /// Journal segment files seen.
    pub segments_seen: u64,
    /// `id` maps written for indexed journals in this walk.
    pub journal_maps: JournalIdMaps,
}

struct Inner {
    lexical: Option<Lexical>,
    #[cfg(feature = "search")]
    vector: Option<VectorStore>,
    dirty: bool,
    can_write: bool,
    next_ord: BTreeMap<String, u64>,
    #[cfg(test)]
    writer_opens: u64,
}

struct Lexical {
    index: Index,
    /// Tantivy `IndexWriter` is `Send` and not `Sync` (`JoinHandle`). It
    /// sits behind a `Mutex` so `Inner` can live in an `RwLock` and
    /// concurrent `find` / `search` can share the reader.
    writer: Mutex<Option<IndexWriter>>,
    reader: IndexReader,
    f_key: Field,
    f_collection: Field,
    f_id: Field,
    f_path: Field,
    f_text: Field,
    f_eq: Field,
    f_exists: Field,
    f_null: Field,
    f_tok: Field,
    f_ord: Field,
}

#[cfg(feature = "search")]
type FieldKeys = BTreeMap<String, u64>;
#[cfg(feature = "search")]
type SlotIndex = BTreeMap<String, BTreeMap<String, FieldKeys>>;

#[cfg(feature = "search")]
struct VectorStore {
    ann: AnnIndex,
    keys: BTreeMap<u64, DocRef>,
    /// `(collection, id) → field → ANN key`. Inverse of `keys` so a Record's
    /// slots are a map lookup, not a scan of every key.
    by_record: SlotIndex,
    model_id: String,
    dimensions: usize,
    field_ids: BTreeSet<String>,
    next_key: u64,
}

#[cfg(feature = "search")]
impl VectorStore {
    fn empty(ann: AnnIndex, model_id: String, dimensions: usize) -> Self {
        Self {
            ann,
            keys: BTreeMap::new(),
            by_record: BTreeMap::new(),
            model_id,
            dimensions,
            field_ids: BTreeSet::new(),
            next_key: 1,
        }
    }

    fn loaded(
        ann: AnnIndex,
        keys: BTreeMap<u64, DocRef>,
        model_id: String,
        dimensions: usize,
        field_ids: BTreeSet<String>,
        next_key: u64,
    ) -> Self {
        let mut by_record: SlotIndex = BTreeMap::new();
        for (key, doc) in &keys {
            by_record
                .entry(doc.collection.clone())
                .or_default()
                .entry(doc.id.clone())
                .or_default()
                .insert(doc.field.clone(), *key);
        }
        Self {
            ann,
            keys,
            by_record,
            model_id,
            dimensions,
            field_ids,
            next_key,
        }
    }

    fn record_slots(&self, collection: &str, id: &str) -> Option<&FieldKeys> {
        self.by_record.get(collection)?.get(id)
    }

    fn slot_key(&self, collection: &str, id: &str, field: &str) -> Option<u64> {
        self.record_slots(collection, id)?.get(field).copied()
    }

    fn record_keys(&self, collection: &str, id: &str) -> Vec<u64> {
        self.record_slots(collection, id)
            .map(|fields| fields.values().copied().collect())
            .unwrap_or_default()
    }

    fn insert_slot(&mut self, key: u64, doc: DocRef) {
        self.by_record
            .entry(doc.collection.clone())
            .or_default()
            .entry(doc.id.clone())
            .or_default()
            .insert(doc.field.clone(), key);
        self.keys.insert(key, doc);
    }

    fn remove_slot(&mut self, key: u64) {
        let Some(doc) = self.keys.remove(&key) else {
            return;
        };
        let drop_collection = {
            let Some(ids) = self.by_record.get_mut(&doc.collection) else {
                return;
            };
            if let Some(fields) = ids.get_mut(&doc.id) {
                fields.remove(&doc.field);
                if fields.is_empty() {
                    ids.remove(&doc.id);
                }
            }
            ids.is_empty()
        };
        if drop_collection {
            self.by_record.remove(&doc.collection);
        }
    }

    fn drop_ann_slot(&mut self, key: u64) -> Result<(), Error> {
        if self.ann.contains(key) {
            self.ann.remove(key)?;
        }
        self.remove_slot(key);
        Ok(())
    }
}

#[cfg(feature = "search")]
struct VectorBatch {
    pieces: Vec<(String, String)>,
    vectors: Vec<(String, Vec<f32>)>,
}

#[cfg(not(feature = "search"))]
struct VectorBatch;

impl VectorBatch {
    fn empty() -> Self {
        #[cfg(feature = "search")]
        {
            Self {
                pieces: Vec::new(),
                vectors: Vec::new(),
            }
        }
        #[cfg(not(feature = "search"))]
        {
            Self
        }
    }
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Deserialize, Serialize)]
struct DocRef {
    collection: String,
    id: String,
    path: String,
    field: String,
    /// Git blob SHA-1 of the exact text handed to the embedder.
    ///
    /// Empty when loading an Index written before digests existed; that
    /// slot is treated as changed and re-embedded once.
    #[serde(default)]
    digest: String,
}

#[cfg(feature = "search")]
#[derive(Clone, Debug, Deserialize, Serialize)]
struct VectorMeta {
    model_id: String,
    dimensions: usize,
    #[serde(default)]
    field_ids: Vec<String>,
    #[serde(default)]
    next_key: u64,
}

#[cfg(feature = "search")]
struct LegHit {
    collection: String,
    id: String,
    score: f64,
}

fn collection_query(field: Field, names: &[&str]) -> Box<dyn Query> {
    if names.len() == 1 {
        Box::new(TermQuery::new(
            Term::from_field_text(field, names[0]),
            IndexRecordOption::Basic,
        ))
    } else {
        Box::new(TermSetQuery::new(
            names
                .iter()
                .map(|name| Term::from_field_text(field, name))
                .collect::<Vec<_>>(),
        ))
    }
}

#[cfg(feature = "search")]
fn allowed_has(allowed: &BTreeMap<String, BTreeSet<String>>, collection: &str, id: &str) -> bool {
    allowed.get(collection).is_some_and(|ids| ids.contains(id))
}

impl SearchEngine {
    pub(crate) fn open(
        index_dir: &Path,
        can_write: bool,
        projector: Option<Arc<dyn crate::Projector>>,
    ) -> Result<Self, Error> {
        let inner = Inner {
            lexical: open_lexical(index_dir).ok().flatten(),
            #[cfg(feature = "search")]
            vector: load_vector(index_dir).ok().flatten(),
            dirty: false,
            can_write,
            next_ord: load_next_ord(index_dir).unwrap_or_default(),
            #[cfg(test)]
            writer_opens: 0,
        };
        Ok(Self {
            index_dir: RwLock::new(index_dir.to_path_buf()),
            inner: RwLock::new(inner),
            projector,
        })
    }

    fn dir(&self) -> Result<PathBuf, Error> {
        Ok(crate::error::rw_read(&self.index_dir, "[SearchEngine] lock index dir")?.clone())
    }

    /// Point this engine at a different generation directory.
    pub(crate) fn set_index_dir(&self, index_dir: &Path) -> Result<(), Error> {
        *crate::error::rw_write(&self.index_dir, "[SearchEngine] lock index dir")? =
            index_dir.to_path_buf();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            index_dir: RwLock::new(PathBuf::new()),
            inner: RwLock::new(Inner {
                lexical: None,
                #[cfg(feature = "search")]
                vector: None,
                dirty: false,
                can_write: false,
                next_ord: BTreeMap::new(),
                #[cfg(test)]
                writer_opens: 0,
            }),
            projector: None,
        }
    }

    pub(crate) fn close_for_rebuild(&self) {
        let Ok(mut inner) =
            crate::error::rw_write(&self.inner, "[SearchEngine::close_for_rebuild] lock")
        else {
            return;
        };
        let lexical = inner.lexical.take();
        #[cfg(feature = "search")]
        {
            inner.vector = None;
        }
        inner.dirty = false;
        inner.next_ord.clear();
        drop(inner);
        shutdown_lexical(lexical);
    }

    pub(crate) fn rebuild(
        &self,
        vault: &Path,
        decls: &Decls,
        embedder: Option<&dyn Embedder>,
    ) -> Result<RebuildStats, Error> {
        self.rebuild_from(vault, decls, embedder, None)
    }

    /// Fill this engine from Vault files. `hook` runs after a collection's
    /// files are listed and before they are read, so a test can `remove`
    /// in that window. One Record at a time: the walk does not hold the
    /// collection in memory. Indexed journals write their `id` map in this
    /// same pass.
    pub(crate) fn rebuild_online(
        &self,
        vault: &Path,
        decls: &Decls,
        embedder: Option<&dyn Embedder>,
        hook: Option<&dyn Fn(&str)>,
    ) -> Result<RebuildStats, Error> {
        self.rebuild_from(vault, decls, embedder, hook)
    }

    /// Install `other`'s engine and generation directory as this handle's
    /// live Index. The previous live engine is shut down.
    pub(crate) fn adopt(&self, other: SearchEngine) -> Result<(), Error> {
        let dir = other.dir()?;
        let mut other_inner = crate::error::rw_write(&other.inner, "[SearchEngine::adopt] take")?;
        let taken = std::mem::replace(&mut *other_inner, empty_inner(false));
        drop(other_inner);
        // Directory first, then engine. `find` / `search` check freshness and
        // format against the directory, then read the engine. Pointing the
        // directory at N+1 first means a concurrent reader may still see N's
        // engine, but never N+1's engine against N's files. Holding both write
        // locks together would deadlock with `flush` (inner then dir) or
        // `find` (dir then inner).
        self.set_index_dir(&dir)?;
        let mut self_inner = crate::error::rw_write(&self.inner, "[SearchEngine::adopt] install")?;
        let old = std::mem::replace(&mut *self_inner, taken);
        drop(self_inner);
        shutdown_lexical(old.lexical);
        Ok(())
    }

    fn rebuild_from(
        &self,
        vault: &Path,
        decls: &Decls,
        embedder: Option<&dyn Embedder>,
        hook: Option<&dyn Fn(&str)>,
    ) -> Result<RebuildStats, Error> {
        #[cfg(not(feature = "search"))]
        let _ = embedder;
        let can_write = {
            let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::rebuild] lock")?;
            let lexical = inner.lexical.take();
            #[cfg(feature = "search")]
            {
                inner.vector = None;
            }
            inner.dirty = false;
            inner.next_ord.clear();
            let can_write = inner.can_write;
            drop(inner);
            shutdown_lexical(lexical);
            can_write
        };
        let index_dir = self.dir()?;
        if can_write {
            index::ensure_format(&index_dir)?;
            write_projector_dump(
                &index_dir,
                &projector::encode_dump(
                    self.projector.as_deref(),
                    decls.collections().map(|decl| decl.name.as_str()),
                )?,
            )?;
        }
        #[cfg(feature = "search")]
        {
            let inherited = inherit_vector(&index_dir, &index_dir, embedder)?;
            let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::rebuild] inherit")?;
            inner.vector = inherited;
        }
        #[cfg(feature = "search")]
        let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut stats = RebuildStats::default();
        for decl in decls.collections().filter(|decl| decl.any_indexed()) {
            match decl.kind {
                Kind::Journal => {
                    if let Some(hook) = hook {
                        hook(&decl.name);
                    }
                    stats.segments_seen += crate::journal::segment_count(vault, decl)?;
                    let mut ids = BTreeMap::new();
                    crate::journal::for_each_record(vault, decl, |record, rel| {
                        stats.records_read += 1;
                        if let Some(id) = record.id.as_deref() {
                            ids.insert(id.to_owned(), rel.to_owned());
                        }
                        index_one_rebuild_record(
                            self,
                            &index_dir,
                            decl,
                            &record,
                            embedder,
                            #[cfg(feature = "search")]
                            &mut seen,
                        )?;
                        Ok(true)
                    })?;
                    index::rewrite_collection(
                        &index_dir,
                        &mut stats.journal_maps,
                        &decl.name,
                        ids,
                    )?;
                }
                Kind::Document | Kind::Singleton => {
                    let files = store::list_files(vault, decl)?;
                    if let Some(hook) = hook {
                        hook(&decl.name);
                    }
                    for path in files {
                        let record = match store::read_file(vault, decl, &path, None) {
                            Ok(record) => record,
                            Err(err) if crate::reindex::is_absent(&err) => continue,
                            Err(err) => return Err(err),
                        };
                        stats.records_read += 1;
                        index_one_rebuild_record(
                            self,
                            &index_dir,
                            decl,
                            &record,
                            embedder,
                            #[cfg(feature = "search")]
                            &mut seen,
                        )?;
                    }
                }
            }
        }
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::rebuild] commit")?;
        #[cfg(feature = "search")]
        drop_unseen_vectors(&mut inner, &seen)?;
        inner.commit(&index_dir, can_write)?;
        Ok(stats)
    }

    pub(crate) fn upsert(
        &self,
        decl: &CollectionDecl,
        record: &Record,
        embedder: Option<&dyn Embedder>,
    ) -> Result<(), Error> {
        if !decl.any_indexed() {
            return Ok(());
        }
        #[cfg(not(feature = "search"))]
        let _ = embedder;
        #[cfg(feature = "search")]
        let batch = embed_changed_slots(self, decl, record, embedder)?;
        #[cfg(not(feature = "search"))]
        let batch = VectorBatch::empty();
        let index_dir = self.dir()?;
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::upsert] lock")?;
        if !inner.can_write {
            bail!(Error::index_lexical("search index is not open for write"));
        }
        let first_write = match index::format_version(&index_dir)? {
            Some(v) if v != index::FORMAT_VERSION => {
                return Err(Error::index_version(format!(
                    "index format {v} is not {}",
                    index::FORMAT_VERSION
                ))
                .context("[SearchEngine::upsert] check version"));
            }
            Some(_) => false,
            None => true,
        };
        index::ensure_format(&index_dir)?;
        self.ensure_projector_dump(&index_dir, first_write, &decl.name)?;
        upsert_record(
            &mut inner,
            &index_dir,
            decl,
            record,
            embedder,
            &batch,
            self.projector.as_deref(),
        )?;
        Ok(())
    }

    pub(crate) fn forget(&self, decl: &CollectionDecl, id: &str) -> Result<(), Error> {
        if !decl.any_indexed() {
            return Ok(());
        }
        let index_dir = self.dir()?;
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::forget] lock")?;
        if !inner.can_write {
            bail!(Error::index_lexical("search index is not open for write"));
        }
        match index::format_version(&index_dir)? {
            Some(v) if v != index::FORMAT_VERSION => {
                return Err(Error::index_version(format!(
                    "index format {v} is not {}",
                    index::FORMAT_VERSION
                ))
                .context("[SearchEngine::forget] check version"));
            }
            _ => index::ensure_format(&index_dir)?,
        }
        forget_record(&mut inner, &decl.name, id)?;
        Ok(())
    }

    fn require_projector_dump(&self, index_dir: &Path) -> Result<(), Error> {
        match read_projector_dump(index_dir)? {
            None => {
                if self.projector.is_some() {
                    return Err(
                        Error::index_version("index was built without this projector")
                            .context("[SearchEngine] projector dump"),
                    );
                }
                Ok(())
            }
            Some(dump) => self.match_stored_dump(&dump),
        }
    }

    fn ensure_projector_dump(
        &self,
        index_dir: &Path,
        first_write: bool,
        collection: &str,
    ) -> Result<(), Error> {
        match read_projector_dump(index_dir)? {
            None if self.projector.is_none() => Ok(()),
            None if first_write => {
                let dump = match self.projector.as_deref() {
                    None => projector::IDENTITY_DUMP.to_owned(),
                    Some(projector) => {
                        match projector::collection_dump_line(projector, collection)? {
                            Some(line) => format!("{line}\n"),
                            None => String::new(),
                        }
                    }
                };
                write_projector_dump(index_dir, &dump)
            }
            None => Err(
                Error::index_version("index was built without this projector")
                    .context("[SearchEngine] projector dump"),
            ),
            Some(ProjectorDump::Identity) if self.projector.is_some() => Err(Error::index_version(
                "index was built without this projector",
            )
            .context("[SearchEngine] projector dump")),
            Some(ProjectorDump::Identity) => Ok(()),
            Some(_) if self.projector.is_none() => {
                Err(Error::index_version("index was built with a projector")
                    .context("[SearchEngine] projector dump"))
            }
            Some(mut dump) => {
                let Some(projector) = self.projector.as_deref() else {
                    return Ok(());
                };
                let current = projector::collection_dump_line(projector, collection)?;
                match (dump.line(collection), current.as_deref()) {
                    (Some(stored), Some(current)) if stored == current => Ok(()),
                    (Some(_), _) => Err(Error::index_version(format!(
                        "projector schema for {collection} changed"
                    ))
                    .context("[SearchEngine] projector dump")),
                    (None, Some(current)) => {
                        dump.insert_line(collection.to_owned(), current.to_owned());
                        write_projector_dump(index_dir, &dump.to_text())
                    }
                    (None, None) => Ok(()),
                }
            }
        }
    }

    fn match_stored_dump(&self, dump: &ProjectorDump) -> Result<(), Error> {
        match dump {
            ProjectorDump::Identity if self.projector.is_some() => Err(Error::index_version(
                "index was built without this projector",
            )
            .context("[SearchEngine] projector dump")),
            ProjectorDump::Identity => Ok(()),
            ProjectorDump::Collections(_) if self.projector.is_none() => {
                Err(Error::index_version("index was built with a projector")
                    .context("[SearchEngine] projector dump"))
            }
            ProjectorDump::Collections(map) => {
                let Some(projector) = self.projector.as_deref() else {
                    return Ok(());
                };
                for (name, stored) in map {
                    let current = projector::collection_dump_line(projector, name)?;
                    match current {
                        Some(current) if current == *stored => {}
                        _ => {
                            return Err(Error::index_version(format!(
                                "projector schema for {name} changed"
                            ))
                            .context("[SearchEngine] projector dump"));
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Mark this engine as writable. The Tantivy writer is not opened
    /// here: the first indexed write opens it, and [`release_writer`]
    /// drops it when the Vault write lock is released.
    pub(crate) fn enable_write(&self) -> Result<(), Error> {
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::enable_write] lock")?;
        inner.can_write = true;
        drop(inner);
        let index_dir = self.dir()?;
        index::restrict_index_tree(&index_dir)
    }

    /// Persist buffered Tantivy / vector writes, then re-apply owner-only
    /// modes under `{vault}/.scheda/` (Tantivy creates files this crate did
    /// not open). A handle that is not open for write is a no-op.
    pub(crate) fn flush(&self) -> Result<(), Error> {
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::flush] lock")?;
        let can_write = inner.can_write;
        let index_dir = self.dir()?;
        inner.commit(&index_dir, can_write)
    }

    /// Commit buffered index writes and drop the Tantivy `IndexWriter`.
    ///
    /// The writer holds a directory lock for as long as it lives. After a
    /// transaction (or one autocommit) ends, this releases that lock so
    /// another handle can open a writer without waiting out `LockBusy`
    /// retries. The `Index` and reader stay; the next mutating call opens
    /// a writer again.
    ///
    /// Opening a writer allocates a 16 MiB heap and starts a merge
    /// thread, so this is not free. A batch of Record writes inside one
    /// transaction still pays it once. Autocommit pays it per call; that
    /// sits next to a git commit, which already dominates. Holding the
    /// writer for the handle lifetime was cheaper per write and left a
    /// consumer no way to free the lock without dropping the handle.
    pub(crate) fn release_writer(&self) -> Result<(), Error> {
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::release_writer] lock")?;
        let can_write = inner.can_write;
        let index_dir = self.dir()?;
        inner.commit(&index_dir, can_write)?;
        let writer = take_lexical_writer(inner.lexical.as_ref())?;
        drop(inner);
        if let Some(writer) = writer {
            writer.wait_merging_threads().context(
                Code::IndexLexical,
                "[SearchEngine::release_writer] close writer",
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn lexical_writer_opens(&self) -> u64 {
        crate::error::rw_read(&self.inner, "[SearchEngine::lexical_writer_opens] lock")
            .map(|inner| inner.writer_opens)
            .unwrap_or(0)
    }

    /// Run `f` on a consistent Index snapshot.
    ///
    /// Concurrent readers share the lock. A handle with buffered writes
    /// (`can_write` and `dirty`) takes the exclusive side first so `find`
    /// on that writer sees the Records it just added.
    fn with_query<T>(
        &self,
        index_dir: &Path,
        frame: &'static str,
        f: impl FnOnce(&Inner) -> Result<T, Error>,
    ) -> Result<T, Error> {
        {
            let inner = crate::error::rw_read(&self.inner, frame)?;
            if inner.query_ready() {
                return f(&inner);
            }
        }
        let mut inner = crate::error::rw_write(&self.inner, frame)?;
        inner.prepare_query(index_dir)?;
        f(&inner)
    }

    pub(crate) fn has_id(&self, collection: &str, id: &str) -> Result<bool, Error> {
        let index_dir = self.dir()?;
        index::require_fresh(&index_dir)?;
        match index::format_version(&index_dir)? {
            Some(v) if v != index::FORMAT_VERSION => {
                return Err(Error::index_version(format!(
                    "index format {v} is not {}",
                    index::FORMAT_VERSION
                ))
                .context("[SearchEngine::has_id] check version"));
            }
            None => return Ok(false),
            _ => {}
        }
        self.with_query(&index_dir, "[SearchEngine::has_id] lock", |inner| {
            let Some(lexical) = inner.lexical.as_ref() else {
                return Ok(false);
            };
            lexical
                .reader
                .reload()
                .context(Code::IndexLexical, "[SearchEngine::has_id] reload reader")?;
            let searcher = lexical.reader.searcher();
            let query = BooleanQuery::new(vec![
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(lexical.f_collection, collection),
                        IndexRecordOption::Basic,
                    )),
                ),
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(lexical.f_id, id),
                        IndexRecordOption::Basic,
                    )),
                ),
            ]);
            let top = searcher
                .search(&query, &TopDocs::with_limit(1).order_by_score())
                .context(Code::IndexLexical, "[SearchEngine::has_id] search")?;
            Ok(!top.is_empty())
        })
    }

    #[cfg(feature = "search")]
    pub(crate) fn find_ids(
        &self,
        collections: &[&str],
        decls: &[CollectionDecl],
        filter: &Filter,
        mlt_fts: impl Fn(&CollectionDecl, &str) -> Result<String, Error>,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, Error> {
        self.with_find(
            collections,
            decls,
            filter,
            mlt_fts,
            "[SearchEngine::find_ids] reload reader",
            |searcher, query, lexical| {
                // `DocSetCollector` walks matches. `TopDocs::with_limit(u32::MAX)`
                // allocates `2 * limit` scored slots (~64 GiB) and aborts on CI.
                let addrs = searcher
                    .search(query, &DocSetCollector)
                    .context(Code::IndexLexical, "[SearchEngine::find_ids] search")?;
                let mut allowed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                for (collection, id) in ids_from_addrs(
                    searcher,
                    lexical,
                    addrs,
                    "[SearchEngine::find_ids] fetch document",
                )? {
                    allowed.entry(collection).or_default().insert(id);
                }
                Ok(allowed)
            },
        )
        .map(Option::unwrap_or_default)
    }

    pub(crate) fn find(
        &self,
        collections: &[&str],
        journal_ord: bool,
        decls: &[CollectionDecl],
        filter: &Filter,
        offset_count: (u32, u32),
        mlt_fts: impl Fn(&CollectionDecl, &str) -> Result<String, Error>,
    ) -> Result<FindPage, Error> {
        let (offset, count) = offset_count;
        let limit = count as usize;
        if limit == 0 {
            return Ok(FindPage::default());
        }
        self.with_find(
            collections,
            decls,
            filter,
            mlt_fts,
            "[SearchEngine::find] reload reader",
            |searcher, query, lexical| {
                let off = offset as usize;
                let (n, addrs): (usize, Vec<tantivy::DocAddress>) = if journal_ord {
                    let (n, top) = searcher
                        .search(
                            query,
                            &(
                                Count,
                                TopDocs::with_limit(limit)
                                    .and_offset(off)
                                    .order_by_fast_field::<u64>("ord", Order::Asc),
                            ),
                        )
                        .context(Code::IndexLexical, "[SearchEngine::find] search")?;
                    (n, top.into_iter().map(|(_, addr)| addr).collect())
                } else {
                    let (n, top) = searcher
                        .search(query, &(Count, top_by_score_then_ord(limit, off)))
                        .context(Code::IndexLexical, "[SearchEngine::find] search")?;
                    (n, top.into_iter().map(|(_, addr)| addr).collect())
                };
                Ok(FindPage {
                    ids: ids_from_addrs(
                        searcher,
                        lexical,
                        addrs,
                        "[SearchEngine::find] fetch document",
                    )?,
                    total: u32::try_from(n).unwrap_or(u32::MAX),
                })
            },
        )
        .map(Option::unwrap_or_default)
    }

    fn with_find<T>(
        &self,
        collections: &[&str],
        decls: &[CollectionDecl],
        filter: &Filter,
        mlt_fts: impl Fn(&CollectionDecl, &str) -> Result<String, Error>,
        reload_frame: &'static str,
        f: impl FnOnce(&tantivy::Searcher, &BooleanQuery, &Lexical) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        if collections.is_empty() || decls.is_empty() {
            return Ok(None);
        }
        let index_dir = self.dir()?;
        index::require_fresh(&index_dir)?;
        index::require_current_format(&index_dir)?;
        self.require_projector_dump(&index_dir)?;
        self.with_query(&index_dir, "[SearchEngine::with_find] lock", |inner| {
            let Some(lexical) = inner.lexical.as_ref() else {
                return Ok(None);
            };
            lexical
                .reader
                .reload()
                .context(Code::IndexLexical, reload_frame)?;
            let fields = FindFields {
                id: lexical.f_id,
                eq: lexical.f_eq,
                exists: lexical.f_exists,
                null: lexical.f_null,
                tok: lexical.f_tok,
                text: lexical.f_text,
            };
            let query = if decls.len() == 1 {
                let compiled = query::compile(&decls[0], &fields, &lexical.index, filter, &|id| {
                    mlt_fts(&decls[0], id)
                })?;
                BooleanQuery::new(vec![
                    (
                        Occur::Must,
                        collection_query(lexical.f_collection, collections),
                    ),
                    (Occur::Must, compiled),
                ])
            } else {
                let mut clauses = Vec::with_capacity(decls.len());
                for decl in decls {
                    let compiled = query::compile(decl, &fields, &lexical.index, filter, &|id| {
                        mlt_fts(decl, id)
                    })?;
                    clauses.push((
                        Occur::Should,
                        Box::new(BooleanQuery::new(vec![
                            (
                                Occur::Must,
                                Box::new(TermQuery::new(
                                    Term::from_field_text(lexical.f_collection, &decl.name),
                                    IndexRecordOption::Basic,
                                )),
                            ),
                            (Occur::Must, compiled),
                        ])) as Box<dyn Query>,
                    ));
                }
                BooleanQuery::new(clauses)
            };
            let searcher = lexical.reader.searcher();
            Ok(Some(f(&searcher, &query, lexical)?))
        })
    }

    #[cfg(feature = "search")]
    pub(crate) fn search(
        &self,
        decls: &[CollectionDecl],
        query: &SearchQuery,
        embedder: Option<&dyn Embedder>,
        allowed: Option<&BTreeMap<String, BTreeSet<String>>>,
        has_reranker: bool,
    ) -> Result<SearchHits, Error> {
        let lexical_names: Vec<&str> = decls
            .iter()
            .filter(|decl| decl.lexical_indexed())
            .map(|decl| decl.name.as_str())
            .collect();
        let vector_names: Vec<&str> = decls
            .iter()
            .filter(|decl| decl.vector_indexed())
            .map(|decl| decl.name.as_str())
            .collect();
        if lexical_names.is_empty() && vector_names.is_empty() {
            return Ok(SearchHits::default());
        }
        let index_dir = self.dir()?;
        index::require_fresh(&index_dir)?;
        index::require_current_format(&index_dir)?;
        let Some(text) = query.text.as_deref() else {
            unreachable!("SearchQuery text is required");
        };
        let k = match query.k {
            None | Some(0) => DEFAULT_K,
            Some(n) => n as usize,
        };
        let lexical_weight = query.lexical_weight.unwrap_or(1.0);
        let vector_weight = query.vector_weight.unwrap_or(1.0);
        let pool = fusion_pool(k, query.rerank_limit.filter(|_| has_reranker));
        let vector_on = self.projector.is_some()
            && vector_weight > 0.0
            && !vector_names.is_empty()
            && projector_has_vector_column(self.projector.as_deref(), decls);
        let query_vec = if vector_on {
            match embedder {
                Some(embedder) => Some(embedder.embed_query(text)?),
                None => None,
            }
        } else {
            None
        };
        self.with_query(&index_dir, "[SearchEngine::search] lock", |inner| {
            let lexical_hits = if lexical_weight > 0.0 && !lexical_names.is_empty() {
                lexical_search(inner, &lexical_names, text, pool, allowed)?
            } else {
                Vec::new()
            };
            let vector_hits = if vector_on {
                vector_search(
                    inner,
                    &vector_names,
                    query_vec.as_deref(),
                    pool,
                    embedder,
                    allowed,
                )?
            } else {
                Vec::new()
            };
            Ok(fuse(
                &lexical_hits,
                &vector_hits,
                lexical_weight,
                vector_weight,
                pool,
                has_reranker,
            ))
        })
    }
}

fn take_lexical_writer(lexical: Option<&Lexical>) -> Result<Option<IndexWriter>, Error> {
    let Some(lexical) = lexical else {
        return Ok(None);
    };
    Ok(crate::error::mutex(&lexical.writer, "[SearchEngine] take writer")?.take())
}

fn shutdown_lexical(lexical: Option<Lexical>) {
    let Some(lexical) = lexical else {
        return;
    };
    let writer = match lexical.writer.into_inner() {
        Ok(writer) => writer,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(writer) = writer {
        let _ = writer.wait_merging_threads();
    }
}

fn empty_inner(can_write: bool) -> Inner {
    Inner {
        lexical: None,
        #[cfg(feature = "search")]
        vector: None,
        dirty: false,
        can_write,
        next_ord: BTreeMap::new(),
        #[cfg(test)]
        writer_opens: 0,
    }
}

fn index_one_rebuild_record(
    engine: &SearchEngine,
    index_dir: &Path,
    decl: &CollectionDecl,
    record: &Record,
    embedder: Option<&dyn Embedder>,
    #[cfg(feature = "search")] seen: &mut BTreeMap<String, BTreeSet<String>>,
) -> Result<(), Error> {
    #[cfg(feature = "search")]
    let batch = embed_changed_slots(engine, decl, record, embedder)?;
    #[cfg(not(feature = "search"))]
    let batch = VectorBatch::empty();
    let mut inner = crate::error::rw_write(&engine.inner, "[SearchEngine::rebuild] lock")?;
    upsert_record(
        &mut inner,
        index_dir,
        decl,
        record,
        embedder,
        &batch,
        engine.projector.as_deref(),
    )?;
    #[cfg(feature = "search")]
    if decl.vector_indexed()
        && let Some(id) = record.id.as_deref().filter(|id| !id.is_empty())
    {
        seen.entry(decl.name.clone())
            .or_default()
            .insert(id.to_owned());
    }
    Ok(())
}

fn open_lexical_writer(index: &Index, frame: &'static str) -> Result<IndexWriter, Error> {
    let mut delay = Duration::from_millis(10);
    let mut last = None;
    for attempt in 0..8 {
        match index.writer_with_num_threads(1, WRITER_HEAP) {
            Ok(writer) => return Ok(writer),
            Err(err) => {
                let busy = err.to_string().contains("LockBusy");
                last = Some(err);
                if !busy || attempt == 7 {
                    break;
                }
                thread::sleep(delay);
                delay = delay.saturating_mul(2);
            }
        }
    }
    match last {
        Some(err) => Err(err).context(Code::IndexLexical, frame),
        None => unreachable!("open_lexical_writer always stores the last Err"),
    }
}

impl Drop for SearchEngine {
    fn drop(&mut self) {
        let Ok(index_dir) = self.index_dir.get_mut() else {
            return;
        };
        let index_dir = index_dir.clone();
        let Ok(inner) = self.inner.get_mut() else {
            return;
        };
        let can_write = inner.can_write;
        let _ = inner.commit(&index_dir, can_write);
        let lexical = inner.lexical.take();
        shutdown_lexical(lexical);
    }
}

impl Inner {
    fn query_ready(&self) -> bool {
        self.lexical.is_some() && !(self.can_write && self.dirty)
    }

    fn prepare_query(&mut self, index_dir: &Path) -> Result<(), Error> {
        let can_write = self.can_write;
        self.commit(index_dir, can_write)?;
        if self.lexical.is_none() {
            self.lexical = open_lexical(index_dir)?;
        }
        Ok(())
    }

    fn ensure_writer(&mut self) -> Result<(), Error> {
        let Some(lexical) = self.lexical.as_ref() else {
            return Ok(());
        };
        let mut slot = crate::error::mutex(&lexical.writer, "[SearchEngine] open writer")?;
        if slot.is_some() {
            return Ok(());
        }
        *slot = Some(open_lexical_writer(
            &lexical.index,
            "[SearchEngine] open writer",
        )?);
        #[cfg(test)]
        {
            self.writer_opens = self.writer_opens.saturating_add(1);
        }
        Ok(())
    }

    fn commit(&mut self, index_dir: &Path, can_write: bool) -> Result<(), Error> {
        if !self.dirty || !can_write {
            return Ok(());
        }
        if let Some(lexical) = &self.lexical {
            {
                let mut slot =
                    crate::error::mutex(&lexical.writer, "[SearchEngine::commit] writer")?;
                if let Some(writer) = slot.as_mut() {
                    writer
                        .commit()
                        .context(Code::IndexLexical, "[SearchEngine::commit] lexical commit")?;
                }
            }
            lexical
                .reader
                .reload()
                .context(Code::IndexLexical, "[SearchEngine::commit] reload reader")?;
        }
        #[cfg(feature = "search")]
        if let Some(vector) = &self.vector {
            save_vector(index_dir, vector)?;
        }
        save_next_ord(index_dir, &self.next_ord)?;
        self.dirty = false;
        index::restrict_index_tree(index_dir)?;
        Ok(())
    }
}

#[cfg(feature = "search")]
fn embed_changed_slots(
    engine: &SearchEngine,
    decl: &CollectionDecl,
    record: &Record,
    embedder: Option<&dyn Embedder>,
) -> Result<VectorBatch, Error> {
    if !decl.vector_indexed() {
        return Ok(VectorBatch::empty());
    }
    let Some(projector) = engine.projector.as_deref() else {
        return Ok(VectorBatch::empty());
    };
    let schema = match projector.schema(&decl.name) {
        Ok(schema) => {
            schema.require_valid()?;
            schema
        }
        Err(_) => return Ok(VectorBatch::empty()),
    };
    let raw = record.raw.as_deref().unwrap_or(&[]);
    let id = record.id.as_deref().unwrap_or("");
    let doc = match projector.project(&decl.name, id, raw) {
        Ok(doc) if !doc.values.is_empty() => doc,
        _ => return Ok(VectorBatch::empty()),
    };
    let pieces = projector::vector_pieces(&schema, &doc);
    let to_embed = {
        let inner = crate::error::rw_read(&engine.inner, "[SearchEngine] lock vector store")?;
        slots_needing_embed(
            inner.vector.as_ref(),
            embedder,
            &decl.name,
            record.id.as_deref(),
            &pieces,
        )?
    };
    let vectors = embed_field_vectors(embedder, to_embed)?;
    Ok(VectorBatch { pieces, vectors })
}

fn write_projector_dump(index_dir: &Path, dump: &str) -> Result<(), Error> {
    fs::write(index_dir.join(PROJECTOR_FILE), dump)
        .context("[SearchEngine] write projector dump")?;
    index::restrict_index_tree(index_dir)
}

fn read_projector_dump(index_dir: &Path) -> Result<Option<ProjectorDump>, Error> {
    match fs::read_to_string(index_dir.join(PROJECTOR_FILE)) {
        Ok(text) => Ok(Some(ProjectorDump::parse(&text))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context("[SearchEngine] read projector dump"),
    }
}

fn upsert_record(
    inner: &mut Inner,
    index_dir: &Path,
    decl: &CollectionDecl,
    record: &Record,
    embedder: Option<&dyn Embedder>,
    batch: &VectorBatch,
    projector: Option<&dyn crate::Projector>,
) -> Result<(), Error> {
    let Some(id) = record.id.as_deref().filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    let raw = match record.raw.as_deref() {
        Some(raw) => raw,
        None => &[],
    };
    let projected = match projector {
        None => Some(IndexDoc::default()),
        Some(projector) => match projector.project(&decl.name, id, raw) {
            Ok(doc) if doc.values.is_empty() => None,
            Ok(doc) => Some(doc),
            Err(_) => None,
        },
    };
    let can_write = inner.can_write;
    if projected.is_none() {
        if can_write {
            forget_record(inner, &decl.name, id)?;
        }
        #[cfg(not(feature = "search"))]
        let _ = (embedder, batch);
        return Ok(());
    }
    let find_schema = match projector {
        Some(projector) => match projector.schema(&decl.name) {
            Ok(schema) => {
                schema.require_valid()?;
                Some(schema)
            }
            Err(_) => None,
        },
        None => None,
    };
    let fts_text = match (projected.as_ref(), find_schema.as_ref()) {
        (Some(doc), Some(schema)) => projector::fts_blob(schema, doc),
        _ => String::new(),
    };
    if can_write {
        if decl.lexical_indexed() {
            if inner.lexical.is_none() {
                inner.lexical = Some(create_lexical(index_dir)?);
            }
            inner.ensure_writer()?;
            let ord = inner.next_ord.get(&decl.name).copied().unwrap_or(0);
            inner
                .next_ord
                .insert(decl.name.clone(), ord.saturating_add(1));
            if let Some(lexical) = inner.lexical.as_mut() {
                index_lexical(
                    lexical,
                    decl,
                    record,
                    &fts_text,
                    ord,
                    projected.as_ref(),
                    find_schema.as_ref(),
                )?;
                inner.dirty = true;
            }
        }
        #[cfg(feature = "search")]
        if decl.vector_indexed()
            && projector.is_some()
            && let Some(embedder) = embedder
        {
            let names = find_schema
                .as_ref()
                .map(projector::vector_slot_names)
                .unwrap_or_default();
            upsert_vector(inner, embedder, decl, record, batch, &names)?;
            inner.dirty = true;
        }
    }
    #[cfg(not(feature = "search"))]
    let _ = (embedder, batch);
    Ok(())
}

fn forget_record(inner: &mut Inner, collection: &str, id: &str) -> Result<(), Error> {
    inner.dirty = true;
    inner.ensure_writer()?;
    if let Some(lexical) = inner.lexical.as_ref() {
        let slot = crate::error::mutex(&lexical.writer, "[SearchEngine::forget] writer")?;
        if let Some(writer) = slot.as_ref() {
            let key = doc_key(collection, id);
            writer.delete_term(Term::from_field_text(lexical.f_key, &key));
        }
    }
    #[cfg(feature = "search")]
    forget_vector(inner, collection, id)?;
    Ok(())
}

#[cfg(feature = "search")]
fn forget_vector(inner: &mut Inner, collection: &str, id: &str) -> Result<(), Error> {
    let Some(store) = inner.vector.as_mut() else {
        return Ok(());
    };
    let stale = store.record_keys(collection, id);
    for key in stale {
        store.drop_ann_slot(key)?;
    }
    refresh_field_ids(store);
    Ok(())
}

fn index_lexical(
    lexical: &Lexical,
    decl: &CollectionDecl,
    record: &Record,
    text: &str,
    ord: u64,
    index_doc: Option<&IndexDoc>,
    index_schema: Option<&IndexSchema>,
) -> Result<(), Error> {
    let slot = crate::error::mutex(&lexical.writer, "[SearchEngine::index_lexical] writer")?;
    let Some(writer) = slot.as_ref() else {
        return Ok(());
    };
    let Some(id) = record.id.as_deref() else {
        return Ok(());
    };
    let path = indexed_rel(decl, id)?;
    let key = doc_key(&decl.name, id);
    writer.delete_term(Term::from_field_text(lexical.f_key, &key));
    let mut doc = TantivyDocument::default();
    doc.add_text(lexical.f_key, &key);
    doc.add_text(lexical.f_collection, &decl.name);
    doc.add_text(lexical.f_id, id);
    doc.add_text(lexical.f_path, &path);
    doc.add_u64(lexical.f_ord, ord);
    if !text.is_empty() {
        doc.add_text(lexical.f_text, text);
    }
    let mut analyzer = lexical.index.tokenizer_for_field(lexical.f_tok).context(
        Code::IndexLexical,
        "[SearchEngine::index_lexical] tokenizer",
    )?;
    if let (Some(index_doc), Some(index_schema)) = (index_doc, index_schema) {
        index_find_fields(lexical, &mut doc, index_schema, index_doc, &mut analyzer)?;
    }
    writer.add_document(doc).context(
        Code::IndexLexical,
        "[SearchEngine::index_lexical] add document",
    )?;
    Ok(())
}

fn index_find_fields(
    lexical: &Lexical,
    doc: &mut TantivyDocument,
    schema: &IndexSchema,
    values: &IndexDoc,
    analyzer: &mut tantivy::tokenizer::TextAnalyzer,
) -> Result<(), Error> {
    for column in &schema.columns {
        if !column.find.unwrap_or(false) {
            continue;
        }
        let name = column.name.as_deref().unwrap_or("");
        match values.values.get(name) {
            None => {}
            Some(IndexValue::Null) => {
                doc.add_text(lexical.f_null, name);
            }
            Some(value) => {
                let texts = projector::eq_texts_owned(value);
                if texts.is_empty() {
                    continue;
                }
                doc.add_text(lexical.f_exists, name);
                for text in &texts {
                    doc.add_text(lexical.f_eq, query::eq_term(name, text)?);
                    query::add_field_tokens(doc, lexical.f_tok, name, text, analyzer)?;
                }
            }
        }
    }
    Ok(())
}

/// SHA-1 git blob object id of `bytes` (`blob <len>\0` + content).
#[cfg(feature = "search")]
fn blob_oid(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(bytes.len().to_string().as_bytes());
    hasher.update([0u8]);
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

#[cfg(feature = "search")]
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 0x0f)] as char);
    }
    out
}

#[cfg(feature = "search")]
fn vector_digest(text: &str) -> String {
    blob_oid(text.as_bytes())
}

#[cfg(feature = "search")]
fn slots_needing_embed(
    store: Option<&VectorStore>,
    embedder: Option<&dyn Embedder>,
    collection: &str,
    id: Option<&str>,
    pieces: &[(String, String)],
) -> Result<Vec<(String, String)>, Error> {
    let Some(embedder) = embedder else {
        return Ok(Vec::new());
    };
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return Ok(Vec::new());
    };
    let nonempty: Vec<(String, String)> = pieces
        .iter()
        .filter(|(_, text)| !text.is_empty())
        .cloned()
        .collect();
    let Some(store) = store else {
        return Ok(nonempty);
    };
    if store.model_id != embedder.model_id() || store.dimensions != embedder.dimensions().get() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (field, text) in nonempty {
        let digest = vector_digest(&text);
        let unchanged = store
            .slot_key(collection, id, &field)
            .and_then(|key| store.keys.get(&key))
            .is_some_and(|doc| doc.digest == digest);
        if !unchanged {
            out.push((field, text));
        }
    }
    Ok(out)
}

#[cfg(feature = "search")]
fn refresh_field_ids(store: &mut VectorStore) {
    store.field_ids = store.keys.values().map(|doc| doc.field.clone()).collect();
}

#[cfg(feature = "search")]
fn drop_unseen_vectors(
    inner: &mut Inner,
    seen: &BTreeMap<String, BTreeSet<String>>,
) -> Result<(), Error> {
    let Some(store) = inner.vector.as_mut() else {
        return Ok(());
    };
    let stale: Vec<u64> = store
        .keys
        .iter()
        .filter(|(_, doc)| !allowed_has(seen, &doc.collection, &doc.id))
        .map(|(key, _)| *key)
        .collect();
    if stale.is_empty() {
        return Ok(());
    }
    for key in stale {
        store.drop_ann_slot(key)?;
    }
    refresh_field_ids(store);
    inner.dirty = true;
    Ok(())
}

#[cfg(feature = "search")]
fn peek_vector_meta(index_dir: &Path) -> Result<Option<VectorMeta>, Error> {
    let meta_path = index_dir.join(VECTOR_DIR).join(VECTOR_META);
    let dump = index_dir.join(VECTOR_DIR).join(VECTOR_INDEX);
    let keys = index_dir.join(VECTOR_DIR).join(VECTOR_KEYS);
    if !meta_path.is_file() || !dump.is_file() || !keys.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&meta_path).context("[SearchEngine] read vector meta")?;
    let meta: VectorMeta = serde_json::from_str(&raw)
        .context(Code::IndexCorrupt, "[SearchEngine] decode vector meta")?;
    Ok(Some(meta))
}

#[cfg(feature = "search")]
fn copy_vector_dir(from: &Path, to: &Path) -> Result<(), Error> {
    let src = from.join(VECTOR_DIR);
    let dst = to.join(VECTOR_DIR);
    fs::create_dir_all(&dst).context("[SearchEngine] create vector dir")?;
    for name in [VECTOR_INDEX, VECTOR_META, VECTOR_KEYS] {
        let from_file = src.join(name);
        if !from_file.is_file() {
            continue;
        }
        fs::copy(&from_file, dst.join(name)).context("[SearchEngine] copy vector file")?;
    }
    Ok(())
}

/// Copy `source`'s vector dump into `dest` when the embedder matches
/// `model_id` and dimensions. Does not restore the ANN index.
#[cfg(feature = "search")]
fn copy_matching_vector_dump(
    source: &Path,
    dest: &Path,
    embedder: Option<&dyn Embedder>,
) -> Result<bool, Error> {
    let Some(embedder) = embedder else {
        return Ok(false);
    };
    let Some(meta) = peek_vector_meta(source)? else {
        return Ok(false);
    };
    if meta.model_id != embedder.model_id() || meta.dimensions != embedder.dimensions().get() {
        return Ok(false);
    }
    if source != dest {
        copy_vector_dir(source, dest)?;
    }
    Ok(true)
}

#[cfg(feature = "search")]
pub(crate) fn copy_vector_dump(
    source: &Path,
    dest: &Path,
    embedder: Option<&dyn Embedder>,
) -> Result<(), Error> {
    let _ = copy_matching_vector_dump(source, dest, embedder)?;
    Ok(())
}

#[cfg(feature = "search")]
fn inherit_vector(
    source: &Path,
    dest: &Path,
    embedder: Option<&dyn Embedder>,
) -> Result<Option<VectorStore>, Error> {
    if !copy_matching_vector_dump(source, dest, embedder)? {
        return Ok(None);
    }
    load_vector(dest)
}

/// Vector slots of one generation, read from its dump.
#[cfg(feature = "search")]
pub(crate) struct VectorSource {
    store: Option<VectorStore>,
}

/// Load the vector dump under `dir` (`dir/vector/`). Missing dump → empty source.
#[cfg(feature = "search")]
pub(crate) fn load_vector_source(dir: &Path) -> Result<VectorSource, Error> {
    Ok(VectorSource {
        store: load_vector(dir)?,
    })
}

#[cfg(feature = "search")]
impl VectorSource {
    /// Copy this Record's slots into `dest`. Same digest already in `dest`
    /// is left in place; slots `dest` has that this source no longer has
    /// are removed. Marks `dest` dirty; does not commit.
    pub(crate) fn copy_record(
        &self,
        dest: &SearchEngine,
        collection: &str,
        id: &str,
    ) -> Result<(), Error> {
        let Some(src) = self.store.as_ref() else {
            return Ok(());
        };
        let model_id = src.model_id.clone();
        let dimensions = src.dimensions;
        let mut copied = Vec::new();
        if let Some(fields) = src.record_slots(collection, id) {
            for key in fields.values() {
                let Some(doc) = src.keys.get(key) else {
                    continue;
                };
                let Some(vector) = src.ann.get(*key)? else {
                    continue;
                };
                copied.push(CopiedSlot {
                    doc: doc.clone(),
                    vector,
                    keep: false,
                });
            }
        }
        let mut inner = crate::error::rw_write(&dest.inner, "[SearchEngine] copy vectors")?;
        if inner.vector.is_none() {
            if copied.is_empty() {
                return Ok(());
            }
            inner.vector = Some(VectorStore::empty(
                crate::ann::AnnIndex::new(dimensions)?,
                model_id.clone(),
                dimensions,
            ));
        }
        let Some(store) = inner.vector.as_mut() else {
            return Ok(());
        };
        if store.model_id != model_id || store.dimensions != dimensions {
            return Ok(());
        }
        let dest_keys: Vec<(u64, String, String)> = match store.record_slots(collection, id) {
            Some(fields) => fields
                .iter()
                .filter_map(|(field, key)| {
                    store
                        .keys
                        .get(key)
                        .map(|doc| (*key, field.clone(), doc.digest.clone()))
                })
                .collect(),
            None => Vec::new(),
        };
        let mut changed = false;
        for (key, field, digest) in dest_keys {
            match copied
                .iter_mut()
                .find(|slot| !slot.keep && slot.doc.field == field && slot.doc.digest == digest)
            {
                Some(slot) => slot.keep = true,
                None => {
                    store.drop_ann_slot(key)?;
                    changed = true;
                }
            }
        }
        for slot in copied {
            if slot.keep || slot.vector.len() != dimensions {
                continue;
            }
            let key = store.next_key.max(1);
            store.next_key = key.saturating_add(1);
            store.ann.add(key, &slot.vector)?;
            store.insert_slot(key, slot.doc);
            changed = true;
        }
        if changed {
            refresh_field_ids(store);
            inner.dirty = true;
        }
        Ok(())
    }
}

#[cfg(feature = "search")]
struct CopiedSlot {
    doc: DocRef,
    vector: Vec<f32>,
    keep: bool,
}

#[cfg(feature = "search")]
fn upsert_vector(
    inner: &mut Inner,
    embedder: &dyn Embedder,
    decl: &CollectionDecl,
    record: &Record,
    batch: &VectorBatch,
    vector_names: &BTreeSet<String>,
) -> Result<(), Error> {
    let Some(id) = record.id.as_deref() else {
        return Ok(());
    };
    let path = indexed_rel(decl, id)?;
    let dimensions = embedder.dimensions().get();
    let model_id = embedder.model_id();
    if inner.vector.is_none() {
        inner.vector = Some(VectorStore::empty(
            AnnIndex::new(dimensions)?,
            model_id.to_owned(),
            dimensions,
        ));
    }
    let Some(store) = inner.vector.as_mut() else {
        return Ok(());
    };
    if store.model_id != model_id || store.dimensions != dimensions {
        return Ok(());
    }
    let current: BTreeSet<&str> = batch
        .pieces
        .iter()
        .filter(|(_, text)| !text.is_empty())
        .map(|(field, _)| field.as_str())
        .collect();
    let vanished: Vec<u64> = match store.record_slots(&decl.name, id) {
        Some(fields) => {
            let mut stale = Vec::new();
            for (name, key) in fields {
                if !current.contains(name.as_str()) {
                    stale.push(*key);
                }
            }
            stale
        }
        None => Vec::new(),
    };
    for key in vanished {
        store.drop_ann_slot(key)?;
    }
    let new_vectors: BTreeMap<&str, &Vec<f32>> = batch
        .vectors
        .iter()
        .map(|(field, vector)| (field.as_str(), vector))
        .collect();
    for (field, text) in &batch.pieces {
        if text.is_empty() {
            continue;
        }
        if let Some(vector) = new_vectors.get(field.as_str()) {
            if vector.len() != dimensions {
                bail!(Error::invalid(format!(
                    "embedder returned {} dimensions, expected {dimensions}",
                    vector.len()
                )));
            }
            let digest = vector_digest(text);
            if let Some(key) = store.slot_key(&decl.name, id, field) {
                store.drop_ann_slot(key)?;
            }
            let key = store.next_key.max(1);
            store.next_key = key.saturating_add(1);
            store.ann.add(key, vector)?;
            store.insert_slot(
                key,
                DocRef {
                    collection: decl.name.clone(),
                    id: id.to_owned(),
                    path: path.clone(),
                    field: field.clone(),
                    digest,
                },
            );
        } else if let Some(key) = store.slot_key(&decl.name, id, field)
            && let Some(doc) = store.keys.get_mut(&key)
        {
            doc.path.clone_from(&path);
        }
    }
    refresh_field_ids(store);
    store.field_ids.extend(vector_names.iter().cloned());
    Ok(())
}

#[cfg(feature = "search")]
fn embed_field_vectors(
    embedder: Option<&dyn Embedder>,
    pieces: Vec<(String, String)>,
) -> Result<Vec<(String, Vec<f32>)>, Error> {
    let Some(embedder) = embedder else {
        return Ok(Vec::new());
    };
    let mut fields = Vec::new();
    let mut texts = Vec::new();
    for (field, text) in pieces {
        if text.is_empty() {
            continue;
        }
        fields.push(field);
        texts.push(text);
    }
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let vectors = crate::embed::embed_all(embedder, &refs)?;
    Ok(fields.into_iter().zip(vectors).collect())
}

#[cfg(feature = "search")]
fn projector_has_vector_column(
    projector: Option<&dyn crate::Projector>,
    decls: &[CollectionDecl],
) -> bool {
    let Some(projector) = projector else {
        return false;
    };
    decls.iter().any(|decl| {
        if !decl.vector_indexed() {
            return false;
        }
        match projector.schema(&decl.name) {
            Ok(schema) => schema
                .columns
                .iter()
                .any(|column| column.vector.unwrap_or(false)),
            Err(_) => false,
        }
    })
}

/// Depth each Index leg retrieves, and the fused list length, before any
/// rerank truncation. Unset `rerank_limit` (or no reranker) keeps the
/// floor. A limit above the floor is the caller's requested depth.
///
/// With a reranker the fused list is filled from each leg's ranks in
/// alternation, not by Reciprocal Rank Fusion order. The depth is still
/// this pool: it is how many candidates the reranker may see.
#[cfg(feature = "search")]
fn fusion_pool(k: usize, rerank_limit: Option<u32>) -> usize {
    let floor = k.max(FUSION_POOL_FLOOR);
    match rerank_limit {
        Some(limit) => floor.max(limit as usize),
        None => floor,
    }
}

#[cfg(feature = "search")]
fn lexical_search(
    inner: &Inner,
    collections: &[&str],
    text: &str,
    pool: usize,
    allowed: Option<&BTreeMap<String, BTreeSet<String>>>,
) -> Result<Vec<LegHit>, Error> {
    let Some(lexical) = inner.lexical.as_ref() else {
        return Ok(Vec::new());
    };
    lexical.reader.reload().context(
        Code::IndexLexical,
        "[SearchEngine::lexical_search] reload reader",
    )?;
    let searcher = lexical.reader.searcher();
    let parser = QueryParser::for_index(&lexical.index, vec![lexical.f_text]);
    let parsed = match parse_text_query(&parser, text) {
        Some(query) => query,
        None => return Ok(Vec::new()),
    };
    let query = BooleanQuery::new(vec![
        (
            Occur::Must,
            collection_query(lexical.f_collection, collections),
        ),
        (Occur::Must, parsed),
    ]);
    if pool == 0 {
        return Ok(Vec::new());
    }
    let top = searcher
        .search(&query, &top_by_score_then_ord(pool, 0))
        .context(Code::IndexLexical, "[SearchEngine::lexical_search] search")?;
    let mut hits = Vec::new();
    for ((score, _), addr) in top {
        let doc: TantivyDocument = searcher.doc(addr).context(
            Code::IndexLexical,
            "[SearchEngine::lexical_search] fetch document",
        )?;
        let Some(collection) = stored_str(&doc, lexical.f_collection) else {
            continue;
        };
        let Some(id) = stored_str(&doc, lexical.f_id) else {
            continue;
        };
        if allowed.is_some_and(|set| !allowed_has(set, &collection, &id)) {
            continue;
        }
        hits.push(LegHit {
            collection,
            id,
            score: f64::from(score),
        });
    }
    Ok(hits)
}

#[cfg(feature = "search")]
fn vector_search(
    inner: &Inner,
    collections: &[&str],
    query: Option<&[f32]>,
    pool: usize,
    embedder: Option<&dyn Embedder>,
    allowed: Option<&BTreeMap<String, BTreeSet<String>>>,
) -> Result<Vec<LegHit>, Error> {
    let Some(embedder) = embedder else {
        return Ok(Vec::new());
    };
    let Some(query) = query else {
        return Ok(Vec::new());
    };
    let Some(store) = inner.vector.as_ref() else {
        return Ok(Vec::new());
    };
    if store.model_id != embedder.model_id() || store.dimensions != embedder.dimensions().get() {
        return Ok(Vec::new());
    }
    if store.ann.is_empty() {
        return Ok(Vec::new());
    }
    if query.len() != store.dimensions {
        bail!(Error::invalid(format!(
            "embedder returned {} dimensions, expected {}",
            query.len(),
            store.dimensions
        )));
    }
    let names: BTreeSet<&str> = collections.iter().copied().collect();
    let slot_k = pool.max(crate::ann::AnnIndex::EXPANSION_SEARCH);
    let raw = store.ann.filtered_search(query, slot_k, |key| {
        store.keys.get(&key).is_some_and(|doc| {
            names.contains(doc.collection.as_str())
                && allowed.is_none_or(|set| allowed_has(set, &doc.collection, &doc.id))
        })
    })?;
    let mut best: BTreeMap<(&str, &str), LegHit> = BTreeMap::new();
    for (key, distance) in raw {
        let Some(doc) = store.keys.get(&key) else {
            continue;
        };
        if !names.contains(doc.collection.as_str()) {
            continue;
        }
        if allowed.is_some_and(|set| !allowed_has(set, &doc.collection, &doc.id)) {
            continue;
        }
        let score = f64::from(1.0 / (1.0 + distance));
        best.entry((doc.collection.as_str(), doc.id.as_str()))
            .and_modify(|hit| {
                if score > hit.score {
                    hit.score = score;
                }
            })
            .or_insert_with(|| LegHit {
                collection: doc.collection.clone(),
                id: doc.id.clone(),
                score,
            });
    }
    let mut hits: Vec<_> = best.into_values().collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.collection.cmp(&b.collection))
            .then_with(|| a.id.cmp(&b.id))
    });
    hits.truncate(pool);
    Ok(hits)
}

#[cfg(feature = "search")]
struct FusedScores {
    rrf: f64,
    lexical: Option<f64>,
    vector: Option<f64>,
}

/// Fuse the two Index legs.
///
/// Reciprocal Rank Fusion scores are always computed (and reported). Without
/// a reranker those scores also decide membership and order: the fused list
/// is sorted by RRF, ties broken by `(collection, id)`, then truncated to
/// `k`. That is the page the caller receives.
///
/// With a reranker the fused order is discarded, so RRF must not decide
/// who enters the pool. Membership is filled by alternating the vector
/// leg's ranks with the lexical leg's ranks (vector rank 1, lexical rank 1,
/// vector rank 2, …), skipping a document already taken, until `k` is
/// filled or both legs are exhausted. A shorter leg does not leave the
/// pool short while the other still has hits. Zero weight still means that
/// leg did not run; when both weights are non-zero the interleave is
/// one-for-one and the weights no longer govern membership.
#[cfg(feature = "search")]
fn fuse<'a>(
    lexical: &'a [LegHit],
    vector: &'a [LegHit],
    lexical_weight: f64,
    vector_weight: f64,
    k: usize,
    has_reranker: bool,
) -> SearchHits {
    let mut scores: BTreeMap<(&'a str, &'a str), FusedScores> = BTreeMap::new();
    if lexical_weight > 0.0 {
        for (rank, hit) in lexical.iter().enumerate() {
            let rrf = lexical_weight / (RRF_K + (rank as f64) + 1.0);
            let entry = scores
                .entry((hit.collection.as_str(), hit.id.as_str()))
                .or_insert_with(|| FusedScores {
                    rrf: 0.0,
                    lexical: None,
                    vector: None,
                });
            entry.rrf += rrf;
            entry.lexical = Some(hit.score);
        }
    }
    if vector_weight > 0.0 {
        for (rank, hit) in vector.iter().enumerate() {
            let rrf = vector_weight / (RRF_K + (rank as f64) + 1.0);
            let entry = scores
                .entry((hit.collection.as_str(), hit.id.as_str()))
                .or_insert_with(|| FusedScores {
                    rrf: 0.0,
                    lexical: None,
                    vector: None,
                });
            entry.rrf += rrf;
            entry.vector = Some(hit.score);
        }
    }
    if has_reranker {
        return fuse_by_leg_rank(lexical, vector, scores, k);
    }
    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.rrf
            .partial_cmp(&a.1.rrf)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut hits = SearchHits::default();
    for ((collection, id), fused) in ranked.into_iter().take(k) {
        hits.hits.push(hit_from_fused(collection, id, fused));
    }
    hits
}

#[cfg(feature = "search")]
fn fuse_by_leg_rank<'a>(
    lexical: &'a [LegHit],
    vector: &'a [LegHit],
    mut scores: BTreeMap<(&'a str, &'a str), FusedScores>,
    k: usize,
) -> SearchHits {
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut chosen: Vec<&LegHit> = Vec::new();
    let mut rank = 0;
    while chosen.len() < k {
        let mut progressed = false;
        if let Some(hit) = vector.get(rank) {
            progressed = true;
            if chosen.len() < k && seen.insert((hit.collection.as_str(), hit.id.as_str())) {
                chosen.push(hit);
            }
        }
        if let Some(hit) = lexical.get(rank) {
            progressed = true;
            if chosen.len() < k && seen.insert((hit.collection.as_str(), hit.id.as_str())) {
                chosen.push(hit);
            }
        }
        if !progressed {
            break;
        }
        rank += 1;
    }
    let mut hits = SearchHits::default();
    for hit in chosen {
        let Some(fused) = scores.remove(&(hit.collection.as_str(), hit.id.as_str())) else {
            continue;
        };
        hits.hits
            .push(hit_from_fused(&hit.collection, &hit.id, fused));
    }
    hits
}

#[cfg(feature = "search")]
fn hit_from_fused(collection: &str, id: &str, fused: FusedScores) -> search_hits::Hit {
    let mut hit = search_hits::Hit::default()
        .with_id(id)
        .with_collection(collection)
        .with_rrf_score(fused.rrf);
    if let Some(score) = fused.lexical {
        hit = hit.with_lexical_score(score);
    }
    if let Some(score) = fused.vector {
        hit = hit.with_vector_score(score);
    }
    hit
}

#[cfg(feature = "search")]
fn parse_text_query(parser: &QueryParser, text: &str) -> Option<Box<dyn Query>> {
    if let Ok(query) = parser.parse_query(text) {
        return Some(query);
    }
    let escaped: String = text
        .chars()
        .map(|ch| {
            if matches!(
                ch,
                ':' | '*' | '?' | '[' | ']' | '{' | '}' | '^' | '"' | '\\' | '(' | ')' | '!' | '~'
            ) {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let trimmed = escaped.trim();
    if trimmed.is_empty() {
        return None;
    }
    parser.parse_query(trimmed).ok()
}

/// Score descending, then Index `ord` ascending. `ord` is unique within a
/// collection; two Records in different collections that share both score and
/// `ord` stay unordered relative to each other (Tantivy then uses `DocAddress`).
fn top_by_score_then_ord(
    limit: usize,
    offset: usize,
) -> impl tantivy::collector::Collector<Fruit = Vec<((Score, Reverse<u64>), tantivy::DocAddress)>> {
    TopDocs::with_limit(limit)
        .and_offset(offset)
        .tweak_score(|segment_reader: &SegmentReader| {
            let ords = segment_reader.fast_fields().u64("ord").ok();
            move |doc, score| {
                (
                    score,
                    Reverse(ords.as_ref().and_then(|col| col.first(doc)).unwrap_or(0)),
                )
            }
        })
}

#[derive(Default)]
pub(crate) struct FindPage {
    pub(crate) ids: Vec<(String, String)>,
    pub(crate) total: u32,
}

fn ids_from_addrs(
    searcher: &tantivy::Searcher,
    lexical: &Lexical,
    addrs: impl IntoIterator<Item = tantivy::DocAddress>,
    fetch_frame: &'static str,
) -> Result<Vec<(String, String)>, Error> {
    let mut ids = Vec::new();
    for addr in addrs {
        let doc: TantivyDocument = searcher
            .doc(addr)
            .context(Code::IndexLexical, fetch_frame)?;
        let Some(collection) = stored_str(&doc, lexical.f_collection) else {
            continue;
        };
        let Some(id) = stored_str(&doc, lexical.f_id) else {
            continue;
        };
        ids.push((collection, id));
    }
    Ok(ids)
}

fn stored_str(doc: &TantivyDocument, field: Field) -> Option<String> {
    doc.get_first(field)
        .and_then(|value| TantivyValue::as_str(&value).map(str::to_owned))
}

fn indexed_rel(decl: &CollectionDecl, id: &str) -> Result<String, Error> {
    match decl.kind {
        Kind::Journal => Ok(String::new()),
        Kind::Document | Kind::Singleton => store::record_rel(decl, id),
    }
}

fn lexical_schema() -> TantivySchema {
    let mut builder = TantivySchema::builder();
    builder.add_text_field("key", STRING | STORED);
    builder.add_text_field("collection", STRING | STORED);
    builder.add_text_field("id", STRING | STORED);
    builder.add_text_field("path", STRING | STORED);
    builder.add_text_field("text", TEXT);
    builder.add_text_field("eq", STRING);
    builder.add_text_field("exists", STRING);
    builder.add_text_field("null", STRING);
    builder.add_text_field("tok", TEXT);
    builder.add_u64_field("ord", FAST | INDEXED | STORED);
    builder.build()
}

fn open_lexical(index_dir: &Path) -> Result<Option<Lexical>, Error> {
    let path = index_dir.join(LEXICAL_DIR);
    if !path.join("meta.json").is_file() {
        return Ok(None);
    }
    let index = Index::open_in_dir(&path).context(
        Code::IndexLexical,
        "[SearchEngine::open_lexical] open index",
    )?;
    Ok(Some(bind_lexical(index)?))
}

fn create_lexical(index_dir: &Path) -> Result<Lexical, Error> {
    let path = index_dir.join(LEXICAL_DIR);
    fs::create_dir_all(&path).context("[SearchEngine::create_lexical] create dir")?;
    let schema = lexical_schema();
    let index = if path.join("meta.json").is_file() {
        Index::open_in_dir(&path).context(
            Code::IndexLexical,
            "[SearchEngine::create_lexical] open index",
        )?
    } else {
        Index::create_in_dir(&path, schema).context(
            Code::IndexLexical,
            "[SearchEngine::create_lexical] create index",
        )?
    };
    index::restrict_index_tree(index_dir)?;
    bind_lexical(index)
}

fn bind_lexical(index: Index) -> Result<Lexical, Error> {
    let schema = index.schema();
    let f_key = schema
        .get_field("key")
        .context(Code::IndexLexical, "[SearchEngine::bind_lexical] field key")?;
    let f_collection = schema.get_field("collection").context(
        Code::IndexLexical,
        "[SearchEngine::bind_lexical] field collection",
    )?;
    let f_id = schema
        .get_field("id")
        .context(Code::IndexLexical, "[SearchEngine::bind_lexical] field id")?;
    let f_path = schema.get_field("path").context(
        Code::IndexLexical,
        "[SearchEngine::bind_lexical] field path",
    )?;
    let f_text = schema.get_field("text").context(
        Code::IndexLexical,
        "[SearchEngine::bind_lexical] field text",
    )?;
    let f_eq = schema
        .get_field("eq")
        .context(Code::IndexLexical, "[SearchEngine::bind_lexical] field eq")?;
    let f_exists = schema.get_field("exists").context(
        Code::IndexLexical,
        "[SearchEngine::bind_lexical] field exists",
    )?;
    let f_null = schema.get_field("null").context(
        Code::IndexLexical,
        "[SearchEngine::bind_lexical] field null",
    )?;
    let f_tok = schema
        .get_field("tok")
        .context(Code::IndexLexical, "[SearchEngine::bind_lexical] field tok")?;
    let f_ord = schema
        .get_field("ord")
        .context(Code::IndexLexical, "[SearchEngine::bind_lexical] field ord")?;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .context(
            Code::IndexLexical,
            "[SearchEngine::bind_lexical] open reader",
        )?;
    Ok(Lexical {
        index,
        writer: Mutex::new(None),
        reader,
        f_key,
        f_collection,
        f_id,
        f_path,
        f_text,
        f_eq,
        f_exists,
        f_null,
        f_tok,
        f_ord,
    })
}

#[cfg(feature = "search")]
fn load_vector(index_dir: &Path) -> Result<Option<VectorStore>, Error> {
    let dir = index_dir.join(VECTOR_DIR);
    let meta_path = dir.join(VECTOR_META);
    let dump = dir.join(VECTOR_INDEX);
    if !meta_path.is_file() || !dump.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&meta_path).context("[SearchEngine::load_vector] read meta")?;
    let meta: VectorMeta = serde_json::from_str(&raw).context(
        Code::IndexCorrupt,
        "[SearchEngine::load_vector] decode meta",
    )?;
    let ann = AnnIndex::restore(&dump)?;
    if ann.dimensions() != meta.dimensions {
        return Ok(None);
    }
    let keys_path = dir.join(VECTOR_KEYS);
    let keys = if keys_path.is_file() {
        let text =
            fs::read_to_string(&keys_path).context("[SearchEngine::load_vector] read keys")?;
        serde_json::from_str(&text).context(
            Code::IndexCorrupt,
            "[SearchEngine::load_vector] decode keys",
        )?
    } else {
        BTreeMap::new()
    };
    let next_key = next_ann_key(meta.next_key, &keys);
    Ok(Some(VectorStore::loaded(
        ann,
        keys,
        meta.model_id,
        meta.dimensions,
        meta.field_ids.into_iter().collect(),
        next_key,
    )))
}

#[cfg(feature = "search")]
fn save_vector(index_dir: &Path, store: &VectorStore) -> Result<(), Error> {
    let dir = index_dir.join(VECTOR_DIR);
    fs::create_dir_all(&dir).context("[SearchEngine::save_vector] create dir")?;
    store.ann.save(&dir.join(VECTOR_INDEX))?;
    let meta = VectorMeta {
        model_id: store.model_id.clone(),
        dimensions: store.dimensions,
        field_ids: store.field_ids.iter().cloned().collect(),
        next_key: store.next_key,
    };
    let meta_bytes = serde_json::to_vec_pretty(&meta)
        .context(Code::Invalid, "[SearchEngine::save_vector] encode meta")?;
    store::atomic_write(&dir.join(VECTOR_META), &meta_bytes)?;
    let keys_bytes = serde_json::to_vec(&store.keys)
        .context(Code::Invalid, "[SearchEngine::save_vector] encode keys")?;
    store::atomic_write(&dir.join(VECTOR_KEYS), &keys_bytes)
}

fn doc_key(collection: &str, id: &str) -> String {
    format!("{collection}\0{id}")
}

#[cfg(feature = "search")]
fn next_ann_key(meta_next: u64, keys: &BTreeMap<u64, DocRef>) -> u64 {
    let max_key = keys.last_key_value().map(|(key, _)| *key).unwrap_or(0);
    meta_next.max(max_key.saturating_add(1)).max(1)
}

fn load_next_ord(index_dir: &Path) -> Result<BTreeMap<String, u64>, Error> {
    let path = index_dir.join(ORD_FILE);
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let raw = fs::read(&path).context("[SearchEngine::load_next_ord] read ord")?;
    serde_json::from_slice(&raw).context(
        Code::IndexCorrupt,
        "[SearchEngine::load_next_ord] decode ord",
    )
}

fn save_next_ord(index_dir: &Path, next_ord: &BTreeMap<String, u64>) -> Result<(), Error> {
    let path = index_dir.join(ORD_FILE);
    if next_ord.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context("[SearchEngine::save_next_ord] remove ord"),
        }
    } else {
        let bytes = serde_json::to_vec(next_ord)
            .context(Code::Invalid, "[SearchEngine::save_next_ord] encode ord")?;
        store::atomic_write(&path, &bytes)
    }
}

#[cfg(test)]
mod engine_sync {
    #[test]
    fn search_engine_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<super::SearchEngine>();
    }
}

#[cfg(all(test, feature = "search"))]
mod blob_oid_tests {
    #[test]
    fn blob_oid_matches_known_git_ids() {
        assert_eq!(
            super::blob_oid(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            super::blob_oid(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }
}
