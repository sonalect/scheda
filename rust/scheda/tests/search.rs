//! C9: hybrid search, edges, incremental Index (SCH-9–11, IDX-3, IDX-6–9, QRY-3–6, REL-1–3).
//! Collection names here and in `testdata/search/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

mod common;

use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use scheda::{
    Code, Embedder, Filter, FindQuery, OpenOptions, Record, Reranker, SearchQuery, Vault,
};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-c9", n),
        }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn write_declaration(&self, file: &str, body: &str) {
        let name = peek_schema_name(body);
        let dir = self.vault().join(name);
        fs::create_dir_all(&dir).unwrap();
        let dest = if file.ends_with(".json") {
            ".collection.json"
        } else {
            ".collection.yaml"
        };
        fs::write(dir.join(dest), body).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const TOKENS: &str = include_str!("../testdata/search/tokens.yaml");
const TOKEN_EXTRA: &str = "\
version: 1
name: token_extra
schema:
  journal: {}
";
const WALK_SRC: &str = include_str!("../testdata/search/walk_src.yaml");
const WALK_DST: &str = include_str!("../testdata/search/walk_dst.yaml");

struct MapEmbedder {
    pairs: Vec<(String, Vec<f32>)>,
}

impl Embedder for MapEmbedder {
    fn model_id(&self) -> &str {
        "c9-stub"
    }

    fn dimensions(&self) -> NonZeroUsize {
        NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, scheda::Error> {
        for (key, vector) in &self.pairs {
            if text == key {
                return Ok(vector.clone());
            }
        }
        Ok(vec![0.0, 1.0, 0.0, 0.0])
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        texts.iter().map(|text| self.embed(text)).collect()
    }
}

fn rec(fields: Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn rec_rep(fields: Value) -> Vec<u8> {
    rec(fields)
}

fn slot_schema(name: &str, id: &str) -> String {
    format!(
        "version: 1
name: {name}
schema:
  singleton:
    id: {id}
"
    )
}

fn drop_journal_id(vault: &Path, collection: &str, id: &str) {
    let dir = vault.join(collection);
    let entries = fs::read_dir(&dir).unwrap();
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        let mut out = String::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let Some((line_id, _)) = line.split_once(": ") else {
                continue;
            };
            if line_id == id {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        fs::write(path, out).unwrap();
    }
}

fn filter(value: Value) -> FindQuery {
    serde_json::from_value::<Filter>(value).unwrap().into()
}

fn q(text: &str, k: u32) -> SearchQuery {
    SearchQuery::default().with_text(text).with_k(k)
}

fn hit_ids(hits: &scheda::SearchHits) -> Vec<&str> {
    hits.hits
        .iter()
        .filter_map(|hit| hit.id.as_deref())
        .collect()
}

fn search_projector() -> Arc<dyn scheda::Projector> {
    common::json_text_projector(&["title", "tag", "target", "extra"])
}

fn open_proj(path: impl AsRef<Path>, embedder: Option<Arc<dyn Embedder>>) -> Vault {
    Vault::open_with(path, OpenOptions::new(), embedder, Some(search_projector())).unwrap()
}

fn open_tokens(scratch: &Scratch, embedder: Option<Arc<dyn Embedder>>) -> Vault {
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("tokens.yaml", TOKENS);
    open_proj(scratch.vault(), embedder)
}

#[test]
fn token_in_marked_field_ranks_and_unmarked_is_absent() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "hit",
            rec(json!({"title": "kryptonite mineral", "aside": "plain", "tag": "red"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "aside_only",
            rec(json!({"title": "ordinary rock", "aside": "kryptonite mention", "tag": "blue"})),
            None,
        )
        .unwrap();
    for i in 0..8 {
        collection
            .add(
                &format!("d{i}"),
                rec(json!({"title": format!("distractor {i}"), "tag": "blue"})),
                None,
            )
            .unwrap();
    }
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"hit"), "marked field must rank: {ids:?}");
    assert!(
        !ids.contains(&"aside_only"),
        "unmarked field must not be a hit: {ids:?}"
    );
    assert!(hits.hits[0].lexical_score.is_some());
    assert!(hits.hits[0].vector_score.is_none());
}

#[test]
fn no_projector_skips_embedder_and_search_slots() {
    let scratch = Scratch::new();
    let embedder = CountingEmbedder::new("count");
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_declaration("tokens.yaml", TOKENS);
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
    )
    .unwrap();
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "hit",
            rec(json!({"title": "kryptonite mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    assert_eq!(embedder.slots(), 0, "no projector must not call embedder");
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    assert!(
        hit_ids(&hits).is_empty(),
        "no projector has no FTS slots: {ids:?}",
        ids = hit_ids(&hits)
    );
    let vector = collection
        .search(&q("kryptonite", 5).with_lexical_weight(0.0), None)
        .unwrap();
    assert!(
        hit_ids(&vector).is_empty(),
        "no projector has no vector slots: {ids:?}",
        ids = hit_ids(&vector)
    );
}

#[test]
fn concurrent_search_on_one_handle() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "hit",
            rec(json!({"title": "kryptonite mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "miss",
            rec(json!({"title": "ordinary rock", "tag": "blue"})),
            None,
        )
        .unwrap();
    let vault = Arc::new(vault);
    thread::scope(|scope| {
        for _ in 0..8 {
            let vault = Arc::clone(&vault);
            scope.spawn(move || {
                let collection = vault.collection("token_notes").unwrap();
                let hits = collection.search(&q("kryptonite", 5), None).unwrap();
                let ids = hit_ids(&hits);
                assert!(ids.contains(&"hit"), "marked field must rank: {ids:?}");
                assert!(
                    !ids.contains(&"miss"),
                    "unrelated title must not hit: {ids:?}"
                );
            });
        }
    });
}

#[test]
fn remove_leaves_search() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "hit",
            rec(json!({"title": "kryptonite mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "keep",
            rec(json!({"title": "kryptonite also", "tag": "red"})),
            None,
        )
        .unwrap();
    drop_journal_id(&scratch.vault(), "token_notes", "hit");
    vault.reindex().unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(!ids.contains(&"hit"), "removed id still a hit: {ids:?}");
    assert!(ids.contains(&"keep"), "kept id missing: {ids:?}");
}

#[test]
fn vault_search_scopes_collections() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("tokens.yaml", TOKENS);
    scratch.write_declaration("extra.yaml", TOKEN_EXTRA);
    let vault = open_proj(scratch.vault(), None);
    vault
        .collection("token_notes")
        .unwrap()
        .add(
            "keep",
            rec(json!({"title": "kryptonite mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    vault
        .collection("token_extra")
        .unwrap()
        .add("other", rec(json!({"title": "kryptonite extra"})), None)
        .unwrap();
    let all = vault.search(&[], &q("kryptonite", 10), None).unwrap();
    let mut all_ids = hit_ids(&all);
    all_ids.sort_unstable();
    assert_eq!(all_ids, ["keep", "other"]);
    let mut names: Vec<_> = all
        .hits
        .iter()
        .map(|hit| {
            (
                hit.id.as_deref().unwrap(),
                hit.collection.as_deref().unwrap(),
            )
        })
        .collect();
    names.sort_unstable();
    assert_eq!(names, [("keep", "token_notes"), ("other", "token_extra")]);
    assert_eq!(
        hit_ids(
            &vault
                .search(&["token_notes"], &q("kryptonite", 10), None)
                .unwrap()
        ),
        ["keep"]
    );
}

#[test]
fn vault_search_bare_projector_filter_is_invalid() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("tokens.yaml", TOKENS);
    scratch.write_declaration("extra.yaml", TOKEN_EXTRA);
    let vault = open_proj(scratch.vault(), None);
    let query: SearchQuery = serde_json::from_value(json!({
        "text": "kryptonite",
        "k": 5,
        "filter": {"term":{"field":"tag","value":"red"}}
    }))
    .unwrap();
    let err = vault.search(&[], &query, None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn body_fts_is_indexed_when_marked() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "bodyhit",
            rec(json!({"title": "kryptonite in the body"})),
            None,
        )
        .unwrap();
    collection
        .add("other", rec(json!({"title": "quiet too"})), None)
        .unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"bodyhit"), "{ids:?}");
}

#[test]
fn synonym_with_no_shared_tokens_hits_vector_leg() {
    let scratch = Scratch::new();
    let embedder = Arc::new(MapEmbedder {
        pairs: vec![
            (String::from("car"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("automobile"), vec![1.0, 0.0, 0.0, 0.0]),
        ],
    });
    let vault = open_tokens(&scratch, Some(embedder));
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("vehicle", rec(json!({"title": "car"})), None)
        .unwrap();
    collection
        .add("other", rec(json!({"title": "banana"})), None)
        .unwrap();
    let hits = collection
        .search(&q("automobile", 5).with_lexical_weight(0.0), None)
        .unwrap();
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"vehicle"), "{ids:?}");
    let hit = hits
        .hits
        .iter()
        .find(|h| h.id.as_deref() == Some("vehicle"));
    assert!(hit.unwrap().vector_score.is_some());
    assert!(hit.unwrap().lexical_score.is_none());
}

#[test]
fn negative_weight_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    let err = collection
        .search(&q("kryptonite", 5).with_lexical_weight(-1.0), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn hybrid_hit_carries_both_scores() {
    let scratch = Scratch::new();
    let embedder = Arc::new(MapEmbedder {
        pairs: vec![
            (String::from("kryptonite mineral"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("kryptonite"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("car"), vec![1.0, 0.0, 0.0, 0.0]),
        ],
    });
    let vault = open_tokens(&scratch, Some(embedder));
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("both", rec(json!({"title": "kryptonite mineral"})), None)
        .unwrap();
    collection
        .add("vector_only", rec(json!({"title": "car"})), None)
        .unwrap();
    collection
        .add("noise", rec(json!({"title": "unrelated prose"})), None)
        .unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"both"), "{ids:?}");
    let both = hits
        .hits
        .iter()
        .find(|hit| hit.id.as_deref() == Some("both"))
        .unwrap();
    assert!(both.lexical_score.is_some());
    assert!(both.vector_score.is_some());
    assert!(both.rrf_score.is_some());
    assert!(both.rerank_score.is_none());
}

struct PreferIdReranker(String);

impl Reranker for PreferIdReranker {
    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn score(&self, _query: &str, record: &Record) -> Result<f64, scheda::Error> {
        Ok(if record.id.as_deref() == Some(self.0.as_str()) {
            10.0
        } else {
            0.0
        })
    }

    fn score_many(&self, query: &str, records: &[&Record]) -> Result<Vec<f64>, scheda::Error> {
        records
            .iter()
            .map(|record| self.score(query, record))
            .collect()
    }
}

struct ScoreByIdReranker {
    scored: AtomicU64,
}

impl ScoreByIdReranker {
    fn new() -> Self {
        Self {
            scored: AtomicU64::new(0),
        }
    }

    fn scored(&self) -> u64 {
        self.scored.load(Ordering::Relaxed)
    }
}

fn id_rank(id: &str) -> f64 {
    id.strip_prefix('d')
        .and_then(|n| n.parse::<f64>().ok())
        .unwrap_or(0.0)
}

impl Reranker for ScoreByIdReranker {
    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn score(&self, _query: &str, record: &Record) -> Result<f64, scheda::Error> {
        self.scored.fetch_add(1, Ordering::Relaxed);
        Ok(record.id.as_deref().map(id_rank).unwrap_or(0.0))
    }

    fn score_many(&self, query: &str, records: &[&Record]) -> Result<Vec<f64>, scheda::Error> {
        records
            .iter()
            .map(|record| self.score(query, record))
            .collect()
    }
}

struct CollectIdsReranker {
    ids: Mutex<Vec<String>>,
}

impl CollectIdsReranker {
    fn new() -> Self {
        Self {
            ids: Mutex::new(Vec::new()),
        }
    }

    fn ids(&self) -> Vec<String> {
        self.ids.lock().unwrap().clone()
    }
}

impl Reranker for CollectIdsReranker {
    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn score(&self, _query: &str, record: &Record) -> Result<f64, scheda::Error> {
        if let Some(id) = record.id.as_deref() {
            self.ids.lock().unwrap().push(id.to_owned());
        }
        Ok(0.0)
    }

    fn score_many(&self, query: &str, records: &[&Record]) -> Result<Vec<f64>, scheda::Error> {
        records
            .iter()
            .map(|record| self.score(query, record))
            .collect()
    }
}

fn open_disagreeing_legs(scratch: &Scratch) -> scheda::Collection {
    let embedder = Arc::new(MapEmbedder {
        pairs: vec![
            (String::from("kryptonite"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("car"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("kryptonite mineral"), vec![0.5, 0.5, 0.0, 0.0]),
            (
                String::from("kryptonite exclusive"),
                vec![0.0, 0.0, 1.0, 0.0],
            ),
        ],
    });
    let vault = open_tokens(scratch, Some(embedder));
    let collection = vault.collection("token_notes").unwrap();
    vault.begin().unwrap();
    collection
        .add("aaa_vec", rec(json!({"title": "car"})), None)
        .unwrap();
    collection
        .add(
            "zzz_lex",
            rec(json!({"title": "kryptonite exclusive"})),
            None,
        )
        .unwrap();
    add_numbered_kryptonite(&collection, 49);
    vault.commit("disagreeing legs").unwrap();
    collection
}

fn add_numbered_kryptonite(collection: &scheda::Collection, n: usize) {
    for i in 0..n {
        collection
            .add(
                &format!("d{i:02}"),
                rec(json!({"title": "kryptonite mineral"})),
                None,
            )
            .unwrap();
    }
}

fn fill_kryptonite(vault: &Vault, n: usize) -> scheda::Collection {
    let collection = vault.collection("token_notes").unwrap();
    vault.begin().unwrap();
    add_numbered_kryptonite(&collection, n);
    vault.commit("numbered kryptonite").unwrap();
    collection
}

fn assert_invalid_rerank_limit(err: &scheda::Error, limit: u32, k: usize) {
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let message = err.message.as_deref().unwrap_or("");
    assert!(
        message.contains(&limit.to_string()),
        "message should name rerank_limit {limit}: {message}"
    );
    assert!(
        message.contains(&k.to_string()),
        "message should name k {k}: {message}"
    );
}

#[test]
fn reranker_reorders_and_sets_score() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("first", rec(json!({"title": "kryptonite mineral"})), None)
        .unwrap();
    collection
        .add(
            "second",
            rec(json!({"title": "kryptonite extra mention"})),
            None,
        )
        .unwrap();
    let fused = collection.search(&q("kryptonite", 2), None).unwrap();
    assert!(fused.hits.iter().all(|hit| hit.rerank_score.is_none()));
    let last = fused.hits.last().and_then(|hit| hit.id.clone()).unwrap();
    let reranked = collection
        .search(&q("kryptonite", 2), Some(&PreferIdReranker(last.clone())))
        .unwrap();
    assert_eq!(hit_ids(&reranked)[0], last.as_str());
    assert!(reranked.hits.iter().all(|hit| hit.rerank_score.is_some()));
}

#[test]
fn unset_rerank_limit_scores_the_whole_fused_pool() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    add_numbered_kryptonite(&collection, 20);
    let fused = collection.search(&q("kryptonite", 20), None).unwrap();
    assert_eq!(fused.hits.len(), 20);
    let reranker = ScoreByIdReranker::new();
    let page = collection
        .search(&q("kryptonite", 3), Some(&reranker))
        .unwrap();
    assert_eq!(reranker.scored(), fused.hits.len() as u64);
    assert_eq!(page.hits.len(), 3);
    assert!(page.hits.iter().all(|hit| hit.rerank_score.is_some()));
}

#[test]
fn rerank_limit_scores_only_the_truncated_fused_prefix() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    add_numbered_kryptonite(&collection, 20);
    let fused = collection.search(&q("kryptonite", 20), None).unwrap();
    assert_eq!(fused.hits.len(), 20);
    let window = 5;
    let mut expected: Vec<&str> = fused
        .hits
        .iter()
        .take(window)
        .filter_map(|hit| hit.id.as_deref())
        .collect();
    expected.sort_by(|a, b| {
        id_rank(b)
            .partial_cmp(&id_rank(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });
    expected.truncate(3);
    let reranker = ScoreByIdReranker::new();
    let page = collection
        .search(
            &q("kryptonite", 3).with_rerank_limit(window as u32),
            Some(&reranker),
        )
        .unwrap();
    assert_eq!(reranker.scored(), window as u64);
    assert_eq!(page.hits.len(), 3);
    assert!(page.hits.iter().all(|hit| hit.rerank_score.is_some()));
    assert_eq!(hit_ids(&page), expected);
    let outside = fused.hits[10].id.as_deref().unwrap();
    assert!(
        !hit_ids(&page).contains(&outside),
        "id outside the rerank window should be dropped: {outside}"
    );
}

#[test]
fn rerank_limit_zero_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    add_numbered_kryptonite(&collection, 3);
    let err = collection
        .search(&q("kryptonite", 3).with_rerank_limit(0), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let err = collection
        .search(
            &q("kryptonite", 3).with_rerank_limit(0),
            Some(&ScoreByIdReranker::new()),
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn rerank_limit_below_k_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    add_numbered_kryptonite(&collection, 20);
    let err = collection
        .search(
            &q("kryptonite", 3).with_rerank_limit(2),
            Some(&ScoreByIdReranker::new()),
        )
        .unwrap_err();
    assert_invalid_rerank_limit(&err, 2, 3);
    let err = collection
        .search(
            &SearchQuery::default()
                .with_text("kryptonite")
                .with_rerank_limit(9),
            Some(&ScoreByIdReranker::new()),
        )
        .unwrap_err();
    assert_invalid_rerank_limit(&err, 9, 10);
    let err = collection
        .search(
            &SearchQuery::default()
                .with_text("kryptonite")
                .with_k(0)
                .with_rerank_limit(9),
            Some(&ScoreByIdReranker::new()),
        )
        .unwrap_err();
    assert_invalid_rerank_limit(&err, 9, 10);
}

#[test]
fn rerank_limit_below_k_is_invalid_when_search_would_return_nothing() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    add_numbered_kryptonite(&collection, 20);
    let empty = collection
        .search(&q("zzzz-no-such-token", 3), None)
        .unwrap();
    assert!(empty.hits.is_empty());
    let err = collection
        .search(
            &q("zzzz-no-such-token", 3).with_rerank_limit(2),
            Some(&ScoreByIdReranker::new()),
        )
        .unwrap_err();
    assert_invalid_rerank_limit(&err, 2, 3);
}

#[test]
fn rerank_limit_larger_than_the_pool_matches_unset() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    add_numbered_kryptonite(&collection, 20);
    let unset = ScoreByIdReranker::new();
    let full = collection
        .search(&q("kryptonite", 3), Some(&unset))
        .unwrap();
    let over = ScoreByIdReranker::new();
    let capped = collection
        .search(&q("kryptonite", 3).with_rerank_limit(100), Some(&over))
        .unwrap();
    assert_eq!(unset.scored(), over.scored());
    assert_eq!(hit_ids(&full), hit_ids(&capped));
    assert_eq!(
        full.hits
            .iter()
            .map(|hit| hit.rerank_score)
            .collect::<Vec<_>>(),
        capped
            .hits
            .iter()
            .map(|hit| hit.rerank_score)
            .collect::<Vec<_>>()
    );
}

#[test]
fn rerank_limit_without_reranker_is_ignored() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    add_numbered_kryptonite(&collection, 20);
    let plain = collection.search(&q("kryptonite", 3), None).unwrap();
    let ignored = collection
        .search(&q("kryptonite", 3).with_rerank_limit(2), None)
        .unwrap();
    assert_eq!(hit_ids(&plain), hit_ids(&ignored));
    assert!(ignored.hits.iter().all(|hit| hit.rerank_score.is_none()));
    assert_eq!(ignored.hits.len(), 3);
}

#[test]
fn unset_rerank_limit_on_a_large_corpus_scores_the_fusion_floor() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = fill_kryptonite(&vault, 80);
    let reranker = ScoreByIdReranker::new();
    let page = collection
        .search(&q("kryptonite", 10), Some(&reranker))
        .unwrap();
    assert_eq!(reranker.scored(), 50);
    assert_eq!(page.hits.len(), 10);
    assert!(page.hits.iter().all(|hit| hit.rerank_score.is_some()));
}

#[test]
fn rerank_limit_at_or_below_the_floor_does_not_deepen_the_pool() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = fill_kryptonite(&vault, 80);
    let below = ScoreByIdReranker::new();
    let below_page = collection
        .search(&q("kryptonite", 10).with_rerank_limit(30), Some(&below))
        .unwrap();
    assert_eq!(below.scored(), 30);
    assert_eq!(below_page.hits.len(), 10);

    let at_floor = ScoreByIdReranker::new();
    let at_floor_page = collection
        .search(&q("kryptonite", 10).with_rerank_limit(50), Some(&at_floor))
        .unwrap();
    assert_eq!(at_floor.scored(), 50);
    assert_eq!(at_floor_page.hits.len(), 10);

    let unset = ScoreByIdReranker::new();
    let unset_page = collection
        .search(&q("kryptonite", 10), Some(&unset))
        .unwrap();
    assert_eq!(unset.scored(), 50);
    assert_eq!(hit_ids(&at_floor_page), hit_ids(&unset_page));
}

#[test]
fn rerank_limit_above_the_floor_hands_that_many_to_the_reranker() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = fill_kryptonite(&vault, 80);
    let fused = collection.search(&q("kryptonite", 80), None).unwrap();
    assert_eq!(fused.hits.len(), 80);
    let deep_id = fused.hits[59].id.clone().unwrap();

    let floor = ScoreByIdReranker::new();
    let floor_page = collection
        .search(&q("kryptonite", 10), Some(&floor))
        .unwrap();
    assert_eq!(floor.scored(), 50);
    assert!(
        !hit_ids(&floor_page).contains(&deep_id.as_str()),
        "id at fused rank 60 must sit outside the default floor: {deep_id}"
    );

    let deep = ScoreByIdReranker::new();
    let page = collection
        .search(&q("kryptonite", 10).with_rerank_limit(80), Some(&deep))
        .unwrap();
    assert_eq!(
        deep.scored(),
        80,
        "reranker must be handed 80 candidates, not the floor of 50"
    );
    assert_eq!(page.hits.len(), 10);
    assert!(page.hits.iter().all(|hit| hit.rerank_score.is_some()));

    let rescued = collection
        .search(
            &q("kryptonite", 10).with_rerank_limit(80),
            Some(&PreferIdReranker(deep_id.clone())),
        )
        .unwrap();
    assert_eq!(hit_ids(&rescued)[0], deep_id.as_str());
}

#[test]
fn rerank_limit_beyond_the_corpus_scores_everything_without_error() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = fill_kryptonite(&vault, 20);
    let reranker = ScoreByIdReranker::new();
    let page = collection
        .search(
            &q("kryptonite", 10).with_rerank_limit(10_000),
            Some(&reranker),
        )
        .unwrap();
    assert_eq!(reranker.scored(), 20);
    assert_eq!(page.hits.len(), 10);
    assert!(page.hits.iter().all(|hit| hit.rerank_score.is_some()));
}

#[test]
fn rerank_limit_without_reranker_does_not_change_a_large_page() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = fill_kryptonite(&vault, 80);
    let plain = collection.search(&q("kryptonite", 10), None).unwrap();
    let ignored = collection
        .search(&q("kryptonite", 10).with_rerank_limit(80), None)
        .unwrap();
    assert_eq!(hit_ids(&plain), hit_ids(&ignored));
    assert!(ignored.hits.iter().all(|hit| hit.rerank_score.is_none()));
    assert_eq!(ignored.hits.len(), 10);
}

