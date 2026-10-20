//! Searches over the Index on Lance: `Collection.Search` and
//! `Vault.SearchDocuments`.
//!
//! A search ranks: the Documents that best fit a text, or a Document, by
//! relevance, with scores. Its scope and its `Filter` are those of a query;
//! the filter narrows where the legs look before they rank (a prefilter),
//! and never changes a score.
//!
//! **Legs.** The lexical leg ranks by BM25 over the `fts` columns, each
//! collection by the statistics of its own table. For `text` it matches the
//! words of the text; for `like` the words of the `fts` columns of that
//! Document, as the Index holds them. The vector leg embeds the text
//! (`Embedder.EmbedQuery`), or takes the stored vectors of the Document
//! (`like`: no model is called), and asks each `vector` column for the
//! nearest vectors by cosine distance: through its IVF_PQ index, the
//! candidates ranked again with the full vectors, and by an exact scan of
//! the rows no index covers yet (a table too small for an index, and the
//! rows written since). A Document with several vectors counts once, by its
//! nearest. The Document `like` names is never a hit of its own search.
//!
//! **Fusion.** Reciprocal Rank Fusion of the legs that ran: a hit at rank
//! `r` (from 0) of a leg of weight `w` adds `w / (60 + r + 1)`. A leg of
//! weight 0 does not run. Ties break by collection, then id.
//! `lexical_score` is the BM25 score, `vector_score` is `1 / (1 + d)` with
//! `d` the cosine distance, `score` the fused one. Each leg is asked for
//! `max(offset + k, rerank_limit, 50)` hits, so a Document one leg ranks
//! low can still be promoted by the other.
//!
//! **Rerank.** The first `rerank_limit` fused hits go to the Vault's
//! reranker, with the query text first (for `like`, the `fts` text of its
//! Document) and then the Documents with their `raw`. They are ordered by
//! its score; the hits after them keep the fused order. `offset` and `k`
//! then cut that order.
//!
//! **Stream.** The ranking is computed whole, and holds keys and scores.
//! The summary comes first; the Documents are read from git as the stream
//! reaches them, at the state the Index reflects, and a hit whose file that
//! state lacks is skipped.

use std::collections::{BTreeMap, HashMap, VecDeque};

use lance::Dataset;
use lance::deps::arrow_array::{
    Array, FixedSizeListArray, Float32Array, ListArray, RecordBatch, StringArray,
};
use lance::deps::datafusion::prelude::{Expr, col, lit};
use lance_index::scalar::FullTextSearchQuery;
use lance_index::scalar::inverted::SCORE_COL;
use lance_index::vector::DIST_COL;
use lance_linalg::distance::DistanceType;
use scheda_api::v1::document_search::Subject;
use scheda_api::v1::search_item::Item;
use scheda_api::v1::search_summary::VectorLeg;
use scheda_api::v1::{
    Document, DocumentKey, DocumentSearch, Filter, SearchHit, SearchItem, SearchSummary,
    VaultErrorCode, VaultSearch,
};
use scheda_blocking::Blocking;

use crate::collection::Declaration;
use crate::error::{Context, Error, Fault};
use crate::filter;
use crate::gate::Pass;
use crate::index::State;
use crate::ports::task;
use crate::queries::{self, Reader, Reading, Table};
use crate::store::{Column, digest_name, lance_fault, vector_name};
use crate::vault::Vault;

/// How many hits `k` has when the request leaves it out.
const DEFAULT_K: usize = 10;

/// The least depth of each leg, so the fusion can promote a Document that
/// one leg ranks below `k`.
const DEPTH_FLOOR: usize = 50;

/// The constant of Reciprocal Rank Fusion.
const RRF_K: f64 = 60.0;

/// How many candidates of the PQ index per hit asked for are ranked again
/// with the full vectors.
const REFINE: u32 = 10;

fn invalid(message: impl Into<String>) -> Error {
    Fault::new(VaultErrorCode::InvalidArgument, message).into()
}

