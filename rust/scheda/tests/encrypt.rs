//! Encrypted `document` / `singleton` collections (`encrypt` crate feature).
//! Collection names here must not appear in `rust/scheda/src`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Body, Code, Encryption, Journal, OpenOptions, RecordAdd, Schema, Singleton, Vault};
use serde_json::{Map, Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

const PASS: &str = "p14-test-passphrase-never-on-disk";
const PLAIN: &str = "fixture-plaintext-body-token";
const IDENTITY: &str = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-encrypt-{}-{n}", process::id()));
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

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields, "body": PLAIN })).unwrap()
}

fn rec_fields(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn fields_of(record: &scheda::Record) -> Map<String, Value> {
    match serde_json::to_value(record).unwrap().get("fields") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn passphrase() -> Encryption {
    Encryption::new().passphrase(PASS)
}

fn document_schema(encrypted: bool, isolated: bool) -> Schema {
    let mut schema = Schema::default().with_version(1);
    schema.kind = Body::default().into();
    if encrypted {
        schema = schema.with_encrypted(true);
    }
    if isolated {
        schema = schema.with_isolated(true);
    }
    schema.fields = vec![serde_json::from_value(json!({"name": "title", "string": {}})).unwrap()];
    schema
}

fn singleton_schema() -> Schema {
    let mut schema = Schema::default().with_version(1).with_encrypted(true);
    schema.kind = Singleton::default().with_id("inbox").into();
    schema.fields = vec![serde_json::from_value(json!({"name": "title", "string": {}})).unwrap()];
    schema
}

fn open_encrypted(scratch: &Scratch, secret: Encryption) -> Vault {
    Vault::open(scratch.vault(), OpenOptions::new().encryption(secret), None).unwrap()
}

fn tree_contains(root: &Path, needle: &str) -> bool {
    fn walk(path: &Path, needle: &str) -> bool {
        let Ok(meta) = fs::metadata(path) else {
            return false;
        };
        if meta.is_file() {
            return fs::read(path)
                .ok()
                .is_some_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()));
        }
        if meta.is_dir() {
            let Ok(entries) = fs::read_dir(path) else {
                return false;
            };
            for entry in entries.flatten() {
                if walk(&entry.path(), needle) {
                    return true;
                }
            }
        }
        false
    }
    walk(root, needle)
}

#[test]
fn passphrase_add_get_round_trip() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    let collection = vault.collection("sealed_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(fields_of(&got)["title"], "Keep");
    assert_eq!(got.body.as_deref(), Some(PLAIN));
    assert_eq!(got.valid, Some(true));
}

#[test]
fn identity_add_get_round_trip() {
    let scratch = Scratch::new();
    let secret = Encryption::new().identity(IDENTITY);
    let vault = open_encrypted(&scratch, secret);
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    let collection = vault.collection("sealed_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Id"})), None)
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(fields_of(&got)["title"], "Id");
    assert_eq!(got.valid, Some(true));
    assert!(!tree_contains(&scratch.vault(), IDENTITY));
}

#[test]
fn on_disk_is_armor_without_plaintext_or_secret() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.commit("encrypted").unwrap();
    let on_disk = fs::read(scratch.vault().join("sealed_notes").join("alpha.md")).unwrap();
    let text = String::from_utf8(on_disk.clone()).unwrap();
    assert!(text.starts_with("-----BEGIN AGE ENCRYPTED FILE-----"));
    assert!(!text.contains('\r'));
    assert!(!on_disk.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert!(!text.contains(PLAIN));
    assert!(!text.contains("Keep"));
    assert!(!tree_contains(&scratch.vault(), PASS));
    assert!(
        !scratch
            .vault()
            .join(".isolated")
            .join(".encrypted")
            .exists()
    );
}

