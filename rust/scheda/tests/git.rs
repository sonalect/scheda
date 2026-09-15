//! C6: local `commit` and `history` (GIT-2, GIT-3, LIB-12, LIB-17).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Author, Code, OpenOptions, RecordAdd, RecordReplace, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c6-{}-{n}", process::id()));
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

const NOTES: &str = include_str!("../testdata/document/notes.yaml");
const INBOX: &str = include_str!("../testdata/document/inbox.yaml");
const LOG: &str = include_str!("../testdata/document/log.yaml");

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn rec_body(fields: Value, body: &str) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields, "body": body })).unwrap()
}

fn rec_rep_body(fields: Value, body: &str) -> RecordReplace {
    serde_json::from_value(json!({ "fields": fields, "body": body })).unwrap()
}

fn open_notes(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    vault.begin().unwrap();
    vault
}

fn git_output(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .expect("git executable")
}

fn git_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = git_output(dir, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(git_ok(dir, args).stdout).unwrap()
}

fn log_len(dir: &Path) -> usize {
    let out = git_output(dir, &["rev-list", "--count", "HEAD"]);
    if !out.status.success() {
        return 0;
    }
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap_or(0)
}

fn git_show(dir: &Path, hash: &str, path: &str) -> Vec<u8> {
    git_ok(dir, &["cat-file", "blob", &format!("{hash}:{path}")]).stdout
}

fn git_commit_time(dir: &Path, hash: &str) -> i64 {
    git_stdout(dir, &["log", "-1", "--format=%at", hash])
        .trim()
        .parse()
        .unwrap()
}

fn head_paths(dir: &Path) -> BTreeSet<String> {
    git_stdout(dir, &["ls-tree", "-r", "--name-only", "HEAD"])
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn create_sets_core_longpaths() {
    let scratch = Scratch::new();
    let _vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
    let value = git_stdout(
        scratch.vault().as_path(),
        &["config", "--local", "--get", "core.longpaths"],
    );
    assert_eq!(value.trim(), "true");
}

#[test]
fn inserts_without_commit_leave_git_log_unchanged() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let before = log_len(vault.path());
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    collection
        .add("beta", rec(json!({"title": "B", "count": 2})), None)
        .unwrap();
    assert_eq!(log_len(vault.path()), before);
}

#[test]
fn commit_stages_written_paths_not_vault_root_junk() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    fs::write(scratch.vault().join("noise.txt"), "leave me out\n").unwrap();
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "alpha",
            rec_body(json!({"title": "A", "count": 1}), "hello"),
            None,
        )
        .unwrap();
    collection
        .add("beta", rec(json!({"title": "B", "count": 2})), None)
        .unwrap();
    let alpha = collection.get("alpha").unwrap();
    let beta = collection.get("beta").unwrap();
    let alpha_path = alpha.path.clone().unwrap();
    let beta_path = beta.path.clone().unwrap();
    vault.commit("two notes").unwrap();
    assert_eq!(log_len(vault.path()), 2);
    let paths = head_paths(vault.path());
    assert!(paths.contains(&alpha_path), "{paths:?}");
    assert!(paths.contains(&beta_path), "{paths:?}");
    assert!(!paths.contains("noise.txt"), "{paths:?}");
}

#[test]
fn abandoned_writes_are_discarded_on_next_autocommit() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    let rel = collection.get("alpha").unwrap().path.unwrap();
    let journal = scratch.vault().join(".scheda").join("tx.log");
    assert!(journal.is_file(), "{}", journal.display());
    drop(collection);
    drop(vault);
    let vault = scratch.reopen();
    assert_eq!(
        vault.commit("after reopen").unwrap_err().known_code(),
        Some(Code::Invalid)
    );
    let collection = vault.collection("north_notes").unwrap();
    assert_eq!(
        collection.get("alpha").unwrap().id.as_deref(),
        Some("alpha")
    );
    let before = log_len(vault.path());
    collection
        .add("beta", rec(json!({"title": "B", "count": 2})), None)
        .unwrap();
    assert_eq!(
        collection.get("alpha").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert_eq!(collection.get("beta").unwrap().id.as_deref(), Some("beta"));
    assert!(!scratch.vault().join(&rel).is_file());
    assert_eq!(log_len(vault.path()), before + 1);
}

