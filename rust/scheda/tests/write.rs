//! The write path through the public handle: making a Vault in one commit,
//! two `open`s of one new path, the write lock and its timeout, and many
//! writers at once, in threads and in processes.

mod common;
mod probe;

use std::fs::File;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::{CollectionDeclaration, OpenMode, OpenOptions, VaultErrorCode as Code, VaultSync};

use common::{Scratch, git, open, snapshot, vault, work_tree, write};
use probe::{assert_settled, commits, git_out};

fn standard(name: &str) -> CollectionDeclaration {
    serde_json::from_value(serde_json::json!({"name": name, "schema": {"standard": {}}})).unwrap()
}

fn opened(path: &Path) -> Arc<dyn VaultSync> {
    open(path, OpenOptions::default()).unwrap()
}

/// The marker is of generation 2 and `.gitignore` ignores Scheda's folders.
fn assert_marked(path: &Path) {
    assert_eq!(
        std::fs::read_to_string(path.join(".scheda.yaml")).unwrap(),
        "version: 2\n"
    );
    let ignore = std::fs::read_to_string(path.join(".gitignore")).unwrap();
    assert!(ignore.lines().any(|line| line == ".scheda/"), "{ignore}");
    assert!(ignore.lines().any(|line| line == ".isolated/"), "{ignore}");
    assert!(path.join(".scheda").is_dir());
}

#[test]
fn a_missing_path_becomes_a_vault_in_one_commit() {
    let scratch = Scratch::new("missing");
    let path = scratch.join("a/b/vault");
    let vault = opened(&path);
    assert_marked(&path);
    assert_eq!(commits(&path), 1);
    assert_eq!(
        git_out(&path, &["ls-tree", "-r", "--name-only", "HEAD"]),
        ".gitignore\n.scheda.yaml"
    );
    assert_settled(&path, &[]);
    assert_eq!(
        git_out(&path, &["config", "--local", "core.longpaths"]),
        "true"
    );
    drop(vault);
    // An open of the Vault made makes no commit.
    drop(opened(&path));
    assert_eq!(commits(&path), 1);
}

#[test]
fn an_empty_folder_becomes_a_vault_in_one_commit() {
    let scratch = Scratch::new("empty");
    let path = scratch.join("vault");
    std::fs::create_dir(&path).unwrap();
    let vault = open(
        &path,
        OpenOptions::default().with_mode(OpenMode::OPEN_MODE_CREATE_NEW),
    )
    .unwrap();
    assert_marked(&path);
    assert_eq!(commits(&path), 1);
    assert_settled(&path, &[]);
    vault.create_collection(standard("notes")).unwrap();
    assert_eq!(commits(&path), 2);
    assert_settled(&path, &[]);
}

#[test]
fn a_git_work_tree_without_the_marker_gets_it_in_one_commit_on_top_of_head() {
    let scratch = Scratch::new("tree");
    let path = scratch.join("tree");
    work_tree(&path, &[("readme.md", "hello\n"), (".gitignore", "target")]);
    write(&path, "draft.txt", "a person's file, not committed");
    let vault = opened(&path);
    assert_marked(&path);
    assert_eq!(commits(&path), 2);
    assert_eq!(
        std::fs::read_to_string(path.join(".gitignore")).unwrap(),
        "target\n.scheda/\n.isolated/\n"
    );
    assert_eq!(git_out(&path, &["show", "HEAD:readme.md"]), "hello");
    assert_settled(&path, &["draft.txt"]);
    assert_eq!(git_out(&path, &["status", "--porcelain"]), "?? draft.txt");
    drop(vault);
    drop(opened(&path));
    assert_eq!(commits(&path), 2, "a Vault opens without a commit");
}

#[test]
fn a_work_tree_with_a_gitignore_that_has_the_entries_gets_only_the_marker() {
    let scratch = Scratch::new("ignored");
    let path = scratch.join("tree");
    work_tree(&path, &[(".gitignore", ".scheda\n.isolated/\n")]);
    drop(opened(&path));
    assert_eq!(commits(&path), 2);
    assert_eq!(
        std::fs::read_to_string(path.join(".gitignore")).unwrap(),
        ".scheda\n.isolated/\n"
    );
    assert_eq!(
        git_out(&path, &["diff", "--name-only", "HEAD~1", "HEAD"]),
        ".scheda.yaml"
    );
}

#[test]
fn two_opens_of_one_new_path_at_once_make_one_vault_and_both_open_it() {
    for round in 0..6 {
        let scratch = Scratch::new("race");
        let path = scratch.join("vault");
        // Alternate between a missing path and an empty folder.
        if round % 2 == 1 {
            std::fs::create_dir(&path).unwrap();
        }
        let barrier = Barrier::new(3);
        let vaults: Vec<Arc<dyn VaultSync>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..3)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        open(&path, OpenOptions::default())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap().unwrap())
                .collect()
        });
        assert_eq!(vaults.len(), 3, "round {round}");
        assert_eq!(commits(&path), 1, "round {round}: one Vault, one commit");
        assert_marked(&path);
        assert_settled(&path, &[]);
        for vault in &vaults {
            assert!(
                vault
                    .list_collections(Empty::default())
                    .unwrap()
                    .collections
                    .is_empty()
            );
        }
    }
}

