//! Reduced crate: `find` and `search` off. Writes still work; the Index is
//! marked stale. Collection names here must not appear in `rust/scheda/src`.

use std::fs;
use std::path::PathBuf;
use std::process;
#[cfg(not(feature = "search"))]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, OpenOptions, Record, RecordAdd, RecordReplace, Vault};
#[cfg(not(feature = "search"))]
use scheda::{Embedder, Error};
use serde_json::{Map, Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-thin-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const NOTES: &str = r#"name: thin_notes
kind: document
version: 1
fields:
  - name: title
    required: true
    string: {}
"#;

#[cfg(not(feature = "search"))]
struct StubEmbedder;

#[cfg(not(feature = "search"))]
impl Embedder for StubEmbedder {
    fn model_id(&self) -> &str {
        "stub"
    }

    fn dimensions(&self) -> usize {
        4
    }

    fn embed(&self, _text: &str) -> Result<Vec<f32>, Error> {
        Ok(vec![0.0; 4])
    }
}

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn rec_rep(fields: Value) -> RecordReplace {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn fields_of(record: &Record) -> Map<String, Value> {
    match serde_json::to_value(record).unwrap().get("fields") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn open_notes(scratch: &Scratch) -> Vault {
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    let dir = scratch.vault().join("thin_notes");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".schema.yaml"), NOTES).unwrap();
    Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap()
}

#[test]
fn add_get_round_trip_without_lexical_index() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(fields_of(&got)["title"], "Keep");
    assert_eq!(got.valid, Some(true));
}

#[test]
fn documents_and_ids_without_find() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("charlie", rec(json!({"title": "c"})), None)
        .unwrap();
    collection
        .add("alpha", rec(json!({"title": "a"})), None)
        .unwrap();
    collection
        .add("bravo", rec(json!({"title": "b"})), None)
        .unwrap();
    assert_eq!(collection.ids().unwrap(), ["alpha", "bravo", "charlie"]);
    let ids: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["alpha", "bravo", "charlie"]);
}

#[cfg(not(feature = "find"))]
#[test]
fn add_marks_index_stale_and_check_ignores_it() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault.check().unwrap();
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    assert!(vault.index_dir().join("stale").is_file());
    vault.check().unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.valid, Some(true));
}

#[test]
fn replace_uses_the_record_file() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "First"})), None)
        .unwrap();
    collection
        .replace("alpha", rec_rep(json!({"title": "Second"})), None)
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(fields_of(&got)["title"], "Second");
    let err = collection
        .replace("missing", rec_rep(json!({"title": "No"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn remove_uses_the_record_file() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "First"})), None)
        .unwrap();
    collection.remove("alpha", None).unwrap();
    assert_eq!(
        collection.get("alpha").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert!(
        !scratch
            .vault()
            .join("thin_notes")
            .join("alpha.md")
            .is_file()
    );
    let err = collection.remove("missing", None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[cfg(not(feature = "find"))]
#[test]
fn remove_marks_index_stale() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    collection.remove("alpha", None).unwrap();
    assert!(vault.index_dir().join("stale").is_file());
    vault.check().unwrap();
}

#[cfg(not(feature = "find"))]
#[test]
fn replace_of_on_disk_file_does_not_need_the_index() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    let path = scratch.vault().join("thin_notes").join("orphan.md");
    fs::write(&path, "---\nid: orphan\ntitle: Ghost\n---\n").unwrap();
    collection
        .replace("orphan", rec_rep(json!({"title": "New"})), None)
        .unwrap();
    let got = collection.get("orphan").unwrap();
    assert_eq!(fields_of(&got)["title"], "New");
}

#[cfg(not(feature = "search"))]
#[test]
fn open_with_embedder_fails_vector_leg() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    let err = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(Arc::new(StubEmbedder)),
    )
    .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexVector));
}

#[cfg(not(feature = "find"))]
#[test]
fn reindex_without_find_marks_stale() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.reindex().unwrap();
    assert!(vault.index_dir().join("stale").is_file());
    vault.check().unwrap();
}
