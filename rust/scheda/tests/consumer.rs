//! P0: a fixture Vault shaped like a first consumer's primary layer.
//! Collection names here and in `testdata/consumer/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1). That grep is library source only;
//! test data must not be able to turn it red.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Filter, OpenOptions, RecordAdd, Vault};
use serde_json::json;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-p0-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn init(&self) -> Vault {
        Vault::open(self.vault(), OpenOptions::new(), None).unwrap()
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), OpenOptions::new(), None).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const PERSONAL_SCHEMA: &str = include_str!("../testdata/consumer/lessons_personal/.schema.yaml");
const PROJECT_SCHEMA: &str = include_str!("../testdata/consumer/lessons_project/.schema.yaml");
const PERSONAL_MD: &str = include_str!("../testdata/consumer/lessons_personal/shared-note.md");
const PROJECT_MD: &str = include_str!("../testdata/consumer/lessons_project/shared-note.md");
const ENTITIES_SCHEMA: &str = include_str!("../testdata/consumer/entities/.schema.yaml");
const ENTITIES_MD: &str = include_str!("../testdata/consumer/entities/named.md");
const PROFILES_SCHEMA: &str = include_str!("../testdata/consumer/profiles/.schema.yaml");
const PROFILES_MD: &str = include_str!("../testdata/consumer/profiles/pilot.md");
const EPISODES_SCHEMA: &str = include_str!("../testdata/consumer/episodes/.schema.yaml");
const EPISODES_JSONL: &str = include_str!("../testdata/consumer/episodes/00000001.jsonl");
const EVENTS_SCHEMA: &str = include_str!("../testdata/consumer/events/.schema.yaml");
const EVENTS_JSONL: &str = include_str!("../testdata/consumer/events/00000001.jsonl");
const REACHABILITY_SCHEMA: &str =
    include_str!("../testdata/consumer/artifact_reachability/.schema.yaml");
const REACHABILITY_JSONL: &str =
    include_str!("../testdata/consumer/artifact_reachability/00000001.jsonl");
const HEADS_SCHEMA: &str = include_str!("../testdata/consumer/chain_heads/.schema.yaml");
const HEADS_YAML: &str = include_str!("../testdata/consumer/chain_heads/record.yaml");
const ALIAS_SCHEMA: &str = include_str!("../testdata/consumer/alias_bindings/.schema.yaml");
const ALIAS_JSONL: &str = include_str!("../testdata/consumer/isolated_alias_bindings.jsonl");

const FIXTURE_NAMES: [&str; 9] = [
    "lessons_personal",
    "lessons_project",
    "entities",
    "profiles",
    "episodes",
    "events",
    "artifact_reachability",
    "chain_heads",
    "alias_bindings",
];

