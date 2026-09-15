//! C10: core acceptance gates (`10` §2, `11` C10).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1). That grep is library source only;
//! test data must not be able to turn it red.

mod common;

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
        common::isolate_test_git_env();
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
        include_str!("../src/git.rs"),
        include_str!("../src/id.rs"),
        include_str!("../src/index.rs"),
        include_str!("../src/isolated.rs"),
        include_str!("../src/journal.rs"),
        include_str!("../src/json.rs"),
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
        .add("alpha", rec(json!({"title": "keep"})), None)
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
        .add("alpha", rec(json!({"title": "west"})), None)
        .unwrap();
    east_vault
        .collection("east_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "east"})), None)
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
        .find(&Filter::all().into(), 0, 1000)
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
        "sealed_log",
        "lessons_personal",
        "lessons_project",
        "entities",
        "profiles",
        "episodes",
        "events",
        "artifact_reachability",
        "chain_heads",
        "run_heads",
        "pending_items",
        "alias_bindings",
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
        .add("local", rec(json!({"title": "local"})), None)
        .unwrap();
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
    collection.add("ok", rec(fields.clone()), None).unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert_eq!(got.get("n").unwrap(), 0.5);
    assert_eq!(got.get("ol").unwrap().as_array().unwrap().len(), 2);

    let err = collection
        .add("extra", rec(json!({"s": "x", "nope": 1})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));

    let err = collection
        .add("frac", rec(json!({"i": 1.5})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn typed_open_map_round_trips_and_names_the_bad_key() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "maps.yaml",
        "\
name: c
kind: document
version: 1
fields:
  - name: env
    object:
      values:
        name: axis
        object:
          fields:
            - name: significant
              integer: {}
",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    let fields = json!({"env": {"prod": {"significant": 1}, "staging": {"significant": 0}}});
    collection.add("ok", rec(fields.clone()), None).unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert_eq!(
        got.get("env").unwrap()["prod"]["significant"].as_f64(),
        Some(1.0)
    );

    let err = collection
        .add("bad", rec(json!({"env": {"prod": "yes"}})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let message = err.to_string();
    assert!(message.contains("env"), "{message}");
    assert!(message.contains("axis"), "{message}");
    assert!(message.contains("prod"), "{message}");

    assert!(collection.is_filter_key("env"));
    assert!(!collection.is_filter_key("axis"));
    assert!(!collection.is_filter_key("prod"));
    assert!(!collection.is_filter_key("significant"));
    let err = collection
        .find(
            &serde_json::from_value::<Filter>(json!({"term":{"field":"axis","value":"prod"}}))
                .unwrap()
                .into(),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    vault.check().unwrap();
}

#[test]
fn typed_open_map_honours_nullable_on_the_value() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "nullable.yaml",
        "\
name: c
kind: document
version: 1
fields:
  - name: loose
    object:
      values:
        name: axis
        nullable: true
        integer: {}
  - name: strict
    object:
      values:
        name: axis
        integer: {}
",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    collection
        .add(
            "ok",
            rec(json!({"loose": {"prod": null}, "strict": {}})),
            None,
        )
        .unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert!(got["loose"]["prod"].is_null());

    let err = collection
        .add(
            "bad",
            rec(json!({"loose": {}, "strict": {"prod": null}})),
            None,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let message = err.to_string();
    assert!(message.contains("strict"), "{message}");
    assert!(message.contains("prod"), "{message}");
    assert!(message.contains("nullable"), "{message}");
}

#[test]
fn object_list_takes_a_typed_open_map_too() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "rows.yaml",
        "\
name: c
kind: document
version: 1
fields:
  - name: rows
    object_list:
      values:
        name: cell
        integer: {}
",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    collection
        .add(
            "ok",
            rec(json!({"rows": [{"a": 1}, {"b": 2, "c": 3}]})),
            None,
        )
        .unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert_eq!(got["rows"][1]["c"].as_f64(), Some(3.0));

    let err = collection
        .add("bad", rec(json!({"rows": [{"a": 1}, {"b": "2"}]})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let message = err.to_string();
    assert!(message.contains("rows"), "{message}");
    assert!(message.contains("cell"), "{message}");
    assert!(message.contains("key b"), "{message}");
}

#[test]
fn nested_open_maps_of_integer_accept_2_and_reject_string() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "grid.yaml",
        "\
name: c
kind: document
version: 1
fields:
  - name: grid
    object:
      values:
        name: row
        object:
          values:
            name: cell
            integer: {}
",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    collection
        .add("ok", rec(json!({"grid": {"r1": {"x": 2, "y": 3}}})), None)
        .unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert_eq!(got["grid"]["r1"]["x"].as_f64(), Some(2.0));

    let err = collection
        .add("bad", rec(json!({"grid": {"r1": {"x": "2"}}})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let message = err.to_string();
    assert!(message.contains("grid"), "{message}");
    assert!(message.contains("cell"), "{message}");
    assert!(message.contains("x"), "{message}");
}

#[test]
fn untyped_open_map_accepts_strings_and_rejects_a_number() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "open.yaml",
        "\
name: c
kind: document
version: 1
fields:
  - name: meta
    object: {}
",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    collection
        .add("ok", rec(json!({"meta": {"k": "v", "empty": ""}})), None)
        .unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert_eq!(got["meta"]["k"], "v");
    assert_eq!(got["meta"]["empty"], "");

    let err = collection
        .add("bad", rec(json!({"meta": {"k": 1}})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let message = err.to_string();
    assert!(message.contains("meta"), "{message}");
    assert!(message.contains("k"), "{message}");
    assert!(message.contains("must be a string"), "{message}");
}

#[test]
fn typed_open_map_may_be_omitted_from_the_record() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "maps.yaml",
        "\
name: c
kind: document
version: 1
fields:
  - name: env
    object:
      values:
        name: axis
        integer: {}
  - name: title
    string: {}
",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    collection
        .add("omit", rec(json!({"title": "none"})), None)
        .unwrap();
    let omitted = fields_of(&collection.get("omit").unwrap());
    assert_eq!(omitted.get("title").unwrap(), "none");
    assert!(!omitted.contains_key("env"));

    collection
        .add("empty", rec(json!({"title": "empty", "env": {}})), None)
        .unwrap();
    let empty = fields_of(&collection.get("empty").unwrap());
    assert_eq!(empty["env"], json!({}));
}

#[test]
fn journal_takes_a_typed_open_map_too() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "log.yaml",
        "\
name: c
kind: journal
version: 1
fields:
  - name: env
    object:
      values:
        name: axis
        integer: {}
",
    );
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let collection = vault.collection("c").unwrap();
    collection
        .add("ok", rec(json!({"env": {"prod": 1, "staging": 0}})), None)
        .unwrap();
    let got = fields_of(&collection.get("ok").unwrap());
    assert_eq!(got["env"]["prod"].as_f64(), Some(1.0));
    let line = fs::read_to_string(
        scratch
            .vault()
            .join("c")
            .join(format!("00000001-{}.jsonl", vault.writer())),
    )
    .unwrap();
    assert_eq!(line, "{\"env\":{\"prod\":1,\"staging\":0},\"id\":\"ok\"}\n");

    let err = collection
        .add("bad", rec(json!({"env": {"prod": "1"}})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let message = err.to_string();
    assert!(message.contains("env"), "{message}");
    assert!(message.contains("axis"), "{message}");
    assert!(message.contains("key prod"), "{message}");
    vault.check().unwrap();
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
