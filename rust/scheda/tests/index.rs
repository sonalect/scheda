//! The indexer through the public handle: a write is indexed, a journal line
//! is found with and without its map, two handles of two processes take
//! turns, a commit made by hand and a pull are noticed, a rebuild runs and
//! shows itself, a layout that is not this one is made again, a round that
//! fails changes nothing, and the thread ends with the handle.
//!
//! What a build without `find` can see of the Index is its files, so the
//! tests wait on `.scheda/index/commit`; with `find` they also watch the
//! status.

mod common;
mod probe;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::{
    Collection, Document, DocumentCreation, DocumentKey, MirrorSetting, OpenOptions,
    VaultErrorCode as Code,
};
use scheda::{Ports, Vault};

use common::{Scratch, git, snapshot, vault as built_vault, write};
use probe::{commits, git_out};

const DEADLINE: Duration = Duration::from_secs(30);

fn journal(name: &str) -> Collection {
    serde_json::from_value(serde_json::json!({"name": name, "schema": {"journal": {}}})).unwrap()
}

fn standard(name: &str) -> Collection {
    serde_json::from_value(serde_json::json!({"name": name, "schema": {"standard": {}}})).unwrap()
}

fn open(path: &Path) -> Vault {
    Vault::open(
        path,
        OpenOptions::default().with_index_poll_ms(10),
        Ports::default(),
    )
    .unwrap()
}

/// A Vault with the journal `log`, opened, with the Index built.
fn built(scratch: &Scratch, name: &str) -> (PathBuf, Vault) {
    let path = scratch.join(name);
    let vault = open(&path);
    vault.create_collection(journal("log")).unwrap();
    wait_indexed(&path);
    (path, vault)
}

/// Appends the line `id` to the journal `log`.
fn line(vault: &Vault, id: &str) {
    vault
        .create_document(DocumentCreation {
            document: Document {
                collection: Some("log".to_owned()),
                id: Some(id.to_owned()),
                raw: Some(format!("{{\"id\":\"{id}\"}}").into_bytes()),
                ..Default::default()
            }
            .into(),
            note: Some(format!("append {id}")),
            ..Default::default()
        })
        .unwrap();
}

fn key(id: &str) -> DocumentKey {
    DocumentKey {
        collection: Some("log".to_owned()),
        id: Some(id.to_owned()),
        ..Default::default()
    }
}

fn found(vault: &Vault, id: &str) -> bool {
    match vault.get_document(key(id)) {
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
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
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
fn wait_indexed(path: &Path) {
    wait_until("the Index to reach HEAD", || is_indexed(path));
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
fn hold_lock(path: &Path) -> File {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".scheda/index.lock"))
        .unwrap();
    file.lock().unwrap();
    file
}

#[test]
fn a_write_is_indexed_and_the_map_lists_its_line() {
    let scratch = Scratch::new("indexed");
    let (path, vault) = built(&scratch, "vault");
    assert_eq!(mapped(&path), Vec::<String>::new());
    line(&vault, "one");
    line(&vault, "two");
    let commits_before = commits(&path);
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["one", "two"]);
    assert_eq!(
        std::fs::read_to_string(path.join(".scheda/index/format")).unwrap(),
        "2\n"
    );
    assert_eq!(commits(&path), commits_before, "indexing commits nothing");
    probe::assert_settled(&path, &[]);
}

#[cfg(feature = "find")]
#[test]
fn the_status_gives_the_state_at_once_and_then_the_version_of_each_write() {
    let scratch = Scratch::new("status");
    let (path, vault) = built(&scratch, "vault");
    let mut watch = vault.watch_index_status(Empty::default()).unwrap();
    let first = watch.next().unwrap().unwrap();
    let head = git_out(&path, &["rev-parse", "HEAD"]);
    assert_eq!(first.version.as_deref(), Some(head.as_str()));
    assert_eq!(first.pending, Some(0));
    assert!(!first.last_error.is_set());
    line(&vault, "one");
    let head = git_out(&path, &["rev-parse", "HEAD"]);
    let mut items = 0;
    loop {
        let state = watch.next().unwrap().unwrap();
        items += 1;
        if state.version.as_deref() == Some(head.as_str()) && state.pending == Some(0) {
            break;
        }
    }
    assert!(items >= 1);
    let now = vault.get_index_status(Empty::default()).unwrap();
    assert_eq!(now.version.as_deref(), Some(head.as_str()));
    assert_eq!(now.role, Some(false));
    vault.close().unwrap();
    assert!(watch.next().is_none(), "a closed handle ends its stream");
}

