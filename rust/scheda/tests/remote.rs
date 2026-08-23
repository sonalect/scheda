//! C7: clone, auto_sync, push/pull/sync (GIT-8–12, LIB-13, LIB-14, ISO-1).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use git2::{IndexAddOption, Oid, Repository, Signature};
use scheda::{Code, OpenOptions, Record, Vault};
use serde_json::json;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c7-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const NOTES: &str = include_str!("../testdata/document/notes.yaml");

fn rec(collection: &str, id: &str, title: &str) -> Record {
    serde_json::from_value(json!({
        "id": id,
        "collection": collection,
        "fields": {"title": title},
    }))
    .unwrap()
}

fn as_url(path: &Path) -> String {
    path.to_str().expect("utf-8 path").to_owned()
}

fn head_id(dir: &Path) -> Oid {
    Repository::open(dir)
        .unwrap()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
}

fn origin_url(dir: &Path) -> String {
    Repository::open(dir)
        .unwrap()
        .find_remote("origin")
        .unwrap()
        .url()
        .unwrap()
        .to_owned()
}

fn set_origin(dir: &Path, url: &str) {
    Repository::open(dir)
        .unwrap()
        .remote_set_url("origin", url)
        .unwrap();
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

fn seed_source(path: &Path) {
    drop(Vault::init(path, OpenOptions::new()).unwrap());
    fs::create_dir_all(path.join("schema")).unwrap();
    fs::write(path.join("schema").join("notes.yaml"), NOTES).unwrap();
    let vault = Vault::open(path, OpenOptions::new()).unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .insert_one(rec("north_notes", "seed", "seed"))
        .unwrap();
    drop(vault);
    commit_all(path, "seed");
}

fn make_bare(src: &Path, bare: &Path) {
    git2::build::RepoBuilder::new()
        .bare(true)
        .clone(&as_url(src), bare)
        .unwrap();
}

fn pair_with_bare(scratch: &Scratch) -> (PathBuf, PathBuf) {
    let source = scratch.dir("source");
    let bare = scratch.dir("bare");
    seed_source(&source);
    make_bare(&source, &bare);
    (source, bare)
}

fn insert_and_commit(vault: &Vault, id: &str, title: &str, message: &str) {
    vault
        .collection("north_notes")
        .unwrap()
        .insert_one(rec("north_notes", id, title))
        .unwrap();
    vault.commit(message).unwrap();
}

fn has_conflict_markers(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == ".scheda" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if text.contains("<<<<<<<") {
                return true;
            }
        }
    }
    false
}

#[test]
fn clone_sets_origin_to_the_url() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let dest = scratch.dir("clone");
    let vault = Vault::clone(&remote, &dest, OpenOptions::new()).unwrap();
    assert!(vault.has_remote());
    assert_eq!(vault.remote_names(), &["origin".to_owned()]);
    assert_eq!(origin_url(vault.path()), remote);
    drop(vault);
    let err = Vault::open(&dest, OpenOptions::new().remote_repo(&remote)).unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Invalid));
}

#[test]
fn init_with_remote_repo_clones() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let dest = scratch.dir("init-clone");
    let vault = Vault::init(&dest, OpenOptions::new().remote_repo(&remote)).unwrap();
    assert_eq!(origin_url(vault.path()), remote);
}

#[test]
fn clone_rejects_empty_url() {
    let scratch = Scratch::new();
    let err = Vault::clone("", scratch.dir("empty"), OpenOptions::new()).unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Invalid));
}

#[test]
fn auto_sync_off_does_not_move_head() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    drop(Vault::clone(&remote, &a, OpenOptions::new()).unwrap());
    let before = head_id(&a);
    let other = Vault::clone(&remote, &b, OpenOptions::new()).unwrap();
    insert_and_commit(&other, "from-b", "from b", "from b");
    other.push_remote().unwrap();
    drop(other);
    let opened = Vault::open(&a, OpenOptions::new()).unwrap();
    assert_eq!(head_id(opened.path()), before);
    assert_ne!(head_id(opened.path()), head_id(&bare));
}

#[test]
fn auto_sync_on_fast_forwards_head() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    drop(Vault::clone(&remote, &a, OpenOptions::new()).unwrap());
    let other = Vault::clone(&remote, &b, OpenOptions::new()).unwrap();
    insert_and_commit(&other, "from-b", "from b", "from b");
    other.push_remote().unwrap();
    drop(other);
    let opened = Vault::open(&a, OpenOptions::new().auto_sync(true)).unwrap();
    assert_eq!(head_id(opened.path()), head_id(&bare));
}