#[test]
fn wrong_passphrase_and_missing_secret_are_crypto_ids_work() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    drop(vault);

    let wrong = Vault::open(
        scratch.vault(),
        OpenOptions::new().encryption(Encryption::new().passphrase("wrong-passphrase")),
        None,
    )
    .unwrap();
    let err = wrong
        .collection("sealed_notes")
        .unwrap()
        .get("alpha")
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Crypto));
    let msg = err.message.as_deref().unwrap_or("");
    assert!(!msg.contains(PASS));
    assert!(!msg.contains(PLAIN));

    let none = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = none.collection("sealed_notes").unwrap();
    let err = collection.get("alpha").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Crypto));
    let err = match collection.documents() {
        Err(err) => err,
        Ok(_) => panic!("documents without secret must fail"),
    };
    assert_eq!(err.known_code(), Some(Code::Crypto));
    assert_eq!(collection.ids().unwrap(), ["alpha"]);
}

#[test]
fn encrypted_journal_schema_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    let mut schema = Schema::default().with_version(1).with_encrypted(true);
    schema.kind = Journal::default().into();
    let err = vault
        .create_collection("sealed_log", schema, None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(
        err.message
            .as_deref()
            .is_some_and(|m| m.contains("journal collections cannot be encrypted"))
    );
}

#[test]
fn info_reports_encrypted_and_omits_fields() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    let info = vault.info().unwrap();
    let item = info
        .collections
        .iter()
        .find(|c| c.name.as_deref() == Some("sealed_notes"))
        .expect("listed");
    assert_eq!(item.schema.encrypted, Some(true));
    assert!(item.schema.fields.is_empty());
}

#[cfg(feature = "search")]
#[test]
fn search_on_encrypted_collection_is_unsupported() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let err = vault
        .collection("sealed_notes")
        .unwrap()
        .search(&scheda::SearchQuery::default().with_text("Keep"), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    let err = vault
        .search(
            &["sealed_notes"],
            &scheda::SearchQuery::default().with_text("Keep"),
            None,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[cfg(feature = "find")]
#[test]
fn find_on_encrypted_collection_is_unsupported() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let err = vault
        .collection("sealed_notes")
        .unwrap()
        .find(&scheda::FindQuery::all(), 0, 10)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    let err = vault
        .find(&["sealed_notes"], &scheda::FindQuery::all(), 0, 10)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn encrypted_plaintext_is_absent_from_the_index() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.commit("encrypted").unwrap();
    drop(vault);
    let scheda = scratch.vault().join(".scheda");
    assert!(
        !tree_contains(&scheda, PLAIN),
        "encrypted plaintext leaked into .scheda"
    );
    assert!(
        !tree_contains(&scheda, "Keep"),
        "encrypted field leaked into .scheda"
    );
    let vault = open_encrypted(&scratch, passphrase());
    vault.reindex().unwrap();
    drop(vault);
    assert!(
        !tree_contains(&scheda, PLAIN),
        "encrypted plaintext leaked into .scheda after reindex"
    );
    assert!(
        !tree_contains(&scheda, "Keep"),
        "encrypted field leaked into .scheda after reindex"
    );
}

#[test]
fn flipping_encrypted_on_nonempty_is_invalid_files_unchanged() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let before = fs::read(scratch.vault().join("sealed_notes").join("alpha.md")).unwrap();
    let err = vault
        .create_collection("sealed_notes", document_schema(false, false), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let after = fs::read(scratch.vault().join("sealed_notes").join("alpha.md")).unwrap();
    assert_eq!(before, after);
    let got = vault
        .collection("sealed_notes")
        .unwrap()
        .get("alpha")
        .unwrap();
    assert_eq!(fields_of(&got)["title"], "Keep");
}

#[test]
fn history_of_encrypted_record_is_plaintext() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    let collection = vault.collection("sealed_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.commit("first").unwrap();
    vault.begin().unwrap();
    collection
        .replace(
            "alpha",
            serde_json::from_value(json!({ "fields": {"title": "Next"}, "body": PLAIN })).unwrap(),
            None,
        )
        .unwrap();
    vault.commit("second").unwrap();
    let snaps = collection.history("alpha", 0, 100).unwrap();
    assert_eq!(snaps.entries.len(), 2);
    const ARMOR: &[u8] = b"-----BEGIN AGE ENCRYPTED FILE-----";
    for (i, entry) in snaps.entries.iter().enumerate() {
        let raw = entry.raw.as_deref().unwrap();
        assert!(!raw.is_empty(), "entry {i} empty");
        assert!(!raw.starts_with(ARMOR), "entry {i} still armor");
    }
    let newest = String::from_utf8(snaps.entries[0].raw.clone().unwrap()).unwrap();
    let oldest = String::from_utf8(snaps.entries[1].raw.clone().unwrap()).unwrap();
    assert!(newest.contains("Next"), "{newest}");
    assert!(!newest.contains("Keep"), "{newest}");
    assert!(oldest.contains("Keep"), "{oldest}");
    assert!(!oldest.contains("Next"), "{oldest}");
    drop(vault);

    let none = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let err = none
        .collection("sealed_notes")
        .unwrap()
        .history("alpha", 0, 100)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Crypto));
}

