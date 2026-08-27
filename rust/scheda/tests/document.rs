//! C3: `document` / `singleton` writes and `find_one` (`03`, `07`, `11`).
//! Collection names here and in `testdata/document/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, OpenOptions, Record, RecordAdd, RecordReplace, Vault};
use serde_json::{Map, Value};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c3-{}-{n}", process::id()));
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
        let vault = Vault::open(self.vault(), self.options(), None).unwrap();
        vault.begin().unwrap();
        vault
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), self.options(), None).unwrap()
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

    fn coll_file(&self, collection: &str, rel: &str) -> PathBuf {
        self.vault().join(collection).join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn coll_rel(collection: &str, rel: &str) -> String {
    format!("{}/{rel}", collection)
}

const NOTES: &str = include_str!("../testdata/document/notes.yaml");
const NOTES_PLUS: &str = include_str!("../testdata/document/notes_plus.yaml");
const NOTES_REQUIRED: &str = include_str!("../testdata/document/notes_required.yaml");
const PAGES: &str = include_str!("../testdata/document/pages.yaml");
const SLUGS: &str = include_str!("../testdata/document/slugs.yaml");
const INBOX: &str = include_str!("../testdata/document/inbox.yaml");
const LOG: &str = include_str!("../testdata/document/log.yaml");
const ALPHA_MD: &str = include_str!("../testdata/document/alpha.md");
const PLAIN_YAML: &str = include_str!("../testdata/document/plain.yaml");

fn rec(fields: Value) -> RecordAdd {
    rec_body(fields, None)
}

fn rec_body(fields: Value, body: Option<&str>) -> RecordAdd {
    let mut value = serde_json::json!({ "fields": fields });
    if let Some(body) = body {
        value["body"] = Value::String(body.to_owned());
    }
    serde_json::from_value(value).unwrap()
}

fn rec_rep(fields: Value) -> RecordReplace {
    rec_rep_body(fields, None)
}

fn rec_rep_body(fields: Value, body: Option<&str>) -> RecordReplace {
    let mut value = serde_json::json!({ "fields": fields });
    if let Some(body) = body {
        value["body"] = Value::String(body.to_owned());
    }
    serde_json::from_value(value).unwrap()
}

fn fields_of(record: &Record) -> Map<String, Value> {
    match serde_json::to_value(record).unwrap().get("fields") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn tmp_names(dir: &std::path::Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.contains(".tmp").then_some(name)
        })
        .collect()
}

fn open_notes(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    scratch.write_schema("notes.yaml", NOTES);
    scratch.reopen()
}

#[test]
fn insert_replace_find_one_round_trip() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "alpha",
            rec_body(
                serde_json::json!({"title": "Hello", "count": 2}),
                Some("Stored body.\n"),
            ),
        )
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    assert_eq!(got.collection.as_deref(), Some("north_notes"));
    assert_eq!(got.valid, Some(true));
    assert_eq!(got.path, Some(coll_rel("north_notes", "alpha.md")));
    assert_eq!(got.body.as_deref(), Some("Stored body.\n"));
    let fields = fields_of(&got);
    assert_eq!(fields["title"], "Hello");
    assert!(
        fields["count"].as_i64() == Some(2)
            || fields["count"].as_u64() == Some(2)
            || fields["count"].as_f64() == Some(2.0)
    );
    assert!(!got.raw.as_ref().unwrap().is_empty());

    collection
        .replace(
            "alpha",
            rec_rep_body(serde_json::json!({"title": "Later"}), Some("New body.\n")),
        )
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(fields_of(&got)["title"], "Later");
    assert_eq!(got.body.as_deref(), Some("New body.\n"));
    let text = fs::read_to_string(scratch.coll_file("north_notes", "alpha.md")).unwrap();
    assert!(!text.contains("Hello"));
    assert!(text.contains("Later"));
}

#[test]
fn missing_id_is_not_found() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let err = vault
        .collection("north_notes")
        .unwrap()
        .get("missing")
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn markdown_and_yaml_only_and_empty_body() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "with-body",
            rec_body(serde_json::json!({"title": "Md"}), Some("Prose.\n")),
        )
        .unwrap();
    collection
        .add("empty", rec(serde_json::json!({"title": "None"})))
        .unwrap();
    let with_body = collection.get("with-body").unwrap();
    assert_eq!(with_body.body.as_deref(), Some("Prose.\n"));
    let md = fs::read_to_string(scratch.coll_file("north_notes", "with-body.md")).unwrap();
    assert!(md.starts_with("---\n"));
    assert!(md.contains("Prose."));
    let empty = collection.get("empty").unwrap();
    assert!(empty.body.as_deref().is_none_or(str::is_empty));
    let yaml = fs::read_to_string(scratch.coll_file("north_notes", "empty.md")).unwrap();
    assert!(!yaml.starts_with("---"));
    assert!(yaml.contains("title:"));
}

