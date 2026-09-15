//! Online `reindex`: writers during the walk, catch-up, publish window.
//! Collection names here and in `testdata/search/` must not appear in
//! `rust/scheda/src`.

mod common;

use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use scheda::{Code, Embedder, FindQuery, OpenOptions, RecordAdd, SearchQuery, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-reindex-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn write_schema(&self, body: &str) {
        let name = peek_schema_name(body);
        let dir = self.vault().join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".schema.yaml"), body).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const TOKENS: &str = include_str!("../testdata/search/tokens.yaml");
const LOG: &str = include_str!("../testdata/query/log.yaml");

fn peek_schema_name(body: &str) -> &str {
    for line in body.lines() {
        if let Some(name) = line.strip_prefix("name:") {
            return name.trim();
        }
    }
    "unknown"
}

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn q(text: &str, k: u32) -> SearchQuery {
    SearchQuery::default().with_text(text).with_k(k)
}

fn find_ids(vault: &Vault, name: &str) -> Vec<String> {
    vault
        .collection(name)
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap()
        .records
        .into_iter()
        .filter_map(|record| record.id)
        .collect()
}

fn search_ids(vault: &Vault, name: &str, text: &str) -> Vec<String> {
    vault
        .collection(name)
        .unwrap()
        .search(&q(text, 10).with_lexical_weight(0.0), None)
        .unwrap()
        .hits
        .into_iter()
        .filter_map(|hit| hit.id)
        .collect()
}

fn live_generation(vault: &Vault) -> String {
    fs::read_to_string(vault.index_dir().join("current"))
        .unwrap()
        .trim()
        .to_owned()
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

struct LatchState {
    parked: bool,
    resume: bool,
}

struct LatchEmbedder {
    model_id: &'static str,
    slots: AtomicU64,
    state: Mutex<LatchState>,
    cv: Condvar,
}

impl LatchEmbedder {
    fn new(model_id: &'static str) -> Arc<Self> {
        Arc::new(Self {
            model_id,
            slots: AtomicU64::new(0),
            state: Mutex::new(LatchState {
                parked: false,
                resume: false,
            }),
            cv: Condvar::new(),
        })
    }

    fn wait_parked(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.parked {
            state = self.cv.wait(state).unwrap();
        }
    }

    fn resume(&self) {
        let mut state = self.state.lock().unwrap();
        state.resume = true;
        self.cv.notify_all();
    }
}

impl Embedder for LatchEmbedder {
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
        {
            let mut state = self.state.lock().unwrap();
            state.parked = true;
            self.cv.notify_all();
            while !state.resume {
                state = self.cv.wait(state).unwrap();
            }
        }
        Ok(texts.iter().copied().map(counting_vector).collect())
    }
}

fn open_tokens(scratch: &Scratch, embedder: Option<Arc<dyn Embedder>>) -> Vault {
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema(TOKENS);
    Vault::open(scratch.vault(), OpenOptions::new(), embedder).unwrap()
}

#[test]
fn writer_during_rebuild_is_found_and_searched_after_publish() {
    let scratch = Scratch::new();
    let first = CountingEmbedder::new("model-a");
    let vault = open_tokens(&scratch, Some(Arc::clone(&first) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add("keep", rec(json!({"title": "keep mineral"})), None)
        .unwrap();
    drop(vault);
    let latch = LatchEmbedder::new("model-b");
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&latch) as Arc<dyn Embedder>),
    )
    .unwrap();
    let other = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    thread::scope(|s| {
        s.spawn(|| {
            latch.wait_parked();
            other
                .collection("token_notes")
                .unwrap()
                .add("fresh", rec(json!({"title": "fresh mineral"})), None)
                .unwrap();
            latch.resume();
        });
        vault.reindex().unwrap();
    });
    let mut ids = find_ids(&vault, "token_notes");
    ids.sort();
    assert_eq!(ids, ["fresh", "keep"]);
    let hits = search_ids(&vault, "token_notes", "fresh mineral");
    assert!(hits.contains(&"fresh".to_owned()), "{hits:?}");
    let reopened = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&latch) as Arc<dyn Embedder>),
    )
    .unwrap();
    let mut ids = find_ids(&reopened, "token_notes");
    ids.sort();
    assert_eq!(ids, ["fresh", "keep"]);
}

#[test]
fn remove_during_rebuild_is_absent_after_publish() {
    let scratch = Scratch::new();
    let first = CountingEmbedder::new("model-a");
    let vault = open_tokens(&scratch, Some(Arc::clone(&first) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add("keep", rec(json!({"title": "keep mineral"})), None)
        .unwrap();
    vault
        .collection("token_notes")
        .unwrap()
        .add("gone", rec(json!({"title": "gone mineral"})), None)
        .unwrap();
    drop(vault);
    let latch = LatchEmbedder::new("model-b");
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&latch) as Arc<dyn Embedder>),
    )
    .unwrap();
    let other = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    thread::scope(|s| {
        s.spawn(|| {
            latch.wait_parked();
            other
                .collection("token_notes")
                .unwrap()
                .remove("gone", None)
                .unwrap();
            latch.resume();
        });
        vault.reindex().unwrap();
    });
    assert_eq!(find_ids(&vault, "token_notes"), ["keep"]);
    let hits = search_ids(&vault, "token_notes", "gone mineral");
    assert!(!hits.contains(&"gone".to_owned()), "{hits:?}");
}

