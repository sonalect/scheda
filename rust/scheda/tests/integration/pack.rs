//! The object database: a pack for each write and each batch, the merge in
//! the background, and a process killed in the middle of one.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use protocontract::IterStream;
use scheda::v1::document_operation::Operation;
use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, Document, DocumentCreation, DocumentId,
    DocumentOperation, DocumentSelection, DynCollectionAsync, DynVaultAsync, OpenOptions,
    VaultAsync, VaultDocumentOperation,
};
use serde_json::json;

use crate::common::{Child, Scratch, all, both, collection, git, open};
use crate::probe::git_out;

/// A line of 200 bytes that does not compress and differs from every other.
fn line(i: u64) -> String {
    let mut x = (i + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..200)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

/// `count: n` and `packs: n` of `git count-objects -v` in `repo`.
fn counted(repo: &Path, key: &str) -> u64 {
    git_out(repo, &["count-objects", "-v"])
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}: ")))
        .unwrap()
        .parse()
        .unwrap()
}

async fn append_all(log: &DynCollectionAsync, n: u64) {
    for i in 0..n {
        log.create(
            DocumentCreation::default()
                .with_id(format!("id{i:04}"))
                .with_raw(line(i).into_bytes())
                .with_note("line"),
        )
        .await
        .unwrap();
    }
}

async fn assert_lines(log: &DynCollectionAsync, n: u64) {
    let selection = DocumentSelection {
        include_raw: Some(true),
        selection: Some(Selection::Listing(Box::default())),
        ..DocumentSelection::default()
    };
    let got: Vec<Document> = all(log.get_many(selection).await.unwrap()).await.unwrap();
    assert_eq!(got.len() as u64, n);
    for (i, document) in got.iter().enumerate() {
        assert_eq!(document.raw.as_deref(), Some(line(i as u64).as_bytes()));
    }
}