#[test]
fn replace_and_remove_encrypted() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    let collection = vault.collection("sealed_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    collection
        .replace(
            "alpha",
            serde_json::from_value(json!({ "fields": {"title": "Next"}, "body": PLAIN })).unwrap(),
            None,
        )
        .unwrap();
    assert_eq!(
        fields_of(&collection.get("alpha").unwrap())["title"],
        "Next"
    );
    collection.remove("alpha", None).unwrap();
    assert_eq!(collection.ids().unwrap(), [] as [&str; 0]);
}

#[test]
fn encrypted_isolated_and_non_isolated_round_trip() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .create_collection("sealed_local", document_schema(true, true), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Main"})), None)
        .unwrap();
    vault
        .collection("sealed_local")
        .unwrap()
        .add("beta", rec(json!({"title": "Nested"})), None)
        .unwrap();
    assert_eq!(
        fields_of(
            &vault
                .collection("sealed_notes")
                .unwrap()
                .get("alpha")
                .unwrap()
        )["title"],
        "Main"
    );
    assert_eq!(
        fields_of(
            &vault
                .collection("sealed_local")
                .unwrap()
                .get("beta")
                .unwrap()
        )["title"],
        "Nested"
    );
    assert!(
        scratch
            .vault()
            .join(".isolated")
            .join("sealed_local")
            .join("beta.md")
            .is_file()
    );
}

#[test]
fn documents_walks_encrypted_collection() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    let collection = vault.collection("sealed_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    collection
        .add("bravo", rec(json!({"title": "Two"})), None)
        .unwrap();
    let ids: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["alpha", "bravo"]);
}

#[test]
fn check_with_and_without_secret() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.check().unwrap();
    drop(vault);

    let none = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let err = none.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Crypto));
    assert!(
        err.message
            .as_deref()
            .is_some_and(|m| m.contains("not checked"))
    );
}

#[test]
fn singleton_encrypted_round_trip() {
    let scratch = Scratch::new();
    let vault = open_encrypted(&scratch, passphrase());
    vault.begin().unwrap();
    vault
        .create_collection("sealed_inbox", singleton_schema(), None)
        .unwrap();
    vault
        .collection("sealed_inbox")
        .unwrap()
        .add("inbox", rec_fields(json!({"title": "One"})), None)
        .unwrap();
    let got = vault
        .collection("sealed_inbox")
        .unwrap()
        .get("inbox")
        .unwrap();
    assert_eq!(fields_of(&got)["title"], "One");
}

#[test]
fn age_cli_decrypts_scheda_file() {
    if Command::new("age").arg("--version").output().is_err() {
        return;
    }
    let scratch = Scratch::new();
    let secret = Encryption::new().identity(IDENTITY);
    let vault = open_encrypted(&scratch, secret);
    vault.begin().unwrap();
    vault
        .create_collection("sealed_notes", document_schema(true, false), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let file = scratch.vault().join("sealed_notes").join("alpha.md");
    let key_path = scratch.root.join("identity.txt");
    fs::write(&key_path, IDENTITY).unwrap();
    let out = Command::new("age")
        .args(["-d", "-i"])
        .arg(&key_path)
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "age -d: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("Keep"));
    assert!(text.contains(PLAIN));
}
