//! A1: `catalog` from Schema (`08` AGT-1, `12`). Collection names here and in
//! `testdata/schema/` must not appear in `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, FindQuery, OpenOptions, RecordAdd, Schema, SearchQuery, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-a1-{}-{n}", process::id()));
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

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), OpenOptions::new(), None).unwrap()
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
const EAST_LOG: &str = include_str!("../testdata/schema/east_log.yaml");
const WEST_INDEXED: &str = include_str!("../testdata/schema/west_indexed.yaml");
const WEST_ISOLATED: &str = include_str!("../testdata/schema/west_isolated.yaml");
const SLUG: &str = include_str!("../testdata/schema/slug.yaml");

fn catalog_json(vault: &Vault) -> Value {
    serde_json::to_value(vault.info().unwrap()).unwrap()
}

fn collection<'a>(json: &'a Value, name: &str) -> &'a Value {
    json["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|col| col["name"] == name)
        .unwrap()
}

fn contains_collection_name(haystack: &str, name: &str) -> bool {
    haystack.match_indices(name).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + name.len()..].chars().next();
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        before.is_none_or(|c| !ident(c)) && after.is_none_or(|c| !ident(c))
    })
}

fn src_rs() -> String {
    [
        include_str!("../src/ann.rs"),
        include_str!("../src/embed.rs"),
        include_str!("../src/error.rs"),
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
fn catalog_json_matches_schema_files() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    scratch.write_schema("west_indexed.yaml", WEST_INDEXED);
    scratch.write_schema("log.yaml", EAST_LOG);
    let json = catalog_json(&scratch.reopen());
    let west = collection(&json, "west_docs");
    let west_schema = &west["schema"];
    assert!(west_schema.get("document").is_some());
    assert_eq!(west_schema["version"], 1);
    assert_eq!(west_schema["fields"][0]["name"], "title");
    assert_eq!(west_schema["fields"][0]["required"], true);
    assert!(west_schema["fields"][0].get("string").is_some());
    assert_eq!(west_schema["fields"][1]["name"], "tag");
    assert!(west_schema.get("isolated").is_none());

    let indexed = &collection(&json, "west_indexed")["schema"];
    assert!(indexed.get("document").is_some());
    assert_eq!(indexed["fields"][0]["name"], "title");
    assert!(indexed["fields"][0].get("string").is_some());
    assert_eq!(indexed["fields"][2]["name"], "rank");
    assert_eq!(indexed["fields"][2]["default"].as_f64(), Some(0.0));
    assert_eq!(indexed["fields"][3]["name"], "see");
    assert_eq!(indexed["fields"][3]["edge"]["collection"], "west_docs");
    assert!(indexed["fields"][3]["edge"].get("one").is_some());
    assert_eq!(indexed["fields"][3]["edge"]["strict"], true);

    let log = &collection(&json, "east_log")["schema"];
    assert!(log.get("journal").is_some());
}

#[test]
fn catalog_is_not_built_from_record_directories() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let forged = scratch.vault().join("east_docs");
    fs::create_dir_all(&forged).unwrap();
    fs::write(forged.join("ghost.md"), "---\nid: ghost\ntitle: no\n---\n").unwrap();
    fs::create_dir_all(scratch.vault().join("not_schema")).unwrap();
    fs::write(scratch.vault().join("not_schema").join("east.yaml"), EAST).unwrap();
    let json = catalog_json(&scratch.reopen());
    let names: Vec<_> = json["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|col| col["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["west_docs"]);
}

#[test]
fn catalog_follows_each_vault_schema() {
    let west = Scratch::new();
    let east = Scratch::new();
    drop(west.init());
    drop(east.init());
    west.write_schema("west.yaml", WEST);
    east.write_schema("east.yaml", EAST);
    let west_json = catalog_json(&west.reopen());
    let east_json = catalog_json(&east.reopen());
    assert!(
        collection(&west_json, "west_docs")["schema"]
            .get("document")
            .is_some()
    );
    assert!(
        west_json["collections"]
            .as_array()
            .unwrap()
            .iter()
            .all(|col| col["name"] != "east_docs")
    );
    assert!(collection(&east_json, "east_docs")["schema"]["version"] == 2);
    assert!(
        east_json["collections"]
            .as_array()
            .unwrap()
            .iter()
            .all(|col| col["name"] != "west_docs")
    );
    let haystack = src_rs();
    for name in [
        "west_docs",
        "east_docs",
        "east_log",
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
fn catalog_lists_isolated_without_fields() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    scratch.write_schema("isolated.yaml", WEST_ISOLATED);
    let vault = scratch.reopen();
    let json = catalog_json(&vault);
    let isolated = collection(&json, "west_isolated");
    assert_eq!(isolated["schema"]["isolated"], true);
    assert!(isolated["schema"].get("document").is_some());
    assert!(isolated["schema"].get("fields").is_none());
    let dump = isolated.to_string();
    assert!(!dump.contains("west_pass"));
    let err = vault.collection("west_isolated").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    vault.collection("west_docs").unwrap();
}

const TREE_SEALED: &str = "west-sealed-tree-token";
const NESTED_SEALED: &str = "west-sealed-nested-token";

fn plant_isolated_sealed(scratch: &Scratch) {
    let tree = scratch.vault().join("west_isolated").join("sealed.md");
    fs::write(
        tree,
        format!("---\nid: sealed\nwest_pass: {TREE_SEALED}\n---\n"),
    )
    .unwrap();
    let nested = scratch.vault().join(".isolated").join("west_isolated");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("sealed.md"),
        format!("---\nid: sealed\nwest_pass: {NESTED_SEALED}\n---\n"),
    )
    .unwrap();
}

