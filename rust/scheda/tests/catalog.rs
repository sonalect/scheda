//! A1: `catalog` from Schema (`08` AGT-1, `12`). Collection names here and in
//! `testdata/schema/` must not appear in `rust/scheda/src` (MOD-1, SCH-1).

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, Declaration, FindQuery, OpenOptions, SearchQuery, Vault};
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
            root: common::scratch_root("scheda-a1", n),
        }
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
            ".collection.json"
        } else {
            ".collection.yaml"
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
fn catalog_json_matches_schema_files() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    scratch.write_schema("west_indexed.yaml", WEST_INDEXED);
    scratch.write_schema("log.yaml", EAST_LOG);
    let json = catalog_json(&scratch.reopen());
    let west = collection(&json, "west_docs");
    let west_schema = &west["declaration"];
    assert!(west_schema.get("document").is_some());
    assert!(west_schema.get("version").is_none());
    assert!(west_schema.get("fields").is_none());
    assert!(west_schema.get("isolated").is_none());

    let indexed = &collection(&json, "west_indexed")["declaration"];
    assert!(indexed.get("document").is_some());
    assert!(indexed.get("fields").is_none());

    let log = &collection(&json, "east_log")["declaration"];
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
        collection(&west_json, "west_docs")["declaration"]
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
    assert!(
        collection(&east_json, "east_docs")["declaration"]
            .get("document")
            .is_some()
    );
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
    assert_eq!(isolated["declaration"]["isolated"], true);
    assert!(isolated["declaration"].get("document").is_some());
    assert!(isolated["declaration"].get("fields").is_none());
    let dump = isolated.to_string();
    assert!(!dump.contains("west_pass"));
    vault.collection("west_isolated").unwrap();
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
        .add("alpha", rec(json!({"title": "Alpha public"})), None)
        .unwrap();
    plant_isolated_sealed(&scratch);
    vault.reindex().unwrap();

    let info = catalog_json(&vault);
    let isolated = collection(&info, "west_isolated");
    assert_eq!(isolated["declaration"]["isolated"], true);
    assert!(isolated["declaration"].get("fields").is_none());
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
    assert_eq!(hits.hits.len(), 0);
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

    let isolated = vault.collection("west_isolated").unwrap();
    let sealed = isolated.get("sealed.md").unwrap();
    assert_eq!(sealed.id.as_deref(), Some("sealed.md"));
    let sealed_text = String::from_utf8_lossy(sealed.raw.as_deref().unwrap_or(&[]));
    assert!(sealed_text.contains(NESTED_SEALED));
    assert!(!sealed_text.contains(TREE_SEALED));
    let named = vault
        .find(&["west_isolated"], &FindQuery::all(), 0, 10)
        .unwrap();
    assert_eq!(named.records.len(), 1);
    assert_eq!(named.records[0].id.as_deref(), Some("sealed.md"));
    assert_eq!(
        named.records[0].collection.as_deref(),
        Some("west_isolated")
    );
}

