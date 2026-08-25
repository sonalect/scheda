//! C9: hybrid search, edges, incremental Index (SCH-9–11, IDX-3, IDX-6–9, QRY-3–6, REL-1–3).
//! Collection names here and in `testdata/search/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scheda::{Code, Embedder, Filter, OpenOptions, Record, SearchQuery, Vault};
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
        let dir = self.vault().join("schema");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file), body).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const TOKENS: &str = include_str!("../testdata/search/tokens.yaml");
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

    fn dimensions(&self) -> usize {
        4
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, scheda::Error> {
        for (key, vector) in &self.pairs {
            if text == key {
                return Ok(vector.clone());
            }
        }
        Ok(vec![0.0, 1.0, 0.0, 0.0])
    }
}

fn rec(collection: &str, id: &str, fields: Value) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": collection,
        "fields": fields,
    }))
    .unwrap()
}

fn rec_body(collection: &str, id: &str, fields: Value, body: &str) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": collection,
        "fields": fields,
        "body": body,
    }))
    .unwrap()
}

fn filter(value: Value) -> Filter {
    serde_json::from_value(value).unwrap()
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
    drop(Vault::init(scratch.vault(), OpenOptions::new()).unwrap());
    scratch.write_schema("tokens.yaml", TOKENS);
    let vault = Vault::open(scratch.vault(), OpenOptions::new()).unwrap();
    match embedder {
        Some(embedder) => vault.with_embedder(embedder),
        None => vault,
    }
}

#[test]
fn token_in_marked_field_ranks_and_unmarked_is_absent() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .insert_one(rec(
            "token_notes",
            "hit",
            json!({"title": "kryptonite mineral", "aside": "plain", "tag": "red"}),
        ))
        .unwrap();
    collection
        .insert_one(rec(
            "token_notes",
            "aside_only",
            json!({"title": "ordinary rock", "aside": "kryptonite mention", "tag": "blue"}),
        ))
        .unwrap();
    for i in 0..8 {
        collection
            .insert_one(rec(
                "token_notes",
                &format!("d{i}"),
                json!({"title": format!("distractor {i}"), "tag": "blue"}),
            ))
            .unwrap();
    }
    let hits = collection.search(&q("kryptonite", 5)).unwrap();
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
fn body_fts_is_indexed_when_marked() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .insert_one(rec_body(
            "token_notes",
            "bodyhit",
            json!({"title": "quiet"}),
            "kryptonite in the body",
        ))
        .unwrap();
    collection
        .insert_one(rec("token_notes", "other", json!({"title": "quiet too"})))
        .unwrap();
    let hits = collection.search(&q("kryptonite", 5)).unwrap();
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
        .insert_one(rec("token_notes", "vehicle", json!({"title": "car"})))
        .unwrap();
    collection
        .insert_one(rec("token_notes", "other", json!({"title": "banana"})))
        .unwrap();
    let hits = collection
        .search(&q("automobile", 5).with_lexical_weight(0.0))
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
        .search(&q("kryptonite", 5).with_lexical_weight(-1.0))
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
        .insert_one(rec(
            "token_notes",
            "both",
            json!({"title": "kryptonite mineral"}),
        ))
        .unwrap();
    collection
        .insert_one(rec("token_notes", "vector_only", json!({"title": "car"})))
        .unwrap();
    collection
        .insert_one(rec(
            "token_notes",
            "noise",
            json!({"title": "unrelated prose"}),
        ))
        .unwrap();
    let hits = collection.search(&q("kryptonite", 5)).unwrap();
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
}

#[test]
fn filter_applies_before_ranking() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .insert_one(rec(
            "token_notes",
            "keep",
            json!({"title": "kryptonite mineral", "tag": "red"}),
        ))
        .unwrap();
    collection
        .insert_one(rec(
            "token_notes",
            "drop",
            json!({"title": "kryptonite mineral extra", "tag": "blue"}),
        ))
        .unwrap();
    let query: SearchQuery = serde_json::from_value(json!({
        "text": "kryptonite",
        "k": 5,
        "filter": {"clauses":[{"equal":{"field":"tag","value":"red"}}]}
    }))
    .unwrap();
    let hits = collection.search(&query).unwrap();
    let ids = hit_ids(&hits);
    assert_eq!(ids, ["keep"]);
}

#[test]
fn invalid_record_is_omitted_from_search() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    vault
        .collection("token_notes")
        .unwrap()
        .insert_one(rec("token_notes", "old", json!({"title": "kryptonite"})))
        .unwrap();
    drop(vault);
    scratch.write_schema(
        "tokens.yaml",
        "name: token_notes\nkind: document\nschema_version: 1\nlayout: notes/{id}.md\nfields:\n  - name: title\n    type: string\n    required: true\n    fts: true\n  - name: extra\n    type: string\n    required: true\n",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new()).unwrap();
    let collection = vault.collection("token_notes").unwrap();
    let got = collection.find_one("old").unwrap();
    assert_eq!(got.valid, Some(false));
    vault.reindex().unwrap();
    let hits = collection.search(&q("kryptonite", 5)).unwrap();
    let ids = hit_ids(&hits);
    assert!(!ids.contains(&"old"), "{ids:?}");
}

