//! C3: `document` / `singleton` writes and `find_one` (`03`, `07`, `11`).
//! Collection names here and in `testdata/document/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

mod common;

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "find")]
use scheda::Filter;
use scheda::{Code, OpenOptions, Record, RecordAdd, RecordReplace, Vault};
use serde_json::{Map, Value};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
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
const INBOX: &str = include_str!("../testdata/document/inbox.yaml");
const LOG: &str = include_str!("../testdata/document/log.yaml");
const ALPHA_MD: &str = include_str!("../testdata/document/alpha.md");
const PLAIN_YAML: &str = include_str!("../testdata/document/plain.yaml");
const WEST_ISOLATED: &str = include_str!("../testdata/schema/west_isolated.yaml");

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
            None,
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
            None,
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
            None,
        )
        .unwrap();
    collection
        .add("empty", rec(serde_json::json!({"title": "None"})), None)
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
        .add("alpha", rec(serde_json::json!({"title": "Personal"})), None)
        .unwrap();
    collection
        .add(
            "beta",
            rec(serde_json::json!({"title": "Work", "project": "one"})),
            None,
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
            .add(id, rec(serde_json::json!({"title": "Nope"})), None)
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
        .add(
            &"a".repeat(252),
            rec(serde_json::json!({"title": "Max"})),
            None,
        )
        .unwrap();
    let err = collection
        .add(
            &"a".repeat(253),
            rec(serde_json::json!({"title": "Over"})),
            None,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[cfg(feature = "find")]
#[test]
fn rename_fails_check_and_find_one_does_not_follow() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Hello"})), None)
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
        .add("omit", rec(serde_json::json!({"title": "No tag"})), None)
        .unwrap();
    collection
        .add(
            "nil",
            rec(serde_json::json!({"title": "Nil tag", "tag": null})),
            None,
        )
        .unwrap();
    let omit = fields_of(&collection.get("omit").unwrap());
    assert_eq!(omit.get("tag"), None);
    let nil = fields_of(&collection.get("nil").unwrap());
    assert_eq!(nil.get("tag"), Some(&Value::Null));
}

#[test]
fn document_id_may_contain_slash_namespaces() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "work/meeting",
            rec(serde_json::json!({"title": "Work"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "2026-01-15/standup",
            rec(serde_json::json!({"title": "Daily"})),
            None,
        )
        .unwrap();
    let work = collection.get("work/meeting").unwrap();
    assert_eq!(work.path.as_deref(), Some("north_notes/work/meeting.md"));
    assert_eq!(fields_of(&work)["title"], "Work");
    assert!(
        scratch
            .coll_file("north_notes", "work/meeting.md")
            .is_file()
    );
    let daily = collection.get("2026-01-15/standup").unwrap();
    assert_eq!(
        daily.path.as_deref(),
        Some("north_notes/2026-01-15/standup.md")
    );
    collection
        .replace(
            "work/meeting",
            rec_rep(serde_json::json!({"title": "Still work"})),
            None,
        )
        .unwrap();
    assert_eq!(
        collection.get("work/meeting").unwrap().path.as_deref(),
        Some("north_notes/work/meeting.md")
    );
    let hist = collection.history("work/meeting", 0, 10).unwrap();
    assert!(!hist.entries.is_empty());
    let err = collection
        .add(
            "work/meeting",
            rec(serde_json::json!({"title": "Again"})),
            None,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::DuplicateId));
    drop(collection);
    drop(vault);
    fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
    let reopened = scratch.reopen();
    let collection = reopened.collection("north_notes").unwrap();
    assert_eq!(
        collection.get("work/meeting").unwrap().id.as_deref(),
        Some("work/meeting")
    );
    assert_eq!(
        collection.ids().unwrap(),
        ["2026-01-15/standup", "work/meeting"]
    );
    reopened.reindex().unwrap();
    collection.remove("work/meeting", None).unwrap();
    assert!(!scratch.coll_file("north_notes", "work").exists());
}

#[test]
fn document_id_rejects_path_injection() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    for id in [
        "../x", "x/../y", "CON/a", "a/CON", "a//b", "a/", "/a", r"a\b",
    ] {
        let err = collection
            .add(id, rec(serde_json::json!({"title": "Bad"})), None)
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid), "id {id:?}");
    }
}