#[test]
fn document_writes_flat_id_md_path() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("pages.yaml", PAGES);
    let vault = scratch.reopen();
    let collection = vault.collection("north_pages").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Personal"})))
        .unwrap();
    collection
        .add(
            "beta",
            rec(serde_json::json!({"title": "Work", "project": "one"})),
        )
        .unwrap();
    let personal = collection.get("alpha").unwrap();
    assert_eq!(personal.path, Some(coll_rel("north_pages", "alpha.md")));
    let nested = collection.get("beta").unwrap();
    assert_eq!(nested.path, Some(coll_rel("north_pages", "beta.md")));
    assert!(scratch.coll_file("north_pages", "alpha.md").is_file());
    assert!(scratch.coll_file("north_pages", "beta.md").is_file());
}

#[test]
fn record_id_rejects_dot_prefix_and_windows_device_names() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    for id in [".hidden", "CON", "con.txt", "foo.", "foo "] {
        let err = collection
            .add(id, rec(serde_json::json!({"title": "Nope"})))
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid), "id {id:?}");
    }
    assert!(!scratch.coll_file("north_notes", ".hidden.md").exists());
}

#[test]
fn record_id_filename_respects_255_byte_component() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(&"a".repeat(252), rec(serde_json::json!({"title": "Max"})))
        .unwrap();
    let err = collection
        .add(&"a".repeat(253), rec(serde_json::json!({"title": "Over"})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn rename_fails_check_and_find_one_does_not_follow() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Hello"})))
        .unwrap();
    fs::rename(
        scratch.coll_file("north_notes", "alpha.md"),
        scratch.coll_file("north_notes", "beta.md"),
    )
    .unwrap();
    let err = collection.get("alpha").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn optional_omit_versus_null() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("omit", rec(serde_json::json!({"title": "No tag"})))
        .unwrap();
    collection
        .add(
            "nil",
            rec(serde_json::json!({"title": "Nil tag", "tag": null})),
        )
        .unwrap();
    let omit = fields_of(&collection.get("omit").unwrap());
    assert_eq!(omit.get("tag"), None);
    let nil = fields_of(&collection.get("nil").unwrap());
    assert_eq!(nil.get("tag"), Some(&Value::Null));
}

#[test]
fn id_pattern_comes_from_schema() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("slugs.yaml", SLUGS);
    let vault = scratch.reopen();
    let collection = vault.collection("north_slugs").unwrap();
    let err = collection
        .add("a/b", rec(serde_json::json!({"title": "Bad"})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    collection
        .add("ok-id", rec(serde_json::json!({"title": "Good"})))
        .unwrap();
    let got = collection.get("ok-id").unwrap();
    assert_eq!(got.path, Some(coll_rel("north_slugs", "ok-id.md")));
    assert_eq!(fields_of(&got)["title"], "Good");
}

#[test]
fn singleton_second_insert_then_replace() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let collection = vault.collection("north_inbox").unwrap();
    collection
        .add("inbox", rec(serde_json::json!({"title": "First"})))
        .unwrap();
    let err = collection
        .add("inbox", rec(serde_json::json!({"title": "Again"})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::DuplicateId));
    collection
        .replace("inbox", rec_rep(serde_json::json!({"title": "Second"})))
        .unwrap();
    let got = collection.get("inbox").unwrap();
    assert_eq!(fields_of(&got)["title"], "Second");
    assert_eq!(got.path, Some(coll_rel("north_inbox", "record.yaml")));
    let text = fs::read_to_string(scratch.coll_file("north_inbox", "record.yaml")).unwrap();
    assert!(text.contains("Second"));
    assert!(!text.contains("First"));
}

#[test]
fn journal_replace_one_is_unsupported() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    let err = vault
        .collection("north_log")
        .unwrap()
        .replace("x", rec_rep(serde_json::json!({"title": "no"})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn failed_write_does_not_become_the_file() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Keep"})))
        .unwrap();
    let before = fs::read(scratch.coll_file("north_notes", "alpha.md")).unwrap();
    let err = collection
        .replace("alpha", rec_rep(serde_json::json!({"count": 1})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(
        fs::read(scratch.coll_file("north_notes", "alpha.md")).unwrap(),
        before
    );
    let north = scratch.vault().join("north_notes");
    let tmps = tmp_names(&north);
    assert!(
        tmps.is_empty(),
        "successful write left temp files: {tmps:?}"
    );
    fs::write(north.join(".alpha.md.99.tmp"), "interrupted").unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.valid, Some(true));
    assert_eq!(fields_of(&got)["title"], "Keep");
}

#[test]
fn invalid_record_rejected_old_file_stays_readable() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Hello"})))
        .unwrap();
    let err = collection
        .add(
            "beta",
            rec(serde_json::json!({"title": "Hello", "unknown": "x"})),
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    drop(collection);
    drop(vault);
    scratch.write_schema("notes.yaml", NOTES_REQUIRED);
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.valid, Some(false));
    assert!(got.raw.as_ref().is_some_and(|raw| !raw.is_empty()));
    assert_eq!(got.id.as_deref(), Some("alpha"));
    let err = collection
        .add("gamma", rec(serde_json::json!({"title": "New"})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn schema_n_plus_one_optional_still_reads() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(serde_json::json!({"title": "Hello"})))
        .unwrap();
    drop(vault);
    scratch.write_schema("notes.yaml", NOTES_PLUS);
    let vault = scratch.reopen();
    let got = vault
        .collection("north_notes")
        .unwrap()
        .get("alpha")
        .unwrap();
    assert_eq!(got.valid, Some(true));
    assert_eq!(fields_of(&got).get("subtitle"), None);
    assert_eq!(fields_of(&got)["title"], "Hello");
}

#[test]
fn fixture_files_are_utf8_text() {
    assert!(!ALPHA_MD.as_bytes().contains(&0));
    assert!(ALPHA_MD.contains("title: Hello"));
    assert!(PLAIN_YAML.contains("title: YAML only"));
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("notes.yaml", NOTES);
    fs::create_dir_all(scratch.coll_file("north_notes", "north")).unwrap();
    fs::write(scratch.coll_file("north_notes", "alpha.md"), ALPHA_MD).unwrap();
    fs::write(scratch.coll_file("north_notes", "plain.md"), PLAIN_YAML).unwrap();
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    let md = collection.get("alpha").unwrap();
    assert_eq!(md.valid, Some(true));
    assert_eq!(md.body.as_deref(), Some("Stored body.\n"));
    let yaml = collection.get("plain").unwrap();
    assert_eq!(yaml.valid, Some(true));
    assert!(yaml.body.as_deref().is_none_or(str::is_empty));
    assert_eq!(fields_of(&yaml)["title"], "YAML only");
    vault.check().unwrap();
}

#[test]
fn reader_find_one_while_writer_holds_lock() {
    let scratch = Scratch::new();
    let writer = open_notes(&scratch);
    writer
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(serde_json::json!({"title": "Hello"})))
        .unwrap();
    let reader = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
    assert!(writer.holds_write_lock());
    assert!(!reader.holds_write_lock());
    let got = reader
        .collection("north_notes")
        .unwrap()
        .get("alpha")
        .unwrap();
    assert_eq!(got.valid, Some(true));
    assert_eq!(fields_of(&got)["title"], "Hello");
}

#[test]
fn header_id_must_match_record_id() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let err = collection
        .add(
            "alpha",
            rec(serde_json::json!({"id": "beta", "title": "Hello"})),
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    collection
        .add("alpha", rec(serde_json::json!({"title": "Hello"})))
        .unwrap();
    let mut text = fs::read_to_string(scratch.coll_file("north_notes", "alpha.md")).unwrap();
    text = text.replace("id: alpha", "id: beta");
    fs::write(scratch.coll_file("north_notes", "alpha.md"), text).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn duplicate_insert_is_conflict() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let record = rec(serde_json::json!({"title": "Hello"}));
    collection.add("alpha", record.clone()).unwrap();
    let err = collection.add("alpha", record).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::DuplicateId));
}

#[test]
fn collection_name_with_slash_is_invalid() {
    let scratch = Scratch::new();
    drop(scratch.init());
    let dir = scratch.vault().join("notes");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(".schema.yaml"),
        "name: \"a/b\"\nkind: document\nversion: 1\nfields:\n  - name: title\n    required: true\n    string: {}\n",
    )
    .unwrap();
    let err = Vault::open(scratch.vault(), scratch.options(), None).unwrap_err();
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
