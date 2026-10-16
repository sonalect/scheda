//! A fixture Vault shaped like a first consumer's primary layer.
//! Collection names here and in `testdata/consumer/` must not appear in
//! `rust/scheda/src`. That grep is library source only;
//! test data must not be able to turn it red.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(feature = "search")]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "find")]
use std::time::Duration;

use scheda::{Code, OpenOptions, Vault};
use serde_json::json;

#[cfg(feature = "find")]
use scheda::Filter;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-p10", n),
        }
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

const PERSONAL_SCHEMA: &str =
    include_str!("../testdata/consumer/lessons_personal/.collection.yaml");
const PROJECT_SCHEMA: &str = include_str!("../testdata/consumer/lessons_project/.collection.yaml");
const PERSONAL_MD: &str = include_str!("../testdata/consumer/lessons_personal/shared-note.md");
const PROJECT_MD: &str = include_str!("../testdata/consumer/lessons_project/shared-note.md");
const ENTITIES_SCHEMA: &str = include_str!("../testdata/consumer/entities/.collection.yaml");
const ENTITIES_MD: &str = include_str!("../testdata/consumer/entities/named.md");
const PROFILES_SCHEMA: &str = include_str!("../testdata/consumer/profiles/.collection.yaml");
const PROFILES_MD: &str = include_str!("../testdata/consumer/profiles/pilot.md");
const PENDING_SCHEMA: &str = include_str!("../testdata/consumer/pending_items/.collection.yaml");
const PENDING_MD: &str = include_str!("../testdata/consumer/pending_items/ticket-one.md");
const EPISODES_SCHEMA: &str = include_str!("../testdata/consumer/episodes/.collection.yaml");
const EPISODES_JSONL: &str = include_str!("../testdata/consumer/episodes/00000001.jsonl");
const EVENTS_SCHEMA: &str = include_str!("../testdata/consumer/events/.collection.yaml");
const EVENTS_JSONL: &str = include_str!("../testdata/consumer/events/00000001.jsonl");
const REACHABILITY_SCHEMA: &str =
    include_str!("../testdata/consumer/artifact_reachability/.collection.yaml");
const REACHABILITY_JSONL: &str =
    include_str!("../testdata/consumer/artifact_reachability/00000001.jsonl");
const HEADS_SCHEMA: &str = include_str!("../testdata/consumer/chain_heads/.collection.yaml");
const HEADS_YAML: &str = include_str!("../testdata/consumer/chain_heads/record.yaml");
const RUN_HEADS_SCHEMA: &str = include_str!("../testdata/consumer/run_heads/.collection.yaml");
const RUN_HEADS_YAML: &str = include_str!("../testdata/consumer/run_heads/record.yaml");
const ALIAS_SCHEMA: &str = include_str!("../testdata/consumer/alias_bindings/.collection.yaml");

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

// 10³ journal / 10² document: `get` and an extra `add` work on that
// volume. Timing is a benchmark, not a gate check.
#[cfg(feature = "find")]
const OPS_JOURNAL: u64 = 1_000;
#[cfg(feature = "find")]
const OPS_DOCS: u64 = 100;