#[cfg(not(feature = "find"))]
#[test]
fn without_find_the_status_calls_are_unimplemented_and_reindex_rebuilds_the_maps() {
    let scratch = Scratch::new("no-find");
    let (path, vault) = built(&scratch, "vault");
    let error = vault.get_index_status(Empty::default()).unwrap_err();
    assert!(error.is(Code::Unimplemented));
    assert!(error.to_string().contains("find"), "{error}");
    let error = vault.watch_index_status(Empty::default()).err().unwrap();
    assert!(error.is(Code::Unimplemented));
    assert!(error.to_string().contains("find"), "{error}");
    line(&vault, "one");
    wait_indexed(&path);
    std::fs::write(map_file(&path), "damaged").unwrap();
    std::fs::write(path.join(".scheda/index/junk"), "x").unwrap();
    vault.reindex(Empty::default()).unwrap();
    wait_until("the rebuild", || !path.join(".scheda/index/junk").exists());
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["one"]);
}

#[test]
fn a_line_is_found_before_the_indexer_ran_after_it_and_without_its_map() {
    let scratch = Scratch::new("lookup");
    let (path, vault) = built(&scratch, "vault");
    line(&vault, "one");
    wait_indexed(&path);
    // Another indexer holds the lock: this handle's indexer cannot run.
    let held = hold_lock(&path);
    line(&vault, "two");
    assert!(found(&vault, "two"), "before the indexer ran");
    assert!(found(&vault, "one"), "through the map");
    assert!(!found(&vault, "three"));
    assert_eq!(
        mapped(&path),
        ["one"],
        "the map is as of the Index's commit"
    );
    held.unlock().unwrap();
    drop(held);
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["one", "two"]);
    assert!(
        found(&vault, "two"),
        "after the indexer ran, through the map"
    );
    std::fs::remove_file(map_file(&path)).unwrap();
    assert!(found(&vault, "one"), "without the map");
    assert!(found(&vault, "two"), "without the map");
    assert!(!found(&vault, "three"));
}

#[test]
fn a_journal_in_the_isolated_repository_is_indexed_and_found_too() {
    let scratch = Scratch::new("isolated");
    let path = scratch.join("vault");
    let vault = open(&path);
    let collection: Collection = serde_json::from_value(serde_json::json!({
        "name": "log", "schema": {"journal": {}, "isolated": true}
    }))
    .unwrap();
    vault.create_collection(collection).unwrap();
    line(&vault, "one");
    wait_indexed(&path);
    let commit = commit_file(&path).unwrap();
    assert!(commit.contains("isolated"), "{commit}");
    assert_eq!(mapped(&path), ["one"]);
    let held = hold_lock(&path);
    line(&vault, "two");
    assert!(found(&vault, "two"), "from the nested repository's changes");
    drop(held);
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["one", "two"]);
}

#[test]
fn a_commit_made_by_hand_is_indexed() {
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
    let vault = open(&path);
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["first"]);
    // A segment of another writer, committed with plain git.
    write(
        &path,
        "log/00000002-hand.jsonl",
        "manual: {\"id\":\"manual\"}\n",
    );
    git(&path, &["add", "log"]);
    git(&path, &["commit", "--quiet", "--message", "by hand"]);
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["first", "manual"]);
    assert!(!files().is_empty() && files().len() > before.len());
    drop(vault);
}

#[test]
fn a_pull_is_indexed() {
    let scratch = Scratch::new("pull");
    let mirror = scratch.join("m.git");
    std::fs::create_dir_all(&mirror).unwrap();
    git(
        &mirror,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let url = format!("file://{}", mirror.display());
    let seed = open(&scratch.join("seed"));
    seed.create_collection(journal("log")).unwrap();
    seed.set_mirror(MirrorSetting::default().with_url(&url))
        .unwrap();
    seed.push(Empty::default()).unwrap();
    let path = scratch.join("clone");
    let clone = Vault::open(
        &path,
        OpenOptions::default()
            .with_mirror(&url)
            .with_index_poll_ms(10),
        Ports::default(),
    )
    .unwrap();
    wait_indexed(&path);
    line(&seed, "remote");
    seed.push(Empty::default()).unwrap();
    clone.pull(Empty::default()).unwrap();
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["remote"]);
    assert!(found(&clone, "remote"));
}

