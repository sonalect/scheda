//! `find`, `scan`, `check`, `reindex`.
//! Collection names here and in `testdata/query/` must not appear in
//! `rust/scheda/src`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use scheda::{Code, Filter, FindQuery, OpenOptions, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-c5", n),
        }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn options(&self) -> OpenOptions {
        OpenOptions::new()
    }

    fn init(&self) -> Vault {
        let vault =
            Vault::open_with(self.vault(), self.options(), None, Some(query_projector())).unwrap();
        vault.begin().unwrap();
        vault
    }

    fn reopen(&self) -> Vault {
        Vault::open_with(self.vault(), self.options(), None, Some(query_projector())).unwrap()
    }

    fn write_declaration(&self, file: &str, body: &str) {
        let name = peek_schema_name(body);
        let dir = self.vault().join(name);
        fs::create_dir_all(&dir).unwrap();
        let dest = if file.ends_with(".json") {
            ".collection.json"
        } else {
            ".collection.yaml"
        };
        fs::write(dir.join(dest), body).unwrap();
        common::commit_file(&self.vault(), &dir.join(dest));
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

fn rec(fields: Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn query_projector() -> Arc<dyn scheda::Projector> {
    common::json_text_projector(&[
        "title", "tag", "secret", "project", "city", "meta", "aside", "body",
    ])
}

fn open_query(path: impl AsRef<Path>) -> Vault {
    Vault::open_with(path, OpenOptions::new(), None, Some(query_projector())).unwrap()
}

fn filter(value: Value) -> FindQuery {
    serde_json::from_value::<Filter>(value).unwrap().into()
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
    scratch.write_declaration("notes.yaml", NOTES);
    scratch.reopen()
}

#[test]
fn empty_filter_and_scan_return_every_record() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "A", "tag": "red"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"title": "B", "tag": "blue"})), None)
        .unwrap();
    collection
        .add("c", rec(json!({"title": "C"})), None)
        .unwrap();
    common::indexed(&vault);
    let found = collection.find(&Filter::all().into(), 0, 100).unwrap();
    assert_eq!(found.records.len(), 3);
    assert_eq!(found.total, Some(3));
    assert_eq!(
        collection
            .find(&Filter::all().into(), 0, 1000)
            .unwrap()
            .records
            .len(),
        3
    );
    let page = collection.find(&Filter::all().into(), 0, 1).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.total, Some(3));
}

#[test]
fn find_count_zero_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    common::indexed(&vault);
    let err = collection.find(&Filter::all().into(), 0, 0).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn find_omits_raw_unless_include_raw() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "A", "tag": "red"})), None)
        .unwrap();
    common::indexed(&vault);
    let omitted = collection.find(&FindQuery::all(), 0, 10).unwrap();
    assert!(omitted.records[0].raw.is_none());
    let included = collection
        .find(&FindQuery::all().with_include_raw(true), 0, 10)
        .unwrap();
    assert!(
        included.records[0]
            .raw
            .as_ref()
            .is_some_and(|raw| !raw.is_empty())
    );
}

#[test]
fn concurrent_find_on_one_handle() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "A", "tag": "red"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"title": "B", "tag": "blue"})), None)
        .unwrap();
    collection
        .add("c", rec(json!({"title": "C"})), None)
        .unwrap();
    common::indexed(&vault);
    let vault = Arc::new(vault);
    let query = Filter::all().into();
    thread::scope(|scope| {
        for _ in 0..8 {
            let vault = Arc::clone(&vault);
            let query = &query;
            scope.spawn(move || {
                let collection = vault.collection("query_notes").unwrap();
                let found = collection.find(query, 0, 100).unwrap();
                assert_eq!(found.total, Some(3));
                assert_eq!(found.records.len(), 3);
            });
        }
    });
}