/// One hit of the ranking: its key and its scores.
#[derive(Clone, Debug)]
struct Ranked {
    collection: String,
    id: String,
    lexical: Option<f64>,
    vector: Option<f64>,
    fused: f64,
    rerank: Option<f64>,
}

/// One hit of a leg: its key and its leg score (BM25, or the cosine
/// distance).
#[derive(Clone, Debug)]
struct Candidate {
    collection: String,
    id: String,
    score: f64,
}

/// What a search found: the summary of its legs, and the ranking cut by
/// `offset` and `k`.
pub(crate) struct Found {
    summary: SearchSummary,
    hits: Vec<Ranked>,
    declarations: BTreeMap<String, Declaration>,
}

/// The numbers of a search, checked.
struct Ask<'a> {
    k: usize,
    offset: usize,
    lexical_weight: f64,
    vector_weight: f64,
    rerank_limit: Option<usize>,
    filter: Option<&'a Filter>,
}

impl<'a> Ask<'a> {
    /// The numbers of `search` on a Vault with or without a reranker.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when both weights are 0, when `rerank_limit` is
    /// set and the Vault has no reranker, and when `rerank_limit` is below
    /// the effective `k`.
    fn of(search: &'a DocumentSearch, has_reranker: bool) -> Result<Ask<'a>, Error> {
        let k = match search.k {
            None | Some(0) => DEFAULT_K,
            Some(k) => usize::try_from(k).unwrap_or(usize::MAX),
        };
        let lexical_weight = search.lexical_weight.unwrap_or(1.0);
        let vector_weight = search.vector_weight.unwrap_or(1.0);
        if lexical_weight == 0.0 && vector_weight == 0.0 {
            return Err(invalid("both weights are 0: no leg would rank"));
        }
        let rerank_limit = search
            .rerank_limit
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
        if let Some(limit) = rerank_limit {
            if !has_reranker {
                return Err(invalid(
                    "rerank_limit is set, and the Vault has no reranker",
                ));
            }
            if limit < k {
                return Err(invalid(format!(
                    "rerank_limit {limit} is below k {k}: every hit of the page must be reranked"
                )));
            }
        }
        Ok(Ask {
            k,
            offset: usize::try_from(search.offset.unwrap_or(0)).unwrap_or(usize::MAX),
            lexical_weight,
            vector_weight,
            rerank_limit,
            filter: search.filter.as_option(),
        })
    }

    /// How many hits each leg is asked for.
    fn depth(&self) -> usize {
        self.offset
            .saturating_add(self.k)
            .max(self.rerank_limit.unwrap_or(0))
            .max(DEPTH_FLOOR)
    }
}

/// The hits one leg asks of a table that holds `rows` rows: the depth and
/// one more (the Document a `like` search leaves out), bounded by the rows
/// the table holds, and at least one.
fn reach_in(depth: usize, rows: usize) -> usize {
    depth.saturating_add(1).min(rows).max(1)
}

/// `Collection.Search` without the Documents.
///
/// # Errors
///
/// `INVALID_ARGUMENT` for a request that breaks a rule; the codes of
/// [`Reading::collection`]; `NOT_FOUND` for a `like` whose Document the
/// Index does not hold; the failure of the Index or of a port.
pub(crate) async fn in_collection(
    pass: &Pass,
    reading: &Reading,
    collection: &str,
    search: &DocumentSearch,
) -> Result<Found, Error> {
    let decl = reading.collection(collection, search.filter.is_set())?;
    if !searchable(&decl) {
        return Err(unsearchable(&decl));
    }
    run(pass, reading, vec![decl], search).await
}

/// `Vault.SearchDocuments` without the Documents.
///
/// # Errors
///
/// As [`in_collection`].
pub(crate) async fn in_vault(
    pass: &Pass,
    reading: &Reading,
    search: &VaultSearch,
) -> Result<Found, Error> {
    let Some(inner) = search.search.as_option() else {
        return Err(invalid("a search of the Vault names its search"));
    };
    let names = search
        .collections
        .as_option()
        .map(|names| names.names.clone())
        .unwrap_or_default();
    let mut scope = reading.scope(&names, inner.filter.is_set())?;
    if names.is_empty() {
        scope.retain(searchable);
    } else if let Some(decl) = scope.iter().find(|decl| !searchable(decl)) {
        return Err(unsearchable(decl));
    }
    run(pass, reading, scope, inner).await
}

/// Whether a leg can rank the Documents of `decl`: the Index holds its
/// columns, or its vectors.
fn searchable(decl: &Declaration) -> bool {
    !(decl.excludes_columns() && decl.excludes_vectors())
}

fn unsearchable(decl: &Declaration) -> Error {
    Fault::new(
        VaultErrorCode::Unimplemented,
        format!(
            "the collection {} is exclude_lexical_index and exclude_vector_index: no leg ranks it",
            decl.name()
        ),
    )
    .into()
}

/// The Document `like` names: its key, the text of its `fts` columns, and
/// its stored vectors.
struct Like {
    collection: String,
    id: String,
    text: String,
    vectors: Vec<Vec<f32>>,
}

async fn run(
    pass: &Pass,
    reading: &Reading,
    scope: Vec<Declaration>,
    search: &DocumentSearch,
) -> Result<Found, Error> {
    let ports = pass.vault().ports();
    let reranker = ports.reranker();
    let embedder = ports.embedder();
    let ask = Ask::of(search, reranker.is_some())?;
    if let Some(filter) = ask.filter {
        let columns: Vec<&[Column]> = scope
            .iter()
            .map(|decl| reading.columns(decl.name()))
            .collect();
        filter::check(filter, &columns, queries::max_depth(pass.vault()))?;
    }
    let like = match &search.subject {
        Some(Subject::Text(_)) => None,
        Some(Subject::Like(key)) => Some(like_of(reading, key).await?),
        None => return Err(invalid("a search names a text or a Document")),
    };
    let text = match (&search.subject, &like) {
        (_, Some(like)) => like.text.clone(),
        (Some(Subject::Text(text)), None) => text.clone(),
        (_, None) => String::new(),
    };
    let depth = ask.depth();
    // Each table in scope that the Index holds, opened once.
    let mut tables = Vec::new();
    for decl in &scope {
        if let Some(table) = reading.table(decl.name()) {
            let opened = table.clone();
            let dataset = task(async move { opened.open().await }).await?;
            tables.push((decl, table, dataset));
        }
    }
    let leg = vector_leg(&ask, reading, &scope, embedder.is_some(), &tables).await?;
    let mut queries_of_vector_leg = Vec::new();
    if leg == VectorLeg::VECTOR_LEG_RAN {
        queries_of_vector_leg = match (&like, &embedder) {
            (Some(like), _) => like.vectors.clone(),
            (None, Some(embedder)) => vec![
                embedder
                    .embed_query(text.clone())
                    .await
                    .context("embed the text of the search")?,
            ],
            (None, None) => Vec::new(),
        };
    }
    let mut lexical = Vec::new();
    let mut nearest: HashMap<(String, String), f64> = HashMap::new();
    for (decl, table, dataset) in &tables {
        // Lance allocates for the limit it is given, so a `k`, `offset`, or
        // `rerank_limit` of billions would abort the process. The table
        // cannot answer with more rows than it holds.
        let reach = reach_in(depth, dataset.count_rows(None).await.map_err(lance_fault)?);
        let prefilter = ask.filter.map_or_else(
            || lit(true),
            |filter| filter::compile(filter, table.columns()),
        );
        let fts = reading.fts_columns(decl.name());
        if ask.lexical_weight > 0.0 && !fts.is_empty() && !text.trim().is_empty() {
            lexical.extend(
                lexical_leg(
                    dataset.clone(),
                    decl.name().to_owned(),
                    text.clone(),
                    fts,
                    prefilter.clone(),
                    reach,
                )
                .await?,
            );
        }
        if queries_of_vector_leg.is_empty() {
            continue;
        }
        for column in &reading.vectors(decl.name()) {
            let found = vector_leg_on(
                dataset.clone(),
                decl.name().to_owned(),
                vector_name(column),
                queries_of_vector_leg.clone(),
                prefilter.clone(),
                reach,
            )
            .await?;
            for candidate in found {
                let distance = nearest
                    .entry((candidate.collection, candidate.id))
                    .or_insert(f64::INFINITY);
                *distance = distance.min(candidate.score);
            }
        }
    }
    let mut vector: Vec<Candidate> = nearest
        .into_iter()
        .map(|((collection, id), score)| Candidate {
            collection,
            id,
            score,
        })
        .collect();
    let itself = like
        .as_ref()
        .map(|like| (like.collection.as_str(), like.id.as_str()));
    let others = |candidate: &Candidate| {
        itself
            .is_none_or(|(collection, id)| candidate.collection != collection || candidate.id != id)
    };
    lexical.retain(others);
    vector.retain(others);
    // BM25 larger first, distance smaller first.
    lexical.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| key_order(a, b)));
    vector.sort_by(|a, b| a.score.total_cmp(&b.score).then_with(|| key_order(a, b)));
    lexical.truncate(depth);
    vector.truncate(depth);
    let mut hits = fuse(&ask, &lexical, &vector);
    let declarations: BTreeMap<String, Declaration> = scope
        .into_iter()
        .map(|decl| (decl.name().to_owned(), decl))
        .collect();
    if let (Some(limit), Some(reranker)) = (ask.rerank_limit, reranker) {
        let head: Vec<Ranked> = hits.drain(..limit.min(hits.len())).collect();
        let keys: Vec<(String, String)> = head
            .iter()
            .map(|hit| (hit.collection.clone(), hit.id.clone()))
            .collect();
        let read = {
            let declarations = declarations.clone();
            let state = reading.state();
            pass.step(move |cx, vault| read_all(cx, vault, &declarations, state, &keys))
                .await?
        };
        // A hit whose file the state lacks is not a hit.
        let (head, documents): (Vec<Ranked>, Vec<Document>) = head
            .into_iter()
            .zip(read)
            .filter_map(|(hit, document)| document.map(|document| (hit, document)))
            .unzip();
        let scores = reranker
            .score(text, documents)
            .await
            .context("rerank the hits")?;
        let mut head: Vec<(Ranked, f64)> = head.into_iter().zip(scores).collect();
        // Stable: equal scores keep the fused order.
        head.sort_by(|(_, a), (_, b)| b.total_cmp(a));
        let mut reranked: Vec<Ranked> = head
            .into_iter()
            .map(|(mut hit, score)| {
                hit.rerank = Some(score);
                hit
            })
            .collect();
        reranked.extend(hits);
        hits = reranked;
    }
    Ok(Found {
        summary: SearchSummary::default().with_vector_leg(leg),
        hits: hits.into_iter().skip(ask.offset).take(ask.k).collect(),
        declarations,
    })
}

