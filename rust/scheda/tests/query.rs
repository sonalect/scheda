//! C5: SCH-8 `find`, `scan`, `check`, `reindex`.
//! Collection names here and in `testdata/query/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, Filter, OpenOptions, RecordAdd, Vault};
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
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const NOTES: &str = include_str!("../testdata/query/notes.yaml");
const PAGES: &str = include_str!("../testdata/query/pages.yaml");
const LOG: &str = include_str!("../testdata/query/log.yaml");

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn rec_body(fields: Value, body: &str) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields, "body": body })).unwrap()
}

fn filter(value: Value) -> Filter {
    serde_json::from_value(value).unwrap()
}

fn ids(result: &scheda::FindResult) -> Vec<&str> {
    result
        .records
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
        .add("a", rec(json!({"title": "A", "tag": "red"})))
        .unwrap();
    collection
        .add("b", rec(json!({"title": "B", "tag": "blue"})))
        .unwrap();
    collection.add("c", rec(json!({"title": "C"}))).unwrap();
    let found = collection.find(&Filter::all(), 0, 100).unwrap();
    assert_eq!(found.records.len(), 3);
    assert_eq!(found.total, Some(3));
    assert_eq!(
        collection
            .find(&Filter::all(), 0, 1000)
            .unwrap()
            .records
            .len(),
        3
    );
    let page = collection.find(&Filter::all(), 0, 1).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.total, Some(3));
}

#[test]
fn find_count_zero_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    let err = collection.find(&Filter::all(), 0, 0).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn vault_find_empty_names_is_all_collections() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("notes.yaml", NOTES);
    scratch.write_schema("pages.yaml", PAGES);
    let vault = scratch.reopen();
    vault
        .collection("query_notes")
        .unwrap()
        .add("red", rec(json!({"title": "R", "tag": "red"})))
        .unwrap();
    vault
        .collection("query_pages")
        .unwrap()
        .add("home", rec(json!({"title": "Home"})))
        .unwrap();
    let all = vault.find(&[], &Filter::all(), 0, 100).unwrap();
    let mut all_ids = ids(&all);
    all_ids.sort_unstable();
    assert_eq!(all_ids, ["home", "red"]);
    let mut names: Vec<_> = all
        .records
        .iter()
        .map(|record| {
            (
                record.id.as_deref().unwrap(),
                record.collection.as_deref().unwrap(),
            )
        })
        .collect();
    names.sort_unstable();
    assert_eq!(names, [("home", "query_pages"), ("red", "query_notes")]);
    assert_eq!(
        ids(&vault
            .find(&["query_notes"], &Filter::all(), 0, 100)
            .unwrap()),
        ["red"]
    );
    assert_eq!(
        ids(&vault
            .find(
                &[],
                &filter(json!({"term":{"field":"tag","value":"red"}})),
                0,
                100
            )
            .unwrap()),
        ["red"]
    );
    assert_eq!(
        vault
            .find(&["missing"], &Filter::all(), 0, 10)
            .unwrap_err()
            .known_code(),
        Some(Code::NotFound)
    );
    assert_eq!(
        vault
            .find(
                &["query_pages"],
                &filter(json!({"term":{"field":"tag","value":"red"}})),
                0,
                10
            )
            .unwrap_err()
            .known_code(),
        Some(Code::Invalid)
    );
}