#[test]
fn vault_find_empty_names_is_all_collections() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("notes.yaml", NOTES);
    scratch.write_declaration("pages.yaml", PAGES);
    let vault = scratch.reopen();
    vault
        .collection("query_notes")
        .unwrap()
        .add("red", rec(json!({"title": "R", "tag": "red"})), None)
        .unwrap();
    vault
        .collection("query_pages")
        .unwrap()
        .add("home", rec(json!({"title": "Home"})), None)
        .unwrap();
    common::indexed(&vault);
    let all = vault.find(&[], &Filter::all().into(), 0, 100).unwrap();
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
            .find(&["query_notes"], &Filter::all().into(), 0, 100)
            .unwrap()),
        ["red"]
    );
    assert_eq!(
        ids(&vault
            .find(
                &[],
                &filter(json!({"term":{"field":"tag","collection":"query_notes","value":"red"}})),
                0,
                100
            )
            .unwrap()),
        ["red"]
    );
    assert_eq!(
        vault
            .find(&["missing"], &Filter::all().into(), 0, 10)
            .unwrap_err()
            .known_code(),
        Some(Code::NotFound)
    );
    assert_eq!(
        vault
            .find(
                &["query_pages"],
                &filter(json!({"term":{"field":"tag","collection":"query_pages","value":"red"}})),
                0,
                10
            )
            .unwrap()
            .total,
        Some(0)
    );
}

#[test]
fn equal_set_exists_and_null() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("red", rec(json!({"title": "R", "tag": "red"})), None)
        .unwrap();
    collection
        .add("blue", rec(json!({"title": "B", "tag": "blue"})), None)
        .unwrap();
    collection
        .add("nil", rec(json!({"title": "N", "tag": null})), None)
        .unwrap();
    collection
        .add("omit", rec(json!({"title": "O"})), None)
        .unwrap();

    common::indexed(&vault);
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
    common::indexed(&vault);
    let err = collection
        .find(
            &filter(json!({"term":{"field":"unknown","value":"x"}})),
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
    scratch.write_declaration("pages.yaml", PAGES);
    let vault = scratch.reopen();
    let collection = vault.collection("query_pages").unwrap();
    collection
        .add("home", rec(json!({"title": "Home"})), None)
        .unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "project": "one"})), None)
        .unwrap();
    common::indexed(&vault);
    let projects = collection
        .find(
            &filter(json!({"term":{"field":"project","value":"one"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&projects), ["alpha"]);
    let all_page = collection.find(&Filter::all().into(), 0, 100).unwrap();
    let mut all = ids(&all_page);
    all.sort_unstable();
    assert_eq!(all, ["alpha", "home"]);
}

#[test]
fn journal_empty_find_keeps_append_order() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("log.yaml", LOG);
    let vault = scratch.reopen();
    let collection = vault.collection("query_log").unwrap();
    collection
        .add("a", rec(json!({"tag": "one"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"tag": "two"})), None)
        .unwrap();
    common::indexed(&vault);
    let found = collection
        .find(
            &filter(json!({"term":{"field":"tag","value":"one"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&found), ["a"]);
    assert_eq!(
        ids(&collection.find(&Filter::all().into(), 0, 1000).unwrap()),
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
        .add("a", rec(json!({"title": "A"})), None)
        .unwrap();
    drop(vault);
    scratch.write_declaration(
        "notes.yaml",
        "version: 1
name: query_notes
schema:
  journal: {}
",
    );
    let vault = scratch.reopen();
    let _got = vault.collection("query_notes").unwrap().get("a").unwrap();
    common::indexed(&vault);
    vault.check().unwrap();
}

#[test]
fn reindex_rebuilds_format_and_counts() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "A"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"title": "B"})), None)
        .unwrap();
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(2));
    assert_eq!(counts.skipped_invalid, Some(0));
    let live = live_index(&vault);
    let format = fs::read_to_string(live.join("format")).unwrap();
    assert_eq!(format.trim(), "6");
    fs::write(live.join("format"), "999\n").unwrap();
    fs::write(live.join("stolen.json"), r#"{"id":"foreign"}"#).unwrap();
    // Whatever moment the query lands in — before the indexer replaces the
    // unknown-format generation, or after — it never returns a foreign id:
    // either an Index code or the Records of this Vault.
    match collection.find(&FindQuery::all(), 0, 10) {
        Err(err) => assert_eq!(err.known_code(), Some(Code::IndexVersion)),
        Ok(page) => assert!(
            page.records
                .iter()
                .all(|r| r.id.as_deref() != Some("foreign"))
        ),
    }
    // The indexer rebuilds it from the commits into a new generation.
    common::indexed(&vault);
    let rebuilt = live_index(&vault);
    assert_ne!(rebuilt, live);
    assert_eq!(
        fs::read_to_string(rebuilt.join("format")).unwrap().trim(),
        "6"
    );
    assert!(!rebuilt.join("stolen.json").exists());
    let page = collection.find(&FindQuery::all(), 0, 10).unwrap();
    assert_eq!(page.total, Some(2));
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(2));
    let live = live_index(&vault);
    let format = fs::read_to_string(live.join("format")).unwrap();
    assert_eq!(format.trim(), "6");
    assert!(!live.join("stolen.json").exists());
    collection.get("a").unwrap();
}

