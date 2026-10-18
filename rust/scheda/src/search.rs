//! Searches over the Index: `Collection.Search` and `Vault.SearchDocuments`.
//!
//! A search ranks: the Documents that best fit a text, or a Document, by
//! relevance, with scores. The scope and the Filter are those of a query
//! ([`crate::queries`]); the Filter only narrows where the legs look.
//!
//! **Phases.** A search is built from steps that the caller drives in
//! order, so a blocking caller and an async one share every line of ranking
//! and differ in the two calls of ports only:
//!
//! 1. [`Vault::prepare_search`] (blocking): checks the request, takes the
//!    snapshots of the Index and of the repositories, decides what the
//!    vector leg can do (the summary), runs the lexical leg, and says what
//!    the vector leg needs.
//! 2. `EmbedQuery` (a port): [`Prepared::embedding`] gives the text to embed,
//!    if the vector leg needs a model; the caller passes the vector on.
//! 3. [`Prepared::rank`] (blocking): the nearest-neighbour search of the
//!    vector leg and the fusion of the legs.
//! 4. `Score` (a port), when the request reranks: [`Ranked::scoring`] (blocking)
//!    reads the Documents the reranker scores and gives them with the query,
//!    the caller passes them to the reranker, and [`Ranked::scored`]
//!    (blocking) puts the scores in.
//! 5. [`Ranked::into_stream`]: the lazy stream of hits.
//!
//! [`Vault::search`] runs them in order on the thread of its caller, which
//! is how a blocking embedder or reranker is called (a port is called the
//! way Scheda is called). No step but 2 and 4 calls a port; the Projector,
//! which `like` asks for the text of its Document, is the exception, and a
//! blocking one.
//!
//! **Legs.** The lexical leg ranks by BM25 over the `text` field (the
//! `fts` columns). For `text` it parses the words of the text as a Tantivy
//! query. For `like` it runs Tantivy's `MoreLikeThis` over the `fts` text of
//! the Document: the Index does not store that text, so the Projector of the
//! Vault projects the Document again (`like` on a Vault without a
//! Projector has no lexical leg). The vector leg embeds the text (`text`),
//! or takes the stored vectors of the Document (`like`: no model is
//! called), and asks the nearest-neighbour index for the nearest vectors; a
//! Document with several vectors counts once, by its nearest. The Document
//! `like` names is never a hit of its own search.
//!
//! **Fusion.** Reciprocal Rank Fusion of the legs that ran: a hit at rank
//! `r` (from 0) of a leg of weight `w` adds `w / (60 + r + 1)`. A leg of
//! weight 0 does not run. Ties break by collection, then id. `lexical_score`
//! is the BM25 score, `vector_score` is `1 / (1 + cosine distance)`, `score`
//! the fused one.
//!
//! **Depth.** Each leg is asked for `max(offset + k, rerank_limit, 50)`
//! hits, so a Document one leg ranks low can still be promoted by the
//! other.
//!
//! **Rerank.** The first `rerank_limit` fused hits go to the Vault's
//! reranker, with the query text first (for `like`, the `fts` text of its
//! Document) and then the Documents with their `raw`. They are ordered by
//! its score; the hits after them keep the fused order. `offset` and `k`
//! then cut that order.
//!
//! **Stream.** The ranking is computed whole, but it holds keys and scores.
//! The Documents are opened as the stream reaches them, from the snapshot
//! taken at the start; `offset` passes hits over without opening a file, and
//! a hit whose file `HEAD` no longer has is skipped.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use scheda_api::v1::search_item::Item;
use scheda_api::v1::search_summary::VectorLeg;
use scheda_api::v1::{
    Document, DocumentKey, DocumentSearch, Filter, SearchHit, SearchItem, SearchSummary,
    VaultErrorCode, VaultSearch, document_search::Subject, filter,
};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    BooleanQuery, ConstScoreQuery, MoreLikeThisQuery, Occur, Query, QueryParser, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, OwnedValue, Value as _};
use tantivy::{TantivyDocument, Term};

use crate::collection::{Declaration, Declarations};
use crate::error::{Context, Error, Fault};
use crate::filter::{Compiler, Scope};
use crate::lexical::{Snapshot, fts_text, key_of, read_fault};
use crate::ports::{EmbedderPort, RerankerPort};
use crate::queries::{Mode, Where, check_id, not_declared};
use crate::reads::Opener;
use crate::repo::Views;
use crate::vault::Vault;
use crate::vector::{self, Leg};

/// How many hits `k` has when the request leaves it out.
const DEFAULT_K: u64 = 10;

/// The least depth of each leg, so the fusion can promote a Document that
/// one leg ranks below `k`.
const DEPTH_FLOOR: u64 = 50;

/// The constant of Reciprocal Rank Fusion.
const RRF_K: f64 = 60.0;

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

/// What a search ranks by.
#[derive(Debug)]
enum By {
    /// A text.
    Text(String),
    /// A Document.
    Like { collection: String, id: String },
}

/// A search, checked against the rules of the contract and ready to
/// prepare.
#[derive(Debug)]
pub(crate) struct Request {
    call: &'static str,
    scope: Where,
    by: By,
    filter: Option<Filter>,
    k: u64,
    lexical_weight: f64,
    vector_weight: f64,
    rerank_limit: Option<u64>,
    include_raw: bool,
    offset: u64,
}

impl Request {
    /// The request of `Collection.Search` (`call`) on `collection`.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the message breaks a rule of the contract,
    /// when both weights are 0, or when `rerank_limit` is below `k`.
    pub(crate) fn of_collection(
        call: &'static str,
        collection: &str,
        search: DocumentSearch,
    ) -> Result<Request, Error> {
        Request::of(call, Where::Collection(collection.to_owned()), search)
    }

