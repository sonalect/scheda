//! C10: core acceptance gates (`10` §2, `11` C10).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, Filter, OpenOptions, Record, RecordAdd, Vault};
use serde_json::{Map, Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c10-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn init(&self) -> Vault {
        let vault = Vault::open(self.vault(), OpenOptions::new(), None).unwrap();
        vault.begin().unwrap();
        vault
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

const WEST: &str = include_str!("../testdata/schema/west.yaml");
const EAST: &str = include_str!("../testdata/schema/east.yaml");
const TYPES: &str = include_str!("../testdata/schema/types.yaml");

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn fields_of(record: &Record) -> Map<String, Value> {
    match serde_json::to_value(record).unwrap().get("fields") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn contains_collection_name(haystack: &str, name: &str) -> bool {
    haystack.match_indices(name).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + name.len()..].chars().next();
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        before.is_none_or(|c| !ident(c)) && after.is_none_or(|c| !ident(c))
    })
}

fn read_src_rs() -> String {
    [
        include_str!("../src/ann.rs"),
        include_str!("../src/embed.rs"),
        include_str!("../src/error.rs"),
        include_str!("../src/fnv.rs"),
        include_str!("../src/git.rs"),
        include_str!("../src/id.rs"),
        include_str!("../src/index.rs"),
        include_str!("../src/journal.rs"),
        include_str!("../src/lib.rs"),
        include_str!("../src/query.rs"),
        include_str!("../src/schema.rs"),
        include_str!("../src/search.rs"),
        include_str!("../src/store.rs"),
        include_str!("../src/vault.rs"),
    ]
    .join("\n")
}

#[test]
fn deleting_index_does_not_lose_records() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    vault
        .collection("west_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "keep"})))
        .unwrap();
    let rel = vault
        .collection("west_docs")
        .unwrap()
        .get("alpha")
        .unwrap()
        .path
        .unwrap();
    let on_disk = fs::read_to_string(vault.path().join(&rel)).unwrap();
    assert!(!on_disk.starts_with('\u{feff}'));
    assert!(on_disk.contains("keep"));
    drop(vault);

    fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
    assert!(scratch.vault().join(&rel).is_file());

    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let got = vault.collection("west_docs").unwrap().get("alpha").unwrap();
    assert_eq!(got.valid, Some(true));
    assert_eq!(fields_of(&got).get("title").unwrap(), "keep");
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(1));
    assert_eq!(counts.skipped_invalid, Some(0));
    let again = vault.collection("west_docs").unwrap().get("alpha").unwrap();
    assert_eq!(fields_of(&again).get("title").unwrap(), "keep");
}

#[test]
fn records_do_not_cross_vaults() {
    let west = Scratch::new();
    let east = Scratch::new();
    drop(west.init());
    drop(east.init());
    west.write_schema("west.yaml", WEST);
    east.write_schema("east.yaml", EAST);
    let west_vault = Vault::open(west.vault(), OpenOptions::new(), None).unwrap();
    let east_vault = Vault::open(east.vault(), OpenOptions::new(), None).unwrap();
    west_vault
        .collection("west_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "west"})))
        .unwrap();
    east_vault
        .collection("east_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "east"})))
        .unwrap();
    assert!(west_vault.collection("east_docs").is_err());
    assert!(east_vault.collection("west_docs").is_err());
    let west_hit = west_vault
        .collection("west_docs")
        .unwrap()
        .get("alpha")
        .unwrap();
    assert_eq!(fields_of(&west_hit).get("title").unwrap(), "west");
    let east_scan = east_vault
        .collection("east_docs")
        .unwrap()
        .find(&Filter::all(), 0, 1000)
        .unwrap();
    assert_eq!(east_scan.records.len(), 1);
    assert_eq!(
        fields_of(&east_scan.records[0]).get("title").unwrap(),
        "east"
    );
}

#[test]
fn src_does_not_name_consumer_collections() {
    let haystack = read_src_rs();
    for name in [
        "west_docs",
        "east_docs",
        "east_log",
        "north_notes",
        "north_slugs",
        "north_pages",
        "north_inbox",
        "north_log",
        "south_log",
        "query_notes",
        "query_log",
        "query_pages",
        "token_notes",
        "walk_src",
        "walk_dst",
        "walk_strict",
        "west_indexed",
        "west_isolated",
    ] {
        assert!(
            !contains_collection_name(&haystack, name),
            "{name} must not appear in rust/scheda/src"
        );
    }
}

#[test]
fn writes_and_commit_do_not_need_the_network() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    assert!(!vault.auto_sync());
    vault
        .collection("west_docs")
        .unwrap()
        .add("local", rec(json!({"title": "local"})))
        .unwrap();
    vault.commit("local only").unwrap();
    vault.reindex().unwrap();
    let got = vault.collection("west_docs").unwrap().get("local").unwrap();
    assert_eq!(fields_of(&got).get("title").unwrap(), "local");
}

#[test]
fn field_types_reject_unknown_and_round_trip() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("types.yaml", TYPES);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    let fields = json!({
        "s": "hi",
        "i": 1,
        "n": 0.5,
        "b": true,
        "sl": ["a"],
        "o": {"k": "v"},
        "ol": [{"a": "1"}, {"b": "2"}],
    });
    collection.add("ok", rec(fields.clone())).unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert_eq!(got.get("n").unwrap(), 0.5);
    assert_eq!(got.get("ol").unwrap().as_array().unwrap().len(), 2);

    let err = collection
        .add("extra", rec(json!({"s": "x", "nope": 1})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));

    let err = collection.add("frac", rec(json!({"i": 1.5}))).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
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