fn rec(fields: Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn west_docs_decl() -> Declaration {
    serde_json::from_value(json!({
        "version": 1,
        "document": {},
    }))
    .unwrap()
}

#[test]
fn create_collection_then_insert_one_without_reopen() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    let written = vault
        .create_collection("west_docs", west_docs_decl(), None)
        .unwrap();
    assert_eq!(written.name(), "west_docs");
    assert_eq!(written.kind(), scheda::Kind::Document);
    assert!(
        scratch
            .vault()
            .join("west_docs")
            .join(".collection.yaml")
            .is_file()
    );
    let yaml =
        fs::read_to_string(scratch.vault().join("west_docs").join(".collection.yaml")).unwrap();
    assert!(
        yaml.contains("version: 1"),
        "create_collection must write generation 1: {yaml}"
    );
    assert!(
        !yaml.contains("fields:"),
        "create_collection must not write fields: {yaml}"
    );
    vault
        .collection("west_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "Hello", "tag": "x"})), None)
        .unwrap();
    let got = vault.collection("west_docs").unwrap().get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    let json = catalog_json(&vault);
    assert!(
        collection(&json, "west_docs")["declaration"]
            .get("document")
            .is_some()
    );
}

#[test]
fn create_collection_rejects_fields_json_key() {
    let scratch = Scratch::new();
    let _vault = scratch.init();
    let err = scheda::declaration_from_json(r#"{"document":{},"fields":[]}"#).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(err.message.as_deref().unwrap_or("").contains("fields"));
}

#[test]
fn create_collection_missing_kind_writes_nothing() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    let decl: Declaration = serde_json::from_value(json!({
        "version": 1,
    }))
    .unwrap();
    let err = vault
        .create_collection("west_docs", decl, None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(
        !scratch
            .vault()
            .join("west_docs")
            .join(".collection.yaml")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn create_collection_restores_schema_when_journal_map_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new();
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let log: Declaration = serde_json::from_value(json!({
        "version": 1,
        "journal": {},
    }))
    .unwrap();
    vault.create_collection("east_log", log, None).unwrap();
    vault
        .collection("east_log")
        .unwrap()
        .add("a", rec(json!({"msg": "x"})), None)
        .unwrap();
    let generation = fs::read_to_string(
        scratch
            .vault()
            .join(".scheda")
            .join("index")
            .join("current"),
    )
    .unwrap()
    .trim()
    .to_owned();
    let ids = scratch
        .vault()
        .join(".scheda")
        .join("index")
        .join(generation)
        .join("journal")
        .join("east_log")
        .join("ids");
    // Taking the write lock re-applies owner-only modes under `.scheda/`,
    // so chmod on `ids` itself would be undone before `load_journal_ids`.
    // A symlink to a file outside that tree is skipped by the walk.
    let blocked = scratch.root.join("blocked-ids");
    fs::write(&blocked, b"x").unwrap();
    let mut deny = fs::metadata(&blocked).unwrap().permissions();
    deny.set_mode(0o000);
    fs::set_permissions(&blocked, deny.clone()).unwrap();
    fs::remove_file(&ids).unwrap();
    std::os::unix::fs::symlink(&blocked, &ids).unwrap();
    let err = vault.create_collection("west_docs", west_docs_decl(), None);
    deny.set_mode(0o600);
    fs::set_permissions(&blocked, deny).unwrap();
    assert!(err.is_err(), "{err:?}");
    assert!(
        !scratch
            .vault()
            .join("west_docs")
            .join(".collection.yaml")
            .exists()
    );
    let missing = scratch.reopen().collection("west_docs").unwrap_err();
    assert_eq!(missing.known_code(), Some(Code::NotFound));
}

#[test]
fn create_collection_isolated_writes_schema_and_nested_git() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    let decl: Declaration = serde_json::from_value(json!({
        "version": 1,
        "document": {},
        "isolated": true,
    }))
    .unwrap();
    let written = vault
        .create_collection("west_isolated", decl, None)
        .unwrap();
    assert_eq!(written.name(), "west_isolated");
    assert!(
        scratch
            .vault()
            .join("west_isolated")
            .join(".collection.yaml")
            .is_file()
    );
    assert!(scratch.vault().join(".isolated").join(".git").exists());
    written
        .add("s1", rec(json!({"secret": "nested"})), None)
        .unwrap();
    let got = vault
        .collection("west_isolated")
        .unwrap()
        .get("s1")
        .unwrap();
    assert_eq!(got.id.as_deref(), Some("s1"));
}

#[test]
fn create_collection_rejects_layout_json_key() {
    let scratch = Scratch::new();
    let _vault = scratch.init();
    let json = r#"{"document":{},"layout":["c/{id}.md"]}"#;
    let err = scheda::declaration_from_json(json).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(err.message.as_deref().unwrap_or("").contains("layout"));
    assert!(
        !scratch
            .vault()
            .join("west_docs")
            .join(".collection.yaml")
            .is_file()
    );
}

#[test]
fn create_collection_on_reader_is_locked() {
    let scratch = Scratch::new();
    let _writer = scratch.init();
    let other = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let err = other
        .create_collection("west_docs", west_docs_decl(), None)
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