#[test]
fn journal_and_singleton_still_reject_slash_in_id() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let err = vault
        .collection("north_log")
        .unwrap()
        .add("a/b", rec(serde_json::json!({})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let err = vault
        .collection("north_inbox")
        .unwrap()
        .add("a/b", rec(serde_json::json!({"title": "Bad"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn singleton_second_insert_then_replace() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let collection = vault.collection("north_inbox").unwrap();
    collection
        .add("inbox", rec(serde_json::json!({"title": "First"})), None)
        .unwrap();
    let err = collection
        .add("inbox", rec(serde_json::json!({"title": "Again"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::DuplicateId));
    collection
        .replace(
            "inbox",
            rec_rep(serde_json::json!({"title": "Second"})),
            None,
        )
        .unwrap();
    let got = collection.get("inbox").unwrap();
    assert_eq!(fields_of(&got)["title"], "Second");
    assert_eq!(got.path, Some(coll_rel("north_inbox", "record.yaml")));
    let text = fs::read_to_string(scratch.coll_file("north_inbox", "record.yaml")).unwrap();
    assert!(text.contains("Second"));
    assert!(!text.contains("First"));
}

#[test]
fn remove_deletes_file_and_get_is_not_found() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Keep"})), None)
        .unwrap();
    collection
        .add("beta", rec(serde_json::json!({"title": "Stay"})), None)
        .unwrap();
    collection.remove("alpha", None).unwrap();
    assert_eq!(
        collection.get("alpha").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert!(!scratch.coll_file("north_notes", "alpha.md").exists());
    assert_eq!(collection.get("beta").unwrap().id.as_deref(), Some("beta"));
    let north = scratch.vault().join("north_notes");
    let tmps = tmp_names(&north);
    assert!(tmps.is_empty(), "partial remove leftover: {tmps:?}");
}

#[test]
fn remove_unknown_id_is_not_found() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let err = collection.remove("missing", None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn remove_illegal_slug_is_invalid_before_any_file() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Keep"})), None)
        .unwrap();
    for id in ["", ".hidden", "CON", "foo.", "foo "] {
        let err = collection.remove(id, None).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid), "id {id:?}");
    }
    assert!(scratch.coll_file("north_notes", "alpha.md").is_file());
}

#[test]
fn journal_remove_is_unsupported() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    let collection = vault.collection("north_log").unwrap();
    collection
        .add("e1", rec(serde_json::json!({})), None)
        .unwrap();
    let err = collection.remove("e1", None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    assert!(collection.get("e1").is_ok());
}

#[test]
fn singleton_remove_then_add_again() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let collection = vault.collection("north_inbox").unwrap();
    collection
        .add("inbox", rec(serde_json::json!({"title": "First"})), None)
        .unwrap();
    collection.remove("inbox", None).unwrap();
    assert_eq!(
        collection.get("inbox").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert!(!scratch.coll_file("north_inbox", "record.yaml").exists());
    collection
        .add("inbox", rec(serde_json::json!({"title": "Again"})), None)
        .unwrap();
    assert_eq!(
        fields_of(&collection.get("inbox").unwrap())["title"],
        "Again"
    );
}

#[cfg(feature = "find")]
#[test]
fn remove_leaves_find() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Gone"})), None)
        .unwrap();
    collection
        .add("beta", rec(serde_json::json!({"title": "Stay"})), None)
        .unwrap();
    collection.remove("alpha", None).unwrap();
    let page = collection.find(&Filter::all().into(), 0, 10).unwrap();
    assert_eq!(page.total, Some(1));
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id.as_deref(), Some("beta"));
}

#[cfg(feature = "find")]
#[test]
fn remove_of_unindexed_file_is_not_found() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let path = scratch.coll_file("north_notes", "orphan.md");
    fs::write(&path, "---\nid: orphan\ntitle: Ghost\n---\n").unwrap();
    assert!(collection.get("orphan").is_ok());
    let err = collection.remove("orphan", None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    assert!(path.is_file());
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
        .replace("x", rec_rep(serde_json::json!({"title": "no"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[cfg(feature = "find")]
#[test]
fn replace_of_unindexed_file_is_not_found() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let path = scratch.coll_file("north_notes", "orphan.md");
    fs::write(&path, "---\nid: orphan\ntitle: Ghost\n---\n").unwrap();
    let got = collection.get("orphan").unwrap();
    assert_eq!(got.valid, Some(true));
    let err = collection
        .replace("orphan", rec_rep(serde_json::json!({"title": "New"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn failed_write_does_not_become_the_file() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Keep"})), None)
        .unwrap();
    let before = fs::read(scratch.coll_file("north_notes", "alpha.md")).unwrap();
    let err = collection
        .replace("alpha", rec_rep(serde_json::json!({"count": 1})), None)
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
        .add("alpha", rec(serde_json::json!({"title": "Hello"})), None)
        .unwrap();
    let err = collection
        .add(
            "beta",
            rec(serde_json::json!({"title": "Hello", "unknown": "x"})),
            None,
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
        .add("gamma", rec(serde_json::json!({"title": "New"})), None)
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
        .add("alpha", rec(serde_json::json!({"title": "Hello"})), None)
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
    writer.begin().unwrap();
    writer
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(serde_json::json!({"title": "Hello"})), None)
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

#[cfg(feature = "find")]
#[test]
fn header_id_must_match_record_id() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let err = collection
        .add(
            "alpha",
            rec(serde_json::json!({"id": "beta", "title": "Hello"})),
            None,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    collection
        .add("alpha", rec(serde_json::json!({"title": "Hello"})), None)
        .unwrap();
    let mut text = fs::read_to_string(scratch.coll_file("north_notes", "alpha.md")).unwrap();
    text = text.replace("id: alpha", "id: beta");
    fs::write(scratch.coll_file("north_notes", "alpha.md"), text).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn nested_document_header_must_match_path() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "work/meeting",
            rec(serde_json::json!({"title": "Work"})),
            None,
        )
        .unwrap();
    let path = scratch.coll_file("north_notes", "work/meeting.md");
    let mut text = fs::read_to_string(&path).unwrap();
    text = text.replace("id: work/meeting", "id: meeting");
    fs::write(&path, text).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[cfg(feature = "find")]
#[test]
fn find_term_on_hierarchical_id() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "work/meeting",
            rec(serde_json::json!({"title": "Work"})),
            None,
        )
        .unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "Flat"})), None)
        .unwrap();
    let query: Filter = serde_json::from_value(serde_json::json!({
        "term": {"field": "id", "value": "work/meeting"}
    }))
    .unwrap();
    let page = collection.find(&query.into(), 0, 10).unwrap();
    assert_eq!(page.total, Some(1));
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id.as_deref(), Some("work/meeting"));
}

#[test]
fn duplicate_insert_is_conflict() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let record = rec(serde_json::json!({"title": "Hello"}));
    collection.add("alpha", record.clone(), None).unwrap();
    let err = collection.add("alpha", record, None).unwrap_err();
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

fn walked_ids(collection: &scheda::Collection) -> Vec<String> {
    collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect()
}

#[test]
fn documents_and_ids_are_id_order() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("charlie", rec(serde_json::json!({"title": "c"})), None)
        .unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "a"})), None)
        .unwrap();
    collection
        .add("bravo", rec(serde_json::json!({"title": "b"})), None)
        .unwrap();
    assert_eq!(collection.ids().unwrap(), ["alpha", "bravo", "charlie"]);
    let mut walked = Vec::new();
    for record in collection.documents().unwrap() {
        let record = record.unwrap();
        let id = record.id.clone().unwrap();
        let got = collection.get(&id).unwrap();
        assert_eq!(record.raw, got.raw);
        assert_eq!(record.valid, got.valid);
        walked.push(id);
    }
    assert_eq!(walked, ["alpha", "bravo", "charlie"]);
}