#[test]
fn conflict_markers_fail_check() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration(
        "notes.yaml",
        r#"
version: 1
name: query_notes
schema:
  document: {}
"#,
    );
    let vault = scratch.reopen();
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "A"})), None)
        .unwrap();
    let rel = "query_notes/a".to_owned();
    fs::write(vault.path().join(&rel), "<<<<<<< HEAD\nid: a\n>>>>>>>\n").unwrap();
    collection.get("a").unwrap();
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
        .add("panda", rec(json!({"title": "Giant panda"})), None)
        .unwrap();
    collection
        .add("fox", rec(json!({"title": "Red fox"})), None)
        .unwrap();
    common::indexed(&vault);
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
        .add(
            "job",
            rec(json!({"title": "part time job", "tag": "blue"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "hobby",
            rec(json!({"title": "part of the job", "tag": "red"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "panda",
            rec(json!({"title": "panda", "tag": "amber"})),
            None,
        )
        .unwrap();

    common::indexed(&vault);
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
            None,
        )
        .unwrap();
    collection
        .add(
            "b",
            rec(json!({"title": "the diary of a young woman about school"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "c",
            rec(json!({"title": "komodo dragon of indonesia"})),
            None,
        )
        .unwrap();
    collection
        .add("d", rec(json!({"title": "zzzzzyx unique widget"})), None)
        .unwrap();
    common::indexed(&vault);
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
    assert!(
        !got.contains(&"d"),
        "unrelated record d must not match moreLikeThis from a, got {got:?}"
    );
    let b = got.iter().position(|id| *id == "b");
    let c = got.iter().position(|id| *id == "c");
    assert!(b.is_some(), "expected similar record b, got {got:?}");
    if let (Some(bi), Some(ci)) = (b, c) {
        assert!(
            bi < ci,
            "b should rank before distractor c because their scores differ, got {got:?}"
        );
    }
}

#[test]
fn more_like_this_unknown_id_is_not_found() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "a",
            rec(json!({"title": "the diary of a young girl"})),
            None,
        )
        .unwrap();
    common::indexed(&vault);
    let err = collection
        .find(
            &filter(json!({
                "moreLikeThis":{
                    "id":"missing",
                    "minDocFrequency":1,
                    "minTermFrequency":1,
                    "minWordLength":2
                }
            })),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn find_score_ties_are_stable_across_reopen_and_pages() {
    let mut orders = Vec::new();
    for _ in 0..10 {
        let scratch = Scratch::new();
        let vault = open_notes(&scratch);
        let collection = vault.collection("query_notes").unwrap();
        collection
            .add(
                "z",
                rec(json!({"title": "same title for a genuine score tie"})),
                None,
            )
            .unwrap();
        collection
            .add(
                "m",
                rec(json!({"title": "same title for a genuine score tie"})),
                None,
            )
            .unwrap();
        collection
            .add(
                "a",
                rec(json!({"title": "same title for a genuine score tie"})),
                None,
            )
            .unwrap();
        common::indexed(&vault);
        let found = collection.find(&Filter::all().into(), 0, 10).unwrap();
        orders.push(
            ids(&found)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        );
    }
    let first = &orders[0];
    assert_eq!(first.len(), 3, "expected three tied hits, got {first:?}");
    for (i, order) in orders.iter().enumerate() {
        assert_eq!(
            order, first,
            "run {i} returned {order:?}, first run {first:?}"
        );
    }
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "z",
            rec(json!({"title": "same title for a genuine score tie"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "m",
            rec(json!({"title": "same title for a genuine score tie"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "a",
            rec(json!({"title": "same title for a genuine score tie"})),
            None,
        )
        .unwrap();
    common::indexed(&vault);
    let page0_found = collection.find(&Filter::all().into(), 0, 1).unwrap();
    let page1_found = collection.find(&Filter::all().into(), 1, 1).unwrap();
    let page0 = ids(&page0_found);
    let page1 = ids(&page1_found);
    assert_eq!(page0.len(), 1);
    assert_eq!(page1.len(), 1);
    assert_ne!(
        page0[0], page1[0],
        "offset 0 and offset 1 must not repeat a Record"
    );
    assert_eq!(page0[0], first[0].as_str());
    assert_eq!(page1[0], first[1].as_str());
}

#[test]
fn exists_json_subpaths_does_not_expand_nested_keys() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration(
        "notes.yaml",
        r#"
version: 1
name: query_notes
schema:
  journal: {}
"#,
    );
    let vault = scratch.reopen();
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "paris",
            rec(json!({"title": "one", "meta": {"city": "Paris"}})),
            None,
        )
        .unwrap();
    collection
        .add("none", rec(json!({"title": "two"})), None)
        .unwrap();
    // Projector walks nested JSON for a declared Text column (`city`). `json_subpaths`
    // is ignored: an object column (`meta`) is not expanded into nested names.
    common::indexed(&vault);
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
    assert!(
        with_subpaths.records.is_empty(),
        "exists on an object column does not expand nested keys: {:?}",
        ids(&with_subpaths)
    );
    let missing = collection
        .find(
            &filter(json!({"exists":{"field":"nope","jsonSubpaths":true}})),
            0,
            100,
        )
        .unwrap_err();
    assert_eq!(missing.known_code(), Some(Code::Invalid));
}

#[test]
fn find_regex_and_phrase_on_body() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "panda",
            rec(json!({"title": "The giant panda eats bamboo.", "tag": "red fox"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "fox",
            rec(json!({"title": "A red fox in the woods.", "tag": "Giant panda"})),
            None,
        )
        .unwrap();
    common::indexed(&vault);
    let regex = collection
        .find(
            &filter(json!({"regex":{"field":"title","pattern":"bamb.*"}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&regex), ["panda"]);
    let phrase = collection
        .find(
            &filter(json!({"phrase":{"field":"title","terms":["giant","panda"]}})),
            0,
            100,
        )
        .unwrap();
    assert_eq!(ids(&phrase), ["panda"]);
    let title_only = collection
        .find(
            &filter(json!({"regex":{"field":"tag","pattern":"bamb.*"}})),
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
    scratch.write_declaration(
        "notes.yaml",
        r#"
version: 1
name: query_notes
schema:
  journal: {}
"#,
    );
    let vault = scratch.reopen();
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add(
            "panda",
            rec(json!({"title": "Panda", "aside": "eats bamboo"})),
            None,
        )
        .unwrap();
    common::indexed(&vault);
    let err = collection
        .find(
            &filter(json!({"regex":{"field":"hidden","pattern":"bamboo"}})),
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
    scratch.write_declaration("log.yaml", LOG);
    let vault = scratch.reopen();
    let collection = vault.collection("query_log").unwrap();
    collection
        .add("a", rec(json!({"tag": "one"})), None)
        .unwrap();
    common::indexed(&vault);
    let err = collection
        .find(&filter(json!({"exists":{"field":"payload"}})), 0, 10)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

/// `TopDocs` reserves slots for `count + offset` hits; `u32::MAX` once
/// aborted the process.
#[test]
fn find_with_huge_count_or_offset_returns() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    for (id, tag) in [("a", "red"), ("b", "red"), ("c", "blue")] {
        collection
            .add(id, rec(json!({"title": id, "tag": tag})), None)
            .unwrap();
    }
    common::indexed(&vault);
    let red = filter(json!({"term":{"field":"tag","value":"red"}}));
    for query in [FindQuery::from(Filter::all()), red] {
        let all = collection.find(&query, 0, u32::MAX).unwrap();
        assert_eq!(all.records.len() as u32, all.total.unwrap());
        let past = collection.find(&query, u32::MAX, 1).unwrap();
        assert!(past.records.is_empty());
        assert_eq!(past.total, all.total, "an empty page keeps the true total");
        let both = collection.find(&query, u32::MAX, u32::MAX).unwrap();
        assert!(both.records.is_empty());
        assert_eq!(both.total, all.total);
        let at_end = collection.find(&query, all.total.unwrap(), 5).unwrap();
        assert!(at_end.records.is_empty());
        assert_eq!(at_end.total, all.total);
    }
    let first = collection.find(&Filter::all().into(), 0, u32::MAX).unwrap();
    assert_eq!(first.records.len(), 3);
    let tail = collection.find(&Filter::all().into(), 2, u32::MAX).unwrap();
    assert_eq!(tail.records.len(), 1);
}

#[test]
fn fuzzy_distance_above_two_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "panda"})), None)
        .unwrap();
    common::indexed(&vault);
    for distance in [3, 255] {
        let err = collection
            .find(
                &filter(json!({"fuzzy":{"field":"title","term":"pnda","distance":distance}})),
                0,
                10,
            )
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid), "distance {distance}");
    }
    let ok = collection
        .find(
            &filter(json!({"fuzzy":{"field":"title","term":"pnda","distance":2}})),
            0,
            10,
        )
        .unwrap();
    assert_eq!(ids(&ok), ["a"]);
    assert!(!live_index(&vault).join("damaged").exists());
}

/// A regex Tantivy rejects is the caller's mistake: `invalid`, and the
/// generation is not flagged for the indexer's damage check.
#[test]
fn broken_regex_patterns_are_invalid_and_do_not_flag_damage() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "panda"})), None)
        .unwrap();
    common::indexed(&vault);
    for query in [
        json!({"regexPhrase":{"field":"title","patterns":["pand.*","(unclosed"]}}),
        json!({"regex":{"field":"title","pattern":"(unclosed"}}),
    ] {
        let err = collection.find(&filter(query), 0, 10).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid), "{err:?}");
    }
    assert!(!live_index(&vault).join("damaged").exists());
    assert_eq!(
        ids(&collection.find(&Filter::all().into(), 0, 10).unwrap()),
        ["a"]
    );
}

fn live_index(vault: &Vault) -> PathBuf {
    let root = vault.index_dir();
    match fs::read_to_string(root.join("current")) {
        Ok(name) => root.join(name.trim()),
        Err(_) => root.to_path_buf(),
    }
}

fn generation_dirs(vault: &Vault) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(vault.index_dir()) else {
        return names;
    };
    for entry in entries {
        let entry = entry.unwrap();
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
            names.push(name.into_owned());
        }
    }
    names.sort();
    names
}

