//! P0 / P10: a fixture Vault shaped like a first consumer's primary layer.
//! Collection names here and in `testdata/consumer/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1). That grep is library source only;
//! test data must not be able to turn it red.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "find")]
use std::time::{Duration, Instant};

use git2::{DiffFormat, IndexAddOption, Repository, Signature};
use scheda::{Code, OpenOptions, RecordAdd, RecordReplace, Vault};
use serde_json::json;

#[cfg(feature = "find")]
use scheda::Filter;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-p10-{}-{n}", process::id()));
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
const PENDING_SCHEMA: &str = include_str!("../testdata/consumer/pending_items/.schema.yaml");
const PENDING_MD: &str = include_str!("../testdata/consumer/pending_items/ticket-one.md");
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
const RUN_HEADS_SCHEMA: &str = include_str!("../testdata/consumer/run_heads/.schema.yaml");
const RUN_HEADS_YAML: &str = include_str!("../testdata/consumer/run_heads/record.yaml");
const ALIAS_SCHEMA: &str = include_str!("../testdata/consumer/alias_bindings/.schema.yaml");

const ISOLATED_TOKEN: &str = "consumer-sealed-nested-token";

const FIXTURE_NAMES: [&str; 11] = [
    "lessons_personal",
    "lessons_project",
    "entities",
    "profiles",
    "pending_items",
    "episodes",
    "events",
    "artifact_reachability",
    "chain_heads",
    "run_heads",
    "alias_bindings",
];

// CI loads 10³ journal / 10² document on every OS so get / extra add
// finish under Bazel `large` (900s) when the Vault is on a real disk:
// each add fsyncs.
#[cfg(feature = "find")]
const OPS_JOURNAL: u64 = 1_000;
#[cfg(feature = "find")]
const OPS_DOCS: u64 = 100;
#[cfg(feature = "find")]
const OPS_BUDGET: Duration = Duration::from_millis(100);

fn rec(fields: serde_json::Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn rec_rep(fields: serde_json::Value) -> RecordReplace {
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
        include_str!("../src/isolated.rs"),
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

fn commit_all(dir: &Path, message: &str) {
    let repo = Repository::open(dir).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["."].iter(), IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = Signature::now("Scheda", "scheda@invalid").unwrap();
    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit().unwrap()),
        Err(_) => None,
    };
    let parents: Vec<&git2::Commit<'_>> = parent.as_ref().into_iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap();
}

fn plant_main_tree(vault: &Path) {
    write_rel(
        vault,
        &["lessons_personal", ".schema.yaml"],
        PERSONAL_SCHEMA,
    );
    write_rel(vault, &["lessons_personal", "shared-note.md"], PERSONAL_MD);
    write_rel(vault, &["lessons_project", ".schema.yaml"], PROJECT_SCHEMA);
    write_rel(vault, &["lessons_project", "shared-note.md"], PROJECT_MD);
    write_rel(vault, &["entities", ".schema.yaml"], ENTITIES_SCHEMA);
    write_rel(vault, &["entities", "named.md"], ENTITIES_MD);
    write_rel(vault, &["profiles", ".schema.yaml"], PROFILES_SCHEMA);
    write_rel(vault, &["profiles", "pilot.md"], PROFILES_MD);
    write_rel(vault, &["pending_items", ".schema.yaml"], PENDING_SCHEMA);
    write_rel(vault, &["pending_items", "ticket-one.md"], PENDING_MD);
    write_rel(vault, &["episodes", ".schema.yaml"], EPISODES_SCHEMA);
    write_rel(vault, &["episodes", "00000001.jsonl"], EPISODES_JSONL);
    write_rel(vault, &["events", ".schema.yaml"], EVENTS_SCHEMA);
    write_rel(vault, &["events", "00000001.jsonl"], EVENTS_JSONL);
    write_rel(
        vault,
        &["artifact_reachability", ".schema.yaml"],
        REACHABILITY_SCHEMA,
    );
    write_rel(
        vault,
        &["artifact_reachability", "00000001.jsonl"],
        REACHABILITY_JSONL,
    );
    write_rel(vault, &["chain_heads", ".schema.yaml"], HEADS_SCHEMA);
    write_rel(vault, &["chain_heads", "record.yaml"], HEADS_YAML);
    write_rel(vault, &["run_heads", ".schema.yaml"], RUN_HEADS_SCHEMA);
    write_rel(vault, &["run_heads", "record.yaml"], RUN_HEADS_YAML);
    write_rel(vault, &["alias_bindings", ".schema.yaml"], ALIAS_SCHEMA);
}

