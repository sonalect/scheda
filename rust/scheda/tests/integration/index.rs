//! The indexer through the public handle: a write is indexed, a journal line
//! is found with and without its map, two handles of two processes take
//! turns, a commit made by hand and a pull are noticed, a rebuild runs and
//! shows itself, a layout that is not this one is made again, a round that
//! fails changes nothing, and the thread ends with the handle.
//!
//! Most tests see the indexer through its files and wait on
//! `.scheda/index/commit`; one reads the Index object for the failure of a
//! round.

use std::fs::File;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionKey, DocumentCreation, DocumentId,
    DynCollectionAsync, DynMirrorAsync, DynVaultAsync, MirrorAsync, MirrorSetting, OpenOptions,
    VaultAsync, VaultErrorCode as Code,
};

use crate::common::{Scratch, both, git, open, snapshot, vault as built_vault, write};
use crate::probe::{assert_settled, commits, git_out};

const DEADLINE: Duration = Duration::from_secs(30);

fn journal(name: &str) -> CollectionDeclaration {
    serde_json::from_value(serde_json::json!({"name": name, "schema": {"journal": {}}})).unwrap()
}

fn standard(name: &str) -> CollectionDeclaration {
    serde_json::from_value(serde_json::json!({"name": name, "schema": {"standard": {}}})).unwrap()
}

/// The collection `name` of `vault`, which must be declared.
async fn collection(vault: &DynVaultAsync, name: &str) -> DynCollectionAsync {
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .await
        .unwrap()
        .to_async_inline()
        .unwrap()
}

/// The mirror of `vault`.
async fn mirror_of(vault: &DynVaultAsync) -> DynMirrorAsync {
    vault
        .mirror(Empty::default())
        .await
        .unwrap()
        .to_async_inline()
        .unwrap()
}

async fn opened(path: &Path) -> DynVaultAsync {
    open(path, OpenOptions::default().with_index_poll_ms(10))
        .await
        .unwrap()
}

/// A Vault with the journal `log`, opened, with the Index built.
async fn built(scratch: &Scratch, name: &str) -> (PathBuf, DynVaultAsync) {
    let path = scratch.join(name);
    let vault = opened(&path).await;
    vault.create_collection(journal("log")).await.unwrap();
    wait_indexed(&path).await;
    (path, vault)
}

/// Appends the line `id` to the journal `log`.
async fn line(vault: &DynVaultAsync, id: &str) {
    collection(vault, "log")
        .await
        .create(DocumentCreation {
            id: Some(id.to_owned()),
            raw: Some(format!("{{\"id\":\"{id}\"}}").into_bytes()),
            note: Some(format!("append {id}")),
            ..Default::default()
        })
        .await
        .unwrap();
}

async fn found(vault: &DynVaultAsync, id: &str) -> bool {
    let key = DocumentId {
        id: Some(id.to_owned()),
        ..Default::default()
    };
    match collection(vault, "log").await.get(key).await {
        Ok(document) => {
            assert_eq!(
                document.raw,
                Some(format!("{{\"id\":\"{id}\"}}").into_bytes())
            );
            true
        }
        Err(error) => {
            assert!(error.is(Code::NotFound), "{error}");
            false
        }
    }
}

/// Waits until `done` is true, or fails the test.
async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The line `commit` should hold for the Vault at `path` as `HEAD` is now.
fn expected_commit(path: &Path) -> String {
    let main = git_out(path, &["rev-parse", "HEAD"]);
    if path.join(".isolated/.git").exists() {
        let nested = git_out(&path.join(".isolated"), &["rev-parse", "HEAD"]);
        format!("main {main} isolated {nested}")
    } else {
        format!("main {main}")
    }
}

fn commit_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path.join(".scheda/index/commit"))
        .ok()
        .map(|text| text.trim().to_owned())
}

fn is_indexed(path: &Path) -> bool {
    commit_file(path).is_some_and(|commit| commit == expected_commit(path))
}

/// Waits until the Index reflects `HEAD`.
async fn wait_indexed(path: &Path) {
    wait_until("the Index to reach HEAD", || is_indexed(path)).await;
}

fn map_file(path: &Path) -> PathBuf {
    path.join(".scheda/index/journal/log/ids")
}

/// The ids the map of `log` lists, sorted.
fn mapped(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(map_file(path)).unwrap();
    // Header, state, count, the segments, then `id<TAB>n` lines.
    let count: usize = text.lines().nth(2).unwrap().parse().unwrap();
    text.lines()
        .skip(3 + count)
        .map(|line| line.split('\t').next().unwrap().to_owned())
        .collect()
}

