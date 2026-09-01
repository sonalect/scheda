//! C9: hybrid search, edges, incremental Index (SCH-9–11, IDX-3, IDX-6–9, QRY-3–6, REL-1–3).
//! Collection names here and in `testdata/search/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{
    Code, Embedder, Filter, FindQuery, OpenOptions, Record, RecordAdd, Reranker, SearchQuery, Vault,
};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c9-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn write_schema(&self, file: &str, body: &str) {
        let name = peek_schema_name(body);
        let dir = self.vault().join(name);
        fs::create_dir_all(&dir).unwrap();
        let dest = if file.ends_with(".json") {
            ".schema.json"
        } else {
            ".schema.yaml"
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
name: token_extra
kind: document
version: 1
fields:
  - name: title
    required: true
    string: {}
";
const WALK_SRC: &str = include_str!("../testdata/search/walk_src.yaml");
const WALK_DST: &str = include_str!("../testdata/search/walk_dst.yaml");
const WALK_STRICT: &str = include_str!("../testdata/search/walk_strict.yaml");

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

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn rec_body(fields: Value, body: &str) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields, "body": body })).unwrap()
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

fn open_tokens(scratch: &Scratch, embedder: Option<Arc<dyn Embedder>>) -> Vault {
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema("tokens.yaml", TOKENS);
    Vault::open(scratch.vault(), OpenOptions::new(), embedder).unwrap()
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
    collection.remove("hit", None).unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(!ids.contains(&"hit"), "removed id still a hit: {ids:?}");
    assert!(ids.contains(&"keep"), "kept id missing: {ids:?}");
}

#[test]
fn vault_search_scopes_collections() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema("tokens.yaml", TOKENS);
    scratch.write_schema("extra.yaml", TOKEN_EXTRA);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
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
fn body_fts_is_indexed_when_marked() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .add(
            "bodyhit",
            rec_body(json!({"title": "quiet"}), "kryptonite in the body"),
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
    scratch.write_schema(
        "tokens.yaml",
        "name: token_notes\nkind: document\nversion: 1\nfields:\n  - name: title\n    required: true\n    string: {}\n  - name: extra\n    required: true\n    string: {}\n",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("token_notes").unwrap();
    let got = collection.get("old").unwrap();
    assert_eq!(got.valid, Some(false));
    vault.reindex().unwrap();
    let hits = collection.search(&q("kryptonite", 5), None).unwrap();
    let ids = hit_ids(&hits);
    assert!(!ids.contains(&"old"), "{ids:?}");
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
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema(
        "tokens.yaml",
        "name: token_notes\nkind: document\nversion: 1\nfields:\n  - name: title\n    required: true\n    string: {}\n  - name: tag\n    string: {}\n  - name: extra\n    string: {}\n",
    );
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&leftover) as Arc<dyn Embedder>),
    )
    .unwrap();
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
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("token_notes").unwrap();
    let got = collection.get("a").unwrap();
    assert_eq!(got.valid, Some(true));
    let err = collection.search(&q("kryptonite", 5), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexLexical));
}

#[test]
fn edge_field_read_matches_hand_list() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema("src.yaml", WALK_SRC);
    scratch.write_schema("dst.yaml", WALK_DST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
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
            &filter(json!({"term":{"field":"title","value":"keep"}})),
            0,
            100,
        )
        .unwrap();
    let mut ids: Vec<String> = Vec::new();
    for record in &found.records {
        let value = serde_json::to_value(record).unwrap();
        let Some(target) = value["fields"]["target"].as_str() else {
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
fn strict_edge_rejects_missing_target() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema("src.yaml", WALK_STRICT);
    scratch.write_schema("dst.yaml", WALK_DST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let src = vault.collection("walk_strict").unwrap();
    let err = src
        .add("bad", rec(json!({"target": "nope"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    vault
        .collection("walk_dst")
        .unwrap()
        .add("ok", rec(json!({"title": "t"})), None)
        .unwrap();
    src.add("good", rec(json!({"target": "ok"})), None).unwrap();
}

#[test]
fn find_by_edge_target_does_not_scan_unrelated() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema("src.yaml", WALK_SRC);
    scratch.write_schema("dst.yaml", WALK_DST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
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
    let got = collection.get("a").unwrap();
    assert_eq!(got.valid, Some(true));
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