#[test]
fn history_matches_git_show_after_two_document_commits() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "alpha",
            rec_body(json!({"title": "one", "count": 1}), "first"),
            None,
        )
        .unwrap();
    vault.commit("first edit").unwrap();
    vault.begin().unwrap();
    collection
        .replace(
            "alpha",
            rec_rep_body(json!({"title": "two", "count": 2}), "second"),
            None,
        )
        .unwrap();
    vault.commit("second edit").unwrap();
    let path = collection.get("alpha").unwrap().path.unwrap();
    let snaps = collection.history("alpha", 0, 100).unwrap();
    assert_eq!(snaps.entries.len(), 2);
    let newest = &snaps.entries[0];
    let oldest = &snaps.entries[1];
    assert_eq!(
        newest.raw.as_deref().unwrap(),
        git_show(vault.path(), newest.hash.as_deref().unwrap(), &path)
    );
    assert_eq!(
        oldest.raw.as_deref().unwrap(),
        git_show(vault.path(), oldest.hash.as_deref().unwrap(), &path)
    );
    let when = git_commit_time(vault.path(), newest.hash.as_deref().unwrap());
    assert_eq!(newest.time.seconds, when);
    assert_eq!(newest.time.nanos, 0);
    let newest_text = String::from_utf8(newest.raw.clone().unwrap()).unwrap();
    let oldest_text = String::from_utf8(oldest.raw.clone().unwrap()).unwrap();
    assert!(newest_text.contains("second"), "{newest_text}");
    assert!(oldest_text.contains("first"), "{oldest_text}");
    assert!(newest.message.as_deref().unwrap().contains("second edit"));
    assert!(oldest.message.as_deref().unwrap().contains("first edit"));
}

#[test]
fn history_of_singleton_and_journal_segment() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("inbox.yaml", INBOX);
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    vault.begin().unwrap();
    vault
        .collection("north_inbox")
        .unwrap()
        .add("inbox", rec(json!({"title": "hi"})), None)
        .unwrap();
    vault
        .collection("north_log")
        .unwrap()
        .add("e1", rec(json!({})), None)
        .unwrap();
    vault.commit("singleton and journal").unwrap();
    let page = vault
        .collection("north_inbox")
        .unwrap()
        .get("inbox")
        .unwrap();
    let page_path = page.path.clone().unwrap();
    let page_hist = vault
        .collection("north_inbox")
        .unwrap()
        .history("inbox", 0, 100)
        .unwrap();
    assert_eq!(page_hist.entries.len(), 1);
    assert_eq!(
        page_hist.entries[0].raw.as_deref().unwrap(),
        git_show(
            vault.path(),
            page_hist.entries[0].hash.as_deref().unwrap(),
            &page_path
        )
    );
    let line = vault.collection("north_log").unwrap().get("e1").unwrap();
    let seg = line.path.clone().unwrap();
    let seg_hist = vault
        .collection("north_log")
        .unwrap()
        .history("e1", 0, 100)
        .unwrap();
    assert_eq!(seg_hist.entries.len(), 1);
    assert_eq!(
        seg_hist.entries[0].raw.as_deref().unwrap(),
        git_show(
            vault.path(),
            seg_hist.entries[0].hash.as_deref().unwrap(),
            &seg
        )
    );
}

#[test]
fn commit_on_writer_holds_lock() {
    let scratch = Scratch::new();
    let writer = open_notes(&scratch);
    writer
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "A"})), None)
        .unwrap();
    let reader = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
    assert!(writer.holds_write_lock());
    let err = reader.commit("no").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Locked));
}

#[test]
fn empty_commit_message_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    let err = vault.commit("  ").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn insert_one_without_begin_autocommits() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    let before = log_len(vault.path());
    vault
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    assert_eq!(log_len(vault.path()), before + 1);
    let err = vault.commit("no txn").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn begin_twice_is_invalid() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    let err = vault.begin().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn discard_removes_uncommitted_insert() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    let rel = collection.get("alpha").unwrap().path.unwrap();
    vault.discard().unwrap();
    assert!(!scratch.vault().join(&rel).is_file());
    assert_eq!(
        collection.get("alpha").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    vault.begin().unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .add("beta", rec(json!({"title": "B", "count": 2})), None)
        .unwrap();
    vault.commit("after discard").unwrap();
    assert!(vault.collection("north_notes").unwrap().get("beta").is_ok());
}