/// Holds `index.lock` as another process's indexer would.
async fn hold_lock(path: &Path) -> File {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".scheda/index.lock"))
        .unwrap();
    // Waited for off the runtime: a round holds the lock across its
    // awaits, and needs the runtime to end.
    tokio::task::spawn_blocking(move || {
        file.lock().unwrap();
        file
    })
    .await
    .unwrap()
}

#[test]
fn a_write_is_indexed_and_the_map_lists_its_line() {
    both(|| async {
        let scratch = Scratch::new("indexed");
        let (path, vault) = built(&scratch, "vault").await;
        assert_eq!(mapped(&path), Vec::<String>::new());
        line(&vault, "one").await;
        line(&vault, "two").await;
        let commits_before = commits(&path);
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["one", "two"]);
        assert_eq!(
            std::fs::read_to_string(path.join(".scheda/index/format")).unwrap(),
            "3\n"
        );
        assert_eq!(commits(&path), commits_before, "indexing commits nothing");
        assert_settled(&path, &[]);
    });
}

#[test]
fn the_index_object_needs_the_feature_and_the_maps_are_kept_either_way() {
    both(|| async {
        let scratch = Scratch::new("no-index-object");
        let (path, vault) = built(&scratch, "vault").await;
        let object = vault.index(Empty::default()).await;
        if cfg!(feature = "index") {
            assert!(object.is_ok());
        } else {
            assert!(object.err().unwrap().is(Code::Unimplemented));
        }
        line(&vault, "one").await;
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["one"]);
    });
}

#[test]
fn a_line_is_found_before_the_indexer_ran_after_it_and_without_its_map() {
    both(|| async {
        let scratch = Scratch::new("lookup");
        let (path, vault) = built(&scratch, "vault").await;
        line(&vault, "one").await;
        wait_indexed(&path).await;
        // Another indexer holds the lock: this handle's indexer cannot run.
        let held = hold_lock(&path).await;
        line(&vault, "two").await;
        assert!(found(&vault, "two").await, "before the indexer ran");
        assert!(found(&vault, "one").await, "through the map");
        assert!(!found(&vault, "three").await);
        assert_eq!(
            mapped(&path),
            ["one"],
            "the map is as of the Index's commit"
        );
        held.unlock().unwrap();
        drop(held);
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["one", "two"]);
        assert!(
            found(&vault, "two").await,
            "after the indexer ran, through the map"
        );
        std::fs::remove_file(map_file(&path)).unwrap();
        assert!(found(&vault, "one").await, "without the map");
        assert!(found(&vault, "two").await, "without the map");
        assert!(!found(&vault, "three").await);
    });
}

#[test]
fn a_journal_in_the_isolated_repository_is_indexed_and_found_too() {
    both(|| async {
        let scratch = Scratch::new("isolated");
        let path = scratch.join("vault");
        let vault = opened(&path).await;
        let collection: CollectionDeclaration = serde_json::from_value(serde_json::json!({
            "name": "log", "schema": {"journal": {}, "isolated": true}
        }))
        .unwrap();
        vault.create_collection(collection).await.unwrap();
        line(&vault, "one").await;
        wait_indexed(&path).await;
        let commit = commit_file(&path).unwrap();
        assert!(commit.contains("isolated"), "{commit}");
        assert_eq!(mapped(&path), ["one"]);
        let held = hold_lock(&path).await;
        line(&vault, "two").await;
        assert!(
            found(&vault, "two").await,
            "from the nested repository's changes"
        );
        drop(held);
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["one", "two"]);
    });
}

#[test]
fn a_commit_made_by_hand_is_indexed() {
    both(|| async {
        let scratch = Scratch::new("by-hand");
        let path = scratch.join("vault");
        // A Vault built with plain git, with a journal `log`.
        built_vault(&path);
        write(
            &path,
            "log/00000001-hand.jsonl",
            "first: {\"id\":\"first\"}\n",
        );
        git(&path, &["add", "log"]);
        git(&path, &["commit", "--quiet", "--message", "a line"]);
        let files = || {
            snapshot(&path)
                .into_iter()
                .filter(|(rel, _)| !rel.starts_with(".scheda") && !rel.starts_with(".git"))
                .collect::<Vec<_>>()
        };
        let before = files();
        let vault = opened(&path).await;
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["first"]);
        // A segment of another writer, committed with plain git.
        write(
            &path,
            "log/00000002-hand.jsonl",
            "manual: {\"id\":\"manual\"}\n",
        );
        git(&path, &["add", "log"]);
        git(&path, &["commit", "--quiet", "--message", "by hand"]);
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["first", "manual"]);
        assert!(!files().is_empty() && files().len() > before.len());
        drop(vault);
    });
}