#[test]
fn reindex_returns_before_the_rebuild_a_second_is_aborted_and_the_maps_match() {
    let scratch = Scratch::new("reindex");
    let (path, vault) = built(&scratch, "vault");
    line(&vault, "one");
    line(&vault, "two");
    wait_indexed(&path);
    #[cfg(feature = "find")]
    let mut watch = vault.watch_index_status(Empty::default()).unwrap();
    #[cfg(feature = "find")]
    watch.next().unwrap().unwrap();
    // The rebuild takes the same lock as any round: while it is held the
    // rebuild is asked for and cannot begin.
    let held = hold_lock(&path);
    std::fs::write(path.join(".scheda/index/junk"), "x").unwrap();
    std::fs::write(map_file(&path), "damaged").unwrap();
    vault.reindex(Empty::default()).unwrap();
    let error = vault.reindex(Empty::default()).unwrap_err();
    assert!(error.is(Code::Aborted));
    assert!(path.join(".scheda/index/junk").exists(), "not begun");
    drop(held);
    wait_until("the rebuild", || !path.join(".scheda/index/junk").exists());
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["one", "two"]);
    #[cfg(feature = "find")]
    {
        use scheda::v1::rebuild::Stage;
        let mut saw_running = false;
        let last = loop {
            let state = watch.next().unwrap().unwrap();
            let stage = state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known());
            saw_running |= stage == Some(Stage::STAGE_RUNNING) && state.version.is_none();
            if stage == Some(Stage::STAGE_DONE) {
                break state;
            }
        };
        assert!(saw_running, "the rebuild shows from its start");
        assert!(last.version.is_some());
        let rebuild = last.rebuild.as_option().unwrap();
        assert_eq!(rebuild.documents_read, Some(2));
        assert_eq!(rebuild.documents_skipped, Some(0));
        assert_eq!(rebuild.segments_read, Some(1));
    }
    // After it ends another can be asked for.
    vault.reindex(Empty::default()).unwrap();
}

#[test]
fn an_index_of_another_format_or_without_a_commit_is_rebuilt() {
    let scratch = Scratch::new("format");
    let (path, vault) = built(&scratch, "vault");
    line(&vault, "one");
    wait_indexed(&path);
    vault.close().unwrap();
    let index = path.join(".scheda/index");
    for damage in ["another", "missing", "commit"] {
        match damage {
            "another" => std::fs::write(index.join("format"), "999\n").unwrap(),
            "missing" => std::fs::remove_file(index.join("format")).unwrap(),
            _ => std::fs::remove_file(index.join("commit")).unwrap(),
        }
        std::fs::create_dir_all(index.join("lexical-of-the-past")).unwrap();
        std::fs::write(index.join("lexical-of-the-past/segment"), "x").unwrap();
        std::fs::remove_file(map_file(&path)).unwrap();
        let reopened = open(&path);
        wait_until(damage, || {
            is_indexed(&path)
                && !index.join("lexical-of-the-past").exists()
                && map_file(&path).exists()
        });
        assert_eq!(
            std::fs::read_to_string(index.join("format")).unwrap(),
            "2\n"
        );
        assert_eq!(mapped(&path), ["one"], "{damage}");
        reopened.close().unwrap();
    }
}

#[test]
fn a_round_that_fails_changes_no_commit_fails_no_write_and_the_next_one_succeeds() {
    let scratch = Scratch::new("failing");
    let (path, vault) = built(&scratch, "vault");
    let folder = path.join(".scheda/index/journal");
    let before = commit_file(&path).unwrap();
    // A file where the folder of the maps belongs: the round cannot write.
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::write(&folder, "in the way").unwrap();
    line(&vault, "one");
    assert!(found(&vault, "one"), "the write is stored and read");
    #[cfg(feature = "find")]
    wait_until("the failure", || {
        vault
            .get_index_status(Empty::default())
            .unwrap()
            .last_error
            .is_set()
    });
    #[cfg(not(feature = "find"))]
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(commit_file(&path).unwrap(), before, "no commit moved");
    #[cfg(feature = "find")]
    {
        let state = vault.get_index_status(Empty::default()).unwrap();
        let error = state.last_error.as_option().unwrap();
        assert_eq!(error.code.as_deref(), Some("VAULT_ERROR_CODE_STORAGE"));
        assert_ne!(error.message.as_deref(), Some(""));
        assert_eq!(state.role, Some(false));
    }
    line(&vault, "two");
    assert!(found(&vault, "two"), "writes go on");
    assert!(found(&vault, "one"), "a journal with no map is scanned");
    std::fs::remove_file(&folder).unwrap();
    wait_indexed(&path);
    assert_eq!(mapped(&path), ["one", "two"]);
    #[cfg(feature = "find")]
    wait_until("the error to clear", || {
        !vault
            .get_index_status(Empty::default())
            .unwrap()
            .last_error
            .is_set()
    });
}

const CHILD_VAULT: &str = "SCHEDA_TEST_INDEX_VAULT";
const CHILD_NAME: &str = "SCHEDA_TEST_INDEX_NAME";
const PER_HANDLE: usize = 12;