#[test]
fn same_model_reindex_does_not_embed_again() {
    let scratch = Scratch::new();
    let embedder = CountingEmbedder::new("count");
    let vault = open_tokens(&scratch, Some(Arc::clone(&embedder) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add(
            "keep",
            rec(json!({"title": "alpha mineral", "tag": "red"})),
            None,
        )
        .unwrap();
    let after_add = embedder.slots();
    vault.reindex().unwrap();
    assert_eq!(
        embedder.slots(),
        after_add,
        "same-model reindex must not embed inherited slots"
    );
}

#[test]
fn different_model_reembeds_queued_add() {
    let scratch = Scratch::new();
    let first = CountingEmbedder::new("model-a");
    let vault = open_tokens(&scratch, Some(Arc::clone(&first) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add("keep", rec(json!({"title": "keep mineral"})), None)
        .unwrap();
    drop(vault);
    let latch = LatchEmbedder::new("model-b");
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&latch) as Arc<dyn Embedder>),
    )
    .unwrap();
    let other = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(CountingEmbedder::new("model-b") as Arc<dyn Embedder>),
    )
    .unwrap();
    thread::scope(|s| {
        s.spawn(|| {
            latch.wait_parked();
            other
                .collection("token_notes")
                .unwrap()
                .add("fresh", rec(json!({"title": "queued mineral"})), None)
                .unwrap();
            latch.resume();
        });
        vault.reindex().unwrap();
    });
    assert!(latch.slots.load(Ordering::Relaxed) > 0);
    let hits = search_ids(&vault, "token_notes", "queued mineral");
    assert!(hits.contains(&"fresh".to_owned()), "{hits:?}");
}

#[test]
fn failing_embedder_leaves_current_on_old_generation() {
    let scratch = Scratch::new();
    let first = CountingEmbedder::new("model-a");
    let vault = open_tokens(&scratch, Some(Arc::clone(&first) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add("keep", rec(json!({"title": "keep mineral"})), None)
        .unwrap();
    let live = live_generation(&vault);
    drop(vault);
    struct Boom;
    impl Embedder for Boom {
        fn model_id(&self) -> &str {
            "boom"
        }
        fn dimensions(&self) -> NonZeroUsize {
            NonZeroUsize::new(4).unwrap()
        }
        fn many_limit(&self) -> NonZeroUsize {
            NonZeroUsize::new(32).unwrap()
        }
        fn embed_many(&self, _: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
            Err(scheda::Error::invalid("embed failed"))
        }
    }
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::new(Boom) as Arc<dyn Embedder>),
    )
    .unwrap();
    let err = vault.reindex().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(live_generation(&vault), live);
    assert_eq!(find_ids(&vault, "token_notes"), ["keep"]);
    assert!(
        !scratch
            .vault()
            .join(".scheda")
            .join("reindex")
            .join("active")
            .exists()
    );
}

#[test]
fn open_during_mismatch_rebuild_does_not_prune() {
    let scratch = Scratch::new();
    let first = CountingEmbedder::new("model-a");
    let vault = open_tokens(&scratch, Some(Arc::clone(&first) as Arc<dyn Embedder>));
    vault
        .collection("token_notes")
        .unwrap()
        .add("keep", rec(json!({"title": "keep mineral"})), None)
        .unwrap();
    let live = live_generation(&vault);
    drop(vault);
    let latch = LatchEmbedder::new("model-b");
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&latch) as Arc<dyn Embedder>),
    )
    .unwrap();
    let next = {
        let n: u64 = live.parse().unwrap();
        scratch
            .vault()
            .join(".scheda")
            .join("index")
            .join((n + 1).to_string())
    };
    thread::scope(|s| {
        s.spawn(|| {
            latch.wait_parked();
            let opened = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
            assert!(next.is_dir());
            assert_eq!(live_generation(&opened), live);
            latch.resume();
        });
        vault.reindex().unwrap();
    });
    let n: u64 = live.parse().unwrap();
    assert_eq!(live_generation(&vault), (n + 1).to_string());
}

#[test]
fn journal_queued_line_is_gettable_after_publish() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema(LOG);
    scratch.write_schema(TOKENS);
    let first = CountingEmbedder::new("model-a");
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&first) as Arc<dyn Embedder>),
    )
    .unwrap();
    vault
        .collection("token_notes")
        .unwrap()
        .add("seed", rec(json!({"title": "seed mineral"})), None)
        .unwrap();
    vault
        .collection("query_log")
        .unwrap()
        .add("keep", rec(json!({"tag": "a"})), None)
        .unwrap();
    drop(vault);
    let latch = LatchEmbedder::new("model-b");
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::clone(&latch) as Arc<dyn Embedder>),
    )
    .unwrap();
    let other = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    thread::scope(|s| {
        s.spawn(|| {
            latch.wait_parked();
            other
                .collection("query_log")
                .unwrap()
                .add("queued", rec(json!({"tag": "b"})), None)
                .unwrap();
            latch.resume();
        });
        vault.reindex().unwrap();
    });
    let got = vault
        .collection("query_log")
        .unwrap()
        .get("queued")
        .unwrap();
    assert_eq!(got.id.as_deref(), Some("queued"));
}
