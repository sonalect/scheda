//! Hybrid lexical + vector search and one-step edges (C9).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema as TantivySchema, TEXT, Value as TantivyValue,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::ann::AnnIndex;
use crate::index;
use crate::schema::{CollectionDecl, Dangling, EdgeDecl, FieldDecl, Schema};
use crate::store;
use crate::{Error, Record, SearchHit, SearchHits, SearchQuery};

const LEXICAL_DIR: &str = "lexical";
const VECTOR_DIR: &str = "vector";
const VECTOR_INDEX: &str = "index.usearch";
const VECTOR_META: &str = "meta.json";
const VECTOR_KEYS: &str = "keys.json";
const EDGES_FILE: &str = "edges.jsonl";
const WRITER_HEAP: usize = 16_000_000;
const RRF_K: f64 = 60.0;
const DEFAULT_K: usize = 10;

/// Caller-supplied embedding model (IDX-8). Not an `api/v1` message.
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error>;
}

pub(crate) struct SearchEngine {
    index_dir: PathBuf,
    can_write: bool,
    inner: Mutex<Inner>,
}

struct Inner {
    lexical: Option<Lexical>,
    vector: Option<VectorStore>,
    edges: EdgeIndex,
    dirty: bool,
}

struct Lexical {
    index: Index,
    writer: Option<IndexWriter>,
    reader: IndexReader,
    f_key: Field,
    f_collection: Field,
    f_id: Field,
    f_path: Field,
    f_text: Field,
}

