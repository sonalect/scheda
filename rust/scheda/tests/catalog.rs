//! A1: `catalog` from Schema (`08` AGT-1, `12`). Collection names here and in
//! `testdata/schema/` must not appear in `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, OpenOptions, Vault, collection_dir};
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
        Vault::init(self.vault(), OpenOptions::new()).unwrap()
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), OpenOptions::new()).unwrap()
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

const WEST: &str = include_str!("../testdata/schema/west.yaml");
const EAST: &str = include_str!("../testdata/schema/east.yaml");
const EAST_LOG: &str = include_str!("../testdata/schema/east_log.yaml");
const WEST_INDEXED: &str = include_str!("../testdata/schema/west_indexed.yaml");
const WEST_ISOLATED: &str = include_str!("../testdata/schema/west_isolated.yaml");
const SLUG: &str = include_str!("../testdata/schema/slug.yaml");

fn catalog_json(vault: &Vault) -> Value {
    serde_json::to_value(vault.catalog().unwrap()).unwrap()
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
        include_str!("../src/error.rs"),
        include_str!("../src/fnv.rs"),
        include_str!("../src/git.rs"),
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
    assert_eq!(west["kind"], "document");
    assert_eq!(west["schemaVersion"], 1);
    assert_eq!(west["layout"], json!(["west/{id}.md"]));
    assert_eq!(west["id"]["field"], "id");
    assert_eq!(west["fields"][0]["name"], "title");
    assert_eq!(west["fields"][0]["type"], "string");
    assert_eq!(west["fields"][0]["required"], true);
    assert_eq!(west["fields"][1]["name"], "tag");
    assert_eq!(west["fields"][1]["filter"], true);
    assert!(west.get("isolated").is_none());

    let indexed = collection(&json, "west_indexed");
    assert_eq!(indexed["body"]["fts"], true);
    assert_eq!(indexed["body"]["vector"], true);
    assert_eq!(indexed["fields"][0]["fts"], true);
    assert_eq!(indexed["fields"][0]["vector"], true);
    assert_eq!(indexed["fields"][0]["fieldId"], "title");
    assert_eq!(indexed["fields"][1]["filter"], true);
    assert_eq!(indexed["fields"][2]["name"], "rank");
    assert_eq!(indexed["fields"][2]["default"].as_f64(), Some(0.0));
    assert_eq!(indexed["fields"][3]["name"], "see");
    assert_eq!(indexed["fields"][3]["edge"]["collection"], "west_docs");
    assert_eq!(indexed["fields"][3]["edge"]["multiplicity"], "one");
    assert_eq!(indexed["fields"][3]["edge"]["dangling"], "strict");

    let log = collection(&json, "east_log");
    assert_eq!(log["kind"], "journal");
    assert_eq!(log["layout"], json!(["log/"]));
    assert_eq!(log["segmentMaxBytes"], "8388608");
}

#[test]
fn catalog_is_not_built_from_record_directories() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let forged = scratch.vault().join(collection_dir("east_docs"));
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
    assert!(collection(&west_json, "west_docs")["kind"] == "document");
    assert!(
        west_json["collections"]
            .as_array()
            .unwrap()
            .iter()
            .all(|col| col["name"] != "east_docs")
    );
    assert!(collection(&east_json, "east_docs")["schemaVersion"] == 2);
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
    assert_eq!(isolated["isolated"], true);
    assert_eq!(isolated["kind"], "document");
    assert_eq!(isolated["layout"], json!(["sealed/{id}.md"]));
    assert!(isolated.get("fields").is_none());
    let dump = isolated.to_string();
    assert!(!dump.contains("west_pass"));
    let err = vault.collection("west_isolated").unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Unsupported));
    vault.collection("west_docs").unwrap();
}

#[test]
fn catalog_keeps_id_pattern_from_schema() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("slug.yaml", SLUG);
    let json = catalog_json(&scratch.reopen());
    let col = collection(&json, "c");
    assert_eq!(col["id"]["field"], "slug");
    assert_eq!(col["id"]["pattern"], "^[a-z0-9-]+$");
}