/// Waits until the merge task of the Vault at `vault` has nothing more to do
/// for the repository at `repo`: fewer packs than a merge waits for, and
/// the merge lock free. Fails the test after a minute.
async fn merged(vault: &Path, repo: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if counted(repo, "packs") < 16 && lock_is_free(vault) {
            return;
        }
        assert!(Instant::now() < deadline, "the merge did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Whether the merge lock of the Vault at `vault` can be taken now.
fn lock_is_free(vault: &Path) -> bool {
    let Ok(file) = File::open(vault.join(".scheda/pack.lock")) else {
        return true;
    };
    file.try_lock().is_ok()
}

/// The repository at `repo`, once the merges are done, holds no loose
/// object and some packs, fewer than a merge waits for, and passes
/// `git fsck`. A merge copies the entries of the packs it takes, so the
/// packs keep the size of the packs of the writes: each holds the segment
/// it was written with, not deltas of the versions before it.
async fn assert_packed(vault: &Path, repo: &Path) {
    merged(vault, repo).await;
    assert_eq!(counted(repo, "count"), 0, "loose objects");
    let packs = counted(repo, "packs");
    assert!((1..16).contains(&packs), "{packs} packs");
    git_out(repo, &["fsck", "--full", "--strict"]);
}

#[test]
fn four_hundred_appends_to_one_journal_are_packed_and_stay_readable() {
    both(|| async {
        let scratch = Scratch::new("pack-journal");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let declaration: CollectionDeclaration =
            serde_json::from_value(json!({"name": "log", "schema": {"journal": {}}})).unwrap();
        vault.create_collection(declaration).await.unwrap();
        let log = collection(&vault, "log").await.unwrap();
        append_all(&log, 400).await;
        assert_packed(&path, &path).await;
        assert_lines(&log, 400).await;
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn the_nested_repository_of_an_isolated_collection_is_packed_too() {
    both(|| async {
        let scratch = Scratch::new("pack-isolated");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let declaration: CollectionDeclaration = serde_json::from_value(
            json!({"name": "log", "schema": {"journal": {}, "isolated": true}}),
        )
        .unwrap();
        vault.create_collection(declaration).await.unwrap();
        let log = collection(&vault, "log").await.unwrap();
        append_all(&log, 400).await;
        assert_packed(&path, &path.join(".isolated")).await;
        assert_lines(&log, 400).await;
        vault.close(Empty::default()).await.unwrap();
    });
}

/// A create operation of a Document of `raw` in `collection`.
fn creation(collection: &str, id: String, raw: Vec<u8>) -> VaultDocumentOperation {
    VaultDocumentOperation {
        collection: Some(collection.to_owned()),
        operation: DocumentOperation {
            operation: Some(Operation::Create(Box::new(
                DocumentCreation::default()
                    .with_id(id)
                    .with_raw(raw)
                    .with_note("n"),
            ))),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// One write and one batch of 500 Documents each leave exactly one new pack
/// in the repository at `repo` and no loose object.
async fn one_pack_each(vault: &DynVaultAsync, repo: &Path, collection_name: &str) {
    let log = collection(vault, collection_name).await.unwrap();
    // The nested repository is made by its first write.
    log.create(
        DocumentCreation::default()
            .with_id("warm.md")
            .with_raw(b"warm".to_vec())
            .with_note("n"),
    )
    .await
    .unwrap();
    let packs = counted(repo, "packs");
    assert_eq!(counted(repo, "count"), 0, "loose objects at the start");
    log.create(
        DocumentCreation::default()
            .with_id("one.md")
            .with_raw(b"one".to_vec())
            .with_note("n"),
    )
    .await
    .unwrap();
    assert_eq!(counted(repo, "packs"), packs + 1, "one write, one pack");
    assert_eq!(counted(repo, "count"), 0, "loose objects after a write");
    let operations: Vec<Result<VaultDocumentOperation, scheda::Error>> = (0..500)
        .map(|n| {
            Ok(creation(
                collection_name,
                format!("b{n:03}.md"),
                format!("document {n}").into_bytes(),
            ))
        })
        .collect();
    vault
        .batch_documents(IterStream::new(operations.into_iter()))
        .await
        .unwrap();
    assert_eq!(counted(repo, "packs"), packs + 2, "a batch, one pack");
    assert_eq!(counted(repo, "count"), 0, "loose objects after a batch");
    git_out(repo, &["fsck", "--full", "--strict"]);
}

#[test]
fn a_write_and_a_batch_of_500_documents_each_leave_one_new_pack_and_no_loose_object() {
    both(|| async {
        let scratch = Scratch::new("pack-each");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        for (name, isolated) in [("notes", false), ("hidden", true)] {
            let declaration: CollectionDeclaration = serde_json::from_value(
                json!({"name": name, "schema": {"standard": {}, "isolated": isolated}}),
            )
            .unwrap();
            vault.create_collection(declaration).await.unwrap();
        }
        one_pack_each(&vault, &path, "notes").await;
        one_pack_each(&vault, &path.join(".isolated"), "hidden").await;
        vault.close(Empty::default()).await.unwrap();
    });
}

/// The role of the child of the test below: opens the Vault at `path`, which
/// merges its packs at once, and stays open until it is killed.
async fn hold_open(path: PathBuf) {
    let _vault = open(&path, OpenOptions::default()).await.unwrap();
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

const CHILD: &str = "SCHEDA_TEST_PACK_CHILD";

/// Copies the folder `from` to `to`.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
    // `create_dir_all` makes the folder with the umask's mode; the Vault's
    // owner-only folders must keep theirs, or `Check` reports the copy.
    std::fs::set_permissions(to, std::fs::metadata(from).unwrap().permissions()).unwrap();
}

#[test]
fn a_process_killed_in_the_middle_of_a_merge_leaves_the_vault_whole() {
    if let Ok(path) = std::env::var(CHILD) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(hold_open(PathBuf::from(path)));
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let scratch = Scratch::new("pack-kill");
        // A Vault whose history is 30 packs of a few hundred KB each, made by
        // `git`: the first open of a handle merges them, which takes long
        // enough to be killed in its middle.
        let base = scratch.join("base");
        crate::common::vault(&base);
        for n in 0..30u64 {
            let text: String = noise(n + 1, 150_000)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            crate::common::write(&base, &format!("notes/n{n:02}.md"), &text);
            git(&base, &["add", "--all"]);
            git(&base, &["commit", "--quiet", "--message", "n"]);
            git(&base, &["repack", "-q", "-d"]);
        }
        assert!(counted(&base, "packs") >= 30);
        // Killed right after the merge starts, and a little later each time.
        for (round, delay_ms) in [0u64, 20, 100, 300, 800].into_iter().enumerate() {
            let path = scratch.join(&format!("run{round}"));
            copy_dir(&base, &path);
            let mut child = Child::spawn(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "pack::a_process_killed_in_the_middle_of_a_merge_leaves_the_vault_whole",
                        "--test-threads=1",
                    ])
                    .env(CHILD, &path),
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            while lock_is_free(&path) {
                assert!(Instant::now() < deadline, "the child did not merge");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            child.kill();
            drop(child);
            // What the kill left is a Vault: git finds nothing wrong, it
            // opens, reads every Document, and `Check` reports no problem.
            assert!(lock_is_free(&path), "the killed child holds a lock");
            git_out(&path, &["fsck", "--full", "--strict"]);
            let vault = open(&path, OpenOptions::default()).await.unwrap();
            let notes = collection(&vault, "notes").await.unwrap();
            for n in 0..30u64 {
                let key = DocumentId {
                    id: Some(format!("n{n:02}.md")),
                    ..Default::default()
                };
                let document = notes.get(key).await.unwrap();
                assert_eq!(document.raw.unwrap().len(), 300_000);
            }
            let report = vault.check(Empty::default()).await.unwrap();
            assert!(report.problems.is_empty(), "{:?}", report.problems);
            // The merge runs again after the open, and ends.
            merged(&path, &path).await;
            git_out(&path, &["fsck", "--full", "--strict"]);
            vault.close(Empty::default()).await.unwrap();
        }
    });
}

/// `len` bytes that do not compress.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}