#[test]
fn reranker_keeps_each_leg_rank1_that_rrf_drops() {
    let scratch = Scratch::new();
    let collection = open_disagreeing_legs(&scratch);

    let lexical_top = collection
        .search(&q("kryptonite", 1).with_vector_weight(0.0), None)
        .unwrap();
    assert_eq!(
        hit_ids(&lexical_top),
        ["zzz_lex"],
        "zzz_lex must be lexical rank 1"
    );
    let vector_top = collection
        .search(&q("kryptonite", 1).with_lexical_weight(0.0), None)
        .unwrap();
    assert_eq!(
        hit_ids(&vector_top),
        ["aaa_vec"],
        "aaa_vec must be vector rank 1"
    );

    let rrf = collection.search(&q("kryptonite", 50), None).unwrap();
    assert_eq!(rrf.hits.len(), 50);
    let rrf_ids = hit_ids(&rrf);
    assert!(
        !rrf_ids.contains(&"aaa_vec") || !rrf_ids.contains(&"zzz_lex"),
        "pure RRF must drop at least one exclusive rank-1 so this fails on old pool membership: {rrf_ids:?}"
    );

    let reranker = CollectIdsReranker::new();
    collection
        .search(&q("kryptonite", 10), Some(&reranker))
        .unwrap();
    let scored = reranker.ids();
    assert!(
        scored.iter().any(|id| id == "aaa_vec"),
        "vector rank-1 must reach the reranker: {scored:?}"
    );
    assert!(
        scored.iter().any(|id| id == "zzz_lex"),
        "lexical rank-1 must reach the reranker: {scored:?}"
    );
}