    /// The request of `Vault.SearchDocuments` (`call`).
    ///
    /// # Errors
    ///
    /// As [`Request::of_collection`]; also for a `like` that names no
    /// collection.
    pub(crate) fn of_vault(call: &'static str, mut search: VaultSearch) -> Result<Request, Error> {
        crate::stored::validate(&search).with_context(|| format!("{call}: check the request"))?;
        let names = search
            .collections
            .take()
            .map(|collections| collections.names)
            .unwrap_or_default();
        let Some(inner) = search.search.take() else {
            return Err(Error::from(invalid("the search is missing"))
                .context(format!("{call}: check the request")));
        };
        Request::of(call, Where::Vault(names), inner)
    }

    fn of(call: &'static str, scope: Where, mut search: DocumentSearch) -> Result<Request, Error> {
        crate::stored::validate(&search).with_context(|| format!("{call}: check the request"))?;
        let by = match search.subject.take() {
            Some(Subject::Text(text)) => By::Text(text),
            Some(Subject::Like(key)) => {
                let DocumentKey { collection, id, .. } = *key;
                By::Like {
                    collection: collection.unwrap_or_default(),
                    id: id.unwrap_or_default(),
                }
            }
            None => {
                return Err(Error::from(invalid("the search names no text or Document"))
                    .context(format!("{call}: check the request")));
            }
        };
        let k = match search.k {
            None | Some(0) => DEFAULT_K,
            Some(k) => u64::from(k),
        };
        let lexical_weight = search.lexical_weight.unwrap_or(1.0);
        let vector_weight = search.vector_weight.unwrap_or(1.0);
        if lexical_weight <= 0.0 && vector_weight <= 0.0 {
            return Err(Error::from(invalid(
                "both weights are 0, so no leg would run; give one a weight above 0",
            ))
            .context(format!("{call}: check the request")));
        }
        let rerank_limit = search.rerank_limit.map(u64::from);
        if let Some(limit) = rerank_limit
            && limit < k
        {
            return Err(
                Error::from(invalid(format!("rerank_limit {limit} is below k {k}")))
                    .context(format!("{call}: check the request")),
            );
        }
        Ok(Request {
            call,
            scope,
            by,
            filter: search.filter.take(),
            k,
            lexical_weight,
            vector_weight,
            rerank_limit,
            include_raw: search.include_raw.unwrap_or(false),
            offset: u64::from(search.offset.unwrap_or(0)),
        })
    }
}

/// A Document a leg ranked.
#[derive(Debug)]
struct LegHit {
    collection: String,
    id: String,
    score: f64,
}

/// A Document of the ranking: its key and its scores.
#[derive(Debug)]
struct Ranking {
    collection: String,
    id: String,
    lexical: Option<f64>,
    vector: Option<f64>,
    fused: f64,
    rerank: Option<f64>,
}

/// What every step of a search carries to the stream: how to cut the
/// ranking and where the Documents are read from.
struct Frame {
    call: &'static str,
    summary: VectorLeg,
    k: u64,
    offset: u64,
    include_raw: bool,
    views: Views,
    /// The collections of the scope, by name.
    decls: HashMap<String, Declaration>,
}

/// A text the embedder must turn into a vector, and the embedder.
pub(crate) struct Embedding {
    /// The embedder installed when the search began.
    pub(crate) port: Arc<EmbedderPort>,
    /// The text of the query.
    pub(crate) text: String,
}

/// What the vector leg searches with.
enum Probe {
    /// The vector of the text, which the caller asks the embedder for.
    Embedded,
    /// The stored vectors of the Document `like` names.
    Stored(Vec<Vec<f32>>),
}

/// The vector leg of a search that runs it.
struct VectorPlan {
    snapshot: Arc<vector::Snapshot>,
    probe: Probe,
    embedding: Option<Embedding>,
}

/// Which Documents the vector leg may hand out: those in the scope that the
/// Filter matches, and not the Document `like` names.
struct Admit {
    names: HashSet<String>,
    exclude: Option<(String, String)>,
    /// The Filter's matches; `None` without a Filter.
    filter: Option<Matches>,
}

/// The query of the Filter on each collection of the scope, over the
/// lexical Index it is evaluated on.
struct Matches {
    snapshot: Arc<Snapshot>,
    bases: HashMap<String, Box<dyn Query>>,
}

impl Admit {
    fn admits(&self, collection: &str, id: &str) -> Result<bool, Error> {
        if !self.names.contains(collection) {
            return Ok(false);
        }
        if self
            .exclude
            .as_ref()
            .is_some_and(|(excluded, own)| excluded == collection && own == id)
        {
            return Ok(false);
        }
        let Some(matches) = &self.filter else {
            return Ok(true);
        };
        let Some(base) = matches.bases.get(collection) else {
            return Ok(false);
        };
        let query = BooleanQuery::new(vec![
            (Occur::Must, key_query(&matches.snapshot, collection, id)),
            (Occur::Must, base.box_clone()),
        ]);
        let found = matches
            .snapshot
            .searcher()
            .search(&query, &Count)
            .map_err(|err| read_fault(err, "match a Document of the vector leg"))?;
        Ok(found > 0)
    }
}

/// The query that matches the one Tantivy document of a Document.
fn key_query(snapshot: &Snapshot, collection: &str, id: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        Term::from_field_text(snapshot.fields().key, &key_of(collection, id)),
        IndexRecordOption::Basic,
    ))
}

/// The reranker and the query it scores against.
struct Rerank {
    limit: usize,
    port: Arc<RerankerPort>,
    query: String,
}