#[test]
fn documents_skips_non_record_names() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "a"})), None)
        .unwrap();
    let dir = scratch.vault().join("north_notes");
    fs::write(dir.join("notes.txt"), "not a record").unwrap();
    fs::create_dir_all(dir.join("$stash")).unwrap();
    fs::write(
        dir.join("$stash").join("hidden.md"),
        "---\nid: hidden\ntitle: Nested\n---\n",
    )
    .unwrap();
    assert_eq!(collection.ids().unwrap(), ["alpha"]);
    assert_eq!(walked_ids(&collection), ["alpha"]);
}

#[test]
fn documents_and_ids_on_journal_are_unsupported() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    let collection = vault.collection("north_log").unwrap();
    let err = match collection.documents() {
        Ok(_) => panic!("journal documents must be unsupported"),
        Err(err) => err,
    };
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    let err = match collection.ids() {
        Ok(_) => panic!("journal ids must be unsupported"),
        Err(err) => err,
    };
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn documents_and_ids_survive_deleted_index() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("charlie", rec(serde_json::json!({"title": "c"})), None)
        .unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "a"})), None)
        .unwrap();
    drop(collection);
    drop(vault);

    fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    assert_eq!(collection.ids().unwrap(), ["alpha", "charlie"]);
    assert_eq!(walked_ids(&collection), ["alpha", "charlie"]);
}