fn add_isolated_journal(vault: &Vault) {
    vault
        .collection("alias_bindings")
        .unwrap()
        .add("bound", rec(json!({"token": ISOLATED_TOKEN})))
        .unwrap();
}

fn open_fixture(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    plant_main_tree(&scratch.vault());
    commit_all(&scratch.vault(), "plant fixture");
    let vault = scratch.reopen();
    add_isolated_journal(&vault);
    #[cfg(feature = "find")]
    vault.reindex().unwrap();
    vault
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

fn fixture_raw_matches(vault: &Vault) {
    assert_raw_eq_file(
        vault,
        "lessons_personal",
        "shared-note",
        &["lessons_personal", "shared-note.md"],
    );
    assert_raw_eq_file(
        vault,
        "lessons_project",
        "shared-note",
        &["lessons_project", "shared-note.md"],
    );
    assert_raw_eq_file(vault, "entities", "named", &["entities", "named.md"]);
    assert_raw_eq_file(vault, "profiles", "pilot", &["profiles", "pilot.md"]);
    assert_raw_eq_file(
        vault,
        "pending_items",
        "ticket-one",
        &["pending_items", "ticket-one.md"],
    );
    assert_raw_eq_file(vault, "episodes", "alpha", &["episodes", "00000001.jsonl"]);
    assert_raw_eq_file(vault, "events", "alpha", &["events", "00000001.jsonl"]);
    assert_raw_eq_file(
        vault,
        "artifact_reachability",
        "alpha",
        &["artifact_reachability", "00000001.jsonl"],
    );
    assert_raw_eq_file(
        vault,
        "chain_heads",
        "heads",
        &["chain_heads", "record.yaml"],
    );
    assert_raw_eq_file(vault, "run_heads", "cursor", &["run_heads", "record.yaml"]);
    assert_raw_eq_file(
        vault,
        "alias_bindings",
        "bound",
        &[".isolated", "alias_bindings", "00000001.jsonl"],
    );
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == ".scheda" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

fn hand_tally(vault: &Path) -> (u64, u64) {
    let mut records = 0u64;
    let mut segments = 0u64;
    for path in walk_files(vault) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".md") || name == "record.yaml" {
            records += 1;
        } else if name.ends_with(".jsonl") {
            segments += 1;
            let text = fs::read_to_string(&path).unwrap();
            records += u64::try_from(text.lines().filter(|line| !line.is_empty()).count()).unwrap();
        }
    }
    (records, segments)
}

fn git_diff_workdir(dir: &Path) -> String {
    let repo = Repository::open(dir).unwrap();
    let head = repo.head().unwrap().peel_to_tree().unwrap();
    let diff = repo.diff_tree_to_workdir(Some(&head), None).unwrap();
    let mut out = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if let Ok(text) = str::from_utf8(line.content()) {
            out.push_str(text);
        }
        true
    })
    .unwrap();
    out
}

fn assert_utf8_text(body: &str) {
    assert!(!body.as_bytes().contains(&0));
    assert!(!body.starts_with('\u{feff}'));
}

