//! Hybrid lexical + vector search.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
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
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery, TermSetQuery};
#[cfg(feature = "search")]
use tantivy::query::{ConstScoreQuery, QueryParser};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema as TantivySchema, TEXT,
    Value as TantivyValue,
};
use tantivy::{
    Index, IndexReader, IndexWriter, Order, ReloadPolicy, Score, SegmentReader, TantivyDocument,
    Term,
};

use crate::error::{Code, ContextAs, ContextIo, bail};
use crate::index;
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
#[cfg(feature = "search")]
const VECTOR_CURRENT: &str = "current";
const ORD_FILE: &str = "ord.json";
/// Slots a vector pass may add before its first checkpoint save of the
/// vector dump. A save writes the whole dump (`index.usearch`, `keys.json`,
/// meta), so saving after every embedder batch makes a first pass over N
/// slots write O(N²) bytes. A pass saves the dump when it ends or stops, and
/// in between only when the slots added since the last save reach this many
/// or half the slots the store holds, whichever is more. Saves then come at
/// geometric sizes (about 2N bytes in all for a pass over N slots), and a
/// killed process loses at most that share of the pass. A save never moves a
/// mark.
#[cfg(feature = "search")]
const VECTOR_CHECKPOINT_FLOOR: usize = 256;
/// Replaces [`VECTOR_CHECKPOINT_FLOOR`] in tests that kill a process
/// mid-pass and need a checkpoint within a few Records.
#[cfg(all(test, feature = "search"))]
pub(crate) static TEST_CHECKPOINT_FLOOR: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(VECTOR_CHECKPOINT_FLOOR);
/// Heap budget for one Tantivy `IndexWriter`. Opening a writer allocates
/// this and starts a merge thread, so the engine keeps a writer only while
/// an indexer round (or a generation build) is applying commits and drops
/// it when that work ends. Holding it for the handle lifetime would block
/// every other process's indexer on the directory lock.
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

/// Which Index legs one call writes. The background indexer holds the
/// lexical and the vector role separately, possibly in two processes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Legs {
    pub(crate) lexical: bool,
    pub(crate) vector: bool,
}

pub(crate) struct SearchEngine {
    index_dir: RwLock<PathBuf>,
    inner: RwLock<Inner>,
    projector: Option<Arc<dyn crate::Projector>>,
}

struct Inner {
    lexical: Option<Lexical>,
    #[cfg(feature = "search")]
    vector: Option<VectorStore>,
    /// A vector slot was added or removed since the last save. Only then is
    /// the dump rewritten: a handle that wrote lexical documents alone must
    /// not overwrite a dump another process keeps current.
    #[cfg(feature = "search")]
    vector_dirty: bool,
    /// Slots added to or dropped from the vector store since its last save:
    /// what [`VECTOR_CHECKPOINT_FLOOR`] counts.
    #[cfg(feature = "search")]
    vector_unsaved: usize,
    /// Saves of the vector dump so far.
    #[cfg(all(test, feature = "search"))]
    vector_saves: u64,
    dirty: bool,
    /// Format and projector-dump checks the current pass already ran.
    checked: Option<Checked>,
    /// `next_ord` holds counters this handle advanced and has not saved.
    /// Only the lexical role advances them, so only it may write `ord.json`.
    ord_dirty: bool,
    can_write: bool,
    next_ord: BTreeMap<String, u64>,
    /// Why the Tantivy files or `ord.json` of this generation did not load
    /// at `open`. Queries return it instead of answering from an engine
    /// that silently has no lexical leg.
    load_error: Option<Error>,
    /// Why the vector dump did not load at `open`. `search` returns it when
    /// it needs vectors and none were loaded since.
    #[cfg(feature = "search")]
    vector_error: Option<Error>,
    #[cfg(test)]
    writer_opens: u64,
}