#[test]
fn reranker_fills_the_pool_from_the_longer_leg() {
    let scratch = Scratch::new();
    let embedder = Arc::new(MapEmbedder { pairs: Vec::new() });
    let vault = open_tokens(&scratch, Some(embedder));
    let collection = vault.collection("token_notes").unwrap();
    vault.begin().unwrap();
    collection
        .add("lex_a", rec(json!({"title": "kryptonite mineral"})), None)
        .unwrap();
    collection
        .add("lex_b", rec(json!({"title": "kryptonite extra"})), None)
        .unwrap();
    collection
        .add("lex_c", rec(json!({"title": "kryptonite mention"})), None)
        .unwrap();
    for i in 0..80 {
        collection
            .add(
                &format!("vec{i:02}"),
                rec(json!({"title": "unrelated prose"})),
                None,
            )
            .unwrap();
    }
    vault.commit("short lexical leg").unwrap();

    let lexical_only = collection
        .search(&q("kryptonite", 10).with_vector_weight(0.0), None)
        .unwrap();
    assert_eq!(lexical_only.hits.len(), 3, "{:?}", hit_ids(&lexical_only));

    let reranker = ScoreByIdReranker::new();
    let page = collection
        .search(&q("kryptonite", 10), Some(&reranker))
        .unwrap();
    assert_eq!(
        reranker.scored(),
        50,
        "a 3-hit lexical leg must not shrink the pool while the vector leg still has candidates"
    );
    assert_eq!(page.hits.len(), 10);
}

