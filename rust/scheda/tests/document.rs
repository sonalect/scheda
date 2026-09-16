//! C3: `document` / `singleton` writes and `find_one` (`03`, `07`, `11`).
//! Collection names here and in `testdata/document/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "find")]
use scheda::Filter;
use scheda::{Code, OpenOptions, Record, Vault};
use serde_json::{Map, Value};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-c3", n),
        }
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

const NOTES: &str = include_str!("../testdata/document/notes.yaml");
const NOTES_PLUS: &str = include_str!("../testdata/document/notes_plus.yaml");
const NOTES_REQUIRED: &str = include_str!("../testdata/document/notes_required.yaml");
const PAGES: &str = include_str!("../testdata/document/pages.yaml");
const INBOX: &str = include_str!("../testdata/document/inbox.yaml");
const LOG: &str = include_str!("../testdata/document/log.yaml");
const ALPHA_MD: &str = include_str!("../testdata/document/alpha.md");
const PLAIN_YAML: &str = include_str!("../testdata/document/plain.yaml");
const WEST_ISOLATED: &str = include_str!("../testdata/schema/west_isolated.yaml");

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
    collection.add("alpha", b"Stored body.\n", None).unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    assert_eq!(got.collection.as_deref(), Some("north_notes"));
    assert_eq!(got.raw.as_deref(), Some(b"Stored body.\n".as_slice()));

    collection.replace("alpha", b"New body.\n", None).unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.raw.as_deref(), Some(b"New body.\n".as_slice()));
    let text = fs::read_to_string(scratch.coll_file("north_notes", "alpha")).unwrap();
    assert_eq!(text, "New body.\n");
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
    collection.add("with-body", b"Prose.\n", None).unwrap();
    collection.add("empty", b"", None).unwrap();
    let with_body = collection.get("with-body").unwrap();
    assert_eq!(with_body.raw.as_deref(), Some(b"Prose.\n".as_slice()));
    let md = fs::read_to_string(scratch.coll_file("north_notes", "with-body")).unwrap();
    assert_eq!(md, "Prose.\n");
    let empty = collection.get("empty").unwrap();
    assert!(empty.raw.as_deref().is_none_or(|raw| raw.is_empty()));
    let yaml = fs::read_to_string(scratch.coll_file("north_notes", "empty")).unwrap();
    assert!(yaml.is_empty());
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
    assert_eq!(personal.id.as_deref(), Some("alpha"));
    let nested = collection.get("beta").unwrap();
    assert_eq!(nested.id.as_deref(), Some("beta"));
    assert!(scratch.coll_file("north_pages", "alpha").is_file());
    assert!(scratch.coll_file("north_pages", "beta").is_file());
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
    assert!(!scratch.coll_file("north_notes", ".hidden").exists());
}