/// What one pass already verified in generation `dir`, so a Record does not
/// repeat it: the `format` file, and the projector dump line of each
/// collection. The indexer starts every leg pass with
/// [`SearchEngine::begin_pass`], which forgets it.
struct Checked {
    dir: PathBuf,
    format: bool,
    collections: BTreeSet<String>,
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
    f_num: Field,
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
    /// Save of `vector/current` this store was loaded from or last saved
    /// as; `None` for a store built in memory or read from the flat layout.
    /// Another handle's save moves `current` past it.
    seq: Option<u64>,
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
            seq: None,
        }
    }

    fn loaded(
        ann: AnnIndex,
        keys: BTreeMap<u64, DocRef>,
        model_id: String,
        dimensions: usize,
        field_ids: BTreeSet<String>,
        next_key: u64,
        seq: Option<u64>,
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
            seq,
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

/// The Filter of a `search` call, for both legs: the lexical leg runs it as
/// a clause of its query, the vector leg searches only `allowed`.
#[cfg(feature = "search")]
pub(crate) struct SearchFilter<'a> {
    pub(crate) filter: &'a Filter,
    /// `(collection, id)` pairs the Filter matches.
    pub(crate) allowed: &'a BTreeMap<String, BTreeSet<String>>,
    /// Source text of a `more_like_this` document.
    pub(crate) mlt_fts: &'a dyn Fn(&CollectionDecl, &str) -> Result<String, Error>,
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
        // The engine stays openable when a file does not read, so the
        // indexer can start and rebuild the generation. The failure is kept
        // and reported by the first query, and the generation is flagged.
        let mut load_error = None;
        let lexical = kept(open_lexical(index_dir), &mut load_error);
        let next_ord = kept(load_next_ord(index_dir).map(Some), &mut load_error);
        #[cfg(feature = "search")]
        let mut vector_error = None;
        #[cfg(feature = "search")]
        let vector = kept(load_vector(index_dir), &mut vector_error);
        #[cfg_attr(not(feature = "search"), allow(unused_mut))]
        let mut damaged = load_error.as_ref().is_some_and(index::is_damage);
        #[cfg(feature = "search")]
        {
            damaged = damaged || vector_error.as_ref().is_some_and(index::is_damage);
        }
        if damaged {
            index::flag_damaged(index_dir);
        }
        let inner = Inner {
            lexical,
            #[cfg(feature = "search")]
            vector,
            #[cfg(feature = "search")]
            vector_dirty: false,
            #[cfg(feature = "search")]
            vector_unsaved: 0,
            #[cfg(all(test, feature = "search"))]
            vector_saves: 0,
            dirty: false,
            checked: None,
            ord_dirty: false,
            can_write,
            next_ord: next_ord.unwrap_or_default(),
            load_error,
            #[cfg(feature = "search")]
            vector_error,
            #[cfg(test)]
            writer_opens: 0,
        };
        Ok(Self {
            index_dir: RwLock::new(index_dir.to_path_buf()),
            inner: RwLock::new(inner),
            projector,
        })
    }

    pub(crate) fn dir(&self) -> Result<PathBuf, Error> {
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
                #[cfg(feature = "search")]
                vector_dirty: false,
                #[cfg(feature = "search")]
                vector_unsaved: 0,
                #[cfg(all(test, feature = "search"))]
                vector_saves: 0,
                dirty: false,
                checked: None,
                ord_dirty: false,
                can_write: false,
                next_ord: BTreeMap::new(),
                load_error: None,
                #[cfg(feature = "search")]
                vector_error: None,
                #[cfg(test)]
                writer_opens: 0,
            }),
            projector: None,
        }
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
        let old_dir = self.dir()?;
        self.set_index_dir(&dir)?;
        let mut self_inner = crate::error::rw_write(&self.inner, "[SearchEngine::adopt] install")?;
        let old = std::mem::replace(&mut *self_inner, taken);
        drop(self_inner);
        if shutdown_lexical(old.lexical) {
            // The old generation stays as the previous one; its last merges
            // must not leave files wider than owner-only.
            index::restrict_index_tree(&old_dir)?;
        }
        Ok(())
    }

    /// Upsert `record` into the legs named by `legs` only. The embedder is
    /// called only for the vector leg. The Record is projected once; the
    /// `format` file and the projector dump are checked once per pass for
    /// each collection (see [`begin_pass`](Self::begin_pass)).
    pub(crate) fn upsert_legs(
        &self,
        decl: &CollectionDecl,
        record: &Record,
        embedder: Option<&dyn Embedder>,
        legs: Legs,
    ) -> Result<(), Error> {
        if !decl.any_indexed() {
            return Ok(());
        }
        #[cfg(not(feature = "search"))]
        let _ = embedder;
        let projection = project_record(
            self.projector.as_deref(),
            decl,
            record,
            legs.vector && decl.vector_indexed(),
        )?;
        #[cfg(feature = "search")]
        let batch = if legs.vector {
            embed_changed_slots(self, decl, record, embedder, &projection)?
        } else {
            VectorBatch::empty()
        };
        #[cfg(not(feature = "search"))]
        let batch = VectorBatch::empty();
        let index_dir = self.dir()?;
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::upsert] lock")?;
        if !inner.can_write {
            bail!(Error::index_lexical("search index is not open for write"));
        }
        self.prepare_write(
            &mut inner,
            &index_dir,
            Some(&decl.name),
            "[SearchEngine::upsert] check version",
        )?;
        upsert_record(
            &mut inner,
            &index_dir,
            decl,
            record,
            embedder.map(|embedder| (embedder, &batch)),
            &projection,
            legs,
        )?;
        Ok(())
    }

    /// Drop `id` from the legs named by `legs` only.
    pub(crate) fn forget_legs(
        &self,
        decl: &CollectionDecl,
        id: &str,
        legs: Legs,
    ) -> Result<(), Error> {
        if !decl.any_indexed() {
            return Ok(());
        }
        let index_dir = self.dir()?;
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::forget] lock")?;
        if !inner.can_write {
            bail!(Error::index_lexical("search index is not open for write"));
        }
        self.prepare_write(
            &mut inner,
            &index_dir,
            None,
            "[SearchEngine::forget] check version",
        )?;
        forget_record(&mut inner, &decl.name, id, legs)?;
        Ok(())
    }

    /// Forget what the previous pass verified: the next write checks the
    /// `format` file and each collection's projector dump line again. The
    /// indexer calls it at the start of every leg pass, so a change another
    /// process made between passes is seen, and a pass costs one check
    /// instead of one per Record.
    pub(crate) fn begin_pass(&self) -> Result<(), Error> {
        crate::error::rw_write(&self.inner, "[SearchEngine::begin_pass] lock")?.checked = None;
        Ok(())
    }

    /// Check, once per pass, that the generation at `index_dir` has the
    /// current `format` (writing the file when it is new) and, for
    /// `collection`, that the projector dump agrees with this engine's
    /// projector (adding the collection's line when the dump lacks it).
    /// `index_version` on a mismatch.
    fn prepare_write(
        &self,
        inner: &mut Inner,
        index_dir: &Path,
        collection: Option<&str>,
        frame: &'static str,
    ) -> Result<(), Error> {
        let mut checked = match inner.checked.take() {
            Some(checked) if checked.dir == index_dir => checked,
            _ => Checked {
                dir: index_dir.to_path_buf(),
                format: false,
                collections: BTreeSet::new(),
            },
        };
        let mut first_write = false;
        if !checked.format {
            match index::format_version(index_dir)? {
                Some(v) if v != index::FORMAT_VERSION => {
                    return Err(Error::index_version(format!(
                        "index format {v} is not {}",
                        index::FORMAT_VERSION
                    ))
                    .context(frame));
                }
                Some(_) => {}
                None => {
                    index::write_format(index_dir)?;
                    first_write = true;
                }
            }
            checked.format = true;
        }
        if let Some(collection) = collection
            && !checked.collections.contains(collection)
        {
            self.ensure_projector_dump(index_dir, first_write, collection)?;
            checked.collections.insert(collection.to_owned());
        }
        inner.checked = Some(checked);
        Ok(())
    }

    /// Drop every Record of `collection` from the legs named by `legs`.
    pub(crate) fn forget_collection(&self, collection: &str, legs: Legs) -> Result<(), Error> {
        let mut guard =
            crate::error::rw_write(&self.inner, "[SearchEngine::forget_collection] lock")?;
        let inner = &mut *guard;
        if !inner.can_write {
            bail!(Error::index_lexical("search index is not open for write"));
        }
        inner.dirty = true;
        if legs.lexical {
            inner.ensure_writer()?;
            if let Some(lexical) = inner.lexical.as_ref() {
                let slot = crate::error::mutex(
                    &lexical.writer,
                    "[SearchEngine::forget_collection] writer",
                )?;
                if let Some(writer) = slot.as_ref() {
                    writer.delete_term(Term::from_field_text(lexical.f_collection, collection));
                }
            }
        }
        #[cfg(feature = "search")]
        if legs.vector
            && let Some(store) = inner.vector.as_mut()
        {
            let keys: Vec<u64> = store
                .by_record
                .get(collection)
                .map(|ids| {
                    ids.values()
                        .flat_map(|fields| fields.values().copied())
                        .collect()
                })
                .unwrap_or_default();
            if !keys.is_empty() {
                for key in keys {
                    store.drop_ann_slot(key)?;
                }
                refresh_field_ids(store);
                inner.vector_dirty = true;
            }
        }
        Ok(())
    }

    /// Drop vector slots of Records that `keep` does not name. `only` limits
    /// the sweep to one collection; `None` sweeps every collection, so one
    /// absent from `keep` loses all its slots.
    #[cfg(feature = "search")]
    pub(crate) fn prune_vectors(
        &self,
        keep: &BTreeMap<String, BTreeSet<String>>,
        only: Option<&str>,
    ) -> Result<(), Error> {
        let mut guard = crate::error::rw_write(&self.inner, "[SearchEngine::prune_vectors] lock")?;
        let inner = &mut *guard;
        let Some(store) = inner.vector.as_mut() else {
            return Ok(());
        };
        let stale: Vec<u64> = store
            .keys
            .iter()
            .filter(|(_, doc)| only.is_none_or(|name| name == doc.collection))
            .filter(|(_, doc)| !allowed_has(keep, &doc.collection, &doc.id))
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
        inner.vector_dirty = true;
        Ok(())
    }

    /// Write the `format` file and the projector dump of a generation this
    /// engine is about to fill from nothing, as a full rebuild does first.
    pub(crate) fn init_generation(&self, decls: &Decls) -> Result<(), Error> {
        let index_dir = self.dir()?;
        index::ensure_format(&index_dir)?;
        write_projector_dump(
            &index_dir,
            &projector::encode_dump(
                self.projector.as_deref(),
                decls.collections().map(|decl| decl.name.as_str()),
            )?,
        )
    }

    /// Whether this engine's projector may write the lexical leg of the
    /// generation at `index_dir`: the same answer the first upsert would
    /// give, without writing. A generation built by another projector (or
    /// without one, when this engine has one) is not writable here.
    pub(crate) fn lexical_writable(&self, index_dir: &Path) -> Result<bool, Error> {
        let fresh = index::format_version(index_dir)?.is_none();
        Ok(
            match (read_projector_dump(index_dir)?, self.projector.as_deref()) {
                (None, None) => true,
                (None, Some(_)) => fresh,
                (Some(dump), _) => self.match_stored_dump(&dump).is_ok(),
            },
        )
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
    /// here: the first indexed write of the indexer round opens it, and
    /// [`release_writer`](Self::release_writer) drops it when the round
    /// ends.
    pub(crate) fn enable_write(&self) -> Result<(), Error> {
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::enable_write] lock")?;
        let was = std::mem::replace(&mut inner.can_write, true);
        drop(inner);
        if was {
            // Every commit re-applies owner-only modes; walking the whole
            // tree again on each indexer round would only cost time.
            return Ok(());
        }
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
        inner.commit(&index_dir, can_write, VectorSave::Now)
    }

    /// [`flush`](Self::flush) between two batches of one pass: Tantivy is
    /// committed at once, but the vector dump is saved only at a checkpoint
    /// (see [`VECTOR_CHECKPOINT_FLOOR`]). The end of the
    /// pass ([`release_writer`](Self::release_writer)) saves what is left.
    pub(crate) fn flush_batch(&self) -> Result<(), Error> {
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::flush_batch] lock")?;
        let can_write = inner.can_write;
        let index_dir = self.dir()?;
        inner.commit(&index_dir, can_write, VectorSave::Checkpoint)
    }

    /// Commit buffered index writes and drop the Tantivy `IndexWriter`.
    ///
    /// The writer holds a directory lock for as long as it lives. At the
    /// end of an indexer round (or of a generation build) this releases
    /// that lock so another process's indexer can open a writer without
    /// waiting out `LockBusy` retries. The `Index` and reader stay; the
    /// next round opens a writer again.
    ///
    /// Opening a writer allocates a 16 MiB heap and starts a merge thread,
    /// so this is not free. A round applies every pending commit with one
    /// writer, so the cost is paid once per round, not per Record.
    pub(crate) fn release_writer(&self) -> Result<(), Error> {
        let mut inner = crate::error::rw_write(&self.inner, "[SearchEngine::release_writer] lock")?;
        let can_write = inner.can_write;
        let index_dir = self.dir()?;
        inner.commit(&index_dir, can_write, VectorSave::Now)?;
        let writer = take_lexical_writer(inner.lexical.as_ref())?;
        drop(inner);
        if let Some(writer) = writer {
            writer.wait_merging_threads().context(
                Code::IndexLexical,
                "[SearchEngine::release_writer] close writer",
            )?;
            // Merges write segment files after the last commit restricted
            // the tree; make those owner-only too.
            index::restrict_index_tree(&index_dir)?;
        }
        Ok(())
    }

    /// Catch up with what other handles saved to this generation: the
    /// vector dump `vector/current` names and the journal `ord` counters.
    /// Each indexer round runs it first, with a role or without, so a
    /// handle that takes a role over writes on top of the last holder's
    /// work, and a reader's USearch view follows the vector mark. Tantivy
    /// needs nothing here: its reader reloads on every query.
    ///
    /// Buffered writes of this handle win: a dirty store is not replaced.
    pub(crate) fn follow_disk(&self) -> Result<(), Error> {
        let index_dir = self.dir()?;
        {
            let mut inner =
                crate::error::rw_write(&self.inner, "[SearchEngine::follow_disk] lock")?;
            if !inner.ord_dirty {
                inner.next_ord = load_next_ord(&index_dir)?;
            }
        }
        #[cfg(feature = "search")]
        self.refresh_vector(&index_dir)?;
        Ok(())
    }

    /// Whether the vectors this engine holds were made by `embedder`'s
    /// `model_id` and dimensions.
    #[cfg(feature = "search")]
    fn holds_vectors_of(&self, embedder: &dyn Embedder) -> Result<bool, Error> {
        let inner = crate::error::rw_read(&self.inner, "[SearchEngine::search] lock vectors")?;
        Ok(inner.vector.as_ref().is_some_and(|store| {
            store.model_id == embedder.model_id() && store.dimensions == embedder.dimensions().get()
        }))
    }

    /// The failure that kept the vector dump from loading at `open`, when
    /// no dump has loaded since.
    #[cfg(feature = "search")]
    fn require_vector_loaded(&self) -> Result<(), Error> {
        let inner = crate::error::rw_read(&self.inner, "[SearchEngine::search] lock vectors")?;
        match (&inner.vector, &inner.vector_error) {
            (None, Some(err)) => Err(err.clone()),
            _ => Ok(()),
        }
    }

    /// Load the vector dump of `index_dir` when another handle saved a
    /// newer one than this engine holds. The dump is read outside the lock,
    /// so queries keep running on the old view meanwhile.
    #[cfg(feature = "search")]
    fn refresh_vector(&self, index_dir: &Path) -> Result<(), Error> {
        let held = |inner: &Inner| inner.vector.as_ref().and_then(|store| store.seq);
        let Some(on_disk) = vector_seq(&index_dir.join(VECTOR_DIR)) else {
            return Ok(());
        };
        {
            let inner = crate::error::rw_read(&self.inner, "[SearchEngine::refresh_vector] lock")?;
            if inner.vector_dirty || held(&inner) == Some(on_disk) {
                return Ok(());
            }
        }
        let Some(store) = load_fresh_vector(index_dir)? else {
            return Ok(());
        };
        let mut inner =
            crate::error::rw_write(&self.inner, "[SearchEngine::refresh_vector] install")?;
        // A save of this handle, or a newer load, may have won the race.
        let newer = match (held(&inner), store.seq) {
            (Some(have), Some(got)) => got > have,
            _ => true,
        };
        if !inner.vector_dirty && newer {
            inner.vector = Some(store);
        }
        Ok(())
    }

    /// Save of `vector/current` this engine's vector view comes from.
    #[cfg(all(test, feature = "search"))]
    pub(crate) fn held_vector_seq(&self) -> Option<u64> {
        crate::error::rw_read(&self.inner, "[SearchEngine::held_vector_seq] lock")
            .ok()
            .and_then(|inner| inner.vector.as_ref().and_then(|store| store.seq))
    }

    /// Save number `vector/current` of this engine's generation names.
    #[cfg(all(test, feature = "search"))]
    pub(crate) fn disk_vector_seq(&self) -> Option<u64> {
        vector_seq(&self.dir().ok()?.join(VECTOR_DIR))
    }

    /// Saves of the vector dump this engine has made.
    #[cfg(all(test, feature = "search"))]
    pub(crate) fn vector_saves(&self) -> u64 {
        crate::error::rw_read(&self.inner, "[SearchEngine::vector_saves] lock")
            .map(|inner| inner.vector_saves)
            .unwrap_or(0)
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
            if let Some(err) = &inner.load_error {
                return Err(err.clone());
            }
            if inner.query_ready() {
                return f(&inner);
            }
        }
        let mut inner = crate::error::rw_write(&self.inner, frame)?;
        inner.prepare_query(index_dir)?;
        f(&inner)
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
                    .map_err(|err| query_error(err, "[SearchEngine::find_ids] search"))?;
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
                // `TopDocs` reserves slots for `limit + offset` hits up front
                // (`u32::MAX` aborts the process). A page cannot hold more
                // than the live documents, so both are clamped to them.
                let live = usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX);
                let off = (offset as usize).min(live);
                let limit = limit.min(live - off);
                if limit == 0 {
                    let n = searcher
                        .search(query, &Count)
                        .map_err(|err| query_error(err, "[SearchEngine::find] count"))?;
                    return Ok(FindPage {
                        ids: Vec::new(),
                        total: u32::try_from(n).unwrap_or(u32::MAX),
                    });
                }
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
                        .map_err(|err| query_error(err, "[SearchEngine::find] search"))?;
                    (n, top.into_iter().map(|(_, addr)| addr).collect())
                } else {
                    let (n, top) = searcher
                        .search(query, &(Count, top_by_score_then_ord(limit, off)))
                        .map_err(|err| query_error(err, "[SearchEngine::find] search"))?;
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
            let refs: Vec<&CollectionDecl> = decls.iter().collect();
            let query = filter_query(
                lexical,
                self.projector.as_deref(),
                collections,
                &refs,
                filter,
                &mlt_fts,
            )?;
            let searcher = lexical.reader.searcher();
            Ok(Some(f(&searcher, &query, lexical)?))
        })
        .inspect_err(|err| note_damage(&index_dir, err))
    }

    #[cfg(feature = "search")]
    pub(crate) fn search(
        &self,
        decls: &[CollectionDecl],
        query: &SearchQuery,
        embedder: Option<&dyn Embedder>,
        filter: Option<&SearchFilter<'_>>,
        has_reranker: bool,
    ) -> Result<SearchHits, Error> {
        let lexical_decls: Vec<&CollectionDecl> =
            decls.iter().filter(|decl| decl.lexical_indexed()).collect();
        let lexical_names: Vec<&str> = lexical_decls
            .iter()
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
        if vector_on {
            // Between indexer rounds, another process may have saved newer
            // vectors than this handle loaded.
            self.refresh_vector(&index_dir)
                .inspect_err(|err| note_damage(&index_dir, err))?;
            self.require_vector_loaded()?;
        }
        // Vectors of another model are not searched: do not pay for
        // embedding a query that cannot use them.
        let query_vec = match embedder {
            Some(embedder) if vector_on && self.holds_vectors_of(embedder)? => {
                Some(embedder.embed_query(text)?)
            }
            _ => None,
        };
        self.with_query(&index_dir, "[SearchEngine::search] lock", |inner| {
            let lexical_hits = if lexical_weight > 0.0 && !lexical_names.is_empty() {
                lexical_search(
                    inner,
                    self.projector.as_deref(),
                    &lexical_names,
                    &lexical_decls,
                    text,
                    pool,
                    filter,
                )?
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
                    filter.map(|set| set.allowed),
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
        .inspect_err(|err| note_damage(&index_dir, err))
    }
}

/// The query `filter` stands for over `decls`, limited to the collections
/// in `collections`. One Filter is compiled once per collection, because
/// its field names resolve against that collection's Projector columns.
fn filter_query(
    lexical: &Lexical,
    projector: Option<&dyn crate::Projector>,
    collections: &[&str],
    decls: &[&CollectionDecl],
    filter: &Filter,
    mlt_fts: &impl Fn(&CollectionDecl, &str) -> Result<String, Error>,
) -> Result<BooleanQuery, Error> {
    let fields = FindFields {
        id: lexical.f_id,
        eq: lexical.f_eq,
        num: lexical.f_num,
        exists: lexical.f_exists,
        null: lexical.f_null,
        tok: lexical.f_tok,
        text: lexical.f_text,
    };
    if let [decl] = decls {
        let schema = column_schema(projector, decl);
        let compiled = query::compile(
            decl,
            schema.as_ref(),
            &fields,
            &lexical.index,
            filter,
            &|id| mlt_fts(decl, id),
        )?;
        return Ok(BooleanQuery::new(vec![
            (
                Occur::Must,
                collection_query(lexical.f_collection, collections),
            ),
            (Occur::Must, compiled),
        ]));
    }
    let mut clauses = Vec::with_capacity(decls.len());
    for decl in decls {
        let schema = column_schema(projector, decl);
        let compiled = query::compile(
            decl,
            schema.as_ref(),
            &fields,
            &lexical.index,
            filter,
            &|id| mlt_fts(decl, id),
        )?;
        clauses.push((
            Occur::Should,
            Box::new(BooleanQuery::new(vec![
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(lexical.f_collection, &decl.name),
                        IndexRecordOption::Basic,
                    )) as Box<dyn Query>,
                ),
                (Occur::Must, compiled),
            ])) as Box<dyn Query>,
        ));
    }
    Ok(BooleanQuery::new(clauses))
}