/// Collection, then id.
fn key_order(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    a.collection
        .cmp(&b.collection)
        .then_with(|| a.id.cmp(&b.id))
}

/// Reciprocal Rank Fusion of the two legs, best first.
fn fuse(ask: &Ask<'_>, lexical: &[Candidate], vector: &[Candidate]) -> Vec<Ranked> {
    let mut fused: BTreeMap<(String, String), Ranked> = BTreeMap::new();
    for (rank, candidate) in lexical.iter().enumerate() {
        let hit = ranked(&mut fused, candidate);
        hit.lexical = Some(candidate.score);
        hit.fused += ask.lexical_weight / (RRF_K + rank as f64 + 1.0);
    }
    for (rank, candidate) in vector.iter().enumerate() {
        let hit = ranked(&mut fused, candidate);
        hit.vector = Some(1.0 / (1.0 + candidate.score));
        hit.fused += ask.vector_weight / (RRF_K + rank as f64 + 1.0);
    }
    let mut hits: Vec<Ranked> = fused.into_values().collect();
    hits.sort_by(|a, b| {
        b.fused
            .total_cmp(&a.fused)
            .then_with(|| a.collection.cmp(&b.collection))
            .then_with(|| a.id.cmp(&b.id))
    });
    hits
}

/// The hit of `candidate` in `fused`, added with no score when new.
fn ranked<'a>(
    fused: &'a mut BTreeMap<(String, String), Ranked>,
    candidate: &Candidate,
) -> &'a mut Ranked {
    fused
        .entry((candidate.collection.clone(), candidate.id.clone()))
        .or_insert_with(|| Ranked {
            collection: candidate.collection.clone(),
            id: candidate.id.clone(),
            lexical: None,
            vector: None,
            fused: 0.0,
            rerank: None,
        })
}