struct VectorStore {
    ann: AnnIndex,
    keys: BTreeMap<u64, DocRef>,
    model_id: String,
    dimensions: usize,
    field_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DocRef {
    collection: String,
    id: String,
    path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct VectorMeta {
    model_id: String,
    dimensions: usize,
    #[serde(default)]
    field_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EdgeLine {
    src: String,
    id: String,
    field: String,
    dst: String,
}

#[derive(Clone, Debug, Default)]
struct EdgeIndex {
    by_target: BTreeMap<(String, String, String), Vec<String>>,
}

struct LegHit {
    id: String,
    path: String,
    score: f64,
}

impl SearchEngine {
    pub(crate) fn open(index_dir: &Path, can_write: bool) -> Result<Self, Error> {
        let inner = Inner {
            lexical: open_lexical(index_dir, can_write).ok().flatten(),
            vector: load_vector(index_dir).ok().flatten(),
            edges: load_edges(index_dir).unwrap_or_else(|_| EdgeIndex::default()),
            dirty: false,
        };
        Ok(Self {
            index_dir: index_dir.to_path_buf(),
            can_write,
            inner: Mutex::new(inner),
        })
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            index_dir: PathBuf::new(),
            can_write: false,
            inner: Mutex::new(Inner {
                lexical: None,
                vector: None,
                edges: EdgeIndex::default(),
                dirty: false,
            }),
        }
    }

    pub(crate) fn close_for_rebuild(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.lexical = None;
        inner.vector = None;
        inner.edges = EdgeIndex::default();
        inner.dirty = false;
    }

    pub(crate) fn rebuild(
        &self,
        vault: &Path,
        schema: &Schema,
        embedder: Option<&dyn Embedder>,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.lexical = None;
        inner.vector = None;
        inner.edges = EdgeIndex::default();
        inner.dirty = false;
        if self.can_write {
            index::ensure_format(&self.index_dir)?;
        }
        for decl in schema.collections() {
            let records = match decl.kind {
                crate::schema::Kind::Journal => crate::journal::scan_journal(vault, decl)?,
                crate::schema::Kind::Document | crate::schema::Kind::Singleton => {
                    store::scan_documents(vault, decl)?
                }
            };
            for record in records {
                if record.valid != Some(true) {
                    continue;
                }
                upsert_record(
                    &mut inner,
                    &self.index_dir,
                    self.can_write,
                    decl,
                    &record,
                    embedder,
                )?;
            }
        }
        inner.commit(&self.index_dir, self.can_write)
    }

    pub(crate) fn upsert(
        &self,
        decl: &CollectionDecl,
        record: &Record,
        embedder: Option<&dyn Embedder>,
    ) -> Result<(), Error> {
        if !self.can_write {
            return Ok(());
        }
        if record.valid != Some(true) {
            return Ok(());
        }
        match index::format_version(&self.index_dir)? {
            Some(v) if v != index::FORMAT_VERSION => return Ok(()),
            _ => index::ensure_format(&self.index_dir)?,
        }
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        upsert_record(
            &mut inner,
            &self.index_dir,
            self.can_write,
            decl,
            record,
            embedder,
        )
    }

    pub(crate) fn inverse(
        &self,
        source_collection: &str,
        field: &str,
        id: &str,
    ) -> Result<Vec<String>, Error> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(inner
            .edges
            .by_target
            .get(&(
                source_collection.to_owned(),
                field.to_owned(),
                id.to_owned(),
            ))
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) fn search(
        &self,
        collection: &str,
        query: &SearchQuery,
        embedder: Option<&dyn Embedder>,
        allowed: Option<&BTreeSet<String>>,
    ) -> Result<SearchHits, Error> {
        index::require_current_format(&self.index_dir)?;
        let Some(text) = query.text.as_deref() else {
            unreachable!("SearchQuery text is required");
        };
        let k = match query.k {
            None | Some(0) => DEFAULT_K,
            Some(n) => n as usize,
        };
        let lexical_weight = query.lexical_weight.unwrap_or(1.0);
        let vector_weight = query.vector_weight.unwrap_or(1.0);
        let pool = k.saturating_mul(8).max(64);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.commit(&self.index_dir, self.can_write)?;
        if inner.lexical.is_none() {
            // Open swallowed a corrupt tree so `open`/`find_one` still work.
            // `search` must fail closed (QRY-4) if the files are present.
            inner.lexical = open_lexical(&self.index_dir, self.can_write)?;
        }
        let lexical_hits = if lexical_weight > 0.0 {
            lexical_search(&mut inner, collection, text, pool, allowed)?
        } else {
            Vec::new()
        };
        let vector_hits = if vector_weight > 0.0 {
            vector_search(&inner, collection, text, pool, embedder, allowed)?
        } else {
            Vec::new()
        };
        Ok(fuse(
            collection,
            &lexical_hits,
            &vector_hits,
            lexical_weight,
            vector_weight,
            k,
        ))
    }
}

impl Drop for SearchEngine {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = inner.commit(&self.index_dir, self.can_write);
    }
}

impl Inner {
    fn commit(&mut self, index_dir: &Path, can_write: bool) -> Result<(), Error> {
        if !self.dirty || !can_write {
            return Ok(());
        }
        if let Some(lexical) = &mut self.lexical
            && let Some(writer) = &mut lexical.writer
        {
            writer
                .commit()
                .map_err(|err| Error::index_unusable(format!("lexical commit: {err}")))?;
            lexical
                .reader
                .reload()
                .map_err(|err| Error::index_unusable(format!("lexical reload: {err}")))?;
        }
        if let Some(vector) = &self.vector {
            save_vector(index_dir, vector)?;
        }
        save_edges(index_dir, &self.edges)?;
        self.dirty = false;
        Ok(())
    }
}

fn upsert_record(
    inner: &mut Inner,
    index_dir: &Path,
    can_write: bool,
    decl: &CollectionDecl,
    record: &Record,
    embedder: Option<&dyn Embedder>,
) -> Result<(), Error> {
    let Some(id) = record.id.as_deref().filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    let path = record.path.clone().unwrap_or_default();
    let (fts_text, vector_text) = marked_text(decl, record)?;
    forget_source(&mut inner.edges, &decl.name, id);
    add_edges(&mut inner.edges, decl, record)?;
    if can_write {
        if inner.lexical.is_none()
            && let Ok(lexical) = create_lexical(index_dir, true)
        {
            inner.lexical = Some(lexical);
        }
        if let Some(lexical) = inner.lexical.as_mut() {
            index_lexical(lexical, &decl.name, id, &path, &fts_text)?;
        }
        if let Some(embedder) = embedder
            && !vector_text.is_empty()
        {
            upsert_vector(inner, embedder, decl, &decl.name, id, &path, &vector_text)?;
        }
        inner.dirty = true;
    }
    Ok(())
}

fn index_lexical(
    lexical: &Lexical,
    collection: &str,
    id: &str,
    path: &str,
    text: &str,
) -> Result<(), Error> {
    let Some(writer) = &lexical.writer else {
        return Ok(());
    };
    let key = doc_key(collection, id);
    writer.delete_term(Term::from_field_text(lexical.f_key, &key));
    if text.is_empty() {
        return Ok(());
    }
    let mut doc = TantivyDocument::default();
    doc.add_text(lexical.f_key, &key);
    doc.add_text(lexical.f_collection, collection);
    doc.add_text(lexical.f_id, id);
    doc.add_text(lexical.f_path, path);
    doc.add_text(lexical.f_text, text);
    writer
        .add_document(doc)
        .map_err(|err| Error::index_unusable(format!("lexical add: {err}")))?;
    Ok(())
}

fn upsert_vector(
    inner: &mut Inner,
    embedder: &dyn Embedder,
    decl: &CollectionDecl,
    collection: &str,
    id: &str,
    path: &str,
    text: &str,
) -> Result<(), Error> {
    let dimensions = embedder.dimensions();
    if dimensions == 0 {
        return Ok(());
    }
    let model_id = embedder.model_id();
    let slots = vector_slots(decl);
    if inner.vector.is_none() {
        inner.vector = Some(VectorStore {
            ann: AnnIndex::new(dimensions)?,
            keys: BTreeMap::new(),
            model_id: model_id.to_owned(),
            dimensions,
            field_ids: slots.clone(),
        });
    }
    let Some(store) = inner.vector.as_mut() else {
        return Ok(());
    };
    if store.model_id != model_id || store.dimensions != dimensions {
        return Ok(());
    }
    store.field_ids.extend(slots);
    let vector = embedder.embed(text)?;
    if vector.len() != dimensions {
        return Err(Error::invalid(format!(
            "embedder returned {} dimensions, expected {dimensions}",
            vector.len()
        )));
    }
    let key = ann_key(collection, id);
    if store.ann.contains(key) {
        store.ann.remove(key)?;
    }
    store.ann.add(key, &vector)?;
    store.keys.insert(
        key,
        DocRef {
            collection: collection.to_owned(),
            id: id.to_owned(),
            path: path.to_owned(),
        },
    );
    Ok(())
}

fn vector_slots(decl: &CollectionDecl) -> BTreeSet<String> {
    let mut slots = BTreeSet::new();
    collect_vector_slots(&decl.fields, &mut slots);
    if let Some(body) = &decl.body
        && body.vector
    {
        slots.insert(body.field_id.clone());
    }
    slots
}

fn collect_vector_slots(fields: &[FieldDecl], slots: &mut BTreeSet<String>) {
    for field in fields {
        if field.vector
            && let Some(id) = &field.field_id
        {
            slots.insert(id.clone());
        }
        collect_vector_slots(&field.fields, slots);
    }
}

fn lexical_search(
    inner: &mut Inner,
    collection: &str,
    text: &str,
    pool: usize,
    allowed: Option<&BTreeSet<String>>,
) -> Result<Vec<LegHit>, Error> {
    let Some(lexical) = inner.lexical.as_mut() else {
        return Ok(Vec::new());
    };
    lexical
        .reader
        .reload()
        .map_err(|err| Error::index_unusable(format!("lexical reload: {err}")))?;
    let searcher = lexical.reader.searcher();
    let parser = QueryParser::for_index(&lexical.index, vec![lexical.f_text]);
    let parsed = match parse_text_query(&parser, text) {
        Some(query) => query,
        None => return Ok(Vec::new()),
    };
    let collection_q: Box<dyn Query> = Box::new(TermQuery::new(
        Term::from_field_text(lexical.f_collection, collection),
        IndexRecordOption::Basic,
    ));
    let query = BooleanQuery::new(vec![(Occur::Must, collection_q), (Occur::Must, parsed)]);
    if pool == 0 {
        return Ok(Vec::new());
    }
    let top = searcher
        .search(&query, &TopDocs::with_limit(pool).order_by_score())
        .map_err(|err| Error::index_unusable(format!("lexical search: {err}")))?;
    let mut hits = Vec::new();
    for (score, addr) in top {
        let doc: TantivyDocument = searcher
            .doc(addr)
            .map_err(|err| Error::index_unusable(format!("lexical fetch: {err}")))?;
        let Some(id) = stored_str(&doc, lexical.f_id) else {
            continue;
        };
        if allowed.is_some_and(|set| !set.contains(&id)) {
            continue;
        }
        hits.push(LegHit {
            id,
            path: stored_str(&doc, lexical.f_path).unwrap_or_default(),
            score: f64::from(score),
        });
    }
    Ok(hits)
}

fn vector_search(
    inner: &Inner,
    collection: &str,
    text: &str,
    pool: usize,
    embedder: Option<&dyn Embedder>,
    allowed: Option<&BTreeSet<String>>,
) -> Result<Vec<LegHit>, Error> {
    let Some(embedder) = embedder else {
        return Ok(Vec::new());
    };
    let Some(store) = inner.vector.as_ref() else {
        return Ok(Vec::new());
    };
    if store.model_id != embedder.model_id() || store.dimensions != embedder.dimensions() {
        return Ok(Vec::new());
    }
    if store.ann.is_empty() {
        return Ok(Vec::new());
    }
    let query = embedder.embed(text)?;
    if query.len() != store.dimensions {
        return Err(Error::invalid(format!(
            "embedder returned {} dimensions, expected {}",
            query.len(),
            store.dimensions
        )));
    }
    let raw = store.ann.search(&query, pool)?;
    let mut hits = Vec::new();
    for (key, distance) in raw {
        let Some(doc) = store.keys.get(&key) else {
            continue;
        };
        if doc.collection != collection {
            continue;
        }
        if allowed.is_some_and(|set| !set.contains(&doc.id)) {
            continue;
        }
        hits.push(LegHit {
            id: doc.id.clone(),
            path: doc.path.clone(),
            score: f64::from(1.0 / (1.0 + distance)),
        });
    }
    Ok(hits)
}

fn fuse(
    collection: &str,
    lexical: &[LegHit],
    vector: &[LegHit],
    lexical_weight: f64,
    vector_weight: f64,
    k: usize,
) -> SearchHits {
    let mut scores: BTreeMap<String, (f64, Option<f64>, Option<f64>, String)> = BTreeMap::new();
    if lexical_weight > 0.0 {
        for (rank, hit) in lexical.iter().enumerate() {
            let rrf = lexical_weight / (RRF_K + (rank as f64) + 1.0);
            let entry = scores
                .entry(hit.id.clone())
                .or_insert_with(|| (0.0, None, None, hit.path.clone()));
            entry.0 += rrf;
            entry.1 = Some(hit.score);
            if entry.3.is_empty() {
                entry.3 = hit.path.clone();
            }
        }
    }
    if vector_weight > 0.0 {
        for (rank, hit) in vector.iter().enumerate() {
            let rrf = vector_weight / (RRF_K + (rank as f64) + 1.0);
            let entry = scores
                .entry(hit.id.clone())
                .or_insert_with(|| (0.0, None, None, hit.path.clone()));
            entry.0 += rrf;
            entry.2 = Some(hit.score);
            if entry.3.is_empty() {
                entry.3 = hit.path.clone();
            }
        }
    }
    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.0
            .partial_cmp(&a.1.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut hits = SearchHits::default();
    for (id, (rrf, lexical_score, vector_score, path)) in ranked.into_iter().take(k) {
        let mut hit = SearchHit::default()
            .with_collection(collection)
            .with_id(id)
            .with_path(path)
            .with_rrf_score(rrf);
        if let Some(score) = lexical_score {
            hit = hit.with_lexical_score(score);
        }
        if let Some(score) = vector_score {
            hit = hit.with_vector_score(score);
        }
        hits.hits.push(hit);
    }
    hits
}

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

fn stored_str(doc: &TantivyDocument, field: Field) -> Option<String> {
    doc.get_first(field)
        .and_then(|value| TantivyValue::as_str(&value).map(str::to_owned))
}

fn lexical_schema() -> (TantivySchema, Field, Field, Field, Field, Field) {
    let mut builder = TantivySchema::builder();
    let f_key = builder.add_text_field("key", STRING | STORED);
    let f_collection = builder.add_text_field("collection", STRING | STORED);
    let f_id = builder.add_text_field("id", STRING | STORED);
    let f_path = builder.add_text_field("path", STRING | STORED);
    let f_text = builder.add_text_field("text", TEXT);
    (builder.build(), f_key, f_collection, f_id, f_path, f_text)
}

fn open_lexical(index_dir: &Path, can_write: bool) -> Result<Option<Lexical>, Error> {
    let path = index_dir.join(LEXICAL_DIR);
    if !path.join("meta.json").is_file() {
        return Ok(None);
    }
    let index = Index::open_in_dir(&path)
        .map_err(|err| Error::index_unusable(format!("open lexical index: {err}")))?;
    Ok(Some(bind_lexical(index, can_write)?))
}

fn create_lexical(index_dir: &Path, can_write: bool) -> Result<Lexical, Error> {
    let path = index_dir.join(LEXICAL_DIR);
    fs::create_dir_all(&path)
        .map_err(|err| Error::io(format!("create {}: {err}", path.display())))?;
    let (schema, _, _, _, _, _) = lexical_schema();
    let index = if path.join("meta.json").is_file() {
        Index::open_in_dir(&path)
            .map_err(|err| Error::index_unusable(format!("open lexical index: {err}")))?
    } else {
        Index::create_in_dir(&path, schema)
            .map_err(|err| Error::io(format!("create lexical index: {err}")))?
    };
    bind_lexical(index, can_write)
}

fn bind_lexical(index: Index, can_write: bool) -> Result<Lexical, Error> {
    let schema = index.schema();
    let f_key = schema
        .get_field("key")
        .map_err(|err| Error::index_unusable(format!("lexical field key: {err}")))?;
    let f_collection = schema
        .get_field("collection")
        .map_err(|err| Error::index_unusable(format!("lexical field collection: {err}")))?;
    let f_id = schema
        .get_field("id")
        .map_err(|err| Error::index_unusable(format!("lexical field id: {err}")))?;
    let f_path = schema
        .get_field("path")
        .map_err(|err| Error::index_unusable(format!("lexical field path: {err}")))?;
    let f_text = schema
        .get_field("text")
        .map_err(|err| Error::index_unusable(format!("lexical field text: {err}")))?;
    let writer = if can_write {
        Some(
            index
                .writer_with_num_threads(1, WRITER_HEAP)
                .map_err(|err| Error::index_unusable(format!("lexical writer: {err}")))?,
        )
    } else {
        None
    };
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .map_err(|err| Error::index_unusable(format!("lexical reader: {err}")))?;
    Ok(Lexical {
        index,
        writer,
        reader,
        f_key,
        f_collection,
        f_id,
        f_path,
        f_text,
    })
}

fn load_vector(index_dir: &Path) -> Result<Option<VectorStore>, Error> {
    let dir = index_dir.join(VECTOR_DIR);
    let meta_path = dir.join(VECTOR_META);
    let dump = dir.join(VECTOR_INDEX);
    if !meta_path.is_file() || !dump.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&meta_path)
        .map_err(|err| Error::io(format!("read {}: {err}", meta_path.display())))?;
    let meta: VectorMeta = serde_json::from_str(&raw)
        .map_err(|err| Error::index_unusable(format!("vector meta: {err}")))?;
    let ann = AnnIndex::restore(&dump)?;
    if ann.dimensions() != meta.dimensions {
        return Ok(None);
    }
    let keys_path = dir.join(VECTOR_KEYS);
    let keys = if keys_path.is_file() {
        let text = fs::read_to_string(&keys_path)
            .map_err(|err| Error::io(format!("read {}: {err}", keys_path.display())))?;
        serde_json::from_str(&text)
            .map_err(|err| Error::index_unusable(format!("vector keys: {err}")))?
    } else {
        BTreeMap::new()
    };
    Ok(Some(VectorStore {
        ann,
        keys,
        model_id: meta.model_id,
        dimensions: meta.dimensions,
        field_ids: meta.field_ids.into_iter().collect(),
    }))
}

fn save_vector(index_dir: &Path, store: &VectorStore) -> Result<(), Error> {
    let dir = index_dir.join(VECTOR_DIR);
    fs::create_dir_all(&dir)
        .map_err(|err| Error::io(format!("create {}: {err}", dir.display())))?;
    store.ann.save(&dir.join(VECTOR_INDEX))?;
    let meta = VectorMeta {
        model_id: store.model_id.clone(),
        dimensions: store.dimensions,
        field_ids: store.field_ids.iter().cloned().collect(),
    };
    let meta_bytes = serde_json::to_vec_pretty(&meta)
        .map_err(|err| Error::io(format!("encode vector meta: {err}")))?;
    store::atomic_write(&dir.join(VECTOR_META), &meta_bytes)?;
    let keys_bytes = serde_json::to_vec(&store.keys)
        .map_err(|err| Error::io(format!("encode vector keys: {err}")))?;
    store::atomic_write(&dir.join(VECTOR_KEYS), &keys_bytes)
}

fn load_edges(index_dir: &Path) -> Result<EdgeIndex, Error> {
    let path = index_dir.join(EDGES_FILE);
    if !path.is_file() {
        return Ok(EdgeIndex::default());
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::io(format!("read {}: {err}", path.display())))?;
    let mut edges = EdgeIndex::default();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let parsed: EdgeLine = serde_json::from_str(line)
            .map_err(|err| Error::index_unusable(format!("edge index: {err}")))?;
        edges
            .by_target
            .entry((parsed.src, parsed.field, parsed.dst))
            .or_default()
            .push(parsed.id);
    }
    Ok(edges)
}