#[test]
fn insert_is_searchable_without_reindex() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .insert_one(rec("token_notes", "fresh", json!({"title": "kryptonite"})))
        .unwrap();
    let hits = collection.search(&q("kryptonite", 5)).unwrap();
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"fresh"), "{ids:?}");
}

#[test]
fn missing_format_fails_closed() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .insert_one(rec("token_notes", "a", json!({"title": "kryptonite"})))
        .unwrap();
    fs::remove_file(vault.index_dir().join("format")).unwrap();
    let err = collection.search(&q("kryptonite", 5)).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexMissing));
}

#[test]
fn corrupt_lexical_index_fails_closed() {
    let scratch = Scratch::new();
    let vault = open_tokens(&scratch, None);
    let collection = vault.collection("token_notes").unwrap();
    collection
        .insert_one(rec("token_notes", "a", json!({"title": "kryptonite"})))
        .unwrap();
    drop(vault);
    fs::write(
        scratch.vault().join(".scheda/index/lexical/meta.json"),
        "not-tantivy",
    )
    .unwrap();
    let vault = Vault::open(scratch.vault(), OpenOptions::new()).unwrap();
    let collection = vault.collection("token_notes").unwrap();
    let got = collection.find_one("a").unwrap();
    assert_eq!(got.valid, Some(true));
    let err = collection.search(&q("kryptonite", 5)).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexLexical));
}

#[test]
fn walk_one_step_matches_hand_list() {
    let scratch = Scratch::new();
    drop(Vault::init(scratch.vault(), OpenOptions::new()).unwrap());
    scratch.write_schema("src.yaml", WALK_SRC);
    scratch.write_schema("dst.yaml", WALK_DST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new()).unwrap();
    let src = vault.collection("walk_src").unwrap();
    let dst = vault.collection("walk_dst").unwrap();
    dst.insert_one(rec("walk_dst", "alpha", json!({"title": "A"})))
        .unwrap();
    dst.insert_one(rec("walk_dst", "beta", json!({"title": "B"})))
        .unwrap();
    src.insert_one(rec(
        "walk_src",
        "one",
        json!({"title": "keep", "target": "alpha"}),
    ))
    .unwrap();
    src.insert_one(rec(
        "walk_src",
        "two",
        json!({"title": "keep", "target": "beta"}),
    ))
    .unwrap();
    src.insert_one(rec(
        "walk_src",
        "skip",
        json!({"title": "other", "target": "alpha"}),
    ))
    .unwrap();
    src.insert_one(rec(
        "walk_src",
        "dangling",
        json!({"title": "keep", "target": "missing"}),
    ))
    .unwrap();
    let walked = src
        .walk(
            "target",
            &filter(json!({"clauses":[{"equal":{"field":"title","value":"keep"}}]})),
        )
        .unwrap();
    let mut ids: Vec<_> = walked
        .iter()
        .filter_map(|record| record.id.as_deref())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["alpha", "beta"]);
}

#[test]
fn strict_edge_rejects_missing_target() {
    let scratch = Scratch::new();
    drop(Vault::init(scratch.vault(), OpenOptions::new()).unwrap());
    scratch.write_schema("src.yaml", WALK_STRICT);
    scratch.write_schema("dst.yaml", WALK_DST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new()).unwrap();
    let src = vault.collection("walk_strict").unwrap();
    let err = src
        .insert_one(rec("walk_strict", "bad", json!({"target": "nope"})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    vault
        .collection("walk_dst")
        .unwrap()
        .insert_one(rec("walk_dst", "ok", json!({"title": "t"})))
        .unwrap();
    src.insert_one(rec("walk_strict", "good", json!({"target": "ok"})))
        .unwrap();
}

#[test]
fn inverse_walk_does_not_scan_unrelated() {
    let scratch = Scratch::new();
    drop(Vault::init(scratch.vault(), OpenOptions::new()).unwrap());
    scratch.write_schema("src.yaml", WALK_SRC);
    scratch.write_schema("dst.yaml", WALK_DST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new()).unwrap();
    let src = vault.collection("walk_src").unwrap();
    let dst = vault.collection("walk_dst").unwrap();
    dst.insert_one(rec("walk_dst", "t", json!({"title": "T"})))
        .unwrap();
    src.insert_one(rec(
        "walk_src",
        "link",
        json!({"title": "keep", "target": "t"}),
    ))
    .unwrap();
    for i in 0..400 {
        src.insert_one(rec(
            "walk_src",
            &format!("u{i}"),
            json!({"title": "other", "target": "nowhere"}),
        ))
        .unwrap();
    }
    let started = Instant::now();
    let walked = dst.walk_inverse("walk_src", "target", "t").unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "inverse took {:?}",
        started.elapsed()
    );
    assert_eq!(walked.len(), 1);
    assert_eq!(walked[0].id.as_deref(), Some("link"));
}