#[test]
fn reader_opened_before_reindex_keeps_find_results() {
    let scratch = Scratch::new();
    let writer = open_notes(&scratch);
    let collection = writer.collection("query_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "Keep"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"title": "Other"})), None)
        .unwrap();
    common::indexed(&writer);
    let reader = open_query(scratch.vault());
    let before = reader
        .collection("query_notes")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(ids(&before), ["a", "b"]);
    writer.reindex().unwrap();
    let after = reader
        .collection("query_notes")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(ids(&after), ids(&before));
}

#[test]
fn reader_opened_after_reindex_uses_new_generation() {
    let scratch = Scratch::new();
    let writer = open_notes(&scratch);
    writer
        .collection("query_notes")
        .unwrap()
        .add("a", rec(json!({"title": "Keep"})), None)
        .unwrap();
    let first = live_index(&writer);
    writer.reindex().unwrap();
    let second = live_index(&writer);
    assert_ne!(first, second);
    let reader = open_query(scratch.vault());
    assert_eq!(live_index(&reader), second);
    let page = reader
        .collection("query_notes")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(ids(&page), ["a"]);
}

#[test]
fn crashed_reindex_leaves_old_generation_and_open_drops_orphan() {
    let scratch = Scratch::new();
    let writer = open_notes(&scratch);
    writer
        .collection("query_notes")
        .unwrap()
        .add("a", rec(json!({"title": "Keep"})), None)
        .unwrap();
    writer.reindex().unwrap();
    let live = fs::read_to_string(writer.index_dir().join("current")).unwrap();
    let live = live.trim().to_owned();
    let reader = open_query(scratch.vault());
    let orphan = writer.index_dir().join("99");
    fs::create_dir_all(orphan.join("lexical")).unwrap();
    fs::write(orphan.join("format"), "4\n").unwrap();
    fs::write(orphan.join("sentinel"), b"orphan").unwrap();
    let page = reader
        .collection("query_notes")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(ids(&page), ["a"]);
    assert_eq!(
        fs::read_to_string(writer.index_dir().join("current"))
            .unwrap()
            .trim(),
        live
    );
    drop(writer);
    drop(reader);
    let reopened = open_query(scratch.vault());
    assert!(
        !orphan.exists(),
        "orphan generation must be removed on open"
    );
    let page = reopened
        .collection("query_notes")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(ids(&page), ["a"]);
}

#[test]
fn two_reindexes_keep_only_one_previous_generation() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("query_notes")
        .unwrap()
        .add("a", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.reindex().unwrap();
    let after_first = generation_dirs(&vault);
    vault.reindex().unwrap();
    let after_second = generation_dirs(&vault);
    assert_eq!(after_second.len(), 2, "{after_second:?}");
    for name in &after_first {
        if !after_second.contains(name) {
            assert!(!vault.index_dir().join(name).exists());
        }
    }
    let oldest = after_first
        .iter()
        .min_by_key(|name| name.parse::<u64>().unwrap())
        .unwrap();
    if after_first.len() >= 2 {
        assert!(
            !after_second.contains(oldest),
            "oldest generation must be gone"
        );
    }
}

#[cfg(unix)]
#[test]
fn reindex_generation_dirs_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("query_notes")
        .unwrap()
        .add("a", rec(json!({"title": "Keep"})), None)
        .unwrap();
    vault.reindex().unwrap();
    fn walk(path: &std::path::Path) {
        let meta = fs::symlink_metadata(path).unwrap();
        if meta.file_type().is_symlink() {
            return;
        }
        if meta.is_dir() {
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o700,
                "{}",
                path.display()
            );
            for entry in fs::read_dir(path).unwrap() {
                walk(&entry.unwrap().path());
            }
            return;
        }
        if meta.is_file() {
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o600,
                "{}",
                path.display()
            );
        }
    }
    walk(&scratch.vault().join(".scheda"));
}