#[test]
fn without_reranker_fused_order_and_scores_stay_rrf() {
    let scratch = Scratch::new();
    let embedder = Arc::new(MapEmbedder {
        pairs: vec![
            (String::from("kryptonite mineral"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("kryptonite"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("car"), vec![0.9, 0.1, 0.0, 0.0]),
        ],
    });
    let vault = open_tokens(&scratch, Some(embedder));
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("both", rec(json!({"title": "kryptonite mineral"})), None)
        .unwrap();
    collection
        .add("vector_only", rec(json!({"title": "car"})), None)
        .unwrap();
    collection
        .add("noise", rec(json!({"title": "unrelated prose"})), None)
        .unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let mut got = serde_json::to_value(&hits).unwrap();
    if let Some(hits) = got.get_mut("hits").and_then(Value::as_array_mut) {
        for hit in hits {
            if let Some(obj) = hit.as_object_mut() {
                obj.remove("path");
            }
        }
    }
    let expected = json!({
        "hits": [
            {
                "collection": "token_notes",
                "id": "both",
                "lexicalScore": 1.0401802062988281,
                "rrfScore": 0.03278688524590164,
                "vectorScore": 1.0
            },
            {
                "collection": "token_notes",
                "id": "vector_only",
                "rrfScore": 0.016129032258064516,
                "vectorScore": 0.9939209222793579
            },
            {
                "collection": "token_notes",
                "id": "noise",
                "rrfScore": 0.015873015873015872,
                "vectorScore": 0.5
            }
        ]
    });
    assert_eq!(got, expected);
}

#[test]
fn reranker_pool_is_deterministic() {
    let scratch = Scratch::new();
    let collection = open_disagreeing_legs(&scratch);
    let first = CollectIdsReranker::new();
    let page_a = collection
        .search(&q("kryptonite", 10), Some(&first))
        .unwrap();
    let second = CollectIdsReranker::new();
    let page_b = collection
        .search(&q("kryptonite", 10), Some(&second))
        .unwrap();
    assert_eq!(first.ids(), second.ids());
    assert_eq!(hit_ids(&page_a), hit_ids(&page_b));
    assert_eq!(
        page_a
            .hits
            .iter()
            .map(|hit| (
                hit.rrf_score,
                hit.lexical_score,
                hit.vector_score,
                hit.rerank_score
            ))
            .collect::<Vec<_>>(),
        page_b
            .hits
            .iter()
            .map(|hit| (
                hit.rrf_score,
                hit.lexical_score,
                hit.vector_score,
                hit.rerank_score
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn filter_applies_before_ranking() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "keep",
            rec(json!({"title": "kryptonite mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "drop",
            rec(json!({"title": "kryptonite mineral extra", "tag": "blue"})),
            None,
        )
        .unwrap();
    let query: SearchQuery = serde_json::from_value(json!({
        "text": "kryptonite",
        "k": 5,
        "filter": {"term":{"field":"tag","value":"red"}}
    }))
    .unwrap();
    let hits = collection.search(&query, None).unwrap();
    let ids = hit_ids(&hits);
    assert_eq!(ids, ["keep"]);
}

#[test]
fn search_omits_raw_unless_include_raw() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "keep",
            rec(json!({"title": "kryptonite mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    let omitted = collection.search(&q("kryptonite", 5), None).unwrap();
    assert!(omitted.hits[0].raw.is_none());
    let included = collection
        .search(&q("kryptonite", 5).with_include_raw(true), None)
        .unwrap();
    assert!(
        included.hits[0]
            .raw
            .as_ref()
            .is_some_and(|raw| !raw.is_empty())
    );
}

#[test]
fn invalid_record_is_omitted_from_search() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    vault
        .collection("token_notes")
        .unwrap()
        .add("old", rec(json!({"title": "kryptonite"})), None)
        .unwrap();
    drop(vault);
    scratch.write_declaration(
        "tokens.yaml",
        "version: 1
name: token_notes
schema:
  journal: {}
",
    );
    let vault = open_proj(scratch.vault(), None);
    let collection = vault.collection("token_notes").unwrap();
    collection.get("old").unwrap();
    vault.reindex().unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"old"), "{ids:?}");
}

#[test]
fn insert_is_searchable_without_reindex() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("fresh", rec(json!({"title": "kryptonite"})), None)
        .unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"fresh"), "{ids:?}");
}

struct FailingEmbedder;

impl Embedder for FailingEmbedder {
    fn model_id(&self) -> &str {
        "fail-stub"
    }

    fn dimensions(&self) -> NonZeroUsize {
        NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn embed(&self, _: &str) -> Result<Vec<f32>, scheda::Error> {
        Err(scheda::Error::invalid("embed failed"))
    }

    fn embed_many(&self, _: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        Err(scheda::Error::invalid("embed failed"))
    }
}

#[test]
fn failed_embed_rolls_back_the_new_record() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, Some(Arc::new(FailingEmbedder)));
    let collection = vault.collection("token_notes").unwrap();
    let err = collection
        .add("fresh", rec(json!({"title": "kryptonite"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let missing = collection.get("fresh").unwrap_err();
    assert_eq!(missing.known_code(), Some(Code::NotFound));
}

struct ProbeEmbedder {
    many_limit: NonZeroUsize,
    embed_calls: AtomicU64,
    many_calls: AtomicU64,
}

impl ProbeEmbedder {
    fn new(many_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            many_limit: NonZeroUsize::new(many_limit).unwrap(),
            embed_calls: AtomicU64::new(0),
            many_calls: AtomicU64::new(0),
        })
    }

    fn counts(&self) -> (u64, u64) {
        (
            self.embed_calls.load(Ordering::Relaxed),
            self.many_calls.load(Ordering::Relaxed),
        )
    }
}

impl Embedder for ProbeEmbedder {
    fn model_id(&self) -> &str {
        "probe"
    }

    fn dimensions(&self) -> NonZeroUsize {
        NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> NonZeroUsize {
        self.many_limit
    }

    fn embed(&self, _: &str) -> Result<Vec<f32>, scheda::Error> {
        self.embed_calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![1.0, 0.0, 0.0, 0.0])
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        self.many_calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![vec![1.0, 0.0, 0.0, 0.0]; texts.len()])
    }
}

struct ProbeReranker {
    many_limit: NonZeroUsize,
    score_calls: AtomicU64,
    many_calls: AtomicU64,
}

impl ProbeReranker {
    fn new(many_limit: usize) -> Self {
        Self {
            many_limit: NonZeroUsize::new(many_limit).unwrap(),
            score_calls: AtomicU64::new(0),
            many_calls: AtomicU64::new(0),
        }
    }

    fn counts(&self) -> (u64, u64) {
        (
            self.score_calls.load(Ordering::Relaxed),
            self.many_calls.load(Ordering::Relaxed),
        )
    }
}

impl Reranker for ProbeReranker {
    fn many_limit(&self) -> NonZeroUsize {
        self.many_limit
    }

    fn score(&self, _: &str, _: &Record) -> Result<f64, scheda::Error> {
        self.score_calls.fetch_add(1, Ordering::Relaxed);
        Ok(1.0)
    }

    fn score_many(&self, _: &str, records: &[&Record]) -> Result<Vec<f64>, scheda::Error> {
        self.many_calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![1.0; records.len()])
    }
}

#[test]
fn embed_uses_single_or_many_by_limit() {
    let one = ProbeEmbedder::new(32);
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, Some(Arc::clone(&one) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add("one", rec(json!({"title": "kryptonite"})), None)
        .unwrap();
    assert_eq!(one.counts(), (1, 0));

    let many = ProbeEmbedder::new(32);
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, Some(Arc::clone(&many) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add(
            "two",
            rec(json!({"title": "kryptonite", "tag": "red"})),
            None,
        )
        .unwrap();
    assert_eq!(many.counts(), (0, 1));

    let limit_one = ProbeEmbedder::new(1);
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, Some(Arc::clone(&limit_one) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add(
            "loop",
            rec(json!({"title": "kryptonite", "tag": "red"})),
            None,
        )
        .unwrap();
    assert_eq!(limit_one.counts(), (2, 0));

    let leftover = ProbeEmbedder::new(2);
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration(
        "tokens.yaml",
        "version: 1
name: token_notes
schema:
  journal: {}
",
    );
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&leftover) as Arc<dyn Embedder>),
    );
    vault
        .collection("token_notes")
        .unwrap()
        .add(
            "three",
            rec(json!({"title": "kryptonite", "tag": "red", "extra": "x"})),
            None,
        )
        .unwrap();
    assert_eq!(leftover.counts(), (1, 1));
}

#[test]
fn score_uses_single_or_many_by_limit() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("first", rec(json!({"title": "kryptonite mineral"})), None)
        .unwrap();
    collection
        .add(
            "second",
            rec(json!({"title": "kryptonite extra mention"})),
            None,
        )
        .unwrap();

    let many = ProbeReranker::new(32);
    collection.search(&q("kryptonite", 2), Some(&many)).unwrap();
    assert_eq!(many.counts(), (0, 1));

    let one = ProbeReranker::new(1);
    collection.search(&q("kryptonite", 2), Some(&one)).unwrap();
    assert_eq!(one.counts(), (2, 0));
}

#[test]
fn missing_format_fails_closed() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "kryptonite"})), None)
        .unwrap();
    fs::remove_file(live_index(&vault).join("format")).unwrap();
    let err = collection.search(&q("kryptonite", 5), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexMissing));
}