/// The Projector schema of `decl`, for the column kinds a Filter compiles
/// against. `None` without a Projector or when its `schema` fails: validation
/// has already rejected a Filter field that needs it.
fn column_schema(
    projector: Option<&dyn crate::Projector>,
    decl: &CollectionDecl,
) -> Option<IndexSchema> {
    projector?.schema(&decl.name).ok()
}

/// `Ok` value of `result`; on `Err` the error goes to `slot` (the first one
/// stays) and the value is `None`.
fn kept<T>(result: Result<Option<T>, Error>, slot: &mut Option<Error>) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(err) => {
            slot.get_or_insert(err);
            None
        }
    }
}

/// Map a failure of running a query. Tantivy reports a query it cannot run
/// because of what the caller put in it (a regex it rejects) as
/// `InvalidArgument`: that is `invalid`, not damage to the Index files.
fn query_error(err: tantivy::TantivyError, frame: &'static str) -> Error {
    let code = match err {
        tantivy::TantivyError::InvalidArgument(_) => Code::Invalid,
        _ => Code::IndexLexical,
    };
    Error::new(code, err.to_string()).context(frame)
}

/// Flag `index_dir` when a query could not read it, so the indexer that
/// holds the lexical role checks it and rebuilds it if it is damaged.
fn note_damage(index_dir: &Path, err: &Error) {
    if index::is_damage(err) {
        index::flag_damaged(index_dir);
    }
}