#[test]
fn equal_set_exists_and_null() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("red", rec(json!({"title": "R", "tag": "red"})))
        .unwrap();
    collection
        .add("blue", rec(json!({"title": "B", "tag": "blue"})))
        .unwrap();
    collection
        .add("nil", rec(json!({"title": "N", "tag": null})))
        .unwrap();
    collection.add("omit", rec(json!({"title": "O"}))).unwrap();

    let red = collection
        .find(
            &filter(json!({"term":{"field":"tag","value":"red"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&red), ["red"]);

    let set = collection
        .find(
            &filter(json!({"termSet":{"field":"tag","values":["red","blue"]}})),
            0,
            100,
        )
        .unwrap();
    let mut set_ids = ids(&set);
    set_ids.sort_unstable();
    assert_eq!(set_ids, ["blue", "red"]);

    let missing = collection
        .find(
            &filter(json!({"term":{"field":"tag","value":null}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&missing), ["nil"]);

    let exists = collection
        .find(&filter(json!({"exists":{"field":"tag"}})), 0, 100)
        .unwrap();
    let mut exists_ids = ids(&exists);
    exists_ids.sort_unstable();
    assert_eq!(exists_ids, ["blue", "red"]);
}

#[test]
fn unmarked_field_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    let err = collection
        .find(
            &filter(json!({"term":{"field":"secret","value":"x"}})),
            0,
            100,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn document_filter_on_project_field() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("pages.yaml", PAGES);
    let vault = scratch.reopen();
    let collection = vault.collection("query_pages").unwrap();
    collection
        .add("home", rec(json!({"title": "Home"})))
        .unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "project": "one"})))
        .unwrap();
    let projects = collection
        .find(
            &filter(json!({"term":{"field":"project","value":"one"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&projects), ["alpha"]);
    let all_page = collection.find(&Filter::all(), 0, 100).unwrap();
    let mut all = ids(&all_page);
    all.sort_unstable();
    assert_eq!(all, ["alpha", "home"]);
}

#[test]
fn journal_empty_find_keeps_append_order() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    let collection = vault.collection("query_log").unwrap();
    collection.add("a", rec(json!({"tag": "one"}))).unwrap();
    collection.add("b", rec(json!({"tag": "two"}))).unwrap();
    let found = collection
        .find(
            &filter(json!({"term":{"field":"tag","value":"one"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&found), ["a"]);
    assert_eq!(
        ids(&collection.find(&Filter::all(), 0, 1000).unwrap()),
        ["a", "b"]
    );
}

#[test]
fn check_reports_schema_failure_with_path() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("query_notes")
        .unwrap()
        .add("a", rec(json!({"title": "A"})))
        .unwrap();
    drop(vault);
    scratch.write_schema(
        "notes.yaml",
        "name: query_notes\nkind: document\nschema_version: 1\nfields:\n  - name: title\n    required: true\n    string: {}\n  - name: tag\n    required: true\n    string: {}\n",
    );
    let vault = scratch.reopen();
    let got = vault.collection("query_notes").unwrap().get("a").unwrap();
    assert_eq!(got.valid, Some(false));
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(err.to_string().contains("a.md"));
}

#[test]
fn reindex_rebuilds_format_and_counts() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection.add("a", rec(json!({"title": "A"}))).unwrap();
    collection.add("b", rec(json!({"title": "B"}))).unwrap();
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(2));
    assert_eq!(counts.skipped_invalid, Some(0));
    let format = fs::read_to_string(vault.index_dir().join("format")).unwrap();
    assert_eq!(format.trim(), "3");
    fs::write(vault.index_dir().join("format"), "999\n").unwrap();
    fs::write(vault.index_dir().join("stolen.json"), r#"{"id":"foreign"}"#).unwrap();
    let err = collection
        .search(&scheda::SearchQuery::default().with_text("foreign"), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IndexVersion));
    assert_ne!(err.to_string(), "foreign");
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(2));
    let format = fs::read_to_string(vault.index_dir().join("format")).unwrap();
    assert_eq!(format.trim(), "3");
    assert!(!vault.index_dir().join("stolen.json").exists());
    let got = collection.get("a").unwrap();
    assert_eq!(got.valid, Some(true));
}

#[test]
fn conflict_markers_fail_check() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection.add("a", rec(json!({"title": "A"}))).unwrap();
    let rel = collection.get("a").unwrap().path.unwrap();
    fs::write(vault.path().join(&rel), "<<<<<<< HEAD\nid: a\n>>>>>>>\n").unwrap();
    let got = collection.get("a").unwrap();
    assert_eq!(got.valid, Some(false));
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::ConflictMarkers));
    assert!(err.to_string().contains(&rel));
}

