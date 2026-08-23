//! C5: SCH-8 `find`, `scan`, `check`, `reindex`.
//! Collection names here and in `testdata/query/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, Filter, OpenOptions, Record, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c5-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn options(&self) -> OpenOptions {
        OpenOptions::new()
    }

    fn init(&self) -> Vault {
        Vault::init(self.vault(), self.options()).unwrap()
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), self.options()).unwrap()
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

const NOTES: &str = include_str!("../testdata/query/notes.yaml");
const PAGES: &str = include_str!("../testdata/query/pages.yaml");
const LOG: &str = include_str!("../testdata/query/log.yaml");

fn rec(collection: &str, id: &str, fields: Value) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": collection,
        "fields": fields,
    }))
    .unwrap()
}

fn filter(value: Value) -> Filter {
    serde_json::from_value(value).unwrap()
}

fn ids(records: &[Record]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record.id.as_deref().unwrap())
        .collect()
}

fn open_notes(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    scratch.write_schema("notes.yaml", NOTES);
    scratch.reopen()
}

#[test]
fn empty_filter_and_scan_return_every_record() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .insert_one(rec("query_notes", "a", json!({"title": "A", "tag": "red"})))
        .unwrap();
    collection
        .insert_one(rec(
            "query_notes",
            "b",
            json!({"title": "B", "tag": "blue"}),
        ))
        .unwrap();
    collection
        .insert_one(rec("query_notes", "c", json!({"title": "C"})))
        .unwrap();
    let found = collection.find(&Filter::default()).unwrap();
    assert_eq!(found.len(), 3);
    assert_eq!(collection.scan().unwrap().len(), 3);
}

#[test]
fn equal_set_exists_and_null() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .insert_one(rec(
            "query_notes",
            "red",
            json!({"title": "R", "tag": "red"}),
        ))
        .unwrap();
    collection
        .insert_one(rec(
            "query_notes",
            "blue",
            json!({"title": "B", "tag": "blue"}),
        ))
        .unwrap();
    collection
        .insert_one(rec(
            "query_notes",
            "nil",
            json!({"title": "N", "tag": null}),
        ))
        .unwrap();
    collection
        .insert_one(rec("query_notes", "omit", json!({"title": "O"})))
        .unwrap();

    let red = collection
        .find(&filter(
            json!({"clauses":[{"equal":{"field":"tag","value":"red"}}]}),
        ))
        .unwrap();
    assert_eq!(ids(&red), ["red"]);

    let set = collection
        .find(&filter(
            json!({"clauses":[{"set":{"field":"tag","values":["red","blue"]}}]}),
        ))
        .unwrap();
    assert_eq!(ids(&set), ["blue", "red"]);

    let missing = collection
        .find(&filter(
            json!({"clauses":[{"equal":{"field":"tag","value":null}}]}),
        ))
        .unwrap();
    assert_eq!(ids(&missing), ["nil"]);

    let exists = collection
        .find(&filter(json!({"clauses":[{"exists":{"field":"tag"}}]})))
        .unwrap();
    assert_eq!(ids(&exists), ["blue", "nil", "red"]);
}

#[test]
fn unmarked_field_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    let err = collection
        .find(&filter(
            json!({"clauses":[{"equal":{"field":"secret","value":"x"}}]}),
        ))
        .unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Invalid));
}

#[test]
fn document_path_prefix() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("pages.yaml", PAGES);
    let vault = scratch.reopen();
    let collection = vault.collection("query_pages").unwrap();
    collection
        .insert_one(rec("query_pages", "home", json!({"title": "Home"})))
        .unwrap();
    collection
        .insert_one(rec(
            "query_pages",
            "alpha",
            json!({"title": "A", "project": "one"}),
        ))
        .unwrap();
    let personal = collection
        .find(&filter(
            json!({"clauses":[{"pathPrefix":{"prefix":"personal/"}}]}),
        ))
        .unwrap();
    assert_eq!(ids(&personal), ["home"]);
    let projects = collection
        .find(&filter(
            json!({"clauses":[{"pathPrefix":{"prefix":"projects/"}}]}),
        ))
        .unwrap();
    assert_eq!(ids(&projects), ["alpha"]);
}

#[test]
fn journal_empty_find_keeps_append_order() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    let collection = vault.collection("query_log").unwrap();
    collection
        .insert_one(rec("query_log", "a", json!({"tag": "one"})))
        .unwrap();
    collection
        .insert_one(rec("query_log", "b", json!({"tag": "two"})))
        .unwrap();
    let found = collection
        .find(&filter(
            json!({"clauses":[{"equal":{"field":"tag","value":"one"}}]}),
        ))
        .unwrap();
    assert_eq!(ids(&found), ["a"]);
    assert_eq!(ids(&collection.scan().unwrap()), ["a", "b"]);
}

#[test]
fn check_reports_schema_failure_with_path() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("query_notes")
        .unwrap()
        .insert_one(rec("query_notes", "a", json!({"title": "A"})))
        .unwrap();
    drop(vault);
    scratch.write_schema(
        "notes.yaml",
        "name: query_notes\nkind: document\nschema_version: 1\nlayout: notes/{id}.md\nfields:\n  - name: title\n    type: string\n    required: true\n  - name: tag\n    type: string\n    required: true\n    filter: true\n",
    );
    let vault = scratch.reopen();
    let got = vault
        .collection("query_notes")
        .unwrap()
        .find_one("a")
        .unwrap();
    assert_eq!(got.valid, Some(false));
    let err = vault.check().unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Invalid));
    assert!(err.to_string().contains("a.md"));
}

#[test]
fn reindex_rebuilds_format_and_counts() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .insert_one(rec("query_notes", "a", json!({"title": "A"})))
        .unwrap();
    collection
        .insert_one(rec("query_notes", "b", json!({"title": "B"})))
        .unwrap();
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(2));
    assert_eq!(counts.skipped_invalid, Some(0));
    let format = fs::read_to_string(vault.index_dir().join("format")).unwrap();
    assert_eq!(format.trim(), "1");
    fs::write(vault.index_dir().join("format"), "999\n").unwrap();
    fs::write(vault.index_dir().join("stolen.json"), r#"{"id":"foreign"}"#).unwrap();
    let err = collection.search("foreign").unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Unsupported));
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(2));
    let format = fs::read_to_string(vault.index_dir().join("format")).unwrap();
    assert_eq!(format.trim(), "1");
    assert!(!vault.index_dir().join("stolen.json").exists());
    let got = collection.find_one("a").unwrap();
    assert_eq!(got.valid, Some(true));
}

#[test]
fn conflict_markers_fail_check() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .insert_one(rec("query_notes", "a", json!({"title": "A"})))
        .unwrap();
    let rel = collection.find_one("a").unwrap().path.unwrap();
    fs::write(vault.path().join(&rel), "<<<<<<< HEAD\nid: a\n>>>>>>>\n").unwrap();
    let got = collection.find_one("a").unwrap();
    assert_eq!(got.valid, Some(false));
    let err = vault.check().unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Conflict));
    assert!(err.to_string().contains(&rel));
}