#[test]
fn src_grep_is_library_source_not_testdata() {
    let haystack = src_rs();
    let schemas = [
        PERSONAL_SCHEMA,
        PROJECT_SCHEMA,
        ENTITIES_SCHEMA,
        PROFILES_SCHEMA,
        PENDING_SCHEMA,
        EPISODES_SCHEMA,
        EVENTS_SCHEMA,
        REACHABILITY_SCHEMA,
        HEADS_SCHEMA,
        RUN_HEADS_SCHEMA,
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
    assert_eq!(
        fields_block(PERSONAL_SCHEMA),
        fields_block(PROJECT_SCHEMA),
        "the two document collections must declare the same fields"
    );
}

#[test]
fn fixture_opens_as_text_and_get_raw_matches() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    #[cfg(feature = "find")]
    vault.check().unwrap();

    for body in [
        PERSONAL_MD,
        PROJECT_MD,
        ENTITIES_MD,
        PROFILES_MD,
        PENDING_MD,
        EPISODES_JSONL,
        EVENTS_JSONL,
        REACHABILITY_JSONL,
        HEADS_YAML,
        RUN_HEADS_YAML,
    ] {
        assert_utf8_text(body);
    }

    fixture_raw_matches(&vault);

    let rel = "lessons_personal/shared-note.md";
    let file = fs::read(scratch.vault().join(rel)).unwrap();
    let hist = vault
        .collection("lessons_personal")
        .unwrap()
        .history("shared-note", 0, 10)
        .unwrap();
    assert_eq!(hist.entries[0].raw.as_deref(), Some(file.as_slice()));

    let path = scratch.vault().join(rel);
    let mut edited = fs::read_to_string(&path).unwrap();
    edited = edited.replace("title: personal", "title: edited-in-git");
    fs::write(&path, &edited).unwrap();
    let diff = git_diff_workdir(&scratch.vault());
    assert!(
        diff.contains("edited-in-git"),
        "git diff of a field must show that field's text, not a binary dump"
    );
    assert!(!diff.contains('\0'));
}

#[test]
fn reindex_after_deleting_index_matches_files() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    fixture_raw_matches(&vault);
    let (records, segments) = hand_tally(&scratch.vault());
    drop(vault);

    fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
    let isolated_index = scratch.vault().join(".isolated").join(".scheda");
    if isolated_index.exists() {
        fs::remove_dir_all(&isolated_index).unwrap();
    }

    let vault = scratch.reopen();
    fixture_raw_matches(&vault);
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(records));
    assert_eq!(counts.skipped_invalid, Some(0));
    assert_eq!(counts.segments_seen, Some(segments));
    fixture_raw_matches(&vault);
}

#[test]
fn default_build_writes_every_kind_and_reads_without_index() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    vault
        .collection("pending_items")
        .unwrap()
        .add(
            "ticket-two",
            rec(json!({"title": "Second", "status": "open"})),
        )
        .unwrap();
    vault
        .collection("episodes")
        .unwrap()
        .add("beta", rec(json!({"note": "episode-two"})))
        .unwrap();
    vault
        .collection("run_heads")
        .unwrap()
        .replace("cursor", rec_rep(json!({"title": "moved"})))
        .unwrap();
    vault
        .collection("alias_bindings")
        .unwrap()
        .add("second", rec(json!({"token": "another-binding"})))
        .unwrap();

    assert_eq!(
        vault
            .collection("pending_items")
            .unwrap()
            .get("ticket-two")
            .unwrap()
            .id
            .as_deref(),
        Some("ticket-two")
    );
    assert_eq!(
        vault
            .collection("episodes")
            .unwrap()
            .get("beta")
            .unwrap()
            .id
            .as_deref(),
        Some("beta")
    );
    assert_eq!(
        vault
            .collection("run_heads")
            .unwrap()
            .get("cursor")
            .unwrap()
            .id
            .as_deref(),
        Some("cursor")
    );
    assert_eq!(
        vault
            .collection("alias_bindings")
            .unwrap()
            .get("second")
            .unwrap()
            .id
            .as_deref(),
        Some("second")
    );

    let hist = vault
        .collection("pending_items")
        .unwrap()
        .history("ticket-two", 0, 10)
        .unwrap();
    assert!(!hist.entries.is_empty());
    drop(vault);

    fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
    let vault = scratch.reopen();
    let ids: Vec<_> = vault
        .collection("episodes")
        .unwrap()
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["alpha", "beta"]);
    assert_eq!(
        vault
            .collection("pending_items")
            .unwrap()
            .get("ticket-two")
            .unwrap()
            .id
            .as_deref(),
        Some("ticket-two")
    );
    let hist = vault
        .collection("pending_items")
        .unwrap()
        .history("ticket-two", 0, 10)
        .unwrap();
    assert!(!hist.entries.is_empty());
}