#[test]
fn phrase_matches_tokens_in_title() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("panda", rec(json!({"title": "Giant panda"})))
        .unwrap();
    collection
        .add("fox", rec(json!({"title": "Red fox"})))
        .unwrap();
    let found = collection
        .find(
            &filter(json!({"phrase":{"field":"title","terms":["giant","panda"]}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&found), ["panda"]);
    let err = collection
        .find(
            &filter(json!({"phrase":{"field":"id","terms":["giant","panda"]}})),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn phrase_prefix_fuzzy_regex_and_range() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("job", rec(json!({"title": "part time job", "tag": "blue"})))
        .unwrap();
    collection
        .add(
            "hobby",
            rec(json!({"title": "part of the job", "tag": "red"})),
        )
        .unwrap();
    collection
        .add("panda", rec(json!({"title": "panda", "tag": "amber"})))
        .unwrap();

    let prefix = collection
        .find(
            &filter(json!({"phrasePrefix":{"field":"title","terms":["part"],"prefix":"ti"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&prefix), ["job"]);

    let fuzzy = collection
        .find(
            &filter(json!({"fuzzy":{"field":"title","term":"pnda","distance":1}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&fuzzy), ["panda"]);

    let regex = collection
        .find(
            &filter(json!({"regex":{"field":"title","pattern":"pand.*"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&regex), ["panda"]);

    let regex_phrase = collection
        .find(
            &filter(json!({"regexPhrase":{"field":"title","patterns":["part","tim.*"]}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&regex_phrase), ["job"]);

    let tags = collection
        .find(
            &filter(json!({
                "range":{
                    "field":"tag",
                    "lower":{"included":"a"},
                    "upper":{"excluded":"c"}
                }
            })),
            0,
            100,
        )
        .unwrap();
    let mut tag_ids = ids(&tags);
    tag_ids.sort_unstable();
    assert_eq!(tag_ids, ["job", "panda"]);

    let one = collection
        .find(
            &filter(json!({
                "range":{
                    "field":"id",
                    "lower":{"included":"job"},
                    "upper":{"included":"job"}
                }
            })),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&one), ["job"]);
}

#[test]
fn more_like_this_from_id_ranks_similar_title() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "a",
            rec(json!({"title": "the diary of a young girl about school"})),
        )
        .unwrap();
    collection
        .add(
            "b",
            rec(json!({"title": "the diary of a young woman about school"})),
        )
        .unwrap();
    collection
        .add("c", rec(json!({"title": "komodo dragon of indonesia"})))
        .unwrap();
    let found = collection
        .find(
            &filter(json!({
                "moreLikeThis":{
                    "id":"a",
                    "minDocFrequency":1,
                    "minTermFrequency":1,
                    "minWordLength":2
                }
            })),
            0,
            10,
        )
        .unwrap();
    let got = ids(&found);
    assert!(got.contains(&"b"), "expected similar record b, got {got:?}");
    if got.contains(&"c") {
        assert!(
            got.iter().position(|id| *id == "b") < got.iter().position(|id| *id == "c"),
            "b should rank before distractor c, got {got:?}"
        );
    }
}

#[test]
fn exists_json_subpaths_matches_nested_field() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "notes.yaml",
        r#"
name: query_notes
kind: document
schema_version: 1
fields:
  - name: title
    string: {}
  - name: meta
    object:
      fields:
        - name: city
          string: {}
"#,
    );
    let vault = scratch.reopen();
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "paris",
            rec(json!({"title": "one", "meta": {"city": "Paris"}})),
        )
        .unwrap();
    collection
        .add("none", rec(json!({"title": "two"})))
        .unwrap();
    let nested = collection
        .find(&filter(json!({"exists":{"field":"city"}})), 0, 100)
        .unwrap();
    assert_eq!(ids(&nested), ["paris"]);
    let with_subpaths = collection
        .find(
            &filter(json!({"exists":{"field":"meta","jsonSubpaths":true}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&with_subpaths), ["paris"]);
}

#[test]
fn find_regex_and_phrase_on_body() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "panda",
            rec_body(json!({"title": "Red fox"}), "The giant panda eats bamboo."),
        )
        .unwrap();
    collection
        .add(
            "fox",
            rec_body(json!({"title": "Giant panda"}), "A red fox in the woods."),
        )
        .unwrap();
    let regex = collection
        .find(
            &filter(json!({"regex":{"field":"body","pattern":"bamb.*"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&regex), ["panda"]);
    let phrase = collection
        .find(
            &filter(json!({"phrase":{"field":"body","terms":["giant","panda"]}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&phrase), ["panda"]);
    let title_only = collection
        .find(
            &filter(json!({"regex":{"field":"title","pattern":"bamb.*"}})),
            0,
            100,
        )
        .unwrap();
    assert!(ids(&title_only).is_empty());
}

#[test]
fn exclude_fts_body_is_not_a_find_key() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "notes.yaml",
        r#"
name: query_notes
kind: document
schema_version: 1
body:
  exclude_fts: true
fields:
  - name: title
    string: {}
"#,
    );
    let vault = scratch.reopen();
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("panda", rec_body(json!({"title": "Panda"}), "eats bamboo"))
        .unwrap();
    let err = collection
        .find(
            &filter(json!({"regex":{"field":"body","pattern":"bamboo"}})),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn journal_rejects_body_filter() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    let collection = vault.collection("query_log").unwrap();
    collection.add("a", rec(json!({"tag": "one"}))).unwrap();
    let err = collection
        .find(&filter(json!({"exists":{"field":"body"}})), 0, 10)
        .unwrap_err();
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