fn assert_no_sealed(haystack: &str) {
    assert!(
        !haystack.contains(TREE_SEALED),
        "main-tree sealed value leaked: {haystack}"
    );
    assert!(
        !haystack.contains(NESTED_SEALED),
        "nested sealed value leaked: {haystack}"
    );
}

#[test]
fn isolated_sealed_values_stay_off_the_main_vault() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    scratch.write_schema("isolated.yaml", WEST_ISOLATED);
    let vault = scratch.reopen();
    vault
        .collection("west_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "Alpha public"})))
        .unwrap();
    plant_isolated_sealed(&scratch);
    vault.reindex().unwrap();

    let info = catalog_json(&vault);
    let isolated = collection(&info, "west_isolated");
    assert_eq!(isolated["schema"]["isolated"], true);
    assert!(isolated["schema"].get("fields").is_none());
    let info_dump = info.to_string();
    assert!(!info_dump.contains("west_pass"));
    assert_no_sealed(&info_dump);

    let page = vault.find(&[], &FindQuery::all(), 0, 10).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id.as_deref(), Some("alpha"));
    assert_eq!(page.records[0].collection.as_deref(), Some("west_docs"));
    let page_dump = serde_json::to_string(&page).unwrap();
    assert!(!page_dump.contains("west_pass"));
    assert_no_sealed(&page_dump);

    let hits = vault
        .search(&[], &SearchQuery::default().with_text("Alpha public"), None)
        .unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].id.as_deref(), Some("alpha"));
    assert_eq!(hits.hits[0].collection.as_deref(), Some("west_docs"));
    assert_no_sealed(&serde_json::to_string(&hits).unwrap());

    let miss = vault
        .search(&[], &SearchQuery::default().with_text(NESTED_SEALED), None)
        .unwrap();
    assert!(miss.hits.is_empty());
    let miss_tree = vault
        .search(&[], &SearchQuery::default().with_text(TREE_SEALED), None)
        .unwrap();
    assert!(miss_tree.hits.is_empty());

    let got = vault.collection("west_docs").unwrap().get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    assert_no_sealed(&serde_json::to_string(&got).unwrap());
    let missing = vault.collection("west_docs").unwrap().get("sealed");
    assert_eq!(missing.unwrap_err().known_code(), Some(Code::NotFound));

    let scoped = vault
        .collection("west_docs")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(scoped.records.len(), 1);
    assert_no_sealed(&serde_json::to_string(&scoped).unwrap());

    let err = vault.collection("west_isolated").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    let named = vault.find(&["west_isolated"], &FindQuery::all(), 0, 10);
    assert_eq!(named.unwrap_err().known_code(), Some(Code::Unsupported));
}

#[test]
fn catalog_rejects_reserved_id_field() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("slug.yaml", SLUG);
    let err = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn west_docs_decl() -> Schema {
    serde_json::from_value(json!({
        "version": 1,
        "document": {},
        "fields": [
            {"name": "title", "required": true, "string": {}},
            {"name": "tag", "string": {}}
        ]
    }))
    .unwrap()
}

#[test]
fn create_collection_then_insert_one_without_reopen() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    let written = vault
        .create_collection("west_docs", west_docs_decl())
        .unwrap();
    assert_eq!(written.name(), "west_docs");
    assert_eq!(written.kind(), scheda::Kind::Document);
    assert!(
        scratch
            .vault()
            .join("west_docs")
            .join(".schema.yaml")
            .is_file()
    );
    vault
        .collection("west_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "Hello", "tag": "x"})))
        .unwrap();
    let got = vault.collection("west_docs").unwrap().get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    let json = catalog_json(&vault);
    assert!(
        collection(&json, "west_docs")["schema"]
            .get("document")
            .is_some()
    );
}

#[test]
fn create_collection_missing_kind_writes_nothing() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    let decl: Schema = serde_json::from_value(json!({
        "version": 1,
        "fields": [{"name": "title", "string": {}}]
    }))
    .unwrap();
    let err = vault.create_collection("west_docs", decl).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(
        !scratch
            .vault()
            .join("west_docs")
            .join(".schema.yaml")
            .exists()
    );
}

#[test]
fn create_collection_does_not_persist_layout_json_key() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    let json = json!({
        "version": 1,
        "document": {},
        "layout": ["c/{id}.md"],
        "fields": [{"name": "title", "string": {}}]
    });
    assert!(json.get("layout").is_some());
    let decl: Schema = serde_json::from_value(json).unwrap();
    vault.create_collection("west_docs", decl).unwrap();
    let path = scratch.vault().join("west_docs").join(".schema.yaml");
    let yaml = fs::read_to_string(&path).unwrap();
    assert!(!yaml.contains("layout"), "{yaml}");
}

#[test]
fn create_collection_on_reader_is_locked() {
    let scratch = Scratch::new();
    let _writer = scratch.init();
    let other = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let err = other
        .create_collection("west_docs", west_docs_decl())
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Locked));
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
