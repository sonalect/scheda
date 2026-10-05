//! Reduced crate: `find` and `search` off. Writes still work and never touch
//! the Index. Collection names here must not appear in `rust/scheda/src`.

mod common;

use std::fs;
#[cfg(not(feature = "search"))]
use std::num::NonZeroUsize;
use std::path::PathBuf;
#[cfg(not(feature = "search"))]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, OpenOptions, Record, Schema, Vault};
#[cfg(not(feature = "search"))]
use scheda::{Embedder, Error};
use serde_json::{Map, Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-thin", n),
        }
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

const NOTES: &str = r#"version: 1
name: thin_notes
schema:
  document: {}
"#;

#[cfg(not(feature = "search"))]
struct StubEmbedder;

#[cfg(not(feature = "search"))]
impl Embedder for StubEmbedder {
    fn model_id(&self) -> &str {
        "stub"
    }

    fn dimensions(&self) -> NonZeroUsize {
        NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn embed(&self, _text: &str) -> Result<Vec<f32>, Error> {
        Ok(vec![0.0; 4])
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        texts.iter().map(|text| self.embed(text)).collect()
    }
}

fn rec(fields: Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn rec_rep(fields: Value) -> Vec<u8> {
    rec(fields)
}

fn fields_of(record: &Record) -> Map<String, Value> {
    match serde_json::from_slice(record.raw.as_deref().unwrap_or(&[])) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn open_notes(scratch: &Scratch) -> Vault {
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    let dir = scratch.vault().join("thin_notes");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".collection.yaml"), NOTES).unwrap();
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
}

#[test]
fn isolated_round_trip_and_documents_without_find() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let mut schema = Schema::default().with_isolated(true);
    schema.kind = scheda::Unit::default().into();
    vault
        .create_collection("thin_sealed", schema, None)
        .unwrap();
    let collection = vault.collection("thin_sealed").unwrap();
    collection
        .add("secret", rec(json!({"title": "nested"})), None)
        .unwrap();
    let got = collection.get("secret").unwrap();
    assert_eq!(fields_of(&got)["title"], "nested");
    assert_eq!(got.id.as_deref(), Some("secret"));
    let ids: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["secret"]);
    assert!(!scratch.vault().join(".isolated").join(".scheda").exists());
}

/// A handle opened before another handle declared a collection finds it
/// by name without reopening: the miss reads the declarations again.
#[test]
fn a_collection_declared_after_open_is_found_without_find() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    let reader = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let writer = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let missing = reader.collection("thin_later").unwrap_err();
    assert_eq!(missing.known_code(), Some(Code::NotFound));
    let schema = Schema {
        kind: scheda::Unit::default().into(),
        ..Schema::default()
    };
    writer
        .create_collection("thin_later", schema, None)
        .unwrap()
        .add("one", rec(json!({"title": "later"})), None)
        .unwrap();
    let found = reader.collection("thin_later").unwrap();
    assert_eq!(found.get("one").unwrap().id.as_deref(), Some("one"));
    let names: Vec<_> = reader
        .info()
        .unwrap()
        .collections
        .iter()
        .filter_map(|item| item.name.clone())
        .collect();
    assert!(names.iter().any(|name| name == "thin_later"), "{names:?}");
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
fn add_without_find_keeps_check_passing() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault.check().unwrap();
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.check().unwrap();
    let _got = collection.get("alpha").unwrap();
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
    assert!(!scratch.vault().join("thin_notes").join("alpha").is_file());
    let err = collection.remove("missing", None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[cfg(not(feature = "find"))]
#[test]
fn remove_without_find_keeps_check_passing() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    collection.remove("alpha", None).unwrap();
    vault.check().unwrap();
}

#[cfg(not(feature = "find"))]
#[test]
fn replace_of_on_disk_file_does_not_need_the_index() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    let path = scratch.vault().join("thin_notes").join("orphan");
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
fn reindex_without_find_keeps_check_passing() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.reindex().unwrap();
    vault.check().unwrap();
}

#[cfg(not(feature = "encrypt"))]
#[test]
fn open_with_secret_is_unsupported_encryption() {
    use scheda::Encryption;
    let scratch = Scratch::new();
    let err = Vault::open(
        scratch.vault(),
        OpenOptions::new().encryption(Encryption::new().passphrase("thin-passphrase")),
        None,
    )
    .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::UnsupportedEncryption));
    assert!(!scratch.vault().exists());
}

#[cfg(not(feature = "encrypt"))]
#[test]
fn vault_with_encrypted_collection_opens_plain_collections() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    drop(vault);
    let dir = scratch.vault().join("sealed_thin");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(".collection.yaml"),
        "version: 1
name: sealed_thin
schema:
  document: {}
  encrypted: true
",
    )
    .unwrap();
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let err = vault.collection("sealed_thin").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::UnsupportedEncryption));
    let collection = vault.collection("thin_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(fields_of(&got)["title"], "Keep");
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::UnsupportedEncryption));
}

#[cfg(not(feature = "encrypt"))]
#[test]
fn create_encrypted_collection_is_unsupported_encryption() {
    use scheda::{Schema, Unit};
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault.begin().unwrap();
    let mut schema = Schema::default().with_encrypted(true);
    schema.kind = Unit::default().into();
    let err = vault
        .create_collection("sealed_thin", schema, None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::UnsupportedEncryption));
    assert!(
        !scratch
            .vault()
            .join("sealed_thin")
            .join(".collection.yaml")
            .is_file()
    );
}