fn take_lexical_writer(lexical: Option<&Lexical>) -> Result<Option<IndexWriter>, Error> {
    let Some(lexical) = lexical else {
        return Ok(None);
    };
    Ok(crate::error::mutex(&lexical.writer, "[SearchEngine] take writer")?.take())
}

/// Close `lexical`'s writer, waiting for its merges. Whether there was one.
fn shutdown_lexical(lexical: Option<Lexical>) -> bool {
    let Some(lexical) = lexical else {
        return false;
    };
    let writer = match lexical.writer.into_inner() {
        Ok(writer) => writer,
        Err(poisoned) => poisoned.into_inner(),
    };
    match writer {
        Some(writer) => {
            let _ = writer.wait_merging_threads();
            true
        }
        None => false,
    }
}

fn empty_inner(can_write: bool) -> Inner {
    Inner {
        lexical: None,
        #[cfg(feature = "search")]
        vector: None,
        #[cfg(feature = "search")]
        vector_dirty: false,
        #[cfg(feature = "search")]
        vector_unsaved: 0,
        #[cfg(all(test, feature = "search"))]
        vector_saves: 0,
        dirty: false,
        checked: None,
        ord_dirty: false,
        can_write,
        next_ord: BTreeMap::new(),
        load_error: None,
        #[cfg(feature = "search")]
        vector_error: None,
        #[cfg(test)]
        writer_opens: 0,
    }
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
        let _ = inner.commit(&index_dir, can_write, VectorSave::Now);
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
        self.commit(index_dir, can_write, VectorSave::Now)?;
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

    /// Whether the vector dump must be saved by a commit that asked for
    /// `mode`.
    fn vector_save_due(&self, mode: VectorSave) -> bool {
        #[cfg(feature = "search")]
        {
            self.vector_dirty
                && match mode {
                    VectorSave::Now => true,
                    VectorSave::Checkpoint => {
                        #[cfg(test)]
                        let floor =
                            TEST_CHECKPOINT_FLOOR.load(std::sync::atomic::Ordering::Relaxed);
                        #[cfg(not(test))]
                        let floor = VECTOR_CHECKPOINT_FLOOR;
                        let held = self.vector.as_ref().map_or(0, |store| store.keys.len());
                        self.vector_unsaved >= floor.max(held / 2)
                    }
                }
        }
        #[cfg(not(feature = "search"))]
        {
            let _ = mode;
            false
        }
    }

    /// Commit Tantivy, save the vector dump when `vector` says so, and save
    /// `ord.json`. A pending vector change keeps `vector_dirty` set until
    /// it is saved, so a later commit with [`VectorSave::Now`] writes it.
    /// Owner-only modes: the whole tree after a Tantivy commit (it creates
    /// files this crate never opened), only the files written otherwise.
    fn commit(&mut self, index_dir: &Path, can_write: bool, mode: VectorSave) -> Result<(), Error> {
        let save_dump = self.vector_save_due(mode);
        if !can_write || !(self.dirty || save_dump) {
            return Ok(());
        }
        let mut committed = false;
        if let Some(lexical) = &self.lexical {
            {
                let mut slot =
                    crate::error::mutex(&lexical.writer, "[SearchEngine::commit] writer")?;
                if let Some(writer) = slot.as_mut() {
                    writer
                        .commit()
                        .context(Code::IndexLexical, "[SearchEngine::commit] lexical commit")?;
                    committed = true;
                }
            }
            lexical
                .reader
                .reload()
                .context(Code::IndexLexical, "[SearchEngine::commit] reload reader")?;
        }
        #[cfg(feature = "search")]
        if save_dump {
            if let Some(vector) = self.vector.as_mut() {
                vector.seq = Some(save_vector(index_dir, vector)?);
                #[cfg(test)]
                {
                    self.vector_saves = self.vector_saves.saturating_add(1);
                }
            }
            self.vector_dirty = false;
            self.vector_unsaved = 0;
        }
        #[cfg(not(feature = "search"))]
        let _ = save_dump;
        if self.ord_dirty {
            save_next_ord(index_dir, &self.next_ord)?;
            self.ord_dirty = false;
        }
        self.dirty = false;
        if committed {
            index::restrict_index_tree(index_dir)?;
        }
        Ok(())
    }
}