#[test]
fn record_id_filename_respects_255_byte_component() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            &"a".repeat(255),
            rec(serde_json::json!({"title": "Max"})),
            None,
        )
        .unwrap();
    let err = collection
        .add(
            &"a".repeat(256),
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
        scratch.coll_file("north_notes", "alpha"),
        scratch.coll_file("north_notes", "beta"),
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
    assert_eq!(work.id.as_deref(), Some("work/meeting"));
    assert_eq!(fields_of(&work)["title"], "Work");
    assert!(scratch.coll_file("north_notes", "work/meeting").is_file());
    let daily = collection.get("2026-01-15/standup").unwrap();
    assert_eq!(daily.id.as_deref(), Some("2026-01-15/standup"));
    collection
        .replace(
            "work/meeting",
            rec_rep(serde_json::json!({"title": "Still work"})),
            None,
        )
        .unwrap();
    assert_eq!(
        collection.get("work/meeting").unwrap().id.as_deref(),
        Some("work/meeting")
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
    let text = String::from_utf8(got.raw.clone().unwrap()).unwrap();
    assert!(text.contains("Second"), "{text}");
    assert!(scratch.coll_file("north_inbox", "inbox").is_file());
    let text = fs::read_to_string(scratch.coll_file("north_inbox", "inbox")).unwrap();
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
    assert!(!scratch.coll_file("north_notes", "alpha").exists());
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
    assert!(scratch.coll_file("north_notes", "alpha").is_file());
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
    assert!(!scratch.coll_file("north_inbox", "inbox").exists());
    collection
        .add("inbox", rec(serde_json::json!({"title": "Again"})), None)
        .unwrap();
    let text = String::from_utf8(collection.get("inbox").unwrap().raw.unwrap()).unwrap();
    assert!(text.contains("Again"), "{text}");
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
    let path = scratch.coll_file("north_notes", "orphan");
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
    let path = scratch.coll_file("north_notes", "orphan");
    fs::write(&path, "---\nid: orphan\ntitle: Ghost\n---\n").unwrap();
    collection.get("orphan").unwrap();
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
    let before = fs::read(scratch.coll_file("north_notes", "alpha")).unwrap();
    let err = collection
        .replace("alpha", vec![0xff, 0xfe], None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotUtf8));
    assert_eq!(
        fs::read(scratch.coll_file("north_notes", "alpha")).unwrap(),
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
    collection
        .add(
            "beta",
            rec(serde_json::json!({"title": "Hello", "unknown": "x"})),
            None,
        )
        .unwrap();
    drop(collection);
    drop(vault);
    scratch.write_schema("notes.yaml", NOTES_REQUIRED);
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    let got = collection.get("alpha").unwrap();
    assert!(got.raw.as_ref().is_some_and(|raw| !raw.is_empty()));
    assert_eq!(got.id.as_deref(), Some("alpha"));
    collection
        .add("gamma", rec(serde_json::json!({"title": "New"})), None)
        .unwrap();
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
    fs::write(scratch.coll_file("north_notes", "alpha"), ALPHA_MD).unwrap();
    fs::write(scratch.coll_file("north_notes", "plain.md"), PLAIN_YAML).unwrap();
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    let md = collection.get("alpha").unwrap();
    assert_eq!(md.raw.as_deref(), Some(ALPHA_MD.as_bytes()));
    let yaml = collection.get("plain.md").unwrap();
    assert_eq!(yaml.raw.as_deref(), Some(PLAIN_YAML.as_bytes()));
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
    assert_eq!(fields_of(&got)["title"], "Hello");
}

#[cfg(feature = "find")]
#[test]
fn header_id_must_match_record_id() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "alpha",
            rec(serde_json::json!({"id": "beta", "title": "Hello"})),
            None,
        )
        .unwrap();
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    vault.check().unwrap();
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
    let path = scratch.coll_file("north_notes", "work/meeting");
    let mut text = fs::read_to_string(&path).unwrap();
    text = text.replace("id: work/meeting", "id: meeting");
    fs::write(&path, text).unwrap();
    vault.check().unwrap();
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
        dir.join("$stash").join("hidden"),
        "---\nid: hidden\ntitle: Nested\n---\n",
    )
    .unwrap();
    assert_eq!(collection.ids().unwrap(), ["alpha", "notes.txt"]);
    assert_eq!(walked_ids(&collection), ["alpha", "notes.txt"]);
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
    let walked: Vec<_> = collection.documents().unwrap().collect();
    assert_eq!(walked.len(), 1);
    let record = walked[0].as_ref().unwrap();
    assert_eq!(record.id.as_deref(), Some("alpha"));
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
        scratch.coll_file("north_notes", "aaa"),
        "this is not a Record\n",
    )
    .unwrap();
    let walked: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap())
        .collect();
    assert_eq!(walked.len(), 2);
    assert_eq!(
        walked[0].raw.as_deref(),
        Some(b"this is not a Record\n".as_slice())
    );
    assert_eq!(walked[1].id.as_deref(), Some("alpha"));
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
}

#[test]
fn singleton_method_id_must_equal_declared_id() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let collection = vault.collection("north_inbox").unwrap();
    let err = collection
        .add("other", rec(serde_json::json!({"title": "Nope"})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(!scratch.coll_file("north_inbox", "other").exists());
    assert!(!scratch.coll_file("north_inbox", "inbox").exists());
}

#[test]
fn singleton_bytes_are_opaque_utf8() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let collection = vault.collection("north_inbox").unwrap();
    collection.add("inbox", b"key = \"toml\"\n", None).unwrap();
    let got = collection.get("inbox").unwrap();
    assert_eq!(got.raw.as_deref(), Some(b"key = \"toml\"\n".as_slice()));
    let text = fs::read_to_string(scratch.coll_file("north_inbox", "inbox")).unwrap();
    assert_eq!(text, "key = \"toml\"\n");
    collection.replace("inbox", b"", None).unwrap();
    assert_eq!(
        collection.get("inbox").unwrap().raw.as_deref(),
        Some(&b""[..])
    );
}

#[test]
fn singleton_non_utf8_writes_no_file() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    let vault = scratch.reopen();
    let collection = vault.collection("north_inbox").unwrap();
    let err = collection.add("inbox", [0xff, 0xfe], None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotUtf8));
    assert!(!scratch.coll_file("north_inbox", "inbox").exists());
}

