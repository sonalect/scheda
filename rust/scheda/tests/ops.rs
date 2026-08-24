//! C8: journal `id` map and OPS-1 / OPS-3 size (IDX-5, LIB-16).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scheda::{Code, Embedder, OpenOptions, Record, SearchQuery, Vault, collection_dir};
use serde_json::json;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c8-{}-{n}", process::id()));
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

const LOG: &str = include_str!("../testdata/journal/log.yaml");

const OPS_JOURNAL: u64 = 10_000;
const OPS_DOCS: u64 = 1_000;
const OP_BUDGET: Duration = Duration::from_millis(100);

const OPS_LOG: &str = "\
name: ops_log
kind: journal
schema_version: 1
layout: ops/log/
fields:
  - name: msg
    type: string
    fts: true
    vector: true
";

const OPS_NOTES: &str = "\
name: north_notes
kind: document
schema_version: 1
layout: north/{id}.md
fields:
  - name: title
    type: string
    required: true
    fts: true
    vector: true
  - name: tag
    type: string
    nullable: true
  - name: count
    type: integer
";

struct OpsEmbedder;

impl Embedder for OpsEmbedder {
    fn model_id(&self) -> &str {
        "ops-stub"
    }

    fn dimensions(&self) -> usize {
        8
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, scheda::Error> {
        let mut vector = vec![0.0; 8];
        for (i, byte) in text.bytes().enumerate() {
            vector[i % 8] += f32::from(byte) / 255.0;
        }
        Ok(vector)
    }
}

fn rec_log(collection: &str, id: &str, msg: &str) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": collection,
        "fields": {"msg": msg},
    }))
    .unwrap()
}

fn rec_note(id: &str, title: &str) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": "north_notes",
        "fields": {"title": title},
    }))
    .unwrap()
}

fn open_log(scratch: &Scratch) -> Vault {
    drop(Vault::init(scratch.vault(), OpenOptions::new()).unwrap());
    scratch.write_schema("log.yaml", LOG);
    Vault::open(scratch.vault(), OpenOptions::new()).unwrap()
}

#[test]
fn find_one_does_not_read_unrelated_segments() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection
        .insert_one(rec_log("south_log", "early", "x"))
        .unwrap();
    collection
        .insert_one(rec_log("south_log", "mid", "x"))
        .unwrap();
    collection
        .insert_one(rec_log("south_log", "late", "x"))
        .unwrap();
    let log_dir = scratch
        .vault()
        .join(collection_dir("south_log"))
        .join("south/log");
    assert!(log_dir.join("00000001.jsonl").is_file());
    fs::write(log_dir.join("ffffffff.jsonl"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    let started = Instant::now();
    let got = collection.find_one("early").unwrap();
    assert!(started.elapsed() < OP_BUDGET);
    assert_eq!(got.id.as_deref(), Some("early"));
    let err = collection.find_one("missing").unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::NotFound));
}

#[test]
fn writer_updates_map_human_edit_needs_reindex() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection
        .insert_one(rec_log("south_log", "a", "x"))
        .unwrap();
    let ids = vault
        .index_dir()
        .join("journal")
        .join(collection_dir("south_log"))
        .join("ids");
    assert!(ids.is_file());
    let log = scratch
        .vault()
        .join(collection_dir("south_log"))
        .join("south/log")
        .join("00000001.jsonl");
    let mut text = fs::read_to_string(&log).unwrap();
    text.push_str(r#"{"id":"human","msg":"edit"}"#);
    text.push('\n');
    fs::write(&log, text).unwrap();
    let err = collection.find_one("human").unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::NotFound));
    vault.reindex().unwrap();
    let got = collection.find_one("human").unwrap();
    assert_eq!(got.id.as_deref(), Some("human"));
}

#[test]
fn missing_map_falls_back_to_scan() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection
        .insert_one(rec_log("south_log", "kept", "x"))
        .unwrap();
    let journal_index = vault.index_dir().join("journal");
    fs::remove_dir_all(&journal_index).unwrap();
    drop(vault);
    let vault = Vault::open(scratch.vault(), OpenOptions::new()).unwrap();
    let got = vault
        .collection("south_log")
        .unwrap()
        .find_one("kept")
        .unwrap();
    assert_eq!(got.id.as_deref(), Some("kept"));
}

#[test]
fn ops_fixture_find_and_insert_and_reindex() {
    let scratch = Scratch::new();
    drop(Vault::init(scratch.vault(), OpenOptions::new()).unwrap());
    scratch.write_schema("log.yaml", OPS_LOG);
    scratch.write_schema("notes.yaml", OPS_NOTES);
    let vault = Vault::open(scratch.vault(), OpenOptions::new())
        .unwrap()
        .with_embedder(std::sync::Arc::new(OpsEmbedder));
    let log = vault.collection("ops_log").unwrap();
    for i in 0..OPS_JOURNAL {
        log.insert_one(rec_log("ops_log", &format!("j{i:05}"), "x"))
            .unwrap();
    }
    let notes = vault.collection("north_notes").unwrap();
    for i in 0..OPS_DOCS {
        notes
            .insert_one(rec_note(&format!("d{i:04}"), &format!("t{i}")))
            .unwrap();
    }
    for id in ["j00000", "j05000", "j09999"] {
        let started = Instant::now();
        let got = log.find_one(id).unwrap();
        assert!(
            started.elapsed() < OP_BUDGET,
            "find_one({id}) took {:?}",
            started.elapsed()
        );
        assert_eq!(got.id.as_deref(), Some(id));
    }
    let started = Instant::now();
    notes.find_one("d0000").unwrap();
    assert!(started.elapsed() < OP_BUDGET);
    let started = Instant::now();
    notes.find_one("d0999").unwrap();
    assert!(started.elapsed() < OP_BUDGET);
    let started = Instant::now();
    log.insert_one(rec_log("ops_log", "j10000", "x")).unwrap();
    assert!(
        started.elapsed() < OP_BUDGET,
        "insert_one took {:?}",
        started.elapsed()
    );
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(OPS_JOURNAL + OPS_DOCS + 1));
    assert_eq!(counts.skipped_invalid, Some(0));
    assert_eq!(counts.segments_seen, Some(1));
    let format = fs::read_to_string(vault.index_dir().join("format")).unwrap();
    assert_eq!(format.trim(), "1");
    let started = Instant::now();
    let hits = notes
        .search(&SearchQuery::default().with_text("t500").with_k(10))
        .unwrap();
    assert!(
        started.elapsed() < OP_BUDGET,
        "search took {:?}",
        started.elapsed()
    );
    assert!(
        hits.hits
            .iter()
            .any(|hit| hit.id.as_deref() == Some("d0500")),
        "OPS-2 expected d0500 in k"
    );
}