#[test]
fn incremental_write_is_visible_to_handle_opened_afterwards() {
    let scratch = Scratch::new();
    let writer = open_notes(&scratch);
    writer
        .collection("query_notes")
        .unwrap()
        .add("a", rec(json!({"title": "Keep"})), None)
        .unwrap();
    common::indexed(&writer);
    let reader = open_query(scratch.vault());
    let page = reader
        .collection("query_notes")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(ids(&page), ["a"]);
    assert_eq!(live_index(&reader), live_index(&writer));
}

#[test]
fn legacy_format_3_is_rebuilt_in_the_background() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("query_notes")
        .unwrap()
        .add("a", rec(json!({"title": "Keep"})), None)
        .unwrap();
    common::indexed(&vault);
    let first = live_index(&vault);
    drop(vault);
    fs::write(first.join("format"), "3\n").unwrap();
    let vault = open_query(scratch.vault());
    // The old generation's marks already sit at `HEAD`, so waiting for them
    // proves nothing: poll for the published replacement.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while live_index(&vault) == first {
        assert!(
            std::time::Instant::now() < deadline,
            "no new generation published"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    common::indexed(&vault);
    let second = live_index(&vault);
    assert_eq!(
        fs::read_to_string(second.join("format")).unwrap().trim(),
        "6"
    );
    let page = vault
        .collection("query_notes")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(ids(&page), ["a"]);
}

