//! Clone, auto_sync, push/pull/sync.
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, OpenOptions, Vault};
use serde_json::json;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-c7", n),
        }
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

fn rec(title: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"title": title})).unwrap()
}

fn as_url(path: &Path) -> String {
    let path = path.to_str().expect("utf-8 path");
    if let Some(rest) = path.strip_prefix('/') {
        format!("file:///{rest}")
    } else {
        format!("file:///{}", path.replace('\\', "/"))
    }
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

fn head_id(dir: &Path) -> String {
    git_stdout(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn origin_url(dir: &Path) -> String {
    git_stdout(dir, &["remote", "get-url", "origin"])
        .trim()
        .to_owned()
}

fn set_origin(dir: &Path, url: &str) {
    git_ok(dir, &["remote", "set-url", "origin", url]);
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

fn seed_source(path: &Path) {
    drop(Vault::open(path, OpenOptions::new(), None).unwrap());
    let dir = path.join("north_notes");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".collection.yaml"), NOTES).unwrap();
    let vault = Vault::open(path, OpenOptions::new(), None).unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .add("seed", rec("seed"), None)
        .unwrap();
    drop(vault);
    commit_all(path, "seed");
}

fn make_bare(src: &Path, bare: &Path) {
    git_ok(
        src,
        &[
            "clone",
            "--bare",
            "--",
            &as_url(src),
            &bare.to_string_lossy(),
        ],
    );
}

fn pair_with_bare(scratch: &Scratch) -> (PathBuf, PathBuf) {
    let source = scratch.dir("source");
    let bare = scratch.dir("bare");
    seed_source(&source);
    make_bare(&source, &bare);
    (source, bare)
}

fn insert_and_commit(vault: &Vault, id: &str, title: &str, message: &str) {
    vault.begin().unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .add(id, rec(title), None)
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
    let vault = Vault::open(&dest, OpenOptions::new().remote_repo(&remote), None).unwrap();
    assert!(vault.has_remote());
    assert_eq!(vault.remote_names(), &["origin".to_owned()]);
    assert_eq!(origin_url(vault.path()), remote);
    drop(vault);
    let opened = Vault::open(&dest, OpenOptions::new().remote_repo(&remote), None).unwrap();
    assert_eq!(origin_url(opened.path()), remote);
}

#[test]
fn init_with_remote_repo_clones() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let dest = scratch.dir("init-clone");
    let vault = Vault::open(&dest, OpenOptions::new().remote_repo(&remote), None).unwrap();
    assert_eq!(origin_url(vault.path()), remote);
}

#[test]
fn empty_remote_repo_on_missing_path_is_invalid() {
    let scratch = Scratch::new();
    let dest = scratch.dir("empty");
    let err = Vault::open(&dest, OpenOptions::new().remote_repo(""), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(!dest.exists() || !dest.join(".git").exists());
}

#[test]
fn auto_sync_off_does_not_move_head() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    drop(Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap());
    let before = head_id(&a);
    let other = Vault::open(&b, OpenOptions::new().remote_repo(&remote), None).unwrap();
    insert_and_commit(&other, "from-b", "from b", "from b");
    other.push().unwrap();
    drop(other);
    let opened = Vault::open(&a, OpenOptions::new(), None).unwrap();
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
    drop(Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap());
    let other = Vault::open(&b, OpenOptions::new().remote_repo(&remote), None).unwrap();
    insert_and_commit(&other, "from-b", "from b", "from b");
    other.push().unwrap();
    drop(other);
    let opened = Vault::open(&a, OpenOptions::new().auto_sync(true), None).unwrap();
    assert_eq!(head_id(opened.path()), head_id(&bare));
}

#[test]
fn auto_sync_open_is_locked_while_writer_holds_lock() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let writer = Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap();
    writer.begin().unwrap();
    let err = Vault::open(&a, OpenOptions::new().auto_sync(true), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Locked));
    drop(writer);
}

#[test]
fn auto_sync_on_diverged_is_not_fast_forward() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    let first = Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap();
    let second = Vault::open(&b, OpenOptions::new().remote_repo(&remote), None).unwrap();
    insert_and_commit(&first, "from-a", "from a", "from a");
    first.push().unwrap();
    drop(first);
    insert_and_commit(&second, "from-b", "from b", "from b");
    drop(second);
    let err = Vault::open(&b, OpenOptions::new().auto_sync(true), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFastForward));
    assert!(!has_conflict_markers(&b));
}

#[test]
fn pull_push_sync_with_auto_sync_off() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    let first = Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap();
    insert_and_commit(&first, "from-a", "from a", "from a");
    first.sync().unwrap();
    drop(first);
    let second = Vault::open(&b, OpenOptions::new().remote_repo(&remote), None).unwrap();
    assert_eq!(head_id(second.path()), head_id(&bare));
    insert_and_commit(&second, "from-b", "from b", "from b");
    second.push().unwrap();
    drop(second);
    let first = Vault::open(&a, OpenOptions::new(), None).unwrap();
    first.pull().unwrap();
    assert_eq!(head_id(first.path()), head_id(&bare));
}