fn rec(fields: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn rec_rep(fields: serde_json::Value) -> Vec<u8> {
    rec(fields)
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

fn git_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .expect("git executable");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn commit_all(dir: &Path, message: &str) {
    git_ok(dir, &["add", "-A"]);
    git_ok(
        dir,
        &[
            "-c",
            "user.name=Scheda",
            "-c",
            "user.email=scheda@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            message,
        ],
    );
}

fn plant_main_tree(vault: &Path) {
    write_rel(
        vault,
        &["lessons_personal", ".collection.yaml"],
        PERSONAL_SCHEMA,
    );
    write_rel(vault, &["lessons_personal", "shared-note.md"], PERSONAL_MD);
    write_rel(
        vault,
        &["lessons_project", ".collection.yaml"],
        PROJECT_SCHEMA,
    );
    write_rel(vault, &["lessons_project", "shared-note.md"], PROJECT_MD);
    write_rel(vault, &["entities", ".collection.yaml"], ENTITIES_SCHEMA);
    write_rel(vault, &["entities", "named.md"], ENTITIES_MD);
    write_rel(vault, &["profiles", ".collection.yaml"], PROFILES_SCHEMA);
    write_rel(vault, &["profiles", "pilot.md"], PROFILES_MD);
    write_rel(
        vault,
        &["pending_items", ".collection.yaml"],
        PENDING_SCHEMA,
    );
    write_rel(vault, &["pending_items", "ticket-one.md"], PENDING_MD);
    write_rel(vault, &["episodes", ".collection.yaml"], EPISODES_SCHEMA);
    write_rel(vault, &["episodes", "00000001.jsonl"], EPISODES_JSONL);
    write_rel(vault, &["events", ".collection.yaml"], EVENTS_SCHEMA);
    write_rel(vault, &["events", "00000001.jsonl"], EVENTS_JSONL);
    write_rel(
        vault,
        &["artifact_reachability", ".collection.yaml"],
        REACHABILITY_SCHEMA,
    );
    write_rel(
        vault,
        &["artifact_reachability", "00000001.jsonl"],
        REACHABILITY_JSONL,
    );
    write_rel(vault, &["chain_heads", ".collection.yaml"], HEADS_SCHEMA);
    write_rel(vault, &["chain_heads", "heads"], HEADS_YAML);
    write_rel(vault, &["run_heads", ".collection.yaml"], RUN_HEADS_SCHEMA);
    write_rel(vault, &["run_heads", "cursor"], RUN_HEADS_YAML);
    write_rel(vault, &["alias_bindings", ".collection.yaml"], ALIAS_SCHEMA);
}

fn add_isolated_journal(vault: &Vault) {
    vault
        .collection("alias_bindings")
        .unwrap()
        .add("bound", rec(json!({"token": ISOLATED_TOKEN})), None)
        .unwrap();
}

fn open_fixture(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    plant_main_tree(&scratch.vault());
    commit_all(&scratch.vault(), "plant fixture");
    let vault = scratch.reopen();
    add_isolated_journal(&vault);
    #[cfg(feature = "find")]
    {
        vault.reindex().unwrap();
        // The indexer may still be writing Index files after a rebuild.
        common::indexed(&vault);
    }
    vault
}

fn assert_raw_eq_file(vault: &Vault, collection: &str, id: &str, rel: &[&str]) {
    let got = vault.collection(collection).unwrap().get(id).unwrap();
    let mut path = vault.path().to_path_buf();
    for part in rel {
        path.push(part);
    }
    let on_disk = fs::read(&path).unwrap();
    if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
        let text = str::from_utf8(&on_disk).unwrap();
        let line = text.strip_suffix('\n').unwrap_or(text);
        let Some((envelope_id, payload)) = line.split_once(": ") else {
            panic!("{} is not a journal envelope", rel.join("/"));
        };
        assert_eq!(got.id.as_deref(), Some(id), "{} {id}", rel.join("/"));
        assert_eq!(envelope_id, id, "{} {id}", rel.join("/"));
        assert_eq!(
            got.raw.as_deref(),
            Some(payload.as_bytes()),
            "{} {id}",
            rel.join("/")
        );
        return;
    }
    assert_eq!(
        got.raw.as_deref(),
        Some(on_disk.as_slice()),
        "{} {id}",
        rel.join("/")
    );
}

fn fixture_raw_matches(vault: &Vault) {
    assert_raw_eq_file(
        vault,
        "lessons_personal",
        "shared-note.md",
        &["lessons_personal", "shared-note.md"],
    );
    assert_raw_eq_file(
        vault,
        "lessons_project",
        "shared-note.md",
        &["lessons_project", "shared-note.md"],
    );
    assert_raw_eq_file(vault, "entities", "named.md", &["entities", "named.md"]);
    assert_raw_eq_file(vault, "profiles", "pilot.md", &["profiles", "pilot.md"]);
    assert_raw_eq_file(
        vault,
        "pending_items",
        "ticket-one.md",
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
    assert_raw_eq_file(vault, "chain_heads", "heads", &["chain_heads", "heads"]);
    assert_raw_eq_file(vault, "run_heads", "cursor", &["run_heads", "cursor"]);
    let bound = vault
        .collection("alias_bindings")
        .unwrap()
        .get("bound")
        .unwrap();
    let dir = vault.path().join(".isolated").join("alias_bindings");
    let text = fs::read_to_string(unique_jsonl(&dir)).unwrap();
    let line = text.strip_suffix('\n').unwrap_or(&text);
    let Some((envelope_id, payload)) = line.split_once(": ") else {
        panic!("alias_bindings is not a journal envelope: {line}");
    };
    assert_eq!(bound.id.as_deref(), Some("bound"));
    assert_eq!(envelope_id, "bound");
    assert_eq!(bound.raw.as_deref(), Some(payload.as_bytes()));
}

fn unique_jsonl(dir: &Path) -> PathBuf {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("jsonl") && path.is_file())
        .collect();
    assert_eq!(files.len(), 1, "jsonl in {}", dir.display());
    files.remove(0)
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
        if name.ends_with(".md") || name == "heads" || name == "cursor" {
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
    String::from_utf8(git_ok(dir, &["diff", "HEAD"]).stdout).unwrap()
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
        .history("shared-note.md", 0, 10)
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
            None,
        )
        .unwrap();
    vault
        .collection("episodes")
        .unwrap()
        .add("beta", rec(json!({"note": "episode-two"})), None)
        .unwrap();
    vault
        .collection("run_heads")
        .unwrap()
        .replace("cursor", rec_rep(json!({"title": "moved"})), None)
        .unwrap();
    vault
        .collection("alias_bindings")
        .unwrap()
        .add("second", rec(json!({"token": "another-binding"})), None)
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
        .join(format!("00000001-{}.jsonl", vault.writer()));
    assert!(nested.is_file());
    assert!(
        !scratch
            .vault()
            .join("alias_bindings")
            .join(format!("00000001-{}.jsonl", vault.writer()))
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
    let named = vault
        .find(&["alias_bindings"], &Filter::all().into(), 0, 10)
        .unwrap();
    assert_eq!(named.records.len(), 1);
    assert_eq!(named.records[0].id.as_deref(), Some("bound"));
    let ignored = Command::new("git")
        .arg("-C")
        .arg(scratch.vault())
        .args(["check-ignore", "-q", "--", ".scheda"])
        .output()
        .unwrap();
    assert_eq!(ignored.status.code(), Some(0));
    let ignored = Command::new("git")
        .arg("-C")
        .arg(scratch.vault())
        .args(["check-ignore", "-q", "--", ".scheda/index"])
        .output()
        .unwrap();
    assert_eq!(ignored.status.code(), Some(0));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let scheda = scratch.vault().join(".scheda");
        let mode = fs::metadata(&scheda).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let index = scheda.join("index");
        let mode = fs::metadata(&index).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}

#[test]
fn remove_work_queue_entry_is_gone() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    let collection = vault.collection("pending_items").unwrap();
    collection
        .replace(
            "ticket-one.md",
            rec_rep(json!({"title": "First ticket", "status": "done"})),
            None,
        )
        .unwrap();
    collection.remove("ticket-one.md", None).unwrap();
    let err = collection.get("ticket-one.md").unwrap_err();
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
                .all(|record| record.id.as_deref() != Some("ticket-one.md"))
        );
        vault.check().unwrap();
    }
}