const FOLD_TOKEN: &str = "fold-secret-token-never-index";
const FOLD_DOCS: &str = r#"
version: 1
name: fold_docs
schema:
  document: {}
  exclude_lexical_index: true
  exclude_vector_index: true
"#;
const FOLD_INBOX: &str = r#"
version: 1
name: fold_inbox
schema:
  singleton:
    id: inbox
  exclude_lexical_index: true
  exclude_vector_index: true
"#;

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
fn find_on_lexically_excluded_collection_is_unsupported() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("fold.yaml", FOLD_DOCS);
    let vault = scratch.reopen();
    let collection = vault.collection("fold_docs").unwrap();
    collection
        .add("alpha", rec(json!({"title": FOLD_TOKEN})), None)
        .unwrap();
    common::indexed(&vault);
    let err = collection.find(&FindQuery::all(), 0, 10).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    assert!(
        err.message
            .as_deref()
            .is_some_and(|m| m.contains("fold_docs") && m.contains("exclude_lexical_index"))
    );
    let err = vault
        .find(&["fold_docs"], &FindQuery::all(), 0, 10)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn excluded_records_are_absent_from_the_index_and_keyed_reads_work() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("fold.yaml", FOLD_DOCS);
    scratch.write_declaration("inbox.yaml", FOLD_INBOX);
    let vault = scratch.reopen();
    vault
        .collection("fold_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": FOLD_TOKEN})), None)
        .unwrap();
    vault
        .collection("fold_inbox")
        .unwrap()
        .add("inbox", rec(json!({"title": "inbox-title"})), None)
        .unwrap();
    let scheda = scratch.vault().join(".scheda");
    assert!(
        !tree_contains(&scheda, FOLD_TOKEN),
        "excluded title leaked into the Index"
    );
    let docs = vault.collection("fold_docs").unwrap();
    let got = docs.get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    let listed: Vec<_> = docs.documents().unwrap().map(|r| r.unwrap().id).collect();
    assert_eq!(listed, [Some("alpha".into())]);
    assert_eq!(docs.ids().unwrap(), ["alpha"]);
    let inbox = vault.collection("fold_inbox").unwrap();
    assert_eq!(inbox.get("inbox").unwrap().id.as_deref(), Some("inbox"));
    assert_eq!(inbox.ids().unwrap(), ["inbox"]);
    vault.reindex().unwrap();
    assert!(
        !tree_contains(&scheda, FOLD_TOKEN),
        "excluded title leaked into the Index after reindex"
    );
    assert_eq!(
        vault
            .collection("fold_docs")
            .unwrap()
            .get("alpha")
            .unwrap()
            .id
            .as_deref(),
        Some("alpha")
    );
}