#[test]
fn documents_schema_fail_is_valid_false() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(serde_json::json!({"title": "Hello"})), None)
        .unwrap();
    drop(vault);
    scratch.write_schema("notes.yaml", NOTES_REQUIRED);
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.valid, Some(false));
    let walked: Vec<_> = collection.documents().unwrap().collect();
    assert_eq!(walked.len(), 1);
    let record = walked[0].as_ref().unwrap();
    assert_eq!(record.id.as_deref(), Some("alpha"));
    assert_eq!(record.valid, Some(false));
    assert_eq!(record.raw, got.raw);
}

#[test]
fn documents_unparseable_file_is_valid_false_and_walk_continues() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "a"})), None)
        .unwrap();
    fs::write(
        scratch.coll_file("north_notes", "aaa.md"),
        "this is not a Record\n",
    )
    .unwrap();
    let walked: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap())
        .collect();
    assert_eq!(walked.len(), 2);
    assert_eq!(walked[0].valid, Some(false));
    assert_eq!(
        walked[0].raw.as_deref(),
        Some(b"this is not a Record\n".as_slice())
    );
    assert_eq!(walked[1].id.as_deref(), Some("alpha"));
    assert_eq!(walked[1].valid, Some(true));
    let got = collection.get("alpha").unwrap();
    assert_eq!(walked[1].raw, got.raw);
    #[cfg(feature = "find")]
    vault.reindex().unwrap();
}

#[test]
fn singleton_documents_and_ids_empty_then_one() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let collection = vault.collection("north_inbox").unwrap();
    assert!(collection.ids().unwrap().is_empty());
    assert!(walked_ids(&collection).is_empty());
    collection
        .add("inbox", rec(serde_json::json!({"title": "First"})), None)
        .unwrap();
    assert_eq!(collection.ids().unwrap(), ["inbox"]);
    let walked: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap())
        .collect();
    assert_eq!(walked.len(), 1);
    assert_eq!(walked[0].id.as_deref(), Some("inbox"));
    assert_eq!(walked[0].valid, Some(true));
}