#[cfg(feature = "find")]
#[test]
fn ops_volume_get_and_add() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    vault.begin().unwrap();
    let log = vault.collection("episodes").unwrap();
    for i in 0..OPS_JOURNAL {
        log.add(&format!("j{i:05}"), rec(json!({"note": "vol"})), None)
            .unwrap();
    }
    let notes = vault.collection("lessons_personal").unwrap();
    for i in 0..OPS_DOCS {
        notes
            .add(
                &format!("vol-{i:04}"),
                rec(json!({"title": format!("t{i}"), "tag": "vol"})),
                None,
            )
            .unwrap();
    }
    vault.commit("ops fixture").unwrap();
    // A journal `get` finds its line through the `id` map the indexer keeps.
    common::indexed(&vault);

    let mid_journal = format!("j{:05}", OPS_JOURNAL / 2);
    let mid_doc = format!("vol-{:04}", OPS_DOCS / 2);
    let got = log.get(&mid_journal).unwrap();
    assert_eq!(got.id.as_deref(), Some(mid_journal.as_str()));
    let got = notes.get(&mid_doc).unwrap();
    assert_eq!(got.id.as_deref(), Some(mid_doc.as_str()));

    let extra = format!("j{OPS_JOURNAL:05}");
    vault.begin().unwrap();
    log.add(&extra, rec(json!({"note": "extra"})), None)
        .unwrap();
    vault.commit("extra").unwrap();
    let got = log.get(&extra).unwrap();
    assert_eq!(got.id.as_deref(), Some(extra.as_str()));
}