#[test]
fn singleton_declared_id_may_include_extension() {
    const SCHEMA: &str = "\
name: box_toml
kind: singleton
version: 1
singleton:
  id: inbox.toml
fields: []
";
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("box.yaml", SCHEMA);
    let vault = scratch.reopen();
    let collection = vault.collection("box_toml").unwrap();
    collection
        .add("inbox.toml", b"[pkg]\nname = \"x\"\n", None)
        .unwrap();
    assert!(scratch.coll_file("box_toml", "inbox.toml").is_file());
    assert!(!scratch.coll_file("box_toml", "record.yaml").exists());
    assert_eq!(collection.ids().unwrap(), ["inbox.toml"]);
    assert_eq!(
        collection.get("inbox.toml").unwrap().raw.as_deref(),
        Some(b"[pkg]\nname = \"x\"\n".as_slice())
    );
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
    assert!(
        scratch
            .vault()
            .join(".isolated")
            .join("west_isolated")
            .join("secret")
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
    assert_eq!(got.id.as_deref(), Some("work/secret"));
    assert!(
        scratch
            .vault()
            .join(".isolated")
            .join("west_isolated")
            .join("work")
            .join("secret")
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
        scratch.coll_file("north_notes", "charlie"),
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

#[test]
fn add_work_meeting_toml_uses_that_path() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("work/meeting.toml", b"title = \"standup\"\n", None)
        .unwrap();
    let got = collection.get("work/meeting.toml").unwrap();
    assert_eq!(got.id.as_deref(), Some("work/meeting.toml"));
    assert_eq!(
        got.raw.as_deref(),
        Some(b"title = \"standup\"\n".as_slice())
    );
    assert!(
        scratch
            .coll_file("north_notes", "work/meeting.toml")
            .is_file()
    );
    collection
        .replace("work/meeting.toml", b"changed", None)
        .unwrap();
    assert_eq!(
        collection.get("work/meeting.toml").unwrap().raw.as_deref(),
        Some(b"changed".as_slice())
    );
    let hist = collection.history("work/meeting.toml", 0, 10).unwrap();
    assert!(!hist.entries.is_empty());
    let ids = collection.ids().unwrap();
    assert!(ids.iter().any(|id| id == "work/meeting.toml"));
    let walked = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap().id)
        .collect::<Vec<_>>();
    assert!(
        walked
            .iter()
            .any(|id| id.as_deref() == Some("work/meeting.toml"))
    );
    collection.remove("work/meeting.toml", None).unwrap();
    assert!(
        !scratch
            .coll_file("north_notes", "work/meeting.toml")
            .exists()
    );
    assert!(!scratch.coll_file("north_notes", "work").exists());
}

#[test]
fn add_foo_md_writes_foo_md() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection.add("foo.md", b"# note\n", None).unwrap();
    let got = collection.get("foo.md").unwrap();
    assert_eq!(got.id.as_deref(), Some("foo.md"));
    assert!(scratch.coll_file("north_notes", "foo.md").is_file());
    assert_eq!(
        collection.get("foo").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
}

#[test]
fn yaml_and_toml_ids_are_distinct_and_unparsed() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection.add("a.yaml", b"title: yaml\n", None).unwrap();
    collection
        .add("a.toml", b"title = \"toml\"\n", None)
        .unwrap();
    let yaml = collection.get("a.yaml").unwrap();
    let toml = collection.get("a.toml").unwrap();
    assert_eq!(yaml.raw.as_deref(), Some(b"title: yaml\n".as_slice()));
    assert_eq!(toml.raw.as_deref(), Some(b"title = \"toml\"\n".as_slice()));
    assert_ne!(yaml.id, toml.id);
}

#[test]
fn non_utf8_add_is_error_and_writes_no_file() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    let err = collection.add("bad", [0xff, 0xfe], None).unwrap_err();
    assert!(
        err.known_code() == Some(Code::NotUtf8) || err.known_code() == Some(Code::Invalid),
        "{err:?}"
    );
    assert!(!scratch.coll_file("north_notes", "bad").exists());
}

#[test]
fn documents_and_ids_skip_dotfiles_and_yield_mixed_extensions() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection.add("keep.txt", b"keep", None).unwrap();
    fs::write(scratch.coll_file("north_notes", ".collection.yaml"), "x").unwrap();
    fs::write(scratch.coll_file("north_notes", ".hidden"), "nope").unwrap();
    let ids = collection.ids().unwrap();
    assert!(ids.contains(&String::from("keep.txt")));
    assert!(!ids.iter().any(|id| id.starts_with('.')));
    let names: Vec<_> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert!(names.contains(&String::from("keep.txt")));
    assert!(!names.iter().any(|id| id.starts_with('.')));
}

#[cfg(feature = "find")]
#[test]
fn find_all_and_term_id_work_on_documents() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection.add("alpha", b"one", None).unwrap();
    collection.add("beta", b"two", None).unwrap();
    let all = collection.find(&Filter::all().into(), 0, 10).unwrap();
    let mut ids: Vec<_> = all
        .records
        .into_iter()
        .filter_map(|record| record.id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["alpha", "beta"]);
    let query: scheda::FindQuery = serde_json::from_value(serde_json::json!({
        "filter": {"term": {"field": "id", "value": "alpha"}}
    }))
    .unwrap();
    let page = collection.find(&query, 0, 10).unwrap();
    assert_eq!(
        page.records
            .iter()
            .filter_map(|record| record.id.as_deref())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
}

#[test]
fn empty_raw_round_trips() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection.add("empty", b"", None).unwrap();
    let got = collection.get("empty").unwrap();
    assert_eq!(got.raw.as_deref(), Some(b"".as_slice()));
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