/// Why the vector leg runs or does not, the first that applies: not asked
/// for; no collection in scope has a `vector` column the Projector marks;
/// the Vault has no embedder; no table in scope holds a vector; it runs.
async fn vector_leg(
    ask: &Ask<'_>,
    reading: &Reading,
    scope: &[Declaration],
    has_embedder: bool,
    tables: &[(&Declaration, Table, Dataset)],
) -> Result<VectorLeg, Error> {
    if ask.vector_weight == 0.0 {
        return Ok(VectorLeg::VECTOR_LEG_NOT_ASKED);
    }
    if scope
        .iter()
        .all(|decl| reading.vectors(decl.name()).is_empty())
    {
        return Ok(VectorLeg::VECTOR_LEG_NO_VECTOR_COLLECTION);
    }
    if !has_embedder {
        return Ok(VectorLeg::VECTOR_LEG_NO_EMBEDDER);
    }
    for (decl, _, dataset) in tables {
        for column in &reading.vectors(decl.name()) {
            if holds_vector(dataset.clone(), digest_name(column)).await? {
                return Ok(VectorLeg::VECTOR_LEG_RAN);
            }
        }
    }
    Ok(VectorLeg::VECTOR_LEG_NO_VECTORS)
}

/// Whether a row of `dataset` has a vector: a digest in the column
/// `digest`, which is set with the vector and only with it. The scan ends
/// at the first such row.
async fn holds_vector(dataset: Dataset, digest: String) -> Result<bool, Error> {
    task(async move {
        let mut scanner = dataset.scan();
        scanner
            .project(&["id"])
            .map_err(lance_fault)?
            .filter_expr(col(digest.as_str()).is_not_null())
            .limit(Some(1), None)
            .map_err(lance_fault)?;
        let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
        Ok(batch.num_rows() > 0)
    })
    .await
}