#[cfg(feature = "find")]
#[test]
fn identical_document_schemas_accept_the_same_record_and_find_stays_in_one() {
    let scratch = Scratch::new();
    let vault = open_fixture(&scratch);
    vault.reindex().unwrap();

    let payload = rec(json!({"title": "Twin", "tag": "same"}));
    vault
        .collection("lessons_personal")
        .unwrap()
        .add("twin", payload.clone(), None)
        .unwrap();
    vault
        .collection("lessons_project")
        .unwrap()
        .add("twin", payload, None)
        .unwrap();
    common::indexed(&vault);

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
            .any(|record| record.id.as_deref() == Some("shared-note.md"))
    );
    assert!(
        personal
            .records
            .iter()
            .any(|record| record.id.as_deref() == Some("twin"))
    );
    assert!(!personal.records.iter().any(|record| {
        record.id.as_deref() == Some("shared-note.md")
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

/// Embeds every text as the same unit vector: enough for one vector leg.
#[cfg(feature = "search")]
struct UnitEmbedder;

#[cfg(feature = "search")]
impl scheda::Embedder for UnitEmbedder {
    fn model_id(&self) -> &str {
        "consumer-unit/d4"
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(16).unwrap()
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        Ok(vec![vec![1.0, 0.0, 0.0, 0.0]; texts.len()])
    }
}

/// The shape of the anchor consumer after the Index moved to the
/// background: one writer handle with an embedder, other handles on the
/// same Vault without one, sync calls, no async runtime.
#[cfg(feature = "search")]
#[test]
fn a_writer_with_an_embedder_and_readers_without_one_share_a_vault() {
    let scratch = Scratch::new();
    let projector = common::json_text_projector(&["body"]);
    let options = || OpenOptions::new().with_index_poll_ms(20);
    let writer = Vault::open_with(
        scratch.vault(),
        options(),
        Some(Arc::new(UnitEmbedder) as Arc<dyn scheda::Embedder>),
        Some(Arc::clone(&projector)),
    )
    .unwrap();
    writer
        .create_collection(
            "notes",
            scheda::Schema {
                kind: scheda::Unit::default().into(),
                ..scheda::Schema::default()
            },
            None,
        )
        .unwrap();
    // Another handle, opened once the collection is declared. Opened
    // from a plain `#[test]`: no runtime is needed.
    let reader = Vault::open_with(
        scratch.vault(),
        options(),
        None,
        Some(Arc::clone(&projector)),
    )
    .unwrap();
    writer.begin().unwrap();
    let notes = writer.collection("notes").unwrap();
    notes
        .add("first", rec(json!({"body": "pin the toolchain"})), None)
        .unwrap();
    notes
        .add("second", rec(json!({"body": "prefer small steps"})), None)
        .unwrap();
    writer.commit("batch").unwrap();

    // `get` and `ids` read files: right after the write, on every handle.
    assert_eq!(notes.ids().unwrap(), ["first", "second"]);
    let reader_notes = reader.collection("notes").unwrap();
    reader_notes.get("first").unwrap();

    // A query that must see the write waits for the Index first.
    let status = writer.wait_indexed(Duration::from_secs(30)).unwrap();
    assert_eq!(status.lexical, status.head);
    assert_eq!(
        status.vector, status.head,
        "the writer's embedder fills vectors"
    );
    assert!(
        status.vector_role,
        "only the handle with an embedder writes vectors"
    );
    let query = scheda::SearchQuery::default()
        .with_text("toolchain")
        .with_k(10);
    let hits = notes.search(&query, None).unwrap();
    assert_eq!(hits.hits[0].id.as_deref(), Some("first"));
    assert!(hits.hits[0].vector_score.is_some(), "{hits:?}");
    let status = reader.wait_indexed(Duration::from_secs(30)).unwrap();
    assert!(!status.vector_role && !status.model_mismatch);
    let hits = reader_notes.search(&query, None).unwrap();
    assert_eq!(hits.hits[0].id.as_deref(), Some("first"));

    // The indexer never takes `write.lock`, and neither does `reindex`:
    // both go on while the writer holds a transaction open.
    writer.begin().unwrap();
    assert_eq!(reader.reindex().unwrap().records_read, Some(2));
    git_ok(&scratch.vault(), &["mv", "notes/second", "notes/third"]);
    commit_all(&scratch.vault(), "hand rename");
    let status = reader.wait_indexed(Duration::from_secs(30)).unwrap();
    assert_eq!(status.lexical, status.head);
    let mut ids: Vec<String> = reader
        .find(&["notes"], &scheda::FindQuery::all(), 0, 10)
        .unwrap()
        .records
        .into_iter()
        .filter_map(|record| record.id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["first", "third"]);
    writer.discard().unwrap();
}

/// Failure kinds of a caller that applies batches of operations.
#[cfg(all(feature = "tokio", feature = "search"))]
#[derive(Debug)]
enum BatchError {
    Refused(String),
    Scheda(scheda::Error),
}

#[cfg(all(feature = "tokio", feature = "search"))]
impl From<scheda::Error> for BatchError {
    fn from(err: scheda::Error) -> Self {
        Self::Scheda(err)
    }
}

/// [`UnitEmbedder`]'s vectors under another model id.
#[cfg(all(feature = "tokio", feature = "search"))]
struct OtherModelEmbedder;

#[cfg(all(feature = "tokio", feature = "search"))]
impl scheda::Embedder for OtherModelEmbedder {
    fn model_id(&self) -> &str {
        "consumer-other/d4"
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(16).unwrap()
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        Ok(vec![vec![1.0, 0.0, 0.0, 0.0]; texts.len()])
    }
}

/// Apply the two operations of one batch: add each, or replace it when an
/// earlier run of the same batch already wrote it.
#[cfg(all(feature = "tokio", feature = "search"))]
fn apply_batch(vault: &Vault) -> Result<usize, BatchError> {
    let batches = vault.collection("inbox_batches")?;
    let ops = [
        ("op-1", "pin the toolchain"),
        ("op-2", "prefer small steps"),
    ];
    for (id, body) in ops {
        let raw = rec(json!({ "body": body }));
        match batches.get(id) {
            Ok(_) => batches.replace(id, &raw, None)?,
            Err(err) if err.known_code() == Some(Code::NotFound) => batches.add(id, &raw, None)?,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(ops.len())
}

#[cfg(all(feature = "tokio", feature = "search"))]
fn head_of(vault: &Path) -> String {
    let out = git_ok(vault, &["rev-parse", "HEAD"]);
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// The shape of a consumer on the async API: a writer that applies each
/// batch in one `transaction` with its own error type and replays it after
/// a crash, readers opened before a collection was declared, and a reader
/// whose embedder has another model than the Index vectors.
#[cfg(all(feature = "tokio", feature = "search"))]
#[tokio::test(flavor = "multi_thread")]
async fn an_async_writer_and_readers_share_a_vault() {
    use scheda::AsyncVault;
    use scheda::search_hits::VectorLeg;

    const WAIT: Duration = Duration::from_secs(30);
    let scratch = Scratch::new();
    let projector = common::json_text_projector(&["body"]);
    let options = || OpenOptions::new().with_index_poll_ms(20);
    let open = |embedder: Arc<dyn scheda::Embedder>| {
        AsyncVault::open_with(
            scratch.vault(),
            options(),
            Some(embedder),
            Some(Arc::clone(&projector)),
        )
    };
    let document = || scheda::Schema {
        kind: scheda::Unit::default().into(),
        ..scheda::Schema::default()
    };
    let writer = open(Arc::new(UnitEmbedder)).await.unwrap();
    writer
        .create_collection("inbox_batches".into(), document(), None)
        .await
        .unwrap();
    let reader = open(Arc::new(UnitEmbedder)).await.unwrap();
    let other_model = open(Arc::new(OtherModelEmbedder)).await.unwrap();

    // (1) One batch, one commit; its replay writes the same bytes and is Ok.
    let before = head_of(writer.path());
    assert_eq!(writer.transaction("batch 1", apply_batch).await.unwrap(), 2);
    let after = head_of(writer.path());
    assert_ne!(after, before);
    assert_eq!(writer.transaction("batch 1", apply_batch).await.unwrap(), 2);
    assert_eq!(head_of(writer.path()), after, "the replay made a commit");
    // The caller's own refusal comes back as its variant; nothing lands.
    let err = writer
        .transaction("batch 2", |vault| -> Result<(), BatchError> {
            let raw = rec(json!({ "body": "over quota" }));
            vault.collection("inbox_batches")?.add("op-3", &raw, None)?;
            Err(BatchError::Refused("quota".into()))
        })
        .await
        .unwrap_err();
    match err {
        BatchError::Refused(reason) => assert_eq!(reason, "quota"),
        BatchError::Scheda(err) => panic!("the caller's refusal came back as {err:?}"),
    }
    assert_eq!(head_of(writer.path()), after);

    // (2) A collection another handle declares after the readers opened.
    writer
        .create_collection("late_topics".into(), document(), None)
        .await
        .unwrap()
        .add(
            "t1".into(),
            rec(json!({ "body": "kryptonite mineral" })),
            None,
        )
        .await
        .unwrap();
    let topics = reader.collection("late_topics").unwrap();
    assert_eq!(topics.name(), "late_topics");
    reader.wait_indexed(WAIT).await.unwrap();
    let vector_only = scheda::SearchQuery::default()
        .with_text("kryptonite")
        .with_lexical_weight(0.0);
    let hits = reader
        .search(vec!["late_topics".into()], vector_only.clone(), None)
        .await
        .unwrap();
    let ids: Vec<_> = hits
        .hits
        .iter()
        .filter_map(|hit| hit.id.as_deref())
        .collect();
    assert_eq!(ids, ["t1"]);
    assert_eq!(hits.vector_leg, Some(VectorLeg::Ran.into()));

    // (3) Vector-only from a reader whose embedder has another model: no
    // hits, and the reply says the vectors were not searched.
    other_model.wait_indexed(WAIT).await.unwrap();
    let hits = other_model
        .search(vec!["late_topics".into()], vector_only, None)
        .await
        .unwrap();
    assert!(hits.hits.is_empty(), "{hits:?}");
    assert_eq!(hits.vector_leg, Some(VectorLeg::ModelMismatch.into()));
    for vault in [writer, reader, other_model] {
        vault.close().await.unwrap();
    }
}