#[test]
fn corrupt_lexical_index_fails_closed() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "kryptonite"})), None)
        .unwrap();
    drop(vault);
    fs::write(
        live_index_at(&scratch.vault().join(".scheda").join("index"))
            .join("lexical")
            .join("meta.json"),
        "not-tantivy",
    )
    .unwrap();
    let vault = open_proj(scratch.vault(), None);
    let collection = vault.collection("token_notes").unwrap();
    collection.get("a").unwrap();
    let err = collection.search(&q("kryptonite", 5), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexLexical));
}

#[test]
fn edge_field_read_matches_hand_list() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("src.yaml", WALK_SRC);
    scratch.write_declaration("dst.yaml", WALK_DST);
    let vault = open_proj(scratch.vault(), None);
    let src = vault.collection("walk_src").unwrap();
    let dst = vault.collection("walk_dst").unwrap();
    dst.add("alpha", rec(json!({"title": "A"})), None).unwrap();
    dst.add("beta", rec(json!({"title": "B"})), None).unwrap();
    src.add(
        "one",
        rec(json!({"title": "keep", "target": "alpha"})),
        None,
    )
    .unwrap();
    src.add("two", rec(json!({"title": "keep", "target": "beta"})), None)
        .unwrap();
    src.add(
        "skip",
        rec(json!({"title": "other", "target": "alpha"})),
        None,
    )
    .unwrap();
    src.add(
        "dangling",
        rec(json!({"title": "keep", "target": "missing"})),
        None,
    )
    .unwrap();
    let found = src
        .find(
            &filter(json!({"term":{"field":"title","value":"keep"}})).with_include_raw(true),
            0,
            100,
        )
        .unwrap();
    let mut ids: Vec<String> = Vec::new();
    for record in &found.records {
        let value: serde_json::Value =
            serde_json::from_slice(record.raw.as_deref().unwrap_or(&[])).unwrap();
        let Some(target) = value["target"].as_str() else {
            continue;
        };
        if dst.get(target).is_ok() {
            ids.push(target.to_owned());
        }
    }
    ids.sort_unstable();
    assert_eq!(ids, ["alpha", "beta"]);
}