/// The Document `like` names, as the Index holds it.
///
/// # Errors
///
/// The codes of [`Reading::collection`]; `NOT_FOUND` when the Index does
/// not hold the Document.
async fn like_of(reading: &Reading, key: &DocumentKey) -> Result<Like, Error> {
    let collection = key.collection.clone().unwrap_or_default();
    let id = key.id.clone().unwrap_or_default();
    reading.collection(&collection, false)?;
    let Some(table) = reading.table(&collection) else {
        return Err(not_held(&collection, &id));
    };
    let fts = reading.fts_columns(&collection);
    let vectors: Vec<String> = reading
        .vectors(&collection)
        .iter()
        .map(|column| vector_name(column))
        .collect();
    task(async move {
        let dataset = table.open().await?;
        let mut wanted = vec![String::from("id")];
        wanted.extend(fts.iter().cloned());
        wanted.extend(vectors.iter().cloned());
        let mut scanner = dataset.scan();
        scanner
            .project(&wanted)
            .map_err(lance_fault)?
            .filter_expr(col("id").eq(lit(id.clone())));
        let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
        if batch.num_rows() == 0 {
            return Err(not_held(&collection, &id));
        }
        let words: Vec<String> = fts.iter().flat_map(|name| texts_at(&batch, name)).collect();
        let stored = vectors
            .iter()
            .filter_map(|name| vector_at(&batch, name))
            .collect();
        Ok(Like {
            collection,
            id,
            text: words.join("\n"),
            vectors: stored,
        })
    })
    .await
}

