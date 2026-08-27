//! P4: unencrypted isolated nested git (ISO-2, ISO-4, ISO-6, IDX-4).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use git2::{IndexEntry, IndexTime, Oid, Repository};
use scheda::{Code, Filter, OpenOptions, RecordAdd, RecordReplace, Schema, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-p4-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn options(&self) -> OpenOptions {
        OpenOptions::new()
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), self.options(), None).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const WEST: &str = include_str!("../testdata/schema/west.yaml");
const WEST_ISOLATED: &str = include_str!("../testdata/schema/west_isolated.yaml");
const SEALED_LOG: &str = include_str!("../testdata/isolated/log.yaml");

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn rec_rep(fields: Value) -> RecordReplace {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn write_schema(scratch: &Scratch, body: &str) {
    let name = peek_schema_name(body);
    let dir = scratch.vault().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".schema.yaml"), body).unwrap();
}

fn log_len(dir: &Path) -> usize {
    let repo = Repository::open(dir).unwrap();
    let mut walk = repo.revwalk().unwrap();
    if walk.push_head().is_err() {
        return 0;
    }
    walk.count()
}

fn peek_schema_name(body: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            return rest.trim().trim_matches('"').to_string();
        }
    }
    panic!("schema fixture name");
}

#[test]
fn create_isolated_journal_writes_nested_git_not_main_log() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault.begin().unwrap();
    let decl: Schema = serde_json::from_value(json!({
        "version": 1,
        "journal": {"segment_max_bytes": 45},
        "isolated": true,
        "fields": [{"name": "msg", "string": {}}]
    }))
    .unwrap();
    vault.create_collection("sealed_log", decl).unwrap();
    let main_before = log_len(vault.path());
    let nested = scratch.vault().join(".isolated");
    assert!(Repository::open(&nested).is_ok());
    let collection = vault.collection("sealed_log").unwrap();
    collection.add("a", rec(json!({"msg": "one"}))).unwrap();
    collection.add("b", rec(json!({"msg": "two"}))).unwrap();
    assert_eq!(log_len(vault.path()), main_before);
    vault.commit("isolated batch").unwrap();
    assert_eq!(log_len(vault.path()), main_before + 1);
    assert_eq!(log_len(&nested), 1);
    assert!(
        git2::Repository::open(&nested)
            .unwrap()
            .remotes()
            .unwrap()
            .is_empty()
    );
    let got = collection.get("a").unwrap();
    assert_eq!(got.id.as_deref(), Some("a"));
    assert_eq!(
        got.path.as_deref(),
        Some(".isolated/sealed_log/00000001.jsonl")
    );
    let ids: Vec<_> = collection
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["a", "b"]);
}

#[test]
fn isolated_journal_rotates_and_ids_map_stays_nested() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault.begin().unwrap();
    let decl: Schema = serde_json::from_value(json!({
        "version": 1,
        "journal": {"segment_max_bytes": 1024, "segment_max_lines": 1},
        "isolated": true,
        "fields": [{"name": "msg", "string": {}}]
    }))
    .unwrap();
    vault.create_collection("sealed_log", decl).unwrap();
    let collection = vault.collection("sealed_log").unwrap();
    collection.add("a", rec(json!({"msg": "one"}))).unwrap();
    collection.add("b", rec(json!({"msg": "two"}))).unwrap();
    vault.commit("rotate").unwrap();
    let nested = scratch.vault().join(".isolated").join("sealed_log");
    assert!(nested.join("00000001.jsonl").is_file());
    assert!(nested.join("00000002.jsonl").is_file());
    let ids = scratch
        .vault()
        .join(".isolated")
        .join(".scheda")
        .join("index")
        .join("journal")
        .join("sealed_log")
        .join("ids");
    assert!(ids.is_file());
    let haystack = fs::read_to_string(&ids).unwrap();
    assert!(haystack.contains("\"a\""));
    assert!(haystack.contains("\"b\""));
    assert!(
        !scratch
            .vault()
            .join(".scheda")
            .join("index")
            .join("journal")
            .join("sealed_log")
            .exists()
    );
    assert_eq!(collection.get("a").unwrap().id.as_deref(), Some("a"));
    assert_eq!(collection.get("b").unwrap().id.as_deref(), Some("b"));
}