#[test]
fn find_by_edge_target_does_not_scan_unrelated() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("src.yaml", WALK_SRC);
    scratch.write_declaration("dst.yaml", WALK_DST);
    let vault = open_proj(scratch.vault(), None);
    let src = vault.collection("walk_src").unwrap();
    let dst = vault.collection("walk_dst").unwrap();
    dst.add("t", rec(json!({"title": "T"})), None).unwrap();
    src.add("link", rec(json!({"title": "keep", "target": "t"})), None)
        .unwrap();
    for i in 0..400 {
        src.add(
            &format!("u{i}"),
            rec(json!({"title": "other", "target": "nowhere"})),
            None,
        )
        .unwrap();
    }
    let found = src
        .find(
            &filter(json!({"term":{"field":"target","value":"t"}})),
            0,
            10,
        )
        .unwrap();
    assert_eq!(found.total, Some(1));
    assert_eq!(found.records.len(), 1);
    assert_eq!(found.records[0].id.as_deref(), Some("link"));
}

#[test]
fn stale_index_fails_closed_until_reindex() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "kryptonite"})), None)
        .unwrap();
    fs::write(live_index(&vault).join("stale"), []).unwrap();
    let err = collection.find(&FindQuery::all(), 0, 10).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexLexical));
    let err = collection.search(&q("kryptonite", 5), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexLexical));
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexLexical));
    collection.get("a").unwrap();
    vault.reindex().unwrap();
    assert!(!live_index(&vault).join("stale").is_file());
    let found = collection.find(&FindQuery::all(), 0, 10).unwrap();
    assert_eq!(found.total, Some(1));
    vault.check().unwrap();
}

fn live_index(vault: &Vault) -> PathBuf {
    live_index_at(vault.index_dir())
}

fn live_index_at(root: &Path) -> PathBuf {
    match fs::read_to_string(root.join("current")) {
        Ok(name) => root.join(name.trim()),
        Err(_) => root.to_path_buf(),
    }
}