#[test]
fn auto_sync_on_diverged_is_not_fast_forward() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    let first = Vault::clone(&remote, &a, OpenOptions::new()).unwrap();
    let second = Vault::clone(&remote, &b, OpenOptions::new()).unwrap();
    insert_and_commit(&first, "from-a", "from a", "from a");
    first.push_remote().unwrap();
    drop(first);
    insert_and_commit(&second, "from-b", "from b", "from b");
    drop(second);
    let err = Vault::open(&b, OpenOptions::new().auto_sync(true)).unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::NotFastForward));
    assert!(!has_conflict_markers(&b));
}

#[test]
fn pull_push_sync_with_auto_sync_off() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    let first = Vault::clone(&remote, &a, OpenOptions::new()).unwrap();
    insert_and_commit(&first, "from-a", "from a", "from a");
    first.sync_remote().unwrap();
    drop(first);
    let second = Vault::clone(&remote, &b, OpenOptions::new()).unwrap();
    assert_eq!(head_id(second.path()), head_id(&bare));
    insert_and_commit(&second, "from-b", "from b", "from b");
    second.push_remote().unwrap();
    drop(second);
    let first = Vault::open(&a, OpenOptions::new()).unwrap();
    first.pull_remote().unwrap();
    assert_eq!(head_id(first.path()), head_id(&bare));
}

#[test]
fn diverged_sync_fails_before_push() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    let first = Vault::clone(&remote, &a, OpenOptions::new()).unwrap();
    let second = Vault::clone(&remote, &b, OpenOptions::new()).unwrap();
    insert_and_commit(&first, "from-a", "from a", "from a");
    first.push_remote().unwrap();
    let published = head_id(&bare);
    drop(first);
    insert_and_commit(&second, "from-b", "from b", "from b");
    let local = head_id(second.path());
    let err = second.sync_remote().unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::NotFastForward));
    assert_eq!(head_id(&bare), published);
    assert_ne!(head_id(&bare), local);
}

#[test]
fn unreachable_push_does_not_roll_back_commit() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let dest = scratch.dir("clone");
    let missing = scratch.dir("missing.git");
    let vault = Vault::clone(&remote, &dest, OpenOptions::new()).unwrap();
    set_origin(vault.path(), &as_url(&missing));
    vault
        .collection("north_notes")
        .unwrap()
        .insert_one(rec("north_notes", "offline", "offline"))
        .unwrap();
    vault.commit("offline").unwrap();
    let committed = head_id(vault.path());
    let err = vault.push_remote().unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Git));
    assert_eq!(head_id(vault.path()), committed);
    set_origin(vault.path(), &remote);
    vault.push_remote().unwrap();
    assert_eq!(head_id(&bare), committed);
}

#[test]
fn auto_sync_commit_pushes_when_remote_is_reachable() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let dest = scratch.dir("clone");
    let vault = Vault::clone(&remote, &dest, OpenOptions::new().auto_sync(true)).unwrap();
    insert_and_commit(&vault, "auto", "auto", "auto push");
    assert_eq!(head_id(vault.path()), head_id(&bare));
}

#[test]
fn writes_and_commit_do_not_need_the_network() {
    let scratch = Scratch::new();
    let vault_path = scratch.dir("vault");
    drop(Vault::init(&vault_path, OpenOptions::new()).unwrap());
    fs::create_dir_all(vault_path.join("schema")).unwrap();
    fs::write(vault_path.join("schema").join("notes.yaml"), NOTES).unwrap();
    let vault = Vault::open(&vault_path, OpenOptions::new()).unwrap();
    git2::Repository::open(vault.path())
        .unwrap()
        .remote("origin", "https://example.invalid/scheda.git")
        .unwrap();
    drop(vault);
    let vault = Vault::open(&vault_path, OpenOptions::new()).unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .insert_one(rec("north_notes", "local", "local"))
        .unwrap();
    vault.commit("local only").unwrap();
    vault.reindex().unwrap();
    assert!(vault.has_remote());
}

#[test]
fn push_without_remote_is_invalid() {
    let scratch = Scratch::new();
    let vault = Vault::init(scratch.dir("vault"), OpenOptions::new()).unwrap();
    let err = vault.push_remote().unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Invalid));
}