/// The role of a child process: write lines into the Vault of the parent,
/// say so, wait for the go, and end at the state the parent ends at.
fn child(path: &Path, name: &str) {
    let vault = open(path);
    for n in 0..PER_HANDLE {
        line(&vault, &format!("{name}-{n}"));
    }
    let signals = path.join(".scheda");
    std::fs::write(signals.join(format!("ready-{name}")), "").unwrap();
    wait_until("the go", || signals.join("go").exists());
    wait_indexed(path);
    #[cfg(feature = "find")]
    {
        let head = git_out(path, &["rev-parse", "HEAD"]);
        wait_until("the status", || {
            let state = vault.get_index_status(Empty::default()).unwrap();
            state.version.as_deref() == Some(head.as_str()) && state.pending == Some(0)
        });
        assert!(
            !vault
                .get_index_status(Empty::default())
                .unwrap()
                .last_error
                .is_set()
        );
    }
    vault.close().unwrap();
}

#[test]
fn handles_of_three_processes_take_turns_and_end_at_one_version() {
    if let (Ok(path), Ok(name)) = (std::env::var(CHILD_VAULT), std::env::var(CHILD_NAME)) {
        child(Path::new(&path), &name);
        return;
    }
    let scratch = Scratch::new("processes");
    let (path, vault) = built(&scratch, "vault");
    let exe = std::env::current_exe().unwrap();
    let mut children: Vec<_> = ["x", "y"]
        .into_iter()
        .map(|name| {
            Command::new(&exe)
                .args([
                    "--exact",
                    "handles_of_three_processes_take_turns_and_end_at_one_version",
                    "--test-threads=1",
                ])
                .env(CHILD_VAULT, &path)
                .env(CHILD_NAME, name)
                .spawn()
                .unwrap()
        })
        .collect();
    for n in 0..PER_HANDLE {
        line(&vault, &format!("p-{n}"));
    }
    wait_until("the children to write", || {
        ["x", "y"]
            .iter()
            .all(|name| path.join(format!(".scheda/ready-{name}")).exists())
    });
    wait_indexed(&path);
    std::fs::write(path.join(".scheda/go"), "").unwrap();
    let statuses: Vec<_> = children
        .iter_mut()
        .map(|child| child.wait().unwrap())
        .collect();
    assert!(
        statuses.iter().all(|status| status.success()),
        "{statuses:?}"
    );
    wait_indexed(&path);
    #[cfg(feature = "find")]
    {
        let head = git_out(&path, &["rev-parse", "HEAD"]);
        let state = vault.get_index_status(Empty::default()).unwrap();
        assert_eq!(state.version.as_deref(), Some(head.as_str()));
        assert!(!state.last_error.is_set());
    }
    let ids = mapped(&path);
    assert_eq!(ids.len(), 3 * PER_HANDLE, "{ids:?}");
    git(&path, &["fsck", "--no-progress", "--strict"]);
}

const THREADS_CHILD: &str = "SCHEDA_TEST_INDEX_THREADS";

/// How many threads of this process carry the indexer's name.
fn indexer_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(Result::ok)
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.trim() == "scheda-indexer")
        })
        .count()
}

#[cfg(target_os = "linux")]
#[test]
fn close_and_drop_end_the_indexer_thread() {
    if let Ok(path) = std::env::var(THREADS_CHILD) {
        // Alone in its process, so the count is its own.
        let path = Path::new(&path);
        assert_eq!(indexer_threads(), 0);
        let one = open(&path.join("one"));
        // The thread names itself as it starts.
        wait_until("open to start the thread", || indexer_threads() == 1);
        let two = open(&path.join("two"));
        wait_until("a thread for each handle", || indexer_threads() == 2);
        one.close().unwrap();
        assert_eq!(indexer_threads(), 1, "close ends it and waits for it");
        drop(two);
        assert_eq!(indexer_threads(), 0, "so does a drop");
        // Closed while a round is at work.
        let three = open(&path.join("three"));
        three.create_collection(journal("log")).unwrap();
        three.close().unwrap();
        assert_eq!(indexer_threads(), 0);
        return;
    }
    let scratch = Scratch::new("threads");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "close_and_drop_end_the_indexer_thread",
            "--test-threads=1",
        ])
        .env(THREADS_CHILD, scratch.join("vaults"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn a_standard_collection_needs_no_map() {
    let scratch = Scratch::new("standard");
    let (path, vault) = built(&scratch, "vault");
    vault.create_collection(standard("notes")).unwrap();
    line(&vault, "one");
    wait_indexed(&path);
    assert!(!path.join(".scheda/index/journal/notes").exists());
    assert!(map_file(&path).exists());
}