/// When a commit saves the vector dump.
#[derive(Clone, Copy, Eq, PartialEq)]
enum VectorSave {
    /// Whenever it has unsaved changes.
    Now,
    /// Only when enough slots changed since the last save (see
    /// [`VECTOR_CHECKPOINT_FLOOR`]).
    Checkpoint,
}

/// A Record as the projector sees it: computed once per upsert and shared
/// by the embedding step and the Index write.
struct Projection {
    /// The projected values; `None` when the projector gives this Record no
    /// columns (or fails), so it has no Index entry. Without a projector,
    /// an empty document.
    doc: Option<IndexDoc>,
    /// The projector's schema for the collection, when it has a valid one.
    /// Read only when it is used: for a Record that has a document, or for
    /// the vector leg of a vector-indexed collection.
    schema: Option<IndexSchema>,
    /// The engine has a projector. Only then does the vector leg write.
    has_projector: bool,
}

/// Project `record` of `decl` once. `need_schema` asks for the schema even
/// when the Record has no document (the vector leg of a vector-indexed
/// collection); an invalid schema is an error then too.
fn project_record(
    projector: Option<&dyn crate::Projector>,
    decl: &CollectionDecl,
    record: &Record,
    need_schema: bool,
) -> Result<Projection, Error> {
    let Some(projector) = projector else {
        return Ok(Projection {
            doc: Some(IndexDoc::default()),
            schema: None,
            has_projector: false,
        });
    };
    let raw = record.raw.as_deref().unwrap_or(&[]);
    let id = record.id.as_deref().unwrap_or("");
    let doc = match projector.project(&decl.name, id, raw) {
        Ok(doc) if !doc.values.is_empty() => Some(doc),
        _ => None,
    };
    let schema = if doc.is_some() || need_schema {
        match projector.schema(&decl.name) {
            Ok(schema) => {
                schema.require_valid()?;
                Some(schema)
            }
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(Projection {
        doc,
        schema,
        has_projector: true,
    })
}

#[cfg(feature = "search")]
fn embed_changed_slots(
    engine: &SearchEngine,
    decl: &CollectionDecl,
    record: &Record,
    embedder: Option<&dyn Embedder>,
    projection: &Projection,
) -> Result<VectorBatch, Error> {
    if !decl.vector_indexed() {
        return Ok(VectorBatch::empty());
    }
    let (Some(schema), Some(doc)) = (projection.schema.as_ref(), projection.doc.as_ref()) else {
        return Ok(VectorBatch::empty());
    };
    let pieces = projector::vector_pieces(schema, doc);
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
    let path = index_dir.join(PROJECTOR_FILE);
    fs::write(&path, dump).context("[SearchEngine] write projector dump")?;
    store::restrict_file(&path)
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
    vector: Option<(&dyn Embedder, &VectorBatch)>,
    projection: &Projection,
    legs: Legs,
) -> Result<(), Error> {
    let has_projector = projection.has_projector;
    let Some(id) = record.id.as_deref().filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    let can_write = inner.can_write;
    let Some(projected) = projection.doc.as_ref() else {
        if can_write {
            forget_record(inner, &decl.name, id, legs)?;
        }
        #[cfg(not(feature = "search"))]
        let _ = (vector, has_projector);
        return Ok(());
    };
    let find_schema = projection.schema.as_ref();
    if can_write {
        if decl.lexical_indexed() && legs.lexical {
            // The full-text blob is only for the lexical leg.
            let fts_text = match find_schema {
                Some(schema) => projector::fts_blob(schema, projected),
                None => String::new(),
            };
            if inner.lexical.is_none() {
                inner.lexical = Some(create_lexical(index_dir)?);
            }
            inner.ensure_writer()?;
            let ord = inner.next_ord.get(&decl.name).copied().unwrap_or(0);
            inner
                .next_ord
                .insert(decl.name.clone(), ord.saturating_add(1));
            inner.ord_dirty = true;
            if let Some(lexical) = inner.lexical.as_mut() {
                index_lexical(
                    lexical,
                    decl,
                    record,
                    &fts_text,
                    ord,
                    Some(projected),
                    find_schema,
                )?;
                inner.dirty = true;
            }
        }
        #[cfg(feature = "search")]
        if legs.vector
            && decl.vector_indexed()
            && has_projector
            && let Some((embedder, batch)) = vector
        {
            let names = find_schema
                .map(projector::vector_slot_names)
                .unwrap_or_default();
            upsert_vector(inner, embedder, decl, record, batch, &names)?;
            inner.dirty = true;
        }
    }
    #[cfg(not(feature = "search"))]
    let _ = (vector, has_projector);
    Ok(())
}

fn forget_record(inner: &mut Inner, collection: &str, id: &str, legs: Legs) -> Result<(), Error> {
    inner.dirty = true;
    if legs.lexical {
        inner.ensure_writer()?;
        if let Some(lexical) = inner.lexical.as_ref() {
            let slot = crate::error::mutex(&lexical.writer, "[SearchEngine::forget] writer")?;
            if let Some(writer) = slot.as_ref() {
                let key = doc_key(collection, id);
                writer.delete_term(Term::from_field_text(lexical.f_key, &key));
            }
        }
    }
    #[cfg(feature = "search")]
    if legs.vector {
        forget_vector(inner, collection, id)?;
    }
    Ok(())
}

#[cfg(feature = "search")]
fn forget_vector(inner: &mut Inner, collection: &str, id: &str) -> Result<(), Error> {
    let Some(store) = inner.vector.as_mut() else {
        return Ok(());
    };
    let stale = store.record_keys(collection, id);
    if stale.is_empty() {
        return Ok(());
    }
    for key in stale {
        store.drop_ann_slot(key)?;
    }
    refresh_field_ids(store);
    inner.vector_dirty = true;
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
                if let Some(key) =
                    query::numeric_key(column.kind.as_ref().and_then(|kind| kind.as_known()), value)
                {
                    doc.add_text(lexical.f_num, query::num_term(name, &key)?);
                }
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

/// Sequence number `vector/current` names, if any.
#[cfg(feature = "search")]
fn vector_seq(root: &Path) -> Option<u64> {
    fs::read_to_string(root.join(VECTOR_CURRENT))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Directory that holds the live vector dump of the generation at
/// `index_dir`, with its save number: `vector/<seq>/` as `vector/current`
/// names it, or the flat `vector/` an older release wrote (no number).
/// `None` without a dump.
///
/// Each save writes a whole new `<seq>/` and only then moves `current`, so
/// a reader never loads half of one save and half of another.
#[cfg(feature = "search")]
fn vector_files(index_dir: &Path) -> Option<(PathBuf, Option<u64>)> {
    let root = index_dir.join(VECTOR_DIR);
    if let Some(seq) = vector_seq(&root) {
        let dir = root.join(seq.to_string());
        if dir.join(VECTOR_META).is_file() {
            return Some((dir, Some(seq)));
        }
    }
    root.join(VECTOR_META).is_file().then_some((root, None))
}

#[cfg(feature = "search")]
fn peek_vector_meta(index_dir: &Path) -> Result<Option<VectorMeta>, Error> {
    let Some((dir, _)) = vector_files(index_dir) else {
        return Ok(None);
    };
    let meta_path = dir.join(VECTOR_META);
    if !dir.join(VECTOR_INDEX).is_file() || !dir.join(VECTOR_KEYS).is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&meta_path).context("[SearchEngine] read vector meta")?;
    let meta: VectorMeta = serde_json::from_str(&raw)
        .context(Code::IndexCorrupt, "[SearchEngine] decode vector meta")?;
    Ok(Some(meta))
}

/// Whether `embedder` may write the vector leg of the generation at
/// `index_dir`: no vector dump yet, or one with the same `model_id` and
/// dimensions.
#[cfg(feature = "search")]
pub(crate) fn vector_model_matches(
    index_dir: &Path,
    embedder: &dyn Embedder,
) -> Result<bool, Error> {
    Ok(match peek_vector_meta(index_dir)? {
        None => true,
        Some(meta) => {
            meta.model_id == embedder.model_id() && meta.dimensions == embedder.dimensions().get()
        }
    })
}

/// Copy the vector dump of `from` into `to` when it loads and, with an
/// `embedder`, has its `model_id` and dimensions; without one, whatever
/// its model. A dump that does not load (corrupt, half a file) is left
/// behind: the new generation must not inherit what made the old one fail.
#[cfg(feature = "search")]
pub(crate) fn copy_loadable_vector_dump(
    from: &Path,
    to: &Path,
    embedder: Option<&dyn Embedder>,
) -> Result<(), Error> {
    let Ok(Some(store)) = load_vector(from) else {
        return Ok(());
    };
    let same_model = embedder.is_none_or(|embedder| {
        store.model_id == embedder.model_id() && store.dimensions == embedder.dimensions().get()
    });
    if same_model {
        copy_vector_dir(from, to)?;
    }
    Ok(())
}

/// Copy the live dump of `from` into `to` as its first save (`vector/1/`).
#[cfg(feature = "search")]
fn copy_vector_dir(from: &Path, to: &Path) -> Result<(), Error> {
    let Some((src, _)) = vector_files(from) else {
        return Ok(());
    };
    let root = to.join(VECTOR_DIR);
    let dst = root.join("1");
    fs::create_dir_all(&dst).context("[SearchEngine] create vector dir")?;
    for name in [VECTOR_INDEX, VECTOR_META, VECTOR_KEYS] {
        let from_file = src.join(name);
        if !from_file.is_file() {
            continue;
        }
        fs::copy(&from_file, dst.join(name)).context("[SearchEngine] copy vector file")?;
    }
    store::atomic_write(&root.join(VECTOR_CURRENT), b"1\n")
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
    let mut changed = false;
    if inner.vector.is_none() {
        inner.vector = Some(VectorStore::empty(
            AnnIndex::new(dimensions)?,
            model_id.to_owned(),
            dimensions,
        ));
        changed = true;
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
    let mut unsaved = vanished.len();
    for key in vanished {
        store.drop_ann_slot(key)?;
        changed = true;
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
            changed = true;
            unsaved += 1;
        } else if let Some(key) = store.slot_key(&decl.name, id, field)
            && let Some(doc) = store.keys.get_mut(&key)
            && doc.path != path
        {
            doc.path.clone_from(&path);
            changed = true;
        }
    }
    let fields_before = store.field_ids.clone();
    refresh_field_ids(store);
    store.field_ids.extend(vector_names.iter().cloned());
    if changed || store.field_ids != fields_before {
        inner.vector_dirty = true;
        inner.vector_unsaved = inner.vector_unsaved.saturating_add(unsaved);
    }
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
    projector: Option<&dyn crate::Projector>,
    collections: &[&str],
    decls: &[&CollectionDecl],
    text: &str,
    pool: usize,
    filter: Option<&SearchFilter<'_>>,
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
    let mut clauses = vec![
        (
            Occur::Must,
            collection_query(lexical.f_collection, collections),
        ),
        (Occur::Must, parsed),
    ];
    if let Some(set) = filter {
        // The Filter narrows the candidates before ranking, so the top
        // `pool` hits are the best of the matching Records. It adds no
        // score: ranking stays the text query's.
        let compiled = filter_query(
            lexical,
            projector,
            collections,
            decls,
            set.filter,
            &set.mlt_fts,
        )?;
        clauses.push((
            Occur::Must,
            Box::new(ConstScoreQuery::new(Box::new(compiled), 0.0)),
        ));
    }
    let query = BooleanQuery::new(clauses);
    // `TopDocs` reserves its slots up front: ask for no more than exist.
    let pool = pool.min(usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX));
    if pool == 0 {
        return Ok(Vec::new());
    }
    let top = searcher
        .search(&query, &top_by_score_then_ord(pool, 0))
        .map_err(|err| query_error(err, "[SearchEngine::lexical_search] search"))?;
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
    // No leg can return more hits than there are vector slots.
    let pool = pool.min(store.ann.len());
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
    builder.add_text_field("num", STRING);
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

/// Read generation `index_dir` as a query would, writing nothing: Tantivy
/// opens and serves a searcher, the journal `ord` counters decode, and with
/// `deep` the vector dump loads in full. An error with an
/// [`index::is_damage`] code means the generation is damaged.
pub(crate) fn probe_generation(index_dir: &Path, deep: bool) -> Result<(), Error> {
    if let Some(lexical) = open_lexical(index_dir)? {
        lexical
            .reader
            .reload()
            .context(Code::IndexLexical, "[SearchEngine::probe] reload reader")?;
        let _ = lexical.reader.searcher();
    }
    load_next_ord(index_dir)?;
    #[cfg(feature = "search")]
    if deep {
        load_vector(index_dir)?;
    }
    #[cfg(not(feature = "search"))]
    let _ = deep;
    Ok(())
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
    let f_num = schema
        .get_field("num")
        .context(Code::IndexLexical, "[SearchEngine::bind_lexical] field num")?;
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
        f_num,
        f_exists,
        f_null,
        f_tok,
        f_ord,
    })
}

#[cfg(feature = "search")]
fn load_vector(index_dir: &Path) -> Result<Option<VectorStore>, Error> {
    let Some((dir, seq)) = vector_files(index_dir) else {
        return Ok(None);
    };
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
        seq,
    )))
}