/// A search with the lexical leg run and the vector leg planned: what is
/// left is the embedding of the query, the nearest-neighbour search, the
/// fusion, and the rerank.
pub(crate) struct Prepared {
    frame: Frame,
    depth: usize,
    lexical_weight: f64,
    vector_weight: f64,
    lexical: Vec<LegHit>,
    vector: Option<VectorPlan>,
    admit: Admit,
    rerank: Option<Rerank>,
}

/// A fused ranking, possibly reranked, ready to stream.
pub(crate) struct Ranked {
    frame: Frame,
    hits: Vec<Ranking>,
    rerank: Option<Rerank>,
    /// How many leading hits the reranker scores: set by [`Ranked::scoring`].
    scoring: usize,
}

/// The Documents a reranker scores, with the query.
pub(crate) struct Scoring {
    /// The reranker installed when the search began.
    pub(crate) port: Arc<RerankerPort>,
    /// The query text, which the reranker gets first.
    pub(crate) query: String,
    /// The Documents, with their `raw`, in the order of the hits.
    pub(crate) documents: Vec<Document>,
}

/// The text query of the lexical leg for `text`; `None` when it holds no
/// word the parser can use.
fn text_query(parser: &QueryParser, text: &str) -> Option<Box<dyn Query>> {
    if let Ok(query) = parser.parse_query(text) {
        return Some(query);
    }
    let plain: String = text
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
    let plain = plain.trim();
    if plain.is_empty() {
        return None;
    }
    parser.parse_query(plain).ok()
}