fn not_held(collection: &str, id: &str) -> Error {
    Fault::new(
        VaultErrorCode::NotFound,
        format!("the Index holds no Document {id} of the collection {collection}"),
    )
    .into()
}

/// The texts of the first row of `batch` in the column `name`: a `text`,
/// or each item of a `text list`.
fn texts_at(batch: &RecordBatch, name: &str) -> Vec<String> {
    let Some(array) = batch.column_by_name(name) else {
        return Vec::new();
    };
    if array.is_null(0) {
        return Vec::new();
    }
    if let Some(strings) = array.as_any().downcast_ref::<StringArray>() {
        return vec![strings.value(0).to_owned()];
    }
    let Some(lists) = array.as_any().downcast_ref::<ListArray>() else {
        return Vec::new();
    };
    let items = lists.value(0);
    let Some(strings) = items.as_any().downcast_ref::<StringArray>() else {
        return Vec::new();
    };
    (0..strings.len())
        .filter(|n| !strings.is_null(*n))
        .map(|n| strings.value(n).to_owned())
        .collect()
}

/// The vector of the first row of `batch` in the column `name`, when it
/// has one.
fn vector_at(batch: &RecordBatch, name: &str) -> Option<Vec<f32>> {
    let lists = batch
        .column_by_name(name)?
        .as_any()
        .downcast_ref::<FixedSizeListArray>()?;
    if lists.is_null(0) {
        return None;
    }
    let values = lists.value(0);
    let floats = values.as_any().downcast_ref::<Float32Array>()?;
    Some(floats.values().to_vec())
}

/// The lexical leg on one table: up to `limit` rows that `prefilter`
/// admits, by BM25 of `text` over the `columns`.
async fn lexical_leg(
    dataset: Dataset,
    collection: String,
    text: String,
    columns: Vec<String>,
    prefilter: Expr,
    limit: usize,
) -> Result<Vec<Candidate>, Error> {
    task(async move {
        let query = FullTextSearchQuery::new(text)
            .with_columns(&columns)
            .map_err(lance_fault)?
            .limit(Some(i64::try_from(limit).unwrap_or(i64::MAX)));
        let mut scanner = dataset.scan();
        scanner
            .full_text_search(query)
            .map_err(lance_fault)?
            .project(&["id"])
            .map_err(lance_fault)?
            .prefilter(true)
            .filter_expr(prefilter);
        let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
        candidates(&batch, &collection, SCORE_COL)
    })
    .await
}

/// The vector leg on one vector column of one table: for each of the
/// `queries`, up to `limit` rows that `prefilter` admits, nearest it by
/// cosine distance. A row without a vector there is never one.
async fn vector_leg_on(
    dataset: Dataset,
    collection: String,
    column: String,
    queries: Vec<Vec<f32>>,
    prefilter: Expr,
    limit: usize,
) -> Result<Vec<Candidate>, Error> {
    task(async move {
        let mut found = Vec::new();
        for query in queries {
            let query = Float32Array::from(query);
            let mut scanner = dataset.scan();
            scanner
                .nearest(&column, &query, limit)
                .map_err(lance_fault)?
                .distance_metric(DistanceType::Cosine)
                .refine(REFINE)
                .project(&["id"])
                .map_err(lance_fault)?
                .prefilter(true)
                .filter_expr(prefilter.clone());
            let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
            found.extend(candidates(&batch, &collection, DIST_COL)?);
        }
        Ok(found)
    })
    .await
}