/// Load the dump `vector/current` names. A save that moves `current` and
/// drops the directory being read makes the load fail; read `current`
/// again then, and fail only when it stayed put.
#[cfg(feature = "search")]
fn load_fresh_vector(index_dir: &Path) -> Result<Option<VectorStore>, Error> {
    const ATTEMPTS: usize = 3;
    let root = index_dir.join(VECTOR_DIR);
    let mut attempt = 1;
    loop {
        let before = vector_seq(&root);
        match load_vector(index_dir) {
            Ok(store) => return Ok(store),
            Err(_) if attempt < ATTEMPTS && vector_seq(&root) != before => attempt += 1,
            Err(err) => return Err(err),
        }
    }
}

/// Write `store` as the next save of `vector/current`; returns its number.
#[cfg(feature = "search")]
fn save_vector(index_dir: &Path, store: &VectorStore) -> Result<u64, Error> {
    let root = index_dir.join(VECTOR_DIR);
    let seq = vector_seq(&root).unwrap_or(0).saturating_add(1);
    let dir = root.join(seq.to_string());
    match fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).context("[SearchEngine::save_vector] clear leftover dump"),
    }
    fs::create_dir_all(&dir).context("[SearchEngine::save_vector] create dir")?;
    write_vector_files(&dir, store)?;
    store::atomic_write(&root.join(VECTOR_CURRENT), format!("{seq}\n").as_bytes())?;
    drop_old_vector_dumps(&root, seq)?;
    // Owner-only modes on what this save wrote: `vector/` is small (two
    // dumps), unlike the whole Index tree.
    store::restrict_tree(&root)?;
    Ok(seq)
}