#[test]
fn search_on_lexically_excluded_collection_uses_vector_leg() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration(
        "fold.yaml",
        r#"
version: 1
name: fold_docs
schema:
  journal: {}
  exclude_lexical_index: true
"#,
    );
    let embedder = Arc::new(MapEmbedder {
        pairs: vec![
            (String::from("fold-vector-alpha"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("fold-vector-other"), vec![0.0, 1.0, 0.0, 0.0]),
        ],
    });
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
    );
    let collection = vault.collection("fold_docs").unwrap();
    collection
        .add("alpha", rec(json!({"title": "fold-vector-alpha"})), None)
        .unwrap();
    collection
        .add("other", rec(json!({"title": "fold-vector-other"})), None)
        .unwrap();
    let hits = collection.search(&q("fold-vector-alpha", 1), None).unwrap();
    assert_eq!(hit_ids(&hits), ["alpha"]);
    assert!(hits.hits[0].vector_score.is_some());
    assert!(hits.hits[0].lexical_score.is_none());
    let err = collection.find(&FindQuery::all(), 0, 10).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn search_on_vector_excluded_collection_uses_lexical_leg() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration(
        "fold.yaml",
        r#"
version: 1
name: fold_docs
schema:
  journal: {}
  exclude_vector_index: true
"#,
    );
    let probe = ProbeEmbedder::new(32);
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&probe) as Arc<dyn Embedder>),
    );
    let collection = vault.collection("fold_docs").unwrap();
    collection
        .add("alpha", rec(json!({"title": "kryptonite mineral"})), None)
        .unwrap();
    assert_eq!(
        probe.counts(),
        (0, 0),
        "exclude_vector_index must not call the embedder on add"
    );
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    assert_eq!(hit_ids(&hits), ["alpha"]);
    assert!(hits.hits[0].lexical_score.is_some());
    assert!(hits.hits[0].vector_score.is_none());
}

#[test]
fn search_with_both_index_legs_excluded_is_unsupported() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration(
        "fold.yaml",
        r#"
version: 1
name: fold_docs
schema:
  journal: {}
  exclude_lexical_index: true
  exclude_vector_index: true
"#,
    );
    let vault = open_proj(scratch.vault(), None);
    let collection = vault.collection("fold_docs").unwrap();
    collection
        .add("alpha", rec(json!({"title": "kryptonite"})), None)
        .unwrap();
    let err = collection.search(&q("kryptonite", 5), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    assert!(
        err.message
            .as_deref()
            .is_some_and(|m| m.contains("fold_docs") && m.contains("both Index legs"))
    );
    let err = vault
        .search(&["fold_docs"], &q("kryptonite", 5), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn new_field_on_vector_excluded_collection_is_not_embedded() {
    let scratch = Scratch::new();
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration(
        "fold.yaml",
        r#"
version: 1
name: fold_docs
schema:
  journal: {}
  exclude_vector_index: true
"#,
    );
    let probe = ProbeEmbedder::new(32);
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&probe) as Arc<dyn Embedder>),
    );
    vault
        .collection("fold_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "keep"})), None)
        .unwrap();
    drop(vault);
    scratch.write_declaration(
        "fold.yaml",
        r#"
version: 1
name: fold_docs
schema:
  journal: {}
  exclude_vector_index: true
"#,
    );
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&probe) as Arc<dyn Embedder>),
    );
    vault
        .collection("fold_docs")
        .unwrap()
        .add(
            "beta",
            rec(json!({"title": "keep", "secret": "new-field-token"})),
            None,
        )
        .unwrap();
    assert_eq!(
        probe.counts(),
        (0, 0),
        "a new field on a vector-excluded collection must not be embedded"
    );
}

struct CountingEmbedder {
    model_id: &'static str,
    slots: AtomicU64,
}

impl CountingEmbedder {
    fn new(model_id: &'static str) -> Arc<Self> {
        Arc::new(Self {
            model_id,
            slots: AtomicU64::new(0),
        })
    }

    fn slots(&self) -> u64 {
        self.slots.load(Ordering::Relaxed)
    }
}

fn counting_vector(text: &str) -> Vec<f32> {
    let mut acc = [0.0f32; 4];
    for (i, b) in text.as_bytes().iter().enumerate() {
        acc[i % 4] += f32::from(*b) / 255.0;
    }
    let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut acc {
            *x /= norm;
        }
    } else {
        acc[0] = 1.0;
    }
    acc.to_vec()
}

impl Embedder for CountingEmbedder {
    fn model_id(&self) -> &str {
        self.model_id
    }

    fn dimensions(&self) -> NonZeroUsize {
        NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        self.slots.fetch_add(
            u64::try_from(texts.len()).expect("embed batch fits u64"),
            Ordering::Relaxed,
        );
        Ok(texts.iter().copied().map(counting_vector).collect())
    }
}

fn open_tokens_counting(scratch: &Scratch, embedder: Arc<CountingEmbedder>) -> Vault {
    open_tokens(scratch, Some(embedder as Arc<dyn Embedder>))
}