/// The hits of a leg in `batch`: the `id`, and the leg score in the column
/// `score`.
fn candidates(batch: &RecordBatch, collection: &str, score: &str) -> Result<Vec<Candidate>, Error> {
    let broken = |what: &str| {
        Error::from(Fault::new(
            VaultErrorCode::IndexBroken,
            format!("a search of the Index gave no {what}"),
        ))
    };
    let ids = batch
        .column_by_name("id")
        .and_then(|array| array.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| broken("id"))?;
    let scores = batch
        .column_by_name(score)
        .and_then(|array| array.as_any().downcast_ref::<Float32Array>())
        .ok_or_else(|| broken("score"))?;
    Ok((0..batch.num_rows())
        .filter(|n| !scores.is_null(*n))
        .map(|n| Candidate {
            collection: collection.to_owned(),
            id: ids.value(n).to_owned(),
            score: f64::from(scores.value(n)),
        })
        .collect())
}

/// The Documents `keys` name, with their `raw`, at `state`: `None` for one
/// that state lacks.
fn read_all(
    cx: &Blocking,
    vault: &Vault,
    declarations: &BTreeMap<String, Declaration>,
    state: State,
    keys: &[(String, String)],
) -> Result<Vec<Option<Document>>, Error> {
    let mut reader = Reader::default();
    keys.iter()
        .map(|(collection, id)| match declarations.get(collection) {
            Some(decl) => queries::read_document(cx, vault, decl, state, id, true, &mut reader)
                .context("read a Document to rerank"),
            None => Ok(None),
        })
        .collect()
}

/// The items a search streams: the summary, then each hit with its
/// Document, read from git at the state the Index reflects.
pub(crate) struct Hits {
    summary: Option<SearchItem>,
    hits: VecDeque<Ranked>,
    state: State,
    include_raw: bool,
    declarations: BTreeMap<String, Declaration>,
    reader: Reader,
}

impl Hits {
    /// The stream of `found`, read in `reading`.
    pub(crate) fn new(found: Found, reading: &Reading, include_raw: bool) -> Hits {
        Hits {
            summary: Some(SearchItem {
                item: Some(Item::Summary(Box::new(found.summary))),
                ..SearchItem::default()
            }),
            hits: found.hits.into(),
            state: reading.state(),
            include_raw,
            declarations: found.declarations,
            reader: Reader::default(),
        }
    }

