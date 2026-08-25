//! C6: local `commit` and `history` (GIT-2, GIT-3, LIB-12, LIB-17).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use git2::{ObjectType, Repository};
use scheda::{Code, OpenOptions, Record, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
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
        Vault::init(self.vault(), self.options()).unwrap()
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), self.options()).unwrap()
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

const NOTES: &str = include_str!("../testdata/document/notes.yaml");
const INBOX: &str = include_str!("../testdata/document/inbox.yaml");
const LOG: &str = include_str!("../testdata/document/log.yaml");

fn rec(collection: &str, id: &str, fields: Value) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": collection,
        "fields": fields,
    }))
    .unwrap()
}

fn rec_body(collection: &str, id: &str, fields: Value, body: &str) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": collection,
        "fields": fields,
        "body": body,
    }))
    .unwrap()
}

fn open_notes(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    scratch.write_schema("notes.yaml", NOTES);
    scratch.reopen()
}

fn log_len(dir: &Path) -> usize {
    let repo = Repository::open(dir).unwrap();
    let mut walk = repo.revwalk().unwrap();
    if walk.push_head().is_err() {
        return 0;
    }
    walk.count()
}

fn git_show(dir: &Path, hash: &str, path: &str) -> Vec<u8> {
    let repo = Repository::open(dir).unwrap();
    let oid = git2::Oid::from_str(hash).unwrap();
    let commit = repo.find_commit(oid).unwrap();
    let blob = commit
        .tree()
        .unwrap()
        .get_path(Path::new(path))
        .unwrap()
        .to_object(&repo)
        .unwrap()
        .peel_to_blob()
        .unwrap();
    blob.content().to_vec()
}

fn git_commit_time(dir: &Path, hash: &str) -> git2::Time {
    let repo = Repository::open(dir).unwrap();
    let oid = git2::Oid::from_str(hash).unwrap();
    repo.find_commit(oid).unwrap().time()
}

fn tree_paths(repo: &Repository, tree: &git2::Tree, prefix: &str, out: &mut BTreeSet<String>) {
    for entry in tree {
        let name = entry.name().unwrap();
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.kind() {
            Some(ObjectType::Tree) => {
                let child = repo.find_tree(entry.id()).unwrap();
                tree_paths(repo, &child, &path, out);
            }
            Some(ObjectType::Blob) => {
                out.insert(path);
            }
            _ => {}
        }
    }
}

fn head_paths(dir: &Path) -> BTreeSet<String> {
    let repo = Repository::open(dir).unwrap();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    let mut out = BTreeSet::new();
    tree_paths(&repo, &tree, "", &mut out);
    out
}

#[test]
fn inserts_without_commit_leave_git_log_unchanged() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let before = log_len(vault.path());
    let collection = vault.collection("north_notes").unwrap();
    collection
        .insert_one(rec(
            "north_notes",
            "alpha",
            json!({"title": "A", "count": 1}),
        ))
        .unwrap();
    collection
        .insert_one(rec(
            "north_notes",
            "beta",
            json!({"title": "B", "count": 2}),
        ))
        .unwrap();
    assert_eq!(log_len(vault.path()), before);
    assert_eq!(before, 0);
}

#[test]
fn commit_stages_written_paths_not_vault_root_junk() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    fs::write(scratch.vault().join("noise.txt"), "leave me out\n").unwrap();
    let collection = vault.collection("north_notes").unwrap();
    collection
        .insert_one(rec_body(
            "north_notes",
            "alpha",
            json!({"title": "A", "count": 1}),
            "hello",
        ))
        .unwrap();
    collection
        .insert_one(rec(
            "north_notes",
            "beta",
            json!({"title": "B", "count": 2}),
        ))
        .unwrap();
    let alpha = collection.find_one("alpha").unwrap();
    let beta = collection.find_one("beta").unwrap();
    let alpha_path = alpha.path.clone().unwrap();
    let beta_path = beta.path.clone().unwrap();
    vault.commit("two notes").unwrap();
    assert_eq!(log_len(vault.path()), 1);
    let paths = head_paths(vault.path());
    assert!(paths.contains(&alpha_path), "{paths:?}");
    assert!(paths.contains(&beta_path), "{paths:?}");
    assert!(!paths.contains("noise.txt"), "{paths:?}");
}

#[test]
fn history_matches_git_show_after_two_document_commits() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    let collection = vault.collection("north_notes").unwrap();
    collection
        .insert_one(rec_body(
            "north_notes",
            "alpha",
            json!({"title": "one", "count": 1}),
            "first",
        ))
        .unwrap();
    vault.commit("first edit").unwrap();
    collection
        .replace_one(
            "alpha",
            rec_body(
                "north_notes",
                "alpha",
                json!({"title": "two", "count": 2}),
                "second",
            ),
        )
        .unwrap();
    vault.commit("second edit").unwrap();
    let path = collection.find_one("alpha").unwrap().path.unwrap();
    let snaps = vault.history(&path).unwrap();
    assert_eq!(snaps.len(), 2);
    let newest = &snaps[0];
    let oldest = &snaps[1];
    assert_eq!(
        newest.raw.as_deref().unwrap(),
        git_show(vault.path(), newest.hash.as_deref().unwrap(), &path)
    );
    assert_eq!(
        oldest.raw.as_deref().unwrap(),
        git_show(vault.path(), oldest.hash.as_deref().unwrap(), &path)
    );
    let when = git_commit_time(vault.path(), newest.hash.as_deref().unwrap());
    assert_eq!(newest.time.seconds, when.seconds());
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
    vault
        .collection("north_inbox")
        .unwrap()
        .insert_one(rec("north_inbox", "inbox", json!({"title": "hi"})))
        .unwrap();
    vault
        .collection("north_log")
        .unwrap()
        .insert_one(rec("north_log", "e1", json!({})))
        .unwrap();
    vault.commit("singleton and journal").unwrap();
    let page = vault
        .collection("north_inbox")
        .unwrap()
        .find_one("inbox")
        .unwrap();
    let page_path = page.path.clone().unwrap();
    let page_hist = vault.history(&page_path).unwrap();
    assert_eq!(page_hist.len(), 1);
    assert_eq!(
        page_hist[0].raw.as_deref().unwrap(),
        git_show(
            vault.path(),
            page_hist[0].hash.as_deref().unwrap(),
            &page_path
        )
    );
    let line = vault
        .collection("north_log")
        .unwrap()
        .find_one("e1")
        .unwrap();
    let seg = line.path.clone().unwrap();
    let seg_hist = vault.history(&seg).unwrap();
    assert_eq!(seg_hist.len(), 1);
    assert_eq!(
        seg_hist[0].raw.as_deref().unwrap(),
        git_show(vault.path(), seg_hist[0].hash.as_deref().unwrap(), &seg)
    );
}

#[test]
fn commit_on_reader_is_locked() {
    let scratch = Scratch::new();
    let _writer = open_notes(&scratch);
    let reader = Vault::open(scratch.vault(), scratch.options().write(false)).unwrap();
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
        .insert_one(rec(
            "north_notes",
            "alpha",
            json!({"title": "A", "count": 1}),
        ))
        .unwrap();
    let err = vault.commit("  ").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn history_rejects_empty_and_dotdot_path() {
    let scratch = Scratch::new();
    let vault = open_notes(&scratch);
    assert_eq!(
        vault.history("").unwrap_err().known_code(),
        Some(Code::Invalid)
    );
    assert_eq!(
        vault.history("../outside").unwrap_err().known_code(),
        Some(Code::Invalid)
    );
    assert!(vault.history("missing/file.md").unwrap().is_empty());
}