#[test]
fn a_pull_is_indexed() {
    both(|| async {
        let scratch = Scratch::new("pull");
        let mirror = scratch.join("m.git");
        std::fs::create_dir_all(&mirror).unwrap();
        git(
            &mirror,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        let url = format!("file://{}", mirror.display());
        let seed = opened(&scratch.join("seed")).await;
        seed.create_collection(journal("log")).await.unwrap();
        mirror_of(&seed)
            .await
            .set(MirrorSetting::default().with_url(&url))
            .await
            .unwrap();
        mirror_of(&seed).await.push(Empty::default()).await.unwrap();
        let path = scratch.join("clone");
        let clone = open(
            &path,
            OpenOptions::default()
                .with_mirror(&url)
                .with_index_poll_ms(10),
        )
        .await
        .unwrap();
        wait_indexed(&path).await;
        line(&seed, "remote").await;
        mirror_of(&seed).await.push(Empty::default()).await.unwrap();
        mirror_of(&clone)
            .await
            .pull(Empty::default())
            .await
            .unwrap();
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["remote"]);
        assert!(found(&clone, "remote").await);
    });
}

#[test]
fn an_index_of_another_format_or_without_a_commit_is_rebuilt() {
    both(|| async {
        let scratch = Scratch::new("format");
        let (path, vault) = built(&scratch, "vault").await;
        line(&vault, "one").await;
        wait_indexed(&path).await;
        vault.close(Empty::default()).await.unwrap();
        let index = path.join(".scheda/index");
        for damage in ["another", "missing", "commit"] {
            match damage {
                "another" => std::fs::write(index.join("format"), "999\n").unwrap(),
                "missing" => std::fs::remove_file(index.join("format")).unwrap(),
                _ => std::fs::remove_file(index.join("commit")).unwrap(),
            }
            // A folder of an earlier layout goes with an Index of another
            // format; with the format right, only the maps are made again.
            let reset = damage != "commit";
            if reset {
                std::fs::create_dir_all(index.join("lexical-of-the-past")).unwrap();
                std::fs::write(index.join("lexical-of-the-past/segment"), "x").unwrap();
            }
            std::fs::remove_file(map_file(&path)).unwrap();
            let reopened = opened(&path).await;
            // The commit is looked at last: the rebuild removes it before the
            // rest, so one seen after the rest is gone is the rebuild's own.
            wait_until(damage, || {
                !index.join("lexical-of-the-past").exists()
                    && map_file(&path).exists()
                    && is_indexed(&path)
            })
            .await;
            assert_eq!(
                std::fs::read_to_string(index.join("format")).unwrap(),
                "3\n"
            );
            assert_eq!(mapped(&path), ["one"], "{damage}");
            reopened.close(Empty::default()).await.unwrap();
        }
    });
}