    /// The next item: the summary, then one hit per call.
    pub(crate) fn next(
        &mut self,
        cx: &Blocking,
        vault: &Vault,
    ) -> Option<Result<SearchItem, Error>> {
        if let Some(summary) = self.summary.take() {
            return Some(Ok(summary));
        }
        loop {
            let hit = self.hits.pop_front()?;
            let Some(decl) = self.declarations.get(&hit.collection) else {
                continue;
            };
            let document = match queries::read_document(
                cx,
                vault,
                decl,
                self.state,
                &hit.id,
                self.include_raw,
                &mut self.reader,
            ) {
                Ok(Some(document)) => document,
                // The Index holds a Document the state it reflects lacks:
                // nothing to give.
                Ok(None) => continue,
                Err(error) => {
                    self.hits.clear();
                    return Some(Err(error.context("read a Document of the search")));
                }
            };
            let mut found = SearchHit::default().with_score(hit.fused);
            found.document = document.into();
            found.lexical_score = hit.lexical;
            found.vector_score = hit.vector;
            found.rerank_score = hit.rerank;
            return Some(Ok(SearchItem {
                item: Some(Item::Hit(Box::new(found))),
                ..SearchItem::default()
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lance::deps::arrow_array::builder::{ListBuilder, StringBuilder};

    use super::*;

    #[tokio::test]
    async fn the_rerank_read_of_journal_lines_reads_their_segment_once() {
        use scheda_api::v1::OpenOptions;

        use crate::repo::blob_reads;
        use crate::testing::{journal_of_lines, open, step};

        let dir = std::env::var_os("TEST_TMPDIR")
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
            .join(format!("scheda-rerank-segment-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let vault = open(&dir, OpenOptions::default().with_write_timeout_ms(2000))
            .await
            .unwrap();
        journal_of_lines(&vault, "log", 60).await;
        let declarations: BTreeMap<String, Declaration> = vault
            .declarations_now()
            .unwrap()
            .iter()
            .map(|decl| (decl.name().to_owned(), decl.clone()))
            .collect();
        let keys: Vec<(String, String)> = (0..50)
            .map(|n| ("log".to_owned(), format!("l{n:02}")))
            .collect();
        let state = step(&vault, |cx, vault| vault.state_now(cx)).await.unwrap();
        let (documents, reads) = step(&vault, move |cx, vault| {
            let before = blob_reads();
            let documents = read_all(cx, vault, &declarations, state, &keys)?;
            Ok((documents, blob_reads() - before))
        })
        .await
        .unwrap();
        assert!(documents.iter().all(Option::is_some), "every line is read");
        assert_eq!(reads, 1, "the segment is read once for the 50 hits");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn candidate(collection: &str, id: &str, score: f64) -> Candidate {
        Candidate {
            collection: collection.to_owned(),
            id: id.to_owned(),
            score,
        }
    }

    fn ask(lexical_weight: f64, vector_weight: f64) -> Ask<'static> {
        Ask {
            k: DEFAULT_K,
            offset: 0,
            lexical_weight,
            vector_weight,
            rerank_limit: None,
            filter: None,
        }
    }

    #[test]
    fn fusion_adds_the_weighted_reciprocal_ranks_and_breaks_ties_by_key() {
        let lexical = [candidate("a", "1", 9.0), candidate("a", "2", 5.0)];
        let vector = [candidate("a", "2", 0.1), candidate("b", "3", 0.2)];
        let hits = fuse(&ask(1.0, 2.0), &lexical, &vector);
        let keys: Vec<(&str, &str)> = hits
            .iter()
            .map(|hit| (hit.collection.as_str(), hit.id.as_str()))
            .collect();
        assert_eq!(keys, [("a", "2"), ("b", "3"), ("a", "1")]);
        assert!((hits[0].fused - (1.0 / 62.0 + 2.0 / 61.0)).abs() < 1e-12);
        assert_eq!(hits[0].lexical, Some(5.0));
        assert!((hits[0].vector.unwrap() - 1.0 / 1.1).abs() < 1e-12);
        assert_eq!(hits[2].vector, None);

        // Equal fused scores: collection, then id.
        let tied = fuse(
            &ask(1.0, 1.0),
            &[candidate("b", "1", 1.0)],
            &[candidate("a", "9", 0.5)],
        );
        assert_eq!(tied[0].collection, "a");
    }

    #[test]
    fn the_depth_covers_the_page_the_rerank_and_the_floor() {
        let mut numbers = ask(1.0, 1.0);
        assert_eq!(numbers.depth(), DEPTH_FLOOR);
        numbers.offset = 40;
        numbers.k = 30;
        assert_eq!(numbers.depth(), 70);
        numbers.rerank_limit = Some(200);
        assert_eq!(numbers.depth(), 200);
    }

    #[test]
    fn a_leg_asks_for_no_more_hits_than_the_table_holds() {
        assert_eq!(reach_in(70, 12_000), 71);
        assert_eq!(reach_in(70, 20), 20);
        assert_eq!(reach_in(usize::MAX, 12_000), 12_000);
        assert_eq!(reach_in(70, 0), 1, "a leg is never asked for none");
    }

    #[test]
    fn the_words_of_a_text_list_are_all_taken() {
        let mut builder = ListBuilder::new(StringBuilder::new());
        builder.values().append_value("red");
        builder.values().append_null();
        builder.values().append_value("blue");
        builder.append(true);
        let batch =
            RecordBatch::try_from_iter([("tags", Arc::new(builder.finish()) as Arc<dyn Array>)])
                .unwrap();
        assert_eq!(texts_at(&batch, "tags"), ["red", "blue"]);
        assert!(texts_at(&batch, "absent").is_empty());
    }
}