#[test]
fn unchanged_record_write_does_not_embed_again() {
    let scratch = Scratch::new();
    let embedder = CountingEmbedder::new("count");
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("slot.yaml", &slot_schema("token_slot", "keep"));
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
    );
    let collection = vault.collection("token_slot").unwrap();
    collection
        .add(
            "keep",
            rec(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    let after_add = embedder.slots();
    assert_eq!(after_add, 2);
    collection
        .replace(
            "keep",
            rec_rep(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    assert_eq!(
        embedder.slots(),
        after_add,
        "identical replace must not call the embedder"
    );
}

#[test]
fn replacing_one_field_embeds_only_that_slot() {
    let scratch = Scratch::new();
    let embedder = CountingEmbedder::new("count");
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("a.yaml", &slot_schema("token_slot_a", "a"));
    scratch.write_declaration("b.yaml", &slot_schema("token_slot_b", "b"));
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
    );
    vault
        .collection("token_slot_a")
        .unwrap()
        .add(
            "a",
            rec(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    vault
        .collection("token_slot_b")
        .unwrap()
        .add(
            "b",
            rec(json!({"title": "beta mineral", "tag": "blue"})),
            None,
        )
        .unwrap();
    let after_add = embedder.slots();
    assert_eq!(after_add, 4);
    vault
        .collection("token_slot_a")
        .unwrap()
        .replace(
            "a",
            rec_rep(json!({"title": "alpha mineral", "tag": "green"})),
            None,
        )
        .unwrap();
    assert_eq!(
        embedder.slots(),
        after_add + 1,
        "only the changed field slot must be re-embedded"
    );
}

#[test]
fn vanished_vector_slot_is_absent_from_search() {
    let scratch = Scratch::new();
    let embedder = Arc::new(MapEmbedder {
        pairs: vec![
            (String::from("unique-slot-token"), vec![1.0, 0.0, 0.0, 0.0]),
            (String::from("plain title"), vec![0.0, 1.0, 0.0, 0.0]),
        ],
    });
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("hit.yaml", &slot_schema("token_hit", "hit"));
    scratch.write_declaration("keep.yaml", &slot_schema("token_keep", "keep"));
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
    );
    vault
        .collection("token_hit")
        .unwrap()
        .add(
            "hit",
            rec(json!({"title": "plain title", "tag": "unique-slot-token"})),
            None,
        )
        .unwrap();
    vault
        .collection("token_keep")
        .unwrap()
        .add("keep", rec(json!({"title": "unique-slot-token"})), None)
        .unwrap();
    let names = ["token_hit", "token_keep"];
    let before = vault
        .search(
            &names,
            &q("unique-slot-token", 5).with_lexical_weight(0.0),
            None,
        )
        .unwrap();
    assert!(
        hit_ids(&before).contains(&"hit"),
        "tag slot must be searchable before it vanishes: {:?}",
        hit_ids(&before)
    );
    vault
        .collection("token_hit")
        .unwrap()
        .replace("hit", rec_rep(json!({"title": "plain title"})), None)
        .unwrap();
    let after = vault
        .search(
            &names,
            &q("unique-slot-token", 1).with_lexical_weight(0.0),
            None,
        )
        .unwrap();
    let ids = hit_ids(&after);
    assert_eq!(
        ids,
        ["keep"],
        "vanished tag vector must not outrank or replace the remaining exact match: {ids:?}"
    );
    let wide = vault
        .search(
            &names,
            &q("unique-slot-token", 5).with_lexical_weight(0.0),
            None,
        )
        .unwrap();
    let keep_score = wide
        .hits
        .iter()
        .find(|hit| hit.id.as_deref() == Some("keep"))
        .and_then(|hit| hit.vector_score)
        .expect("keep must still have a vector score");
    if let Some(hit) = wide.hits.iter().find(|h| h.id.as_deref() == Some("hit")) {
        let score = hit.vector_score.expect("hit leftover title still scored");
        assert!(
            score < keep_score,
            "stale tag vector would tie keep; leftover title must be farther: hit={score} keep={keep_score}"
        );
    }
}

#[test]
fn reindex_on_unchanged_content_does_not_embed_again() {
    let scratch = Scratch::new();
    let embedder = CountingEmbedder::new("count");
    let vault = open_tokens_counting(&scratch, Arc::clone(&embedder));
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "keep",
            rec(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    collection
        .add("other", rec(json!({"title": "beta mineral"})), None)
        .unwrap();
    let after_add = embedder.slots();
    assert_eq!(after_add, 3);
    vault.reindex().unwrap();
    assert_eq!(
        embedder.slots(),
        after_add,
        "reindex of unchanged content must not call the embedder"
    );
}

#[test]
fn reindex_drops_vectors_for_records_that_are_gone() {
    let scratch = Scratch::new();
    let embedder = CountingEmbedder::new("count");
    let vault = open_tokens_counting(&scratch, Arc::clone(&embedder));
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add("gone", rec(json!({"title": "unique-gone-token"})), None)
        .unwrap();
    collection
        .add("keep", rec(json!({"title": "unique-keep-token"})), None)
        .unwrap();
    let after_add = embedder.slots();
    drop_journal_id(&scratch.vault(), "token_notes", "gone");
    vault.reindex().unwrap();
    assert_eq!(
        embedder.slots(),
        after_add,
        "dropping a missing record must not re-embed the rest"
    );
    let hits = collection
        .search(&q("unique-gone-token", 5).with_lexical_weight(0.0), None)
        .unwrap();
    assert!(
        !hit_ids(&hits).contains(&"gone"),
        "deleted record still returned: {:?}",
        hit_ids(&hits)
    );
    let keep = collection
        .search(&q("unique-keep-token", 5).with_lexical_weight(0.0), None)
        .unwrap();
    assert!(
        hit_ids(&keep).contains(&"keep"),
        "surviving record missing: {:?}",
        hit_ids(&keep)
    );
}

#[test]
fn different_model_id_reembeds_every_slot() {
    let scratch = Scratch::new();
    let first = CountingEmbedder::new("model-a");
    let vault = open_tokens_counting(&scratch, Arc::clone(&first));
    vault
        .collection("token_notes")
        .unwrap()
        .add(
            "keep",
            rec(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    assert_eq!(first.slots(), 2);
    drop(vault);
    let second = CountingEmbedder::new("model-b");
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&second) as Arc<dyn Embedder>),
    );
    vault.reindex().unwrap();
    assert_eq!(
        second.slots(),
        2,
        "a new model_id must re-embed every stored slot"
    );
}

#[test]
fn vector_digest_is_stable_across_runs() {
    let scratch = Scratch::new();
    let embedder = CountingEmbedder::new("count");
    drop(open_proj(scratch.vault(), None));
    scratch.write_declaration("slot.yaml", &slot_schema("token_slot", "keep"));
    let vault = open_proj(
        scratch.vault(),
        Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
    );
    vault
        .collection("token_slot")
        .unwrap()
        .add(
            "keep",
            rec(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    let after_add = embedder.slots();
    vault.reindex().unwrap();
    vault
        .collection("token_slot")
        .unwrap()
        .replace(
            "keep",
            rec_rep(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    vault.reindex().unwrap();
    assert_eq!(
        embedder.slots(),
        after_add,
        "the same field text must keep the same digest across writes and reindex"
    );
}

struct RoleEmbedder {
    queries: Mutex<Vec<String>>,
    fields: Mutex<Vec<String>>,
}

impl Embedder for RoleEmbedder {
    fn model_id(&self) -> &str {
        "role"
    }

    fn dimensions(&self) -> NonZeroUsize {
        NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        self.fields
            .lock()
            .unwrap()
            .extend(texts.iter().map(|text| (*text).to_string()));
        Ok(texts.iter().copied().map(counting_vector).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, scheda::Error> {
        self.queries.lock().unwrap().push(text.to_string());
        Ok(counting_vector(text))
    }
}

#[test]
fn search_sends_the_query_through_embed_query_not_embed_many() {
    let scratch = Scratch::new();
    let embedder = Arc::new(RoleEmbedder {
        queries: Mutex::new(Vec::new()),
        fields: Mutex::new(Vec::new()),
    });
    let vault = open_tokens(&scratch, Some(Arc::clone(&embedder) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add("vehicle", rec(json!({"title": "car"})), None)
        .unwrap();
    let _ = vault
        .collection("token_notes")
        .unwrap()
        .search(&q("automobile", 5).with_lexical_weight(0.0), None)
        .unwrap();
    let queries = embedder.queries.lock().unwrap().clone();
    let fields = embedder.fields.lock().unwrap().clone();
    assert_eq!(queries, vec!["automobile".to_string()]);
    assert!(
        fields.iter().any(|text| text == "car"),
        "record fields must still travel embed_many: {fields:?}"
    );
    assert!(
        !fields.iter().any(|text| text == "automobile"),
        "the search query must not travel embed_many: {fields:?}"
    );
}

fn peek_schema_name(body: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            return rest.trim().trim_matches('"').to_string();
        }
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("name")?.as_str().map(str::to_owned))
        .expect("schema fixture name")
}