#[test]
fn a_call_that_cannot_take_the_lock_in_time_is_aborted_and_stores_nothing() {
    let scratch = Scratch::new("timeout");
    let path = scratch.join("vault");
    let vault = open(&path, OpenOptions::default().with_write_timeout_ms(150)).unwrap();
    let before = commits(&path);
    let held = File::options()
        .read(true)
        .write(true)
        .open(path.join(".scheda/commit.lock"))
        .unwrap();
    held.lock().unwrap();
    for _ in 0..2 {
        let start = Instant::now();
        let error = vault.create_collection(standard("notes")).unwrap_err();
        assert!(error.is(Code::Aborted));
        assert!(error.retryable());
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert!(start.elapsed() < Duration::from_secs(10));
    }
    assert_eq!(commits(&path), before);
    assert!(snapshot(&path.join("notes")).is_empty());
    held.unlock().unwrap();
    drop(held);
    // The timeout stored nothing then, and nothing later.
    let error = vault
        .get_collection(scheda::v1::CollectionKey::default().with_name("notes"))
        .unwrap_err();
    assert!(error.is(Code::NotFound));
    vault.create_collection(standard("notes")).unwrap();
    assert_eq!(commits(&path), before + 1);
    assert_settled(&path, &[]);
}

#[test]
fn a_vault_built_by_hand_takes_the_writes_of_two_handles() {
    let scratch = Scratch::new("handles");
    let path = scratch.join("vault");
    vault(&path);
    let first = opened(&path);
    let second = opened(&path);
    first.create_collection(standard("one")).unwrap();
    // The second handle reads again: HEAD moved under it.
    let error = second.create_collection(standard("one")).unwrap_err();
    assert!(error.is(Code::AlreadyExists));
    second.create_collection(standard("two")).unwrap();
    let listed = first.list_collections(Empty::default()).unwrap();
    assert_eq!(listed.collections.len(), 4, "notes, log, one, two");
    assert_eq!(commits(&path), 3);
    // The Vault built by hand has no `.gitignore`: `.scheda/` is untracked.
    assert_settled(&path, &[".scheda/"]);
}

#[test]
fn sixty_four_writes_from_one_handle_at_once_are_each_stored_whole() {
    let scratch = Scratch::new("threads");
    let path = scratch.join("vault");
    let vault = opened(&path);
    let before = commits(&path);
    std::thread::scope(|scope| {
        for n in 0..64 {
            let vault = Arc::clone(&vault);
            scope.spawn(move || {
                vault.create_collection(standard(&format!("c{n}"))).unwrap();
            });
        }
    });
    assert_eq!(commits(&path), before + 64);
    assert_settled(&path, &[]);
    let listed = vault.list_collections(Empty::default()).unwrap();
    assert_eq!(listed.collections.len(), 64);
    for n in 0..64 {
        assert!(path.join(format!("c{n}/.collection.yaml")).is_file());
    }
    // One commit each: no commit holds two declaration files.
    let files = git_out(&path, &["log", "--format=", "--name-only", "-n", "64"]);
    assert_eq!(files.lines().filter(|line| !line.is_empty()).count(), 64);
}

const CHILD_VAULT: &str = "SCHEDA_TEST_CHILD_VAULT";
const CHILD_INDEX: &str = "SCHEDA_TEST_CHILD_INDEX";
const PER_CHILD: usize = 22;

#[test]
fn sixty_six_writes_from_three_processes_are_each_stored_whole() {
    if let Ok(path) = std::env::var(CHILD_VAULT) {
        // The role of a child process: write into the Vault of the parent.
        let index = std::env::var(CHILD_INDEX).unwrap();
        let vault = open(Path::new(&path), OpenOptions::default()).unwrap();
        std::thread::scope(|scope| {
            for half in 0..2 {
                let vault = Arc::clone(&vault);
                let index = index.clone();
                scope.spawn(move || {
                    for n in (half * PER_CHILD / 2)..((half + 1) * PER_CHILD / 2) {
                        vault
                            .create_collection(standard(&format!("p{index}-{n}")))
                            .unwrap();
                    }
                });
            }
        });
        return;
    }
    let scratch = Scratch::new("processes");
    let path = scratch.join("vault");
    let vault = opened(&path);
    let before = commits(&path);
    let exe = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for index in 0..3 {
        children.push(
            Command::new(&exe)
                .args([
                    "--exact",
                    "sixty_six_writes_from_three_processes_are_each_stored_whole",
                    "--test-threads=1",
                ])
                .env(CHILD_VAULT, &path)
                .env(CHILD_INDEX, index.to_string())
                .spawn()
                .unwrap(),
        );
    }
    // Wait for every child before anything is asserted.
    let statuses: Vec<_> = children
        .iter_mut()
        .map(|child| child.wait().unwrap())
        .collect();
    assert!(
        statuses.iter().all(|status| status.success()),
        "{statuses:?}"
    );
    assert_eq!(commits(&path), before + 3 * PER_CHILD);
    assert_settled(&path, &[]);
    let listed = vault.list_collections(Empty::default()).unwrap();
    assert_eq!(listed.collections.len(), 3 * PER_CHILD);
    git(&path, &["fsck", "--no-progress", "--strict"]);
}