#[test]
fn history_rejects_empty_id_and_zero_count() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    assert_eq!(
        collection.history("", 0, 10).unwrap_err().known_code(),
        Some(Code::Invalid)
    );
    assert_eq!(
        collection.history("alpha", 0, 0).unwrap_err().known_code(),
        Some(Code::Invalid)
    );
    assert_eq!(
        collection
            .history("missing", 0, 10)
            .unwrap_err()
            .known_code(),
        Some(Code::NotFound)
    );
}

#[test]
fn history_after_remove_still_answers_from_git() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add(
            "alpha",
            rec_body(json!({"title": "one", "count": 1}), "first"),
            None,
        )
        .unwrap();
    vault.commit("first edit").unwrap();
    vault.begin().unwrap();
    collection
        .replace(
            "alpha",
            rec_rep_body(json!({"title": "two", "count": 2}), "second"),
            None,
        )
        .unwrap();
    vault.commit("second edit").unwrap();
    let path = collection.get("alpha").unwrap().path.unwrap();
    let before = collection.history("alpha", 0, 100).unwrap();
    assert_eq!(before.entries.len(), 2);
    vault.begin().unwrap();
    collection.remove("alpha", None).unwrap();
    vault.commit("gone").unwrap();
    assert_eq!(
        collection.get("alpha").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    let snaps = collection.history("alpha", 0, 100).unwrap();
    assert_eq!(snaps.entries.len(), before.entries.len());
    let before_hashes: BTreeSet<_> = before
        .entries
        .iter()
        .filter_map(|entry| entry.hash.as_deref())
        .collect();
    let after_hashes: BTreeSet<_> = snaps
        .entries
        .iter()
        .filter_map(|entry| entry.hash.as_deref())
        .collect();
    assert_eq!(after_hashes, before_hashes);
    for entry in &snaps.entries {
        assert_eq!(
            entry.raw.as_deref().unwrap(),
            git_show(vault.path(), entry.hash.as_deref().unwrap(), &path)
        );
    }
    let texts: Vec<String> = snaps
        .entries
        .iter()
        .map(|entry| String::from_utf8(entry.raw.clone().unwrap()).unwrap())
        .collect();
    assert!(
        texts.iter().any(|text| text.contains("second")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|text| text.contains("first")), "{texts:?}");
}

#[test]
fn remove_autocommits_outside_a_transaction() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    let before = log_len(vault.path());
    collection.remove("alpha", None).unwrap();
    assert_eq!(log_len(vault.path()), before + 1);
    assert_eq!(
        collection.get("alpha").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert!(
        !scratch
            .vault()
            .join("north_notes")
            .join("alpha.md")
            .is_file()
    );
    assert!(!head_paths(vault.path()).contains("north_notes/alpha.md"));
}

#[test]
fn remove_inside_transaction_is_one_commit() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    collection
        .add("beta", rec(json!({"title": "B", "count": 2})), None)
        .unwrap();
    vault.commit("keep both").unwrap();
    let before = log_len(vault.path());
    vault.begin().unwrap();
    collection.remove("alpha", None).unwrap();
    assert_eq!(log_len(vault.path()), before);
    vault.commit("drop alpha").unwrap();
    assert_eq!(log_len(vault.path()), before + 1);
    assert_eq!(
        collection.get("alpha").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert_eq!(collection.get("beta").unwrap().id.as_deref(), Some("beta"));
}

#[test]
fn abandoned_remove_is_restored_on_next_autocommit() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    vault.commit("keep").unwrap();
    vault.begin().unwrap();
    collection.remove("alpha", None).unwrap();
    assert!(
        !scratch
            .vault()
            .join("north_notes")
            .join("alpha.md")
            .is_file()
    );
    drop(collection);
    drop(vault);
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    collection
        .add("beta", rec(json!({"title": "B", "count": 2})), None)
        .unwrap();
    assert_eq!(
        collection.get("alpha").unwrap().id.as_deref(),
        Some("alpha")
    );
    assert_eq!(collection.get("beta").unwrap().id.as_deref(), Some("beta"));
}