#[test]
fn diverged_sync_fails_before_push() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    let first = Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap();
    let second = Vault::open(&b, OpenOptions::new().remote_repo(&remote), None).unwrap();
    insert_and_commit(&first, "from-a", "from a", "from a");
    first.push().unwrap();
    let published = head_id(&bare);
    drop(first);
    insert_and_commit(&second, "from-b", "from b", "from b");
    let local = head_id(second.path());
    let err = second.sync().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFastForward));
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
    let vault = Vault::open(&dest, OpenOptions::new().remote_repo(&remote), None).unwrap();
    set_origin(vault.path(), &as_url(&missing));
    vault.begin().unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .add("offline", rec("offline"), None)
        .unwrap();
    vault.commit("offline").unwrap();
    let committed = head_id(vault.path());
    let err = vault.push().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::GitGeneric));
    assert_eq!(head_id(vault.path()), committed);
    set_origin(vault.path(), &remote);
    vault.push().unwrap();
    assert_eq!(head_id(&bare), committed);
}

#[test]
fn auto_sync_commit_pushes_when_remote_is_reachable() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let dest = scratch.dir("clone");
    let vault = Vault::open(
        &dest,
        OpenOptions::new().auto_sync(true).remote_repo(&remote),
        None,
    )
    .unwrap();
    insert_and_commit(&vault, "auto", "auto", "auto push");
    assert_eq!(head_id(vault.path()), head_id(&bare));
}

#[test]
fn writes_and_commit_do_not_need_the_network() {
    let scratch = Scratch::new();
    let vault_path = scratch.dir("vault");
    drop(Vault::open(&vault_path, OpenOptions::new(), None).unwrap());
    let dir = vault_path.join("north_notes");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".collection.yaml"), NOTES).unwrap();
    let vault = Vault::open(&vault_path, OpenOptions::new(), None).unwrap();
    git_ok(
        vault.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/scheda.git",
        ],
    );
    drop(vault);
    let vault = Vault::open(&vault_path, OpenOptions::new(), None).unwrap();
    vault.begin().unwrap();
    vault
        .collection("north_notes")
        .unwrap()
        .add("local", rec("local"), None)
        .unwrap();
    vault.commit("local only").unwrap();
    vault.reindex().unwrap();
    assert!(vault.has_remote());
}

#[test]
fn push_without_remote_is_invalid() {
    let scratch = Scratch::new();
    let vault = Vault::open(scratch.dir("vault"), OpenOptions::new(), None).unwrap();
    let err = vault.push().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

fn current_branch(dir: &Path) -> String {
    git_stdout(dir, &["symbolic-ref", "--short", "HEAD"])
        .trim()
        .to_owned()
}

/// Publish an unrelated branch `aaa-other`, which sorts before every real
/// branch name, to `bare`. Returns the commit it points at.
fn publish_other_branch(scratch: &Scratch, bare: &Path) -> String {
    let work = scratch.dir("other-work");
    git_ok(
        scratch.dir("").as_path(),
        &["init", "--", &work.to_string_lossy()],
    );
    fs::write(work.join("unrelated.txt"), "unrelated").unwrap();
    commit_all(&work, "unrelated root");
    let tip = head_id(&work);
    git_ok(&work, &["push", &as_url(bare), "HEAD:refs/heads/aaa-other"]);
    tip
}

#[test]
fn pull_without_upstream_takes_only_the_current_branch() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    let b = scratch.dir("b");
    drop(Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap());
    let branch = current_branch(&a);
    // A Vault whose branch has no upstream: `init`, then `origin` added.
    git_ok(&a, &["branch", "--unset-upstream"]);
    let other = publish_other_branch(&scratch, &bare);
    let before = head_id(&a);

    let vault = Vault::open(&a, OpenOptions::new(), None).unwrap();
    // Up to date on the current branch; the other branch must not matter.
    vault.pull().unwrap();
    assert_eq!(head_id(&a), before);

    // The current branch moves on; the other branch still must not matter.
    let second = Vault::open(&b, OpenOptions::new().remote_repo(&remote), None).unwrap();
    insert_and_commit(&second, "from-b", "from b", "from b");
    second.push().unwrap();
    drop(second);
    vault.pull().unwrap();
    assert_eq!(
        head_id(&a),
        git_stdout(&bare, &["rev-parse", &branch]).trim()
    );
    assert_ne!(head_id(&a), other);
}

#[test]
fn pull_when_remote_lacks_the_branch_fails() {
    let scratch = Scratch::new();
    let (_source, bare) = pair_with_bare(&scratch);
    let remote = as_url(&bare);
    let a = scratch.dir("a");
    drop(Vault::open(&a, OpenOptions::new().remote_repo(&remote), None).unwrap());
    git_ok(&a, &["checkout", "-b", "feature"]);
    let vault = Vault::open(&a, OpenOptions::new(), None).unwrap();
    let err = vault.pull().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::GitNotFound));
}

#[test]
fn info_remote_is_origin_or_empty() {
    let scratch = Scratch::new();
    let vault = scratch.dir("vault");
    drop(Vault::open(&vault, OpenOptions::new(), None).unwrap());
    let remote = |dir: &Path| {
        Vault::open(dir, OpenOptions::new(), None)
            .unwrap()
            .info()
            .unwrap()
            .remote
    };
    assert_eq!(remote(&vault), None);
    git_ok(
        &vault,
        &["remote", "add", "backup", "file:///nowhere/backup"],
    );
    assert_eq!(remote(&vault), None);
    git_ok(
        &vault,
        &["remote", "add", "origin", "file:///nowhere/origin"],
    );
    assert_eq!(remote(&vault).as_deref(), Some("origin"));
}