fn rec(fields: serde_json::Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
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

fn fields_block(schema: &str) -> &str {
    schema
        .split_once("fields:\n")
        .map(|(_, rest)| rest)
        .unwrap_or("")
}

fn write_rel(vault: &Path, parts: &[&str], body: &str) {
    let mut path = vault.to_path_buf();
    for part in parts {
        path.push(part);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, body).unwrap();
}

fn install_fixture(scratch: &Scratch) {
    let vault = scratch.vault();
    write_rel(
        &vault,
        &["lessons_personal", ".schema.yaml"],
        PERSONAL_SCHEMA,
    );
    write_rel(&vault, &["lessons_personal", "shared-note.md"], PERSONAL_MD);
    write_rel(&vault, &["lessons_project", ".schema.yaml"], PROJECT_SCHEMA);
    write_rel(&vault, &["lessons_project", "shared-note.md"], PROJECT_MD);
    write_rel(&vault, &["entities", ".schema.yaml"], ENTITIES_SCHEMA);
    write_rel(&vault, &["entities", "named.md"], ENTITIES_MD);
    write_rel(&vault, &["profiles", ".schema.yaml"], PROFILES_SCHEMA);
    write_rel(&vault, &["profiles", "pilot.md"], PROFILES_MD);
    write_rel(&vault, &["episodes", ".schema.yaml"], EPISODES_SCHEMA);
    write_rel(&vault, &["episodes", "00000001.jsonl"], EPISODES_JSONL);
    write_rel(&vault, &["events", ".schema.yaml"], EVENTS_SCHEMA);
    write_rel(&vault, &["events", "00000001.jsonl"], EVENTS_JSONL);
    write_rel(
        &vault,
        &["artifact_reachability", ".schema.yaml"],
        REACHABILITY_SCHEMA,
    );
    write_rel(
        &vault,
        &["artifact_reachability", "00000001.jsonl"],
        REACHABILITY_JSONL,
    );
    write_rel(&vault, &["chain_heads", ".schema.yaml"], HEADS_SCHEMA);
    write_rel(&vault, &["chain_heads", "record.yaml"], HEADS_YAML);
    write_rel(&vault, &["alias_bindings", ".schema.yaml"], ALIAS_SCHEMA);
    write_rel(
        &vault,
        &[".isolated", "alias_bindings", "00000001.jsonl"],
        ALIAS_JSONL,
    );
}

fn assert_raw_eq_file(vault: &Vault, collection: &str, id: &str, rel: &[&str]) {
    let got = vault.collection(collection).unwrap().get(id).unwrap();
    let mut path = vault.path().to_path_buf();
    for part in rel {
        path.push(part);
    }
    let on_disk = fs::read(&path).unwrap();
    assert_eq!(
        got.raw.as_deref(),
        Some(on_disk.as_slice()),
        "{} {id}",
        rel.join("/")
    );
    assert_eq!(got.valid, Some(true));
}

#[test]
fn src_grep_is_library_source_not_testdata() {
    let haystack = src_rs();
    let schemas = [
        PERSONAL_SCHEMA,
        PROJECT_SCHEMA,
        ENTITIES_SCHEMA,
        PROFILES_SCHEMA,
        EPISODES_SCHEMA,
        EVENTS_SCHEMA,
        REACHABILITY_SCHEMA,
        HEADS_SCHEMA,
        ALIAS_SCHEMA,
    ]
    .join("\n");
    for name in FIXTURE_NAMES {
        assert!(
            contains_collection_name(&schemas, name),
            "{name} must appear in testdata/consumer Schema files"
        );
        assert!(
            !contains_collection_name(&haystack, name),
            "{name} must not appear in rust/scheda/src"
        );
    }
}

#[test]
fn check_passes_and_get_raw_matches_each_kind() {
    let scratch = Scratch::new();
    drop(scratch.init());
    install_fixture(&scratch);
    let vault = scratch.reopen();
    vault.check().unwrap();

    assert_raw_eq_file(
        &vault,
        "lessons_personal",
        "shared-note",
        &["lessons_personal", "shared-note.md"],
    );
    assert_raw_eq_file(
        &vault,
        "lessons_project",
        "shared-note",
        &["lessons_project", "shared-note.md"],
    );
    assert_raw_eq_file(&vault, "entities", "named", &["entities", "named.md"]);
    assert_raw_eq_file(&vault, "profiles", "pilot", &["profiles", "pilot.md"]);
    assert_raw_eq_file(&vault, "episodes", "alpha", &["episodes", "00000001.jsonl"]);
    assert_raw_eq_file(&vault, "events", "alpha", &["events", "00000001.jsonl"]);
    assert_raw_eq_file(
        &vault,
        "artifact_reachability",
        "alpha",
        &["artifact_reachability", "00000001.jsonl"],
    );
    assert_raw_eq_file(
        &vault,
        "chain_heads",
        "heads",
        &["chain_heads", "record.yaml"],
    );

    assert_raw_eq_file(
        &vault,
        "alias_bindings",
        "bound",
        &[".isolated", "alias_bindings", "00000001.jsonl"],
    );
}

#[test]
fn identical_document_schemas_accept_the_same_record_and_find_stays_in_one() {
    assert_eq!(
        fields_block(PERSONAL_SCHEMA),
        fields_block(PROJECT_SCHEMA),
        "the two document collections must declare the same fields"
    );

    let scratch = Scratch::new();
    drop(scratch.init());
    install_fixture(&scratch);
    let vault = scratch.reopen();
    vault.reindex().unwrap();

    let payload = rec(json!({"title": "Twin", "tag": "same"}));
    vault
        .collection("lessons_personal")
        .unwrap()
        .add("twin", payload.clone())
        .unwrap();
    vault
        .collection("lessons_project")
        .unwrap()
        .add("twin", payload)
        .unwrap();

    let personal = vault
        .find(&["lessons_personal"], &Filter::all().into(), 0, 100)
        .unwrap();
    assert!(
        personal
            .records
            .iter()
            .all(|record| record.collection.as_deref() == Some("lessons_personal"))
    );
    assert!(
        personal
            .records
            .iter()
            .any(|record| record.id.as_deref() == Some("shared-note"))
    );
    assert!(
        personal
            .records
            .iter()
            .any(|record| record.id.as_deref() == Some("twin"))
    );
    assert!(!personal.records.iter().any(|record| {
        record.id.as_deref() == Some("shared-note")
            && record.collection.as_deref() == Some("lessons_project")
    }));

    let project = vault
        .find(&["lessons_project"], &Filter::all().into(), 0, 100)
        .unwrap();
    assert!(
        project
            .records
            .iter()
            .all(|record| record.collection.as_deref() == Some("lessons_project"))
    );
    assert_eq!(
        personal.records.len(),
        project.records.len(),
        "each collection has the planted Record plus the shared add"
    );
}