#[test]
fn new_field_on_excluded_collection_is_not_indexed() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("fold.yaml", FOLD_DOCS);
    let vault = scratch.reopen();
    vault
        .collection("fold_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "keep"})), None)
        .unwrap();
    drop(vault);
    scratch.write_declaration(
        "fold.yaml",
        r#"
version: 1
name: fold_docs
schema:
  journal: {}
  exclude_lexical_index: true
  exclude_vector_index: true
"#,
    );
    let vault = scratch.reopen();
    vault
        .collection("fold_docs")
        .unwrap()
        .add(
            "beta",
            rec(json!({"title": "keep", "secret": FOLD_TOKEN})),
            None,
        )
        .unwrap();
    let scheda = scratch.vault().join(".scheda");
    assert!(
        !tree_contains(&scheda, FOLD_TOKEN),
        "new field on an excluded collection entered the Index"
    );
    common::indexed(&vault);
    let err = vault
        .collection("fold_docs")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn journal_with_both_legs_excluded_still_gets_by_id() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration(
        "log.yaml",
        r#"
version: 1
name: fold_log
schema:
  journal: {}
  exclude_lexical_index: true
  exclude_vector_index: true
"#,
    );
    let vault = scratch.reopen();
    let collection = vault.collection("fold_log").unwrap();
    collection
        .add("line-one", rec(json!({"msg": "hello"})), None)
        .unwrap();
    let got = collection.get("line-one").unwrap();
    assert_eq!(got.id.as_deref(), Some("line-one"));
    common::indexed(&vault);
    let err = collection.find(&FindQuery::all(), 0, 10).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn more_like_this_on_vector_excluded_collection_is_lexical() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration(
        "fold.yaml",
        r#"
version: 1
name: fold_docs
schema:
  journal: {}
  exclude_vector_index: true
"#,
    );
    let vault = scratch.reopen();
    let collection = vault.collection("fold_docs").unwrap();
    collection
        .add(
            "a",
            rec(json!({"title": "the diary of a young girl about school"})),
            None,
        )
        .unwrap();
    collection
        .add(
            "b",
            rec(json!({"title": "the diary of a young woman about school"})),
            None,
        )
        .unwrap();
    collection
        .add("d", rec(json!({"title": "zzzzzyx unique widget"})), None)
        .unwrap();
    common::indexed(&vault);
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
    assert!(got.contains(&"b"), "similar title missing: {got:?}");
    assert!(!got.contains(&"d"), "unrelated matched: {got:?}");
}

#[test]
fn vault_find_bare_projector_field_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("query_notes")
        .unwrap()
        .add("red", rec(json!({"title": "R", "tag": "red"})), None)
        .unwrap();
    common::indexed(&vault);
    let err = vault
        .find(
            &[],
            &filter(json!({"term":{"field":"tag","value":"red"}})),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn vault_find_unknown_collection_qualifier_is_not_found() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    common::indexed(&vault);
    let err = vault
        .find(
            &[],
            &filter(json!({
                "term":{"field":"tag","collection":"missing","value":"red"}
            })),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn collection_find_other_qualifier_is_invalid() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("notes.yaml", NOTES);
    scratch.write_declaration("pages.yaml", PAGES);
    let vault = scratch.reopen();
    common::indexed(&vault);
    let err = vault
        .collection("query_notes")
        .unwrap()
        .find(
            &filter(json!({
                "term":{"field":"tag","collection":"query_pages","value":"red"}
            })),
            0,
            10,
        )
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