fn save_edges(index_dir: &Path, edges: &EdgeIndex) -> Result<(), Error> {
    let mut bytes = Vec::new();
    for ((src, field, dst), ids) in &edges.by_target {
        for id in ids {
            let line = EdgeLine {
                src: src.clone(),
                id: id.clone(),
                field: field.clone(),
                dst: dst.clone(),
            };
            let mut encoded = serde_json::to_vec(&line)
                .map_err(|err| Error::io(format!("encode edge index: {err}")))?;
            encoded.push(b'\n');
            bytes.extend_from_slice(&encoded);
        }
    }
    store::atomic_write(&index_dir.join(EDGES_FILE), &bytes)
}

fn forget_source(edges: &mut EdgeIndex, src: &str, id: &str) {
    edges.by_target.retain(|(s, _, _), ids| {
        if s == src {
            ids.retain(|existing| existing != id);
        }
        !ids.is_empty()
    });
}

fn add_edges(edges: &mut EdgeIndex, decl: &CollectionDecl, record: &Record) -> Result<(), Error> {
    let Some(id) = record.id.as_deref() else {
        return Ok(());
    };
    let fields = store::fields_map(record)?;
    collect_edges(&decl.fields, &fields, &decl.name, id, edges);
    Ok(())
}

fn collect_edges(
    decls: &[FieldDecl],
    values: &serde_json::Map<String, Value>,
    src: &str,
    src_id: &str,
    edges: &mut EdgeIndex,
) {
    for field in decls {
        if field.edge.is_some() {
            for dst in edge_target_ids(values.get(&field.name)) {
                edges
                    .by_target
                    .entry((src.to_owned(), field.name.clone(), dst))
                    .or_default()
                    .push(src_id.to_owned());
            }
        }
        match values.get(&field.name) {
            Some(Value::Object(map)) => {
                collect_edges(&field.fields, map, src, src_id, edges);
            }
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(map) = item.as_object() {
                        collect_edges(&field.fields, map, src, src_id, edges);
                    }
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn edge_target_ids(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(id)) if !id.is_empty() => vec![id.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn marked_text(
    decl: &CollectionDecl,
    record: &Record,
) -> Result<(String, String), Error> {
    let fields = store::fields_map(record)?;
    let mut fts = Vec::new();
    let mut vector = Vec::new();
    collect_marked(&decl.fields, &fields, &mut fts, &mut vector);
    if let Some(body) = &decl.body
        && let Some(text) = record.body.as_deref().filter(|text| !text.is_empty())
    {
        if body.fts {
            fts.push(text.to_owned());
        }
        if body.vector {
            vector.push(text.to_owned());
        }
    }
    Ok((fts.join("\n"), vector.join("\n")))
}

fn collect_marked(
    decls: &[FieldDecl],
    values: &serde_json::Map<String, Value>,
    fts: &mut Vec<String>,
    vector: &mut Vec<String>,
) {
    for field in decls {
        if let Some(value) = values.get(&field.name) {
            if field.fts {
                push_strings(value, fts);
            }
            if field.vector {
                push_strings(value, vector);
            }
            match value {
                Value::Object(map) => collect_marked(&field.fields, map, fts, vector),
                Value::Array(items) => {
                    for item in items {
                        if let Some(map) = item.as_object() {
                            collect_marked(&field.fields, map, fts, vector);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn push_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) if !text.is_empty() => out.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                if let Some(text) = item.as_str().filter(|text| !text.is_empty()) {
                    out.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn check_strict_edges(
    schema: &Schema,
    decl: &CollectionDecl,
    record: &Record,
    lookup: impl Fn(&str, &str) -> Result<Record, Error>,
) -> Result<(), Error> {
    let fields = store::fields_map(record)?;
    check_strict_fields(&decl.fields, &fields, schema, &lookup)
}

fn check_strict_fields(
    decls: &[FieldDecl],
    values: &serde_json::Map<String, Value>,
    schema: &Schema,
    lookup: &impl Fn(&str, &str) -> Result<Record, Error>,
) -> Result<(), Error> {
    for field in decls {
        if let Some(edge) = &field.edge
            && edge.dangling == Dangling::Strict
        {
            if schema.get(&edge.collection).is_none() {
                return Err(Error::invalid(format!(
                    "edge {} targets unknown collection {}",
                    field.name, edge.collection
                )));
            }
            for id in edge_target_ids(values.get(&field.name)) {
                match lookup(&edge.collection, &id) {
                    Ok(_) => {}
                    Err(err) if err.code.as_deref() == Some(crate::Code::NotFound.as_str()) => {
                        return Err(Error::invalid(format!(
                            "strict edge {} target {id} is missing",
                            field.name
                        )));
                    }
                    Err(err) => return Err(err),
                }
            }
        }
        match values.get(&field.name) {
            Some(Value::Object(map)) => check_strict_fields(&field.fields, map, schema, lookup)?,
            Some(Value::Array(items)) => {
                for item in items {
                    if let Some(map) = item.as_object() {
                        check_strict_fields(&field.fields, map, schema, lookup)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn require_edge<'a>(
    decl: &'a CollectionDecl,
    field: &str,
) -> Result<&'a EdgeDecl, Error> {
    decl.edge(field)
        .ok_or_else(|| Error::invalid(format!("field {field} is not an edge on {}", decl.name)))
}

fn doc_key(collection: &str, id: &str) -> String {
    format!("{collection}\0{id}")
}

fn ann_key(collection: &str, id: &str) -> u64 {
    crate::fnv::digest(doc_key(collection, id).as_bytes())
}