#[test]
fn killed_mid_batch_keeps_complete_journal_lines() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    scratch.write_schema("log.yaml", LOG);
    let vault = scratch.reopen();
    vault.begin().unwrap();
    let before = log_len(vault.path());
    let collection = vault.collection("north_log").unwrap();
    collection.add("e1", rec(json!({})), None).unwrap();
    collection.add("e2", rec(json!({})), None).unwrap();
    let path = scratch
        .vault()
        .join("north_log")
        .join(format!("00000001-{}.jsonl", vault.writer()));
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str(r#"{"id":"partial"}"#);
    fs::write(&path, &text).unwrap();
    assert_eq!(log_len(vault.path()), before);
    drop(collection);
    drop(vault);
    let vault = scratch.reopen();
    let collection = vault.collection("north_log").unwrap();
    assert_eq!(collection.get("e1").unwrap().id.as_deref(), Some("e1"));
    assert_eq!(collection.get("e2").unwrap().id.as_deref(), Some("e2"));
    assert_eq!(
        collection.get("partial").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .ends_with(r#"{"id":"partial"}"#)
    );
    assert_eq!(log_len(vault.path()), before);
}

fn head_identity(dir: &Path) -> (String, String, String, String) {
    let text = git_stdout(dir, &["log", "-1", "--format=%an%n%ae%n%cn%n%ce"]);
    let mut lines = text.lines().map(str::trim);
    (
        lines.next().unwrap_or("").to_owned(),
        lines.next().unwrap_or("").to_owned(),
        lines.next().unwrap_or("").to_owned(),
        lines.next().unwrap_or("").to_owned(),
    )
}

fn set_local_identity(dir: &Path, name: &str, email: &str) {
    git_ok(dir, &["config", "user.name", name]);
    git_ok(dir, &["config", "user.email", email]);
}

#[test]
fn autocommit_uses_caller_message() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    vault
        .collection("north_notes")
        .unwrap()
        .add(
            "alpha",
            rec(json!({"title": "A", "count": 1})),
            Some("note alpha"),
        )
        .unwrap();
    let hist = vault
        .collection("north_notes")
        .unwrap()
        .history("alpha", 0, 10)
        .unwrap();
    assert_eq!(
        hist.entries[0].message.as_deref().unwrap(),
        "note alpha",
        "{hist:?}"
    );
}

#[test]
fn omitted_autocommit_message_is_scheda() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    vault
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    let hist = vault
        .collection("north_notes")
        .unwrap()
        .history("alpha", 0, 10)
        .unwrap();
    assert!(
        hist.entries[0]
            .message
            .as_deref()
            .unwrap()
            .contains("scheda"),
        "{hist:?}"
    );
}

#[test]
fn empty_override_message_is_invalid_before_write() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    let collection = vault.collection("north_notes").unwrap();
    for message in [Some(""), Some("  ")] {
        let err = collection
            .add("alpha", rec(json!({"title": "A", "count": 1})), message)
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid), "{err}");
        assert_eq!(
            collection.get("alpha").unwrap_err().known_code(),
            Some(Code::NotFound)
        );
    }
}

#[test]
fn message_inside_transaction_is_ignored() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    vault
        .collection("north_notes")
        .unwrap()
        .add(
            "alpha",
            rec(json!({"title": "A", "count": 1})),
            Some("ignored"),
        )
        .unwrap();
    vault.commit("txn subject").unwrap();
    let hist = vault
        .collection("north_notes")
        .unwrap()
        .history("alpha", 0, 10)
        .unwrap();
    let message = hist.entries[0].message.as_deref().unwrap();
    assert_eq!(message, "txn subject", "{message}");
    assert!(!message.contains("ignored"), "{message}");
}