/// The first value of a stored text field of a Tantivy document.
fn stored_text(doc: &TantivyDocument, field: Field) -> Option<String> {
    doc.get_first(field)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

fn clamp(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The collections of a search, the Index at one state, and the query of
/// the Filter on each collection.
struct Reach {
    declarations: Arc<Declarations>,
    decls: Vec<Declaration>,
    snapshot: Arc<Snapshot>,
    /// The query of the Filter on each collection that is in the lexical
    /// Index.
    bases: HashMap<String, Box<dyn Query>>,
    /// The request has a Filter that is not `all`.
    filtered: bool,
}

/// The Document `like` names: its declaration, and its `fts` text.
struct Like {
    decl: Declaration,
    id: String,
    text: String,
}

/// Runs the lexical leg: the best `depth` Documents of `in_scope` that
/// `subject` matches, best first.
fn lexical_leg(
    snapshot: &Snapshot,
    in_scope: BooleanQuery,
    subject: Box<dyn Query>,
    exclude: Option<&(String, String)>,
    depth: usize,
) -> Result<Vec<LegHit>, Error> {
    let fields = snapshot.fields();
    let searcher = snapshot.searcher();
    let mut clauses = vec![
        (Occur::Must, subject),
        // The Filter narrows the candidates and adds no score.
        (
            Occur::Must,
            Box::new(ConstScoreQuery::new(Box::new(in_scope), 0.0)) as Box<dyn Query>,
        ),
    ];
    if let Some((collection, id)) = exclude {
        clauses.push((Occur::MustNot, key_query(snapshot, collection, id)));
    }
    let query = BooleanQuery::new(clauses);
    // `TopDocs` reserves its slots up front: ask for no more than exist.
    let depth = depth.min(clamp(searcher.num_docs()));
    if depth == 0 {
        return Ok(Vec::new());
    }
    let top = searcher
        .search(&query, &TopDocs::with_limit(depth).order_by_score())
        .map_err(|err| read_fault(err, "search the lexical index"))?;
    let mut hits = Vec::with_capacity(top.len());
    for (score, address) in top {
        let doc: TantivyDocument = searcher
            .doc(address)
            .map_err(|err| read_fault(err, "read a hit of the lexical index"))?;
        let (Some(collection), Some(id)) = (
            stored_text(&doc, fields.collection),
            stored_text(&doc, fields.id),
        ) else {
            return Err(Fault::new(
                VaultErrorCode::IndexBroken,
                "a Document of the lexical index has no collection or no id",
            )
            .into());
        };
        hits.push(LegHit {
            collection,
            id,
            score: f64::from(score),
        });
    }
    sort_leg(&mut hits);
    Ok(hits)
}

/// Best first; equal scores by collection, then id.
fn sort_leg(hits: &mut [LegHit]) {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.collection.cmp(&b.collection))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Runs the vector leg: the best `depth` Documents `admit` lets through
/// whose vectors are nearest to any of `queries`, best first. A Document
/// counts once, by its nearest vector.
fn vector_leg(
    snapshot: &vector::Snapshot,
    queries: &[&[f32]],
    admit: &Admit,
    depth: usize,
) -> Result<Vec<LegHit>, Error> {
    let total = snapshot.len();
    let mut best: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut verdicts: HashMap<(String, String), bool> = HashMap::new();
    for query in queries {
        let mut fetch = depth.min(total).max(1);
        loop {
            let found = snapshot.search(query, fetch)?;
            let returned = found.len();
            let mut seen: BTreeMap<(String, String), f64> = BTreeMap::new();
            for (slot, distance) in found {
                let key = (slot.collection.clone(), slot.id.clone());
                let allowed = match verdicts.get(&key) {
                    Some(allowed) => *allowed,
                    None => {
                        let allowed = admit.admits(&slot.collection, &slot.id)?;
                        verdicts.insert(key.clone(), allowed);
                        allowed
                    }
                };
                if !allowed {
                    continue;
                }
                let score = f64::from(1.0 / (1.0 + distance));
                let entry = seen.entry(key).or_insert(score);
                if score > *entry {
                    *entry = score;
                }
            }
            // The walk is done when it found enough, or there is no more.
            if seen.len() >= depth || returned < fetch || fetch >= total {
                for (key, score) in seen {
                    let entry = best.entry(key).or_insert(score);
                    if score > *entry {
                        *entry = score;
                    }
                }
                break;
            }
            fetch = fetch.saturating_mul(2).min(total);
        }
    }
    let mut hits: Vec<LegHit> = best
        .into_iter()
        .map(|((collection, id), score)| LegHit {
            collection,
            id,
            score,
        })
        .collect();
    sort_leg(&mut hits);
    hits.truncate(depth);
    Ok(hits)
}

/// Reciprocal Rank Fusion of the legs, best first, at most `depth` hits. A
/// leg of weight 0 adds nothing.
fn fuse(
    lexical: &[LegHit],
    lexical_weight: f64,
    vector: &[LegHit],
    vector_weight: f64,
    depth: usize,
) -> Vec<Ranking> {
    let mut fused: BTreeMap<(&str, &str), Ranking> = BTreeMap::new();
    let legs = [
        (lexical, lexical_weight, true),
        (vector, vector_weight, false),
    ];
    for (hits, weight, is_lexical) in legs {
        if weight <= 0.0 {
            continue;
        }
        for (rank, hit) in hits.iter().enumerate() {
            let entry = fused
                .entry((hit.collection.as_str(), hit.id.as_str()))
                .or_insert_with(|| Ranking {
                    collection: hit.collection.clone(),
                    id: hit.id.clone(),
                    lexical: None,
                    vector: None,
                    fused: 0.0,
                    rerank: None,
                });
            entry.fused += weight / (RRF_K + rank as f64 + 1.0);
            if is_lexical {
                entry.lexical = Some(hit.score);
            } else {
                entry.vector = Some(hit.score);
            }
        }
    }
    let mut ranked: Vec<Ranking> = fused.into_values().collect();
    // `BTreeMap` order is collection, then id: the stable sort keeps it
    // for equal scores.
    ranked.sort_by(|a, b| b.fused.total_cmp(&a.fused));
    ranked.truncate(depth);
    ranked
}

impl Vault {
    /// The first step of a search; see the module documentation.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a `rerank_limit` when the Vault has no
    /// reranker, for a Filter or an id that breaks a rule, and for a
    /// reranked `like` whose Document has no text to rerank by; `NOT_FOUND`
    /// for a collection that is not declared and for a `like` Document the
    /// Index does not hold; `UNIMPLEMENTED` for a named collection that is
    /// encrypted or leaves the lexical Index out; `INDEX_REBUILDING`
    /// (retryable) while the Index is not built; `INDEX_STALE` for a Vault
    /// whose Projector is of another projection than the Index, or a
    /// collection whose columns changed; `INDEX_BROKEN` for a damaged
    /// Index.
    pub(crate) fn prepare_search(&self, request: Request) -> Result<Prepared, Error> {
        let call = request.call;
        self.prepare(request).context(call)
    }

    fn prepare(&self, request: Request) -> Result<Prepared, Error> {
        let ports = self.ports();
        let reranker = ports.reranker();
        if request.rerank_limit.is_some() && reranker.is_none() {
            return Err(invalid(
                "rerank_limit is set, and the Vault has no reranker; give it one (Vault.SetReranker) or leave rerank_limit out",
            )
            .into());
        }
        let reach = self.reach(&request)?;
        let views = self.repos().views()?;
        let embedder = ports.embedder();
        let (summary, vector_snapshot) =
            self.vector_summary(&request, &reach, embedder.as_deref())?;
        let like = match &request.by {
            By::Like { collection, id } => {
                Some(self.like(&request, &reach, &views, collection, id)?)
            }
            By::Text(_) => None,
        };
        let exclude = like
            .as_ref()
            .map(|like| (like.decl.name().to_owned(), like.id.clone()));

        let depth = request
            .offset
            .saturating_add(request.k)
            .max(request.rerank_limit.unwrap_or(0))
            .max(DEPTH_FLOOR);
        let depth = clamp(depth);
        let lexical = if request.lexical_weight > 0.0 {
            self.lexical_hits(&request, &reach, like.as_ref(), exclude.as_ref(), depth)?
        } else {
            Vec::new()
        };
        let vector = vector_plan(&request, vector_snapshot, embedder, like.as_ref())?;
        let rerank = match (request.rerank_limit, reranker) {
            (Some(limit), Some(port)) => Some(Rerank {
                limit: clamp(limit),
                port,
                query: match (&request.by, like) {
                    (By::Text(text), _) => text.clone(),
                    (By::Like { .. }, Some(like)) if !like.text.is_empty() => like.text,
                    (By::Like { .. }, _) => {
                        return Err(invalid(
                            "the Document like names has no text to rerank by: the Vault has no Projector, or the Document has no fts column",
                        )
                        .into());
                    }
                },
            }),
            _ => None,
        };

        let names = reach
            .decls
            .iter()
            .map(|decl| decl.name().to_owned())
            .collect();
        let filtered = reach.filtered;
        let Reach {
            decls,
            snapshot,
            bases,
            ..
        } = reach;
        Ok(Prepared {
            frame: Frame {
                call: request.call,
                summary,
                k: request.k,
                offset: request.offset,
                include_raw: request.include_raw,
                views,
                decls: decls
                    .into_iter()
                    .map(|decl| (decl.name().to_owned(), decl))
                    .collect(),
            },
            depth,
            lexical_weight: request.lexical_weight,
            vector_weight: request.vector_weight,
            lexical,
            vector,
            admit: Admit {
                names,
                exclude,
                filter: filtered.then_some(Matches { snapshot, bases }),
            },
            rerank,
        })
    }

    /// The scope of `request`, the lexical Index it reads, and the Filter
    /// compiled for each collection.
    fn reach(&self, request: &Request) -> Result<Reach, Error> {
        let (declarations, mut decls) = self.declared_targets(&request.scope, Mode::Search)?;
        // A Filter is checked in the lexical Index, which holds nothing of a
        // collection that leaves it out: the default scope of a Vault search
        // drops such collections, and naming one is refused.
        let filtered = request
            .filter
            .as_ref()
            .is_some_and(|filter| !matches!(filter.kind, Some(filter::Kind::All(_))));
        if filtered {
            if matches!(&request.scope, Where::Vault(names) if names.is_empty()) {
                decls.retain(Declaration::lexical_indexed);
            } else if let Some(decl) = decls.iter().find(|decl| !decl.lexical_indexed()) {
                return Err(invalid(format!(
                    "the collection {} leaves the lexical Index out (exclude_lexical_index), and a filter is checked there; search it without a filter",
                    decl.name()
                ))
                .into());
            }
        }
        let mut also = Vec::new();
        if let By::Like { collection, id } = &request.by {
            let decl = declarations
                .get(collection)
                .ok_or_else(|| not_declared(collection))?;
            check_id(decl, id).context("check like")?;
            also.push(collection.as_str());
        }
        let snapshot = self.lexical_snapshot(&decls, &also)?;
        let scope = match &request.scope {
            Where::Collection(own) => Scope::Collection(own),
            Where::Vault(_) => Scope::Vault,
        };
        let all = Filter {
            kind: filter::All::default().into(),
            ..Filter::default()
        };
        let filter = request.filter.as_ref().unwrap_or(&all);
        if !decls.iter().any(Declaration::lexical_indexed) {
            // Nothing to match in, and a Filter that breaks a rule is still wrong.
            Compiler::new(&snapshot, &declarations, scope, None).compile(filter)?;
        }
        let fields = snapshot.fields();
        let mut bases: HashMap<String, Box<dyn Query>> = HashMap::new();
        for decl in decls.iter().filter(|decl| decl.lexical_indexed()) {
            let name = decl.name();
            let matches =
                Compiler::new(&snapshot, &declarations, scope, Some(name)).compile(filter)?;
            let in_collection = TermQuery::new(
                Term::from_field_text(fields.collection, name),
                IndexRecordOption::Basic,
            );
            let base = BooleanQuery::new(vec![
                (Occur::Must, Box::new(in_collection) as Box<dyn Query>),
                (Occur::Must, matches),
            ]);
            bases.insert(name.to_owned(), Box::new(base));
        }
        Ok(Reach {
            declarations,
            decls,
            snapshot,
            bases,
            filtered,
        })
    }

    /// Whether the vector leg runs, and if not why, with the vectors it
    /// would search. Known from the Vault and the Index before any leg runs.
    fn vector_summary(
        &self,
        request: &Request,
        reach: &Reach,
        embedder: Option<&EmbedderPort>,
    ) -> Result<(VectorLeg, Option<Arc<vector::Snapshot>>), Error> {
        if request.vector_weight <= 0.0 {
            return Ok((VectorLeg::VECTOR_LEG_NOT_ASKED, None));
        }
        let has_vector_collection = self.ports().projector().is_some()
            && reach.decls.iter().any(|decl| {
                decl.vector_indexed()
                    && reach
                        .snapshot
                        .columns(decl.name())
                        .is_some_and(|columns| columns.iter().any(|column| column.vector))
            });
        if !has_vector_collection {
            return Ok((VectorLeg::VECTOR_LEG_NO_VECTOR_COLLECTION, None));
        }
        Ok(match self.vectors().leg(embedder)? {
            Leg::NoEmbedder => (VectorLeg::VECTOR_LEG_NO_EMBEDDER, None),
            Leg::NoVectors => (VectorLeg::VECTOR_LEG_NO_VECTORS, None),
            Leg::ModelMismatch => (VectorLeg::VECTOR_LEG_MODEL_MISMATCH, None),
            Leg::Ran => (VectorLeg::VECTOR_LEG_RAN, self.vectors().snapshot()?),
        })
    }

    /// The Document `like` names: checked to be in the Index, and its `fts`
    /// text when a leg or the rerank needs it.
    ///
    /// The Index does not store that text, so the Projector of the Vault
    /// projects the Document again. Without a Projector the text is empty.
    fn like(
        &self,
        request: &Request,
        reach: &Reach,
        views: &Views,
        collection: &str,
        id: &str,
    ) -> Result<Like, Error> {
        let decl = reach
            .declarations
            .get(collection)
            .ok_or_else(|| not_declared(collection))?
            .clone();
        let held = if decl.lexical_indexed() {
            let query = key_query(&reach.snapshot, collection, id);
            reach
                .snapshot
                .searcher()
                .search(&query, &Count)
                .map_err(|err| read_fault(err, "look up the Document like names"))?
                > 0
        } else {
            self.vectors()
                .snapshot()?
                .is_some_and(|vectors| vectors.has(collection, id))
        };
        if !held {
            return Err(Fault::new(
                VaultErrorCode::NotFound,
                format!("the Index holds no Document {id} of the collection {collection}"),
            )
            .into());
        }
        let mut text = String::new();
        let wanted = request.lexical_weight > 0.0 || request.rerank_limit.is_some();
        if wanted
            && let Some(projector) = self.ports().projector()
            && let Some(columns) = reach.snapshot.columns(collection)
        {
            let opener = Opener::new(views.of(decl.repo()), &decl, true);
            let raw = opener
                .open(self, id.to_owned())?
                .ok_or_else(|| {
                    Fault::new(
                        VaultErrorCode::NotFound,
                        format!("the Document {id} of the collection {collection} has no file"),
                    )
                })?
                .raw
                .unwrap_or_default();
            if let Some(values) = projector.project(collection.to_owned(), id.to_owned(), raw)? {
                text = fts_text(columns, &values);
            }
        }
        Ok(Like {
            decl,
            id: id.to_owned(),
            text,
        })
    }

    /// The lexical leg of `request`.
    fn lexical_hits(
        &self,
        request: &Request,
        reach: &Reach,
        like: Option<&Like>,
        exclude: Option<&(String, String)>,
        depth: usize,
    ) -> Result<Vec<LegHit>, Error> {
        if reach.bases.is_empty() {
            return Ok(Vec::new());
        }
        let fields = reach.snapshot.fields();
        let subject: Option<Box<dyn Query>> = match (&request.by, like) {
            (By::Text(text), _) => {
                let parser = QueryParser::for_index(reach.snapshot.tantivy(), vec![fields.text]);
                text_query(&parser, text)
            }
            (By::Like { .. }, Some(like)) if !like.text.is_empty() => Some(Box::new(
                MoreLikeThisQuery::builder()
                    .with_min_doc_frequency(1)
                    .with_min_term_frequency(1)
                    .with_max_query_terms(30)
                    .with_document_fields(vec![(
                        fields.text,
                        vec![OwnedValue::Str(like.text.clone())],
                    )]),
            )),
            (By::Like { .. }, _) => None,
        };
        let Some(subject) = subject else {
            return Ok(Vec::new());
        };
        let in_scope = BooleanQuery::new(
            reach
                .bases
                .values()
                .map(|base| (Occur::Should, base.box_clone()))
                .collect(),
        );
        lexical_leg(&reach.snapshot, in_scope, subject, exclude, depth)
    }

    /// Runs a whole search on the thread of the caller: the steps of the
    /// module documentation in order, a blocking embedder and reranker
    /// called as they are.
    ///
    /// # Errors
    ///
    /// As [`Vault::prepare_search`]; the failure of the embedder or the
    /// reranker, which keeps its code; `INDEX_BROKEN` for a damaged Index.
    pub(crate) fn search(&self, request: Request) -> Result<SearchStream, Error> {
        let call = request.call;
        let prepared = self.prepare_search(request)?;
        let query = match prepared.embedding() {
            Some(embedding) => Some(
                embedding
                    .port
                    .embed_query(embedding.text.clone())
                    .context(call)?,
            ),
            None => None,
        };
        let mut ranked = prepared.rank(query)?;
        if let Some(scoring) = ranked.scoring(self)? {
            let scores = scoring
                .port
                .score(scoring.query, scoring.documents)
                .context(call)?;
            ranked.scored(scores)?;
        }
        Ok(ranked.into_stream())
    }
}

/// What the vector leg searches with, if it runs.
fn vector_plan(
    request: &Request,
    snapshot: Option<Arc<vector::Snapshot>>,
    embedder: Option<Arc<EmbedderPort>>,
    like: Option<&Like>,
) -> Result<Option<VectorPlan>, Error> {
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    Ok(match (&request.by, like) {
        (By::Text(text), _) => embedder.map(|port| VectorPlan {
            snapshot,
            probe: Probe::Embedded,
            embedding: Some(Embedding {
                port,
                text: text.clone(),
            }),
        }),
        (By::Like { .. }, Some(like)) => {
            let vectors = snapshot.vectors_of(like.decl.name(), &like.id)?;
            Some(VectorPlan {
                snapshot,
                probe: Probe::Stored(vectors),
                embedding: None,
            })
        }
        (By::Like { .. }, None) => None,
    })
}

impl Prepared {
    /// The text the embedder must turn into a vector for the vector leg,
    /// when the leg runs on a text: pass the vector to [`Prepared::rank`].
    pub(crate) fn embedding(&self) -> Option<&Embedding> {
        self.vector.as_ref()?.embedding.as_ref()
    }

    /// The nearest-neighbour search of the vector leg and the fusion of the
    /// legs. `query` is the vector of [`Prepared::embedding`], when it gave
    /// a text; a leg that gets no query finds nothing.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a vector of another length than the Index
    /// holds; `INDEX_BROKEN` when the vector index cannot be searched.
    pub(crate) fn rank(self, query: Option<Vec<f32>>) -> Result<Ranked, Error> {
        let call = self.frame.call;
        let vector = match &self.vector {
            Some(plan) if self.vector_weight > 0.0 => {
                let queries: Vec<&[f32]> = match &plan.probe {
                    Probe::Embedded => query.as_deref().into_iter().collect(),
                    Probe::Stored(vectors) => vectors.iter().map(Vec::as_slice).collect(),
                };
                vector_leg(&plan.snapshot, &queries, &self.admit, self.depth).context(call)?
            }
            _ => Vec::new(),
        };
        let hits = fuse(
            &self.lexical,
            self.lexical_weight,
            &vector,
            self.vector_weight,
            self.depth,
        );
        Ok(Ranked {
            frame: self.frame,
            hits,
            rerank: self.rerank,
            scoring: 0,
        })
    }
}

impl Ranked {
    /// Reads the Documents the reranker scores, when the search reranks:
    /// the first `rerank_limit` hits, with their `raw`. A hit whose file
    /// `HEAD` no longer has leaves the ranking. Pass the result to the
    /// reranker and its scores to [`Ranked::scored`].
    ///
    /// # Errors
    ///
    /// The failure of a read of a Document.
    pub(crate) fn scoring(&mut self, vault: &Vault) -> Result<Option<Scoring>, Error> {
        let call = self.frame.call;
        let Some(rerank) = self.rerank.take() else {
            return Ok(None);
        };
        let take = rerank.limit.min(self.hits.len());
        let tail = self.hits.split_off(take);
        let mut kept = Vec::with_capacity(take);
        let mut documents = Vec::with_capacity(take);
        for hit in std::mem::take(&mut self.hits) {
            let document = open(&self.frame, vault, &hit, true).context(call)?;
            if let Some(document) = document {
                documents.push(document);
                kept.push(hit);
            }
        }
        self.scoring = kept.len();
        kept.extend(tail);
        self.hits = kept;
        Ok(Some(Scoring {
            port: rerank.port,
            query: rerank.query,
            documents,
        }))
    }

    /// Puts the scores of the reranker in, one per Document of
    /// [`Ranked::scoring`] in its order, and orders those hits by them.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the scores are not as many as the Documents.
    pub(crate) fn scored(&mut self, scores: Vec<f64>) -> Result<(), Error> {
        if scores.len() != self.scoring {
            return Err(Error::from(invalid(format!(
                "the reranker gave {} scores for {} Documents",
                scores.len(),
                self.scoring
            )))
            .context(self.frame.call));
        }
        for (hit, score) in self.hits.iter_mut().zip(scores) {
            hit.rerank = Some(score);
        }
        let scored = self.scoring.min(self.hits.len());
        if let Some(head) = self.hits.get_mut(..scored) {
            head.sort_by(|a, b| {
                let (a, b) = (a.rerank.unwrap_or_default(), b.rerank.unwrap_or_default());
                b.total_cmp(&a)
            });
        }
        Ok(())
    }

    /// The stream of the search: its summary, then its hits from `offset` on,
    /// at most `k`, best first. No file is opened here.
    pub(crate) fn into_stream(self) -> SearchStream {
        let skip = clamp(self.frame.offset);
        let take = clamp(self.frame.k);
        let hits: VecDeque<Ranking> = self.hits.into_iter().skip(skip).take(take).collect();
        SearchStream {
            frame: self.frame,
            summary: true,
            hits,
            done: false,
        }
    }
}

/// The Document of a hit, or `None` when `HEAD` has no such file (the Index
/// is behind). With `raw` it holds the bytes, whatever the search asked.
fn open(frame: &Frame, vault: &Vault, hit: &Ranking, raw: bool) -> Result<Option<Document>, Error> {
    let Some(decl) = frame.decls.get(&hit.collection) else {
        return Ok(None);
    };
    let snap = frame.views.of(decl.repo());
    Opener::new(snap, decl, raw || frame.include_raw).open(vault, hit.id.clone())
}

/// A lazy stream of the items of a search. Pull it with
/// [`next`](Self::next), giving the Vault it was made by.
pub(crate) struct SearchStream {
    frame: Frame,
    /// The summary is still to be handed out.
    summary: bool,
    hits: VecDeque<Ranking>,
    done: bool,
}

impl SearchStream {
    /// The next item, `None` at the end. A failure is the last item.
    pub(crate) fn next(&mut self, vault: &Vault) -> Option<Result<SearchItem, Error>> {
        if self.done {
            return None;
        }
        if self.summary {
            self.summary = false;
            let summary = SearchSummary::default().with_vector_leg(self.frame.summary);
            return Some(Ok(SearchItem {
                item: Some(Item::from(summary)),
                ..SearchItem::default()
            }));
        }
        loop {
            let Some(hit) = self.hits.pop_front() else {
                self.done = true;
                return None;
            };
            match open(&self.frame, vault, &hit, false) {
                Ok(Some(document)) => return Some(Ok(item_of(document, &hit))),
                Ok(None) => {}
                Err(error) => {
                    self.done = true;
                    return Some(Err(
                        error.context(format!("{}: read a Document", self.frame.call))
                    ));
                }
            }
        }
    }
}

/// The hit item for `document` with the scores of `hit`.
fn item_of(document: Document, hit: &Ranking) -> SearchItem {
    let mut out = SearchHit::default().with_score(hit.fused);
    out.document = document.into();
    out.lexical_score = hit.lexical;
    out.vector_score = hit.vector;
    out.rerank_score = hit.rerank;
    SearchItem {
        item: Some(Item::from(out)),
        ..SearchItem::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use scheda_api::v1::projector_ref::ProjectorRef;
    use scheda_api::v1::{
        CollectionDeclaration, CollectionKey, ColumnKind, DocumentDeletion, DocumentUpsert,
        IndexColumn, IndexDoc, IndexSchema, IndexValue, OpenOptions, ProjectorDescriptor,
        ProjectorSync, RawDocument, VaultErrorCode, index_value,
    };

    use super::*;
    use crate::commit::hooks;
    use crate::index::State;
    use crate::ports::Ports;
    use crate::repo::{Repos, blob_reads};

    /// A Projector that searches the bytes of a Document as text.
    struct Text;

    impl ProjectorSync for Text {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("text@1"))
        }

        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema {
                columns: vec![
                    IndexColumn::default()
                        .with_name("text")
                        .with_kind(ColumnKind::COLUMN_KIND_TEXT)
                        .with_fts(true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let text = String::from_utf8_lossy(document.raw.as_deref().unwrap_or(&[])).into_owned();
            let mut doc = IndexDoc::default();
            doc.values.insert(
                "text".to_owned(),
                IndexValue {
                    value: Some(index_value::Value::Text(text)),
                    ..IndexValue::default()
                },
            );
            Ok(doc)
        }
    }

    /// A Vault on disk made by `Vault::open`, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-search-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }

        fn options() -> OpenOptions {
            OpenOptions::default()
                .with_write_timeout_ms(2000)
                .with_index_poll_ms(10)
        }

        /// A handle that indexes: its Projector says what is searched.
        fn indexing(&self) -> Vault {
            let ports = Ports {
                projector: Some(ProjectorRef::from_sync(Text)),
                ..Ports::default()
            };
            Vault::open(&self.0, Scratch::options(), ports).unwrap()
        }

        /// A handle that only reads: it has no port.
        fn reading(&self) -> Vault {
            Vault::open(&self.0, Scratch::options(), Ports::default()).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn declare(vault: &Vault, name: &str) {
        let collection: CollectionDeclaration = serde_json::from_value(serde_json::json!({
            "name": name, "schema": {"standard": {}}
        }))
        .unwrap();
        vault.create_collection(collection).unwrap();
    }

    fn upsert(id: &str) -> DocumentUpsert {
        DocumentUpsert::default()
            .with_id(id)
            .with_raw(format!("alpha {id}").into_bytes())
            .with_note("test")
    }

    /// Waits until the Index of `scratch` reflects `HEAD`.
    fn wait_indexed(scratch: &Scratch, vault: &Vault) {
        let head = State::of(
            &Repos::new(&scratch.0.canonicalize().unwrap())
                .views()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(snapshot) = vault.lexical().snapshot(&["notes"])
                && snapshot.state() == head
            {
                return;
            }
            assert!(Instant::now() < deadline, "the Index did not reach HEAD");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn request(json: serde_json::Value) -> Request {
        let search: DocumentSearch = serde_json::from_value(json).unwrap();
        Request::of_collection("Collection.Search", "notes", search).unwrap()
    }

    /// The ids of the hits, in the order of the stream, after the summary.
    fn ids(vault: &Vault, request: Request) -> Vec<String> {
        let mut stream = vault.search(request).unwrap();
        let mut ids = Vec::new();
        while let Some(item) = stream.next(vault) {
            if let Some(Item::Hit(hit)) = item.unwrap().item {
                let document = hit.document.as_option();
                ids.push(document.and_then(|d| d.id.clone()).unwrap_or_default());
            }
        }
        ids
    }

    fn filled(name: &str, count: usize) -> (Scratch, Vault) {
        let scratch = Scratch::new(name);
        let vault = scratch.indexing();
        declare(&vault, "notes");
        for n in 0..count {
            vault.upsert("notes", upsert(&format!("d{n:02}"))).unwrap();
        }
        wait_indexed(&scratch, &vault);
        (scratch, vault)
    }

    #[test]
    fn a_stream_opens_one_file_for_each_hit_it_reaches_and_none_before() {
        let (_scratch, vault) = filled("lazy", 30);
        let search = || request(serde_json::json!({"text": "alpha", "k": 20, "include_raw": true}));

        // The ranking is made, and no file is opened for it.
        let before = blob_reads();
        let mut stream = vault.search(search()).unwrap();
        assert_eq!(blob_reads() - before, 0, "the ranking opens no Document");

        // The first item is the summary and opens nothing.
        let first = stream.next(&vault).unwrap().unwrap();
        assert!(matches!(first.item, Some(Item::Summary(_))));
        assert_eq!(blob_reads() - before, 0);

        // A hit opens its file as the stream reaches it.
        let hit = stream.next(&vault).unwrap().unwrap();
        assert!(matches!(hit.item, Some(Item::Hit(_))));
        assert_eq!(blob_reads() - before, 1, "one file for the first hit");

        // A stream dropped there reads no more.
        drop(stream);
        assert_eq!(blob_reads() - before, 1);

        // Without `include_raw` the keys come from the tree: no blob at all.
        let before = blob_reads();
        let bare = request(serde_json::json!({"text": "alpha", "k": 20}));
        assert_eq!(ids(&vault, bare).len(), 20);
        assert_eq!(blob_reads() - before, 0);
    }

    #[test]
    fn offset_skips_hits_and_opens_none_of_their_files() {
        let (_scratch, vault) = filled("offset", 30);
        let everything = ids(
            &vault,
            request(serde_json::json!({"text": "alpha", "k": 30, "include_raw": true})),
        );
        assert_eq!(everything.len(), 30);

        let before = blob_reads();
        let page = ids(
            &vault,
            request(serde_json::json!({
                "text": "alpha", "k": 10, "offset": 25, "include_raw": true
            })),
        );
        assert_eq!(page, everything[25..], "the last five hits");
        assert_eq!(blob_reads() - before, 5, "no file of a skipped hit");

        let before = blob_reads();
        let none = ids(
            &vault,
            request(serde_json::json!({
                "text": "alpha", "offset": 100, "include_raw": true
            })),
        );
        assert!(none.is_empty());
        assert_eq!(blob_reads() - before, 0);
    }

    #[test]
    fn a_write_that_has_not_returned_is_seen_by_no_search() {
        let scratch = Scratch::new("not-returned");
        let vault = scratch.indexing();
        declare(&vault, "notes");
        vault.upsert("notes", upsert("old.md")).unwrap();
        wait_indexed(&scratch, &vault);
        let other = scratch.reading();
        // The call stops after the list of its paths, before the swap on
        // `HEAD`, as a kill would.
        hooks::stop_after_list();
        let error = vault.upsert("notes", upsert("new.md")).unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        std::thread::sleep(Duration::from_millis(100));
        for handle in [&vault, &other] {
            let found = ids(handle, request(serde_json::json!({"text": "alpha"})));
            assert_eq!(found, ["old.md"]);
        }
    }

    #[test]
    fn a_hit_the_index_holds_for_a_file_head_no_longer_has_is_skipped() {
        let scratch = Scratch::new("behind");
        let vault = scratch.indexing();
        declare(&vault, "notes");
        for id in ["a.md", "b.md", "c.md"] {
            vault.upsert("notes", upsert(id)).unwrap();
        }
        wait_indexed(&scratch, &vault);
        // The indexer ends, so the Index stays at the state before the
        // delete while `HEAD` moves on.
        vault.close().unwrap();
        vault
            .delete(
                "notes",
                DocumentDeletion::default()
                    .with_id("b.md")
                    .with_note("test"),
            )
            .unwrap();
        for raw in [false, true] {
            let search = request(serde_json::json!({"text": "alpha", "include_raw": raw}));
            let mut found = ids(&vault, search);
            found.sort();
            assert_eq!(found, ["a.md", "c.md"]);
        }
    }
}