#[test]
fn documents_and_ids_empty_and_missing_are_empty() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    assert!(collection.ids().unwrap().is_empty());
    assert!(walked_ids(&collection).is_empty());

    drop(collection);
    drop(vault);
    scratch.write_schema("west.yaml", WEST_ISOLATED);
    let vault = scratch.reopen();
    let collection = vault.collection("west_isolated").unwrap();
    assert!(collection.ids().unwrap().is_empty());
    assert!(walked_ids(&collection).is_empty());
    assert!(
        !scratch
            .vault()
            .join(".isolated")
            .join("west_isolated")
            .is_dir()
    );
}

#[test]
fn documents_isolated_walks_nested_dir() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST_ISOLATED);
    let vault = scratch.reopen();
    let collection = vault.collection("west_isolated").unwrap();
    collection
        .add(
            "secret",
            rec(serde_json::json!({"west_pass": "nested-secret"})),
            None,
        )
        .unwrap();
    assert_eq!(collection.ids().unwrap(), ["secret"]);
    let walked: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap())
        .collect();
    assert_eq!(walked.len(), 1);
    assert_eq!(walked[0].id.as_deref(), Some("secret"));
    assert_eq!(
        walked[0].path.as_deref(),
        Some(".isolated/west_isolated/secret.md")
    );
    assert!(
        scratch
            .vault()
            .join(".isolated")
            .join("west_isolated")
            .join("secret.md")
            .is_file()
    );
}

#[test]
fn isolated_document_id_may_contain_slash_namespaces() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST_ISOLATED);
    let vault = scratch.reopen();
    let collection = vault.collection("west_isolated").unwrap();
    collection
        .add(
            "work/secret",
            rec(serde_json::json!({"west_pass": "nested-secret"})),
            None,
        )
        .unwrap();
    let got = collection.get("work/secret").unwrap();
    assert_eq!(
        got.path.as_deref(),
        Some(".isolated/west_isolated/work/secret.md")
    );
    assert!(
        scratch
            .vault()
            .join(".isolated")
            .join("west_isolated")
            .join("work")
            .join("secret.md")
            .is_file()
    );
    assert_eq!(collection.ids().unwrap(), ["work/secret"]);
    collection.remove("work/secret", None).unwrap();
    assert!(
        !scratch
            .vault()
            .join(".isolated")
            .join("west_isolated")
            .join("work")
            .exists()
    );
}

#[test]
fn documents_thousand_in_id_order() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    vault.begin().unwrap();
    for i in 0..1000 {
        collection
            .add(
                &format!("d-{i:04}"),
                rec(serde_json::json!({"title": "n"})),
                None,
            )
            .unwrap();
    }
    vault.commit("bulk").unwrap();
    let expected: Vec<String> = (0..1000).map(|i| format!("d-{i:04}")).collect();
    assert_eq!(collection.ids().unwrap(), expected);
    let walked = walked_ids(&collection);
    assert_eq!(walked.len(), 1000);
    assert_eq!(walked, expected);
}

#[cfg(feature = "find")]
#[test]
fn ids_matches_files_when_index_is_stale() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(serde_json::json!({"title": "a"})), None)
        .unwrap();
    collection
        .add("bravo", rec(serde_json::json!({"title": "b"})), None)
        .unwrap();
    fs::write(
        scratch.coll_file("north_notes", "charlie.md"),
        "---\nid: charlie\ntitle: c\n---\n",
    )
    .unwrap();
    assert_eq!(collection.ids().unwrap(), ["alpha", "bravo", "charlie"]);
    let mut found: Vec<_> = collection
        .find(&Filter::all().into(), 0, 100)
        .unwrap()
        .records
        .into_iter()
        .map(|record| record.id.unwrap())
        .collect();
    found.sort();
    assert_eq!(found, ["alpha", "bravo"]);
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