#[test]
fn a_round_that_fails_changes_no_commit_fails_no_write_and_the_next_one_succeeds() {
    both(|| async {
        let scratch = Scratch::new("failing");
        let (path, vault) = built(&scratch, "vault").await;
        let folder = path.join(".scheda/index/journal");
        let before = commit_file(&path).unwrap();
        // A file where the folder of the maps belongs: the round cannot write.
        std::fs::remove_dir_all(&folder).unwrap();
        std::fs::write(&folder, "in the way").unwrap();
        line(&vault, "one").await;
        assert!(found(&vault, "one").await, "the write is stored and read");
        // The round has failed once the Index says so; only then does an
        // unmoved commit show that a failed round changes none.
        #[cfg(feature = "index")]
        {
            use scheda::v1::IndexAsync;

            let index = vault
                .index(Empty::default())
                .await
                .unwrap()
                .to_async_inline()
                .unwrap();
            let deadline = Instant::now() + DEADLINE;
            while !index
                .status(Empty::default())
                .await
                .unwrap()
                .last_error
                .is_set()
            {
                assert!(Instant::now() < deadline, "the round did not fail");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        // A build without the feature has no Index to ask: nothing shows
        // the failed round, so a margin stands for it. A slow runner makes
        // the check below weaker (the round may not have run yet), never
        // pass for a wrong reason.
        #[cfg(not(feature = "index"))]
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(commit_file(&path).unwrap(), before, "no commit moved");
        line(&vault, "two").await;
        assert!(found(&vault, "two").await, "writes go on");
        assert!(
            found(&vault, "one").await,
            "a journal with no map is scanned"
        );
        std::fs::remove_file(&folder).unwrap();
        wait_indexed(&path).await;
        assert_eq!(mapped(&path), ["one", "two"]);
    });
}

const CHILD_VAULT: &str = "SCHEDA_TEST_INDEX_VAULT";
const CHILD_NAME: &str = "SCHEDA_TEST_INDEX_NAME";
const PER_HANDLE: usize = 12;

/// The role of a child process: write lines into the Vault of the parent,
/// say so, wait for the go, and end at the state the parent ends at.
async fn child(path: &Path, name: &str) {
    let vault = opened(path).await;
    for n in 0..PER_HANDLE {
        line(&vault, &format!("{name}-{n}")).await;
    }
    let signals = path.join(".scheda");
    std::fs::write(signals.join(format!("ready-{name}")), "").unwrap();
    wait_until("the go", || signals.join("go").exists()).await;
    wait_indexed(path).await;
    vault.close(Empty::default()).await.unwrap();
}

#[test]
fn handles_of_three_processes_take_turns_and_end_at_one_version() {
    if let (Ok(path), Ok(name)) = (std::env::var(CHILD_VAULT), std::env::var(CHILD_NAME)) {
        // The child runs the body once: a second run would write the same
        // lines again.
        in_child(child(Path::new(&path), &name));
        return;
    }
    both(|| async {
        let scratch = Scratch::new("processes");
        let (path, vault) = built(&scratch, "vault").await;
        let exe = std::env::current_exe().unwrap();
        let mut children: Vec<_> = ["x", "y"]
            .into_iter()
            .map(|name| {
                Command::new(&exe)
                    .args([
                        "--exact",
                        "index::handles_of_three_processes_take_turns_and_end_at_one_version",
                        "--test-threads=1",
                    ])
                    .env(CHILD_VAULT, &path)
                    .env(CHILD_NAME, name)
                    .spawn()
                    .unwrap()
            })
            .collect();
        for n in 0..PER_HANDLE {
            line(&vault, &format!("p-{n}")).await;
        }
        wait_until("the children to write", || {
            ["x", "y"]
                .iter()
                .all(|name| path.join(format!(".scheda/ready-{name}")).exists())
        })
        .await;
        wait_indexed(&path).await;
        std::fs::write(path.join(".scheda/go"), "").unwrap();
        let mut statuses = Vec::new();
        for child in &mut children {
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            statuses.push(status);
        }
        assert!(
            statuses.iter().all(|status| status.success()),
            "{statuses:?}"
        );
        wait_indexed(&path).await;
        let ids = mapped(&path);
        assert_eq!(ids.len(), 3 * PER_HANDLE, "{ids:?}");
        git(&path, &["fsck", "--no-progress", "--strict"]);
    });
}

const TASKS_CHILD: &str = "SCHEDA_TEST_INDEX_TASKS";

/// How many tasks are alive on the runtime of the caller.
fn alive_tasks() -> usize {
    tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks()
}

/// Runs `future` to its end on a runtime of its own: the role of a child
/// process, which runs the body once.
fn in_child(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

/// The role of a child process alone on its runtime, so the task count is
/// its own: each open Vault adds the task of its indexer, and `close` and a
/// drop end it.
async fn count_indexer_tasks(path: &Path) {
    let base = alive_tasks();
    let one = opened(&path.join("one")).await;
    wait_until("open to start the task", || alive_tasks() == base + 1).await;
    let two = opened(&path.join("two")).await;
    wait_until("a task for each handle", || alive_tasks() == base + 2).await;
    // A task that has ended is counted until the runtime has seen it end, so
    // the count is waited for, not read once.
    one.close(Empty::default()).await.unwrap();
    wait_until("close to end its task", || alive_tasks() == base + 1).await;
    drop(two);
    wait_until("a drop to end its task", || alive_tasks() == base).await;
    // Closed while a round is at work.
    let three = opened(&path.join("three")).await;
    three.create_collection(journal("log")).await.unwrap();
    three.close(Empty::default()).await.unwrap();
    wait_until("close to end the task of a round at work", || {
        alive_tasks() == base
    })
    .await;
}

#[test]
fn close_and_drop_end_the_indexer_task() {
    if let Ok(path) = std::env::var(TASKS_CHILD) {
        in_child(count_indexer_tasks(Path::new(&path)));
        return;
    }
    both(|| async {
        let scratch = Scratch::new("threads");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "index::close_and_drop_end_the_indexer_task",
                "--test-threads=1",
            ])
            .env(TASKS_CHILD, scratch.join("vaults"))
            .status()
            .unwrap();
        assert!(status.success());
    });
}

#[test]
fn a_standard_collection_needs_no_map() {
    both(|| async {
        let scratch = Scratch::new("standard");
        let (path, vault) = built(&scratch, "vault").await;
        vault.create_collection(standard("notes")).await.unwrap();
        line(&vault, "one").await;
        wait_indexed(&path).await;
        assert!(!path.join(".scheda/index/journal/notes").exists());
        assert!(map_file(&path).exists());
    });
}