#[test]
fn isolated_add_without_begin_is_one_nested_commit() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    let nested = scratch.vault().join(".isolated");
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})))
        .unwrap();
    assert_eq!(log_len(&nested), 1);
    assert_eq!(log_len(vault.path()), 1);
}

#[test]
fn isolated_document_round_trip_and_main_find_skips_it() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST);
    write_schema(&scratch, WEST_ISOLATED);
    let vault = scratch.reopen();
    vault
        .collection("west_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "public"})))
        .unwrap();
    vault
        .collection("west_isolated")
        .unwrap()
        .add("secret", rec(json!({"west_pass": "nested-secret"})))
        .unwrap();
    let got = vault
        .collection("west_isolated")
        .unwrap()
        .get("secret")
        .unwrap();
    assert_eq!(got.id.as_deref(), Some("secret"));
    let page = vault.find(&[], &Filter::all().into(), 0, 10).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id.as_deref(), Some("alpha"));
    let named = vault.find(&["west_isolated"], &Filter::all().into(), 0, 10);
    assert_eq!(named.unwrap_err().known_code(), Some(Code::Unsupported));
    let index_dir = vault.index_dir().to_path_buf();
    let haystack = fs::read_to_string(index_dir.join("format")).unwrap();
    assert!(!haystack.contains("nested-secret"));
    // Windows: Tantivy/USearch keep mmap locks until the handle is dropped.
    drop(vault);
    let mut leaked = false;
    for entry in walkdir_files(&index_dir) {
        if fs::read(&entry)
            .unwrap()
            .windows(b"nested-secret".len())
            .any(|w| w == b"nested-secret")
        {
            leaked = true;
            break;
        }
    }
    assert!(!leaked, "main Index copied an isolated field");
}

#[test]
fn isolated_replace_and_history_use_nested_git() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST_ISOLATED);
    let vault = scratch.reopen();
    let collection = vault.collection("west_isolated").unwrap();
    collection
        .add("secret", rec(json!({"west_pass": "one"})))
        .unwrap();
    vault.begin().unwrap();
    collection
        .replace("secret", rec_rep(json!({"west_pass": "two"})))
        .unwrap();
    vault.commit("second").unwrap();
    let got = collection.get("secret").unwrap();
    let hist = collection.history("secret", 0, 10).unwrap();
    assert!(hist.entries.len() >= 2);
    let nested = scratch.vault().join(".isolated");
    assert!(log_len(&nested) >= 2);
    assert_eq!(got.id.as_deref(), Some("secret"));
}

#[test]
fn check_fails_when_nested_git_has_a_remote() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})))
        .unwrap();
    vault.check().unwrap();
    let nested = scratch.vault().join(".isolated");
    let repo = Repository::open(&nested).unwrap();
    repo.remote("origin", "https://example.invalid/isolated.git")
        .unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn check_fails_when_parent_tracks_isolated() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})))
        .unwrap();
    let rel = ".isolated/sealed_log/00000001.jsonl";
    let bytes = fs::read(vault.path().join(rel)).unwrap();
    let repo = Repository::open(vault.path()).unwrap();
    let mut index = repo.index().unwrap();
    let entry = IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        file_size: bytes.len() as u32,
        id: Oid::ZERO_SHA1,
        flags: 0,
        flags_extended: 0,
        path: rel.as_bytes().to_vec(),
    };
    index.add_frombuffer(&entry, &bytes).unwrap();
    index.write().unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[cfg(unix)]
#[test]
fn isolated_record_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})))
        .unwrap();
    let path = scratch
        .vault()
        .join(".isolated")
        .join("sealed_log")
        .join("00000001.jsonl");
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn open_succeeds_when_isolated_tree_is_missing() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST);
    write_schema(&scratch, WEST_ISOLATED);
    assert!(!scratch.vault().join(".isolated").exists());
    let vault = scratch.reopen();
    vault.reindex().unwrap();
    vault.find(&[], &Filter::all().into(), 0, 10).unwrap();
    vault.collection("west_isolated").unwrap();
}

fn walkdir_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walkdir_files(&path));
        } else if path.is_file() {
            out.push(path);
        }
    }
    out
}