#[test]
fn history_message_keeps_the_body_and_drops_the_terminator() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    vault
        .collection("north_notes")
        .unwrap()
        .add(
            "alpha",
            rec(json!({"title": "A", "count": 1})),
            Some("subject line\n\nbody line"),
        )
        .unwrap();
    let hist = vault
        .collection("north_notes")
        .unwrap()
        .history("alpha", 0, 10)
        .unwrap();
    assert_eq!(
        hist.entries[0].message.as_deref().unwrap(),
        "subject line\n\nbody line",
        "{hist:?}"
    );
}

#[test]
fn open_options_identity_is_author_and_committer() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    set_local_identity(scratch.vault().as_path(), "Config", "config@invalid");
    scratch.write_schema("notes.yaml", NOTES);
    let options = OpenOptions::new().author(Author::new().name("Ada").email("ada@invalid"));
    let vault = Vault::open(scratch.vault(), options, None).unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    let hist = vault
        .collection("north_notes")
        .unwrap()
        .history("alpha", 0, 10)
        .unwrap();
    assert_eq!(hist.entries[0].author.as_deref(), Some("Ada <ada@invalid>"));
    let (an, ae, cn, ce) = head_identity(vault.path());
    assert_eq!((an, ae), ("Ada".into(), "ada@invalid".into()));
    assert_eq!((cn, ce), ("Ada".into(), "ada@invalid".into()));
}

#[test]
fn git_config_identity_when_open_options_unset() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    set_local_identity(scratch.vault().as_path(), "Config", "config@invalid");
    scratch.write_schema("notes.yaml", NOTES);
    let vault = scratch.reopen();
    vault
        .collection("north_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "A", "count": 1})), None)
        .unwrap();
    let hist = vault
        .collection("north_notes")
        .unwrap()
        .history("alpha", 0, 10)
        .unwrap();
    assert_eq!(
        hist.entries[0].author.as_deref(),
        Some("Config <config@invalid>")
    );
}

#[test]
fn open_options_only_name_is_invalid() {
    let scratch = Scratch::new();
    let err = Vault::open(
        scratch.vault(),
        OpenOptions::new().author(Author::new().name("Ada")),
        None,
    )
    .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(!scratch.vault().exists() || !scratch.vault().join(".git").exists());
}

#[test]
fn open_options_identity_chars_are_invalid() {
    let scratch = Scratch::new();
    for options in [
        OpenOptions::new().author(Author::new().name("Ada<").email("ada@invalid")),
        OpenOptions::new().author(Author::new().name("Ada>").email("ada@invalid")),
        OpenOptions::new().author(Author::new().name("Ada\n").email("ada@invalid")),
        OpenOptions::new().author(Author::new().name("Ada").email("ada invalid")),
    ] {
        let err = Vault::open(scratch.vault(), options, None).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid), "{err}");
    }
}

#[test]
fn isolated_commit_uses_open_options_identity() {
    let scratch = Scratch::new();
    let options = OpenOptions::new().author(Author::new().name("Ada").email("ada@invalid"));
    let vault = Vault::open(scratch.vault(), options, None).unwrap();
    let decl: scheda::Schema = serde_json::from_value(json!({
        "version": 1,
        "document": {},
        "isolated": true,
        "fields": [{"name": "title", "string": {}}]
    }))
    .unwrap();
    vault.create_collection("north_sealed", decl, None).unwrap();
    vault
        .collection("north_sealed")
        .unwrap()
        .add("alpha", rec(json!({"title": "secret"})), None)
        .unwrap();
    let hist = vault
        .collection("north_sealed")
        .unwrap()
        .history("alpha", 0, 10)
        .unwrap();
    assert_eq!(hist.entries[0].author.as_deref(), Some("Ada <ada@invalid>"));
    let nested = scratch.vault().join(".isolated");
    let (an, ae, cn, ce) = head_identity(&nested);
    assert_eq!((an, ae), ("Ada".into(), "ada@invalid".into()));
    assert_eq!((cn, ce), ("Ada".into(), "ada@invalid".into()));
    let (man, mae, mcn, mce) = head_identity(vault.path());
    assert_eq!((man, mae), ("Ada".into(), "ada@invalid".into()));
    assert_eq!((mcn, mce), ("Ada".into(), "ada@invalid".into()));
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