/// Remove dumps older than the one before `seq` (a reader may still be
/// loading that one) and the flat files of an older layout.
#[cfg(feature = "search")]
fn drop_old_vector_dumps(root: &Path, seq: u64) -> Result<(), Error> {
    for name in [VECTOR_INDEX, VECTOR_META, VECTOR_KEYS] {
        match fs::remove_file(root.join(name)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).context("[SearchEngine::save_vector] drop flat dump"),
        }
    }
    let entries = fs::read_dir(root).context("[SearchEngine::save_vector] list dumps")?;
    for entry in entries {
        let entry = entry.context("[SearchEngine::save_vector] list dumps")?;
        let old = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
            .is_some_and(|n| n.saturating_add(1) < seq);
        if old {
            fs::remove_dir_all(entry.path())
                .context("[SearchEngine::save_vector] drop old dump")?;
        }
    }
    Ok(())
}

#[cfg(feature = "search")]
fn write_vector_files(dir: &Path, store: &VectorStore) -> Result<(), Error> {
    let dump = dir.join(VECTOR_INDEX);
    store.ann.save(&dump)?;
    fs::File::open(&dump)
        .and_then(|file| file.sync_all())
        .context("[SearchEngine::save_vector] fsync dump")?;
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
        store::atomic_write(&path, &bytes)?;
        store::restrict_file(&path)
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

#[cfg(test)]
mod ord_tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    use super::{ORD_FILE, empty_inner};

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("TEST_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("scheda-ord-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // `<vault>/.scheda/index/<generation>`: the commit restricts `.scheda`.
        let dir = root.join(".scheda").join("index").join("1");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn remove(dir: &std::path::Path) {
        // Only the scratch root `scratch` made: never a parent of it.
        if let Some(root) = dir.ancestors().nth(3)
            && root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("scheda-ord-"))
        {
            let _ = fs::remove_dir_all(root);
        }
    }

    /// A commit of vector writes alone must leave the `ord.json` another
    /// process keeps for the lexical leg exactly as it is, even when this
    /// handle never loaded it (an empty in-memory map would delete it).
    #[test]
    fn commit_without_lexical_writes_leaves_ord_json_alone() {
        let dir = scratch("untouched");
        fs::write(dir.join(ORD_FILE), br#"{"j":7}"#).unwrap();
        let mut inner = empty_inner(true);
        inner.dirty = true;
        inner.commit(&dir, true, super::VectorSave::Now).unwrap();
        assert!(!inner.dirty);
        assert_eq!(fs::read(dir.join(ORD_FILE)).unwrap(), br#"{"j":7}"#);

        inner.next_ord = BTreeMap::from([(String::from("j"), 1)]);
        inner.dirty = true;
        inner.commit(&dir, true, super::VectorSave::Now).unwrap();
        assert_eq!(
            fs::read(dir.join(ORD_FILE)).unwrap(),
            br#"{"j":7}"#,
            "counters this handle did not advance are not saved"
        );
        remove(&dir);
    }

    #[test]
    fn commit_after_lexical_writes_saves_ord_json_once() {
        let dir = scratch("saved");
        let mut inner = empty_inner(true);
        inner.dirty = true;
        inner.ord_dirty = true;
        inner.next_ord = BTreeMap::from([(String::from("j"), 3)]);
        inner.commit(&dir, true, super::VectorSave::Now).unwrap();
        assert!(!inner.ord_dirty);
        assert_eq!(fs::read(dir.join(ORD_FILE)).unwrap(), br#"{"j":3}"#);
        remove(&dir);
    }
}