#[test]
fn isolated_journal_is_nested_and_check_covers_it() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    let nested = scratch
        .vault()
        .join(".isolated")
        .join("alias_bindings")
        .join("00000001.jsonl");
    assert!(nested.is_file());
    assert!(
        !scratch
            .vault()
            .join("alias_bindings")
            .join("00000001.jsonl")
            .exists()
    );
    #[cfg(feature = "find")]
    vault.check().unwrap();
    assert_eq!(
        vault
            .collection("alias_bindings")
            .unwrap()
            .get("bound")
            .unwrap()
            .id
            .as_deref(),
        Some("bound")
    );
}

#[cfg(feature = "find")]
#[test]
fn isolated_round_trip_skipped_by_main_find() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    vault.reindex().unwrap();
    vault.check().unwrap();
    let page = vault.find(&[], &Filter::all().into(), 0, 100).unwrap();
    assert!(page.records.iter().all(|record| {
        record.collection.as_deref() != Some("alias_bindings")
            && record.id.as_deref() != Some("bound")
    }));
    let named = vault.find(&["alias_bindings"], &Filter::all().into(), 0, 10);
    assert_eq!(named.unwrap_err().known_code(), Some(Code::Unsupported));
    let index_dir = vault.index_dir().to_path_buf();
    drop(vault);
    let needle = ISOLATED_TOKEN.as_bytes();
    for path in walk_files(&index_dir) {
        let bytes = fs::read(&path).unwrap();
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "main Index copied an isolated field: {}",
            path.display()
        );
    }
}

#[test]
fn remove_work_queue_entry_is_gone() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    let collection = vault.collection("pending_items").unwrap();
    collection
        .replace(
            "ticket-one",
            rec_rep(json!({"title": "First ticket", "status": "done"})),
        )
        .unwrap();
    collection.remove("ticket-one").unwrap();
    let err = collection.get("ticket-one").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    assert!(
        !scratch
            .vault()
            .join("pending_items")
            .join("ticket-one.md")
            .exists()
    );

    #[cfg(feature = "find")]
    {
        vault.reindex().unwrap();
        let page = vault
            .find(&["pending_items"], &Filter::all().into(), 0, 100)
            .unwrap();
        assert!(
            page.records
                .iter()
                .all(|record| record.id.as_deref() != Some("ticket-one"))
        );
        vault.check().unwrap();
    }
}

#[cfg(feature = "find")]
#[test]
fn ops_volume_get_and_add_stay_in_milliseconds() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    vault.begin().unwrap();
    let log = vault.collection("episodes").unwrap();
    for i in 0..OPS_JOURNAL {
        log.add(&format!("j{i:05}"), rec(json!({"note": "vol"})))
            .unwrap();
    }
    let notes = vault.collection("lessons_personal").unwrap();
    for i in 0..OPS_DOCS {
        notes
            .add(
                &format!("vol-{i:04}"),
                rec(json!({"title": format!("t{i}"), "tag": "vol"})),
            )
            .unwrap();
    }
    vault.commit("ops fixture").unwrap();

    let mid_journal = format!("j{:05}", OPS_JOURNAL / 2);
    let mid_doc = format!("vol-{:04}", OPS_DOCS / 2);
    let started = Instant::now();
    log.get(&mid_journal).unwrap();
    notes.get(&mid_doc).unwrap();
    let got = started.elapsed();
    assert!(
        got < OPS_BUDGET,
        "sample get on the OPS-1 fixture took {got:?}, want < {OPS_BUDGET:?}"
    );

    // Time `add` inside a transaction: outside one, autocommit fsyncs git
    // on every call, and that is hundreds of milliseconds on Windows NTFS.
    let extra = format!("j{OPS_JOURNAL:05}");
    vault.begin().unwrap();
    let started = Instant::now();
    log.add(&extra, rec(json!({"note": "extra"}))).unwrap();
    let added = started.elapsed();
    vault.commit("extra").unwrap();
    assert!(
        added < OPS_BUDGET,
        "journal add on the OPS-1 fixture took {added:?}, want < {OPS_BUDGET:?}"
    );
}

#[cfg(feature = "find")]
#[test]
fn identical_document_schemas_accept_the_same_record_and_find_stays_in_one() {
    assert_eq!(
        fields_block(PERSONAL_SCHEMA),
        fields_block(PROJECT_SCHEMA),
        "the two document collections must declare the same fields"
    );

    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
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
