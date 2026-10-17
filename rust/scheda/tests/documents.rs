//! The Document calls of the handle: `GetDocument`, the four writes, and the
//! batch, against a Vault on disk. Every kind of collection takes the rows
//! of the table: `standard`, `singleton`, `journal`, isolated (the nested
//! repository under `.isolated/`), and encrypted.

mod common;
mod probe;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use scheda::v1::document_operation::Operation;
use scheda::v1::document_result::Result as ResultArm;
use scheda::v1::{
    Collection, Document, DocumentCreation, DocumentDeletion, DocumentKey, DocumentOperation,
    DocumentUpdate, DocumentUpsert, OpenOptions, VaultErrorCode as Code,
};
use scheda::{Error, Fault, Vault};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use common::{Scratch, command, snapshot, vault as built_vault, work_tree, write};
use probe::{assert_settled, commits, git_out};

fn collection(value: Value) -> Collection {
    serde_json::from_value(value).unwrap()
}

const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82, 0xFF, 0xFE, 0xC3,
    0x28,
];

fn document(collection: &str, id: &str, raw: &[u8]) -> Document {
    Document {
        collection: Some(collection.to_owned()),
        id: Some(id.to_owned()),
        raw: Some(raw.to_vec()),
        ..Default::default()
    }
}

fn creation(collection: &str, id: &str, raw: &[u8], note: &str) -> DocumentCreation {
    DocumentCreation {
        document: document(collection, id, raw).into(),
        note: Some(note.to_owned()),
        ..Default::default()
    }
}

fn keeping(collection: &str, id: &str, raw: &[u8]) -> DocumentCreation {
    DocumentCreation {
        keep_existing: Some(true),
        ..creation(collection, id, raw, "keep")
    }
}

fn update(collection: &str, id: &str, raw: &[u8], revision: Option<&str>) -> DocumentUpdate {
    DocumentUpdate {
        document: document(collection, id, raw).into(),
        revision: revision.map(str::to_owned),
        note: Some("update".to_owned()),
        ..Default::default()
    }
}

fn upsert(collection: &str, id: &str, raw: &[u8]) -> DocumentUpsert {
    DocumentUpsert {
        document: document(collection, id, raw).into(),
        note: Some("upsert".to_owned()),
        ..Default::default()
    }
}

fn deletion(collection: &str, id: &str) -> DocumentDeletion {
    DocumentDeletion {
        document: key(collection, id).into(),
        note: Some("delete".to_owned()),
        ..Default::default()
    }
}

fn key(collection: &str, id: &str) -> DocumentKey {
    DocumentKey {
        collection: Some(collection.to_owned()),
        id: Some(id.to_owned()),
        ..Default::default()
    }
}

fn create_op(collection: &str, id: &str, raw: &[u8], note: &str) -> DocumentOperation {
    DocumentOperation {
        operation: Some(Operation::Create(Box::new(creation(
            collection, id, raw, note,
        )))),
        ..Default::default()
    }
}

fn update_op(collection: &str, id: &str, raw: &[u8], note: &str) -> DocumentOperation {
    let mut write = update(collection, id, raw, None);
    write.note = Some(note.to_owned());
    DocumentOperation {
        operation: Some(Operation::Update(Box::new(write))),
        ..Default::default()
    }
}

fn upsert_op(collection: &str, id: &str, raw: &[u8], note: &str) -> DocumentOperation {
    let mut write = upsert(collection, id, raw);
    write.note = Some(note.to_owned());
    DocumentOperation {
        operation: Some(Operation::Upsert(Box::new(write))),
        ..Default::default()
    }
}

fn delete_op(collection: &str, id: &str, note: &str) -> DocumentOperation {
    let mut write = deletion(collection, id);
    write.note = Some(note.to_owned());
    DocumentOperation {
        operation: Some(Operation::Delete(Box::new(write))),
        ..Default::default()
    }
}

fn batch(
    vault: &Vault,
    items: Vec<Result<DocumentOperation, Error>>,
) -> Result<scheda::v1::DocumentBatchResult, Error> {
    vault.batch_documents(Box::new(items.into_iter()))
}

fn ok(items: Vec<DocumentOperation>) -> Vec<Result<DocumentOperation, Error>> {
    items.into_iter().map(Ok).collect()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn declare(vault: &Vault, value: Value) {
    vault.create_collection(collection(value)).unwrap();
}

/// A Vault with `notes` (standard), `log` (journal), `config` (a singleton
/// with the id `settings`), `diary` (standard, isolated), and `trail`
/// (journal, isolated).
fn fresh(name: &str) -> (Scratch, PathBuf, Vault) {
    let scratch = Scratch::new(name);
    let path = scratch.join("vault");
    let vault = Vault::open(&path, OpenOptions::default().with_write_timeout_ms(30_000)).unwrap();
    declare(&vault, json!({"name": "notes", "schema": {"standard": {}}}));
    declare(&vault, json!({"name": "log", "schema": {"journal": {}}}));
    declare(
        &vault,
        json!({"name": "config", "schema": {"singleton": {"id": "settings"}}}),
    );
    declare(
        &vault,
        json!({"name": "diary", "schema": {"standard": {}, "isolated": true}}),
    );
    declare(
        &vault,
        json!({"name": "trail", "schema": {"journal": {}, "isolated": true}}),
    );
    (scratch, path, vault)
}

/// The commits of the nested repository; none before it is made.
fn nested_commits(root: &Path) -> usize {
    if root.join(".isolated/.git").exists() {
        commits(&root.join(".isolated"))
    } else {
        0
    }
}

/// The repository a collection's commits land in.
fn commits_of(root: &Path, isolated: bool) -> usize {
    if isolated {
        nested_commits(root)
    } else {
        commits(root)
    }
}

/// A stored write leaves both repositories clean and the lock empty.
fn settled(root: &Path) {
    assert_settled(root, &[]);
    if root.join(".isolated/.git").exists() {
        let status = git_out(&root.join(".isolated"), &["status", "--porcelain"]);
        assert_eq!(status, "", "the nested repository is not clean");
    }
}

fn read_file(root: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(root.join(rel)).unwrap()
}

// ---------------------------------------------------------------------
// The table: every row and option, for each kind of file.

struct Target {
    collection: &'static str,
    id: &'static str,
    rel: &'static str,
    isolated: bool,
}

const FILE_TARGETS: [Target; 3] = [
    Target {
        collection: "notes",
        id: "work/meeting.md",
        rel: "notes/work/meeting.md",
        isolated: false,
    },
    Target {
        collection: "config",
        id: "settings",
        rel: "config/settings",
        isolated: false,
    },
    Target {
        collection: "diary",
        id: "2026/day-1",
        rel: ".isolated/diary/2026/day-1",
        isolated: true,
    },
];

/// Every row and option of the table for a collection that keeps a file per
/// Document.
fn file_table(vault: &Vault, root: &Path, target: &Target) {
    let (c, id) = (target.collection, target.id);
    let repo_commits = || commits_of(root, target.isolated);
    let main_head = git_out(root, &["rev-parse", "HEAD"]);
    let start = repo_commits();

    // Absent: update and delete are NOT_FOUND, `ignore_missing` is nothing.
    let error = vault
        .update_document(update(c, id, b"x", None))
        .unwrap_err();
    assert!(error.is(Code::NotFound), "{c}");
    let error = vault.delete_document(deletion(c, id)).unwrap_err();
    assert!(error.is(Code::NotFound), "{c}");
    let mut quiet = deletion(c, id);
    quiet.ignore_missing = Some(true);
    vault.delete_document(quiet).unwrap();
    let error = vault.get_document(key(c, id)).unwrap_err();
    assert!(error.is(Code::NotFound), "{c}");
    assert_eq!(repo_commits(), start, "{c}: nothing was stored");

    // Create.
    let made = vault
        .create_document(creation(c, id, b"one", "first"))
        .unwrap();
    assert_eq!(made.collection.as_deref(), Some(c));
    assert_eq!(made.id.as_deref(), Some(id));
    assert_eq!(made.raw.as_deref(), Some(&b"one"[..]));
    let first = made.revision.clone().unwrap();
    assert_eq!(
        first,
        sha256(b"one"),
        "the revision is the hash of the file"
    );
    assert_eq!(read_file(root, target.rel), b"one");
    assert_eq!(repo_commits(), start + 1, "{c}");
    settled(root);
    assert_eq!(vault.get_document(key(c, id)).unwrap(), made);
    if target.isolated {
        assert_eq!(git_out(root, &["rev-parse", "HEAD"]), main_head);
    }

    // Create again: ALREADY_EXISTS, or the stored Document with
    // `keep_existing`.
    let error = vault
        .create_document(creation(c, id, b"other", "again"))
        .unwrap_err();
    assert!(error.is(Code::AlreadyExists), "{c}");
    let kept = vault.create_document(keeping(c, id, b"other")).unwrap();
    assert_eq!(kept, made, "the Document that was already stored");
    assert_eq!(read_file(root, target.rel), b"one");
    assert_eq!(repo_commits(), start + 1);

    // Update: replaces, with a new revision.
    let changed = vault
        .update_document(update(c, id, b"two", Some(&first)))
        .unwrap();
    let second = changed.revision.clone().unwrap();
    assert_ne!(second, first);
    assert_eq!(second, sha256(b"two"));
    assert_eq!(changed.raw.as_deref(), Some(&b"two"[..]));
    assert_eq!(read_file(root, target.rel), b"two");
    assert_eq!(repo_commits(), start + 2);
    settled(root);

    // A stale revision fails and stores nothing.
    let error = vault
        .update_document(update(c, id, b"three", Some(&first)))
        .unwrap_err();
    assert!(error.is(Code::FailedPrecondition), "{c}");
    let mut stale = deletion(c, id);
    stale.revision = Some(first.clone());
    let error = vault.delete_document(stale).unwrap_err();
    assert!(error.is(Code::FailedPrecondition), "{c}");
    assert_eq!(read_file(root, target.rel), b"two");
    assert_eq!(repo_commits(), start + 2);

    // Identical bytes store nothing and keep the revision.
    let same = vault.update_document(update(c, id, b"two", None)).unwrap();
    assert_eq!(same, changed);
    let same = vault.upsert_document(upsert(c, id, b"two")).unwrap();
    assert_eq!(same, changed);
    assert_eq!(repo_commits(), start + 2, "{c}: identical bytes, no commit");
    settled(root);

    // Upsert replaces a present Document.
    let upserted = vault.upsert_document(upsert(c, id, b"three")).unwrap();
    assert_eq!(
        upserted.revision.as_deref(),
        Some(sha256(b"three").as_str())
    );
    assert_eq!(read_file(root, target.rel), b"three");
    assert_eq!(repo_commits(), start + 3);

    // Delete with the right revision.
    let mut exact = deletion(c, id);
    exact.revision = upserted.revision.clone();
    vault.delete_document(exact).unwrap();
    assert!(!root.join(target.rel).exists());
    assert!(
        vault
            .get_document(key(c, id))
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert_eq!(repo_commits(), start + 4);
    settled(root);

    // Upsert creates an absent Document, and a plain delete removes it.
    let again = vault.upsert_document(upsert(c, id, b"four")).unwrap();
    assert_eq!(again.revision.as_deref(), Some(sha256(b"four").as_str()));
    assert_eq!(repo_commits(), start + 5);
    vault.delete_document(deletion(c, id)).unwrap();
    assert!(!root.join(target.rel).exists());
    assert_eq!(repo_commits(), start + 6);
    settled(root);
    if target.isolated {
        assert_eq!(git_out(root, &["rev-parse", "HEAD"]), main_head);
    }
}

#[test]
fn the_table_holds_for_a_standard_collection() {
    let (_scratch, path, vault) = fresh("table-standard");
    file_table(&vault, &path, &FILE_TARGETS[0]);
}

#[test]
fn the_table_holds_for_a_singleton() {
    let (_scratch, path, vault) = fresh("table-singleton");
    file_table(&vault, &path, &FILE_TARGETS[1]);
}

#[test]
fn the_table_holds_for_an_isolated_collection() {
    let (_scratch, path, vault) = fresh("table-isolated");
    file_table(&vault, &path, &FILE_TARGETS[2]);
}

/// The segment files below `dir` (Vault-relative), sorted.
fn segments(root: &Path, dir: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(root.join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    found.sort();
    found
}

/// The rows of the table for a journal: an append, a repeat, and the calls
/// a line never takes.
fn journal_table(vault: &Vault, root: &Path, c: &str, dir: &str, isolated: bool) {
    let repo_commits = || commits_of(root, isolated);
    let start = repo_commits();
    let one = vault
        .create_document(creation(c, "e1", b"first", "one"))
        .unwrap();
    assert_eq!(one.raw.as_deref(), Some(&b"first"[..]));
    assert_eq!(one.revision, None, "a journal line has no revision");
    let two = vault
        .create_document(creation(c, "e2", b"second", "two"))
        .unwrap();
    assert_eq!(two.revision, None);
    assert_eq!(repo_commits(), start + 2);
    let files = segments(root, dir);
    assert_eq!(files.len(), 1, "both lines are in one segment");
    assert_eq!(
        std::fs::read(&files[0]).unwrap(),
        b"e1: first\ne2: second\n"
    );
    settled(root);

    let got = vault.get_document(key(c, "e2")).unwrap();
    assert_eq!(got, two);
    assert!(
        vault
            .get_document(key(c, "e3"))
            .unwrap_err()
            .is(Code::NotFound)
    );

    let error = vault
        .create_document(creation(c, "e1", b"other", "again"))
        .unwrap_err();
    assert!(error.is(Code::AlreadyExists));
    let kept = vault.create_document(keeping(c, "e1", b"other")).unwrap();
    assert_eq!(kept, one, "the line that was already stored");
    assert_eq!(repo_commits(), start + 2);
    assert_eq!(
        std::fs::read(&files[0]).unwrap(),
        b"e1: first\ne2: second\n"
    );

    for error in [
        vault
            .update_document(update(c, "e1", b"x", None))
            .unwrap_err(),
        vault.upsert_document(upsert(c, "e1", b"x")).unwrap_err(),
        vault.delete_document(deletion(c, "e1")).unwrap_err(),
    ] {
        assert!(error.is(Code::Unimplemented));
    }
    let mut quiet = deletion(c, "e9");
    quiet.ignore_missing = Some(true);
    assert!(
        vault
            .delete_document(quiet)
            .unwrap_err()
            .is(Code::Unimplemented)
    );
    assert_eq!(repo_commits(), start + 2);

    // A payload holds no line break; any other bytes are stored.
    for payload in [&b"a\nb"[..], b"a\rb", b"\n"] {
        let error = vault
            .create_document(creation(c, "bad", payload, "bad"))
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{payload:?}");
    }
    let bytes = [0xFF, 0xFE, 0x00, 0xC3, 0x28, b' ', b':'];
    vault
        .create_document(creation(c, "binary", &bytes, "bytes"))
        .unwrap();
    assert_eq!(
        vault.get_document(key(c, "binary")).unwrap().raw.as_deref(),
        Some(&bytes[..])
    );
    assert_eq!(repo_commits(), start + 3);
    settled(root);
}

#[test]
fn the_table_holds_for_a_journal() {
    let (_scratch, path, vault) = fresh("table-journal");
    journal_table(&vault, &path, "log", "log", false);
}

#[test]
fn the_table_holds_for_an_isolated_journal() {
    let (_scratch, path, vault) = fresh("table-trail");
    let main = commits(&path);
    journal_table(&vault, &path, "trail", ".isolated/trail", true);
    assert_eq!(commits(&path), main, "the Vault repository did not move");
}

#[test]
fn a_journal_rolls_over_to_a_new_segment_inside_one_batch() {
    let (_scratch, path, vault) = fresh("rotate");
    declare(
        &vault,
        json!({"name": "small", "schema": {"journal": {"segmentMaxLines": 2}}}),
    );
    let before = commits(&path);
    let items: Vec<DocumentOperation> = (0..5)
        .map(|n| create_op("small", &format!("l{n}"), b"x", "lines"))
        .collect();
    batch(&vault, ok(items)).unwrap();
    assert_eq!(commits(&path), before + 1);
    let files = segments(&path, "small");
    assert_eq!(files.len(), 3, "two lines, two lines, one line");
    let mut lines = 0;
    for file in &files {
        lines += std::fs::read(file)
            .unwrap()
            .iter()
            .filter(|b| **b == b'\n')
            .count();
    }
    assert_eq!(lines, 5);
    settled(&path);
}

// ---------------------------------------------------------------------
// Bytes.

#[test]
fn any_bytes_round_trip_in_standard_and_singleton_collections() {
    let (_scratch, path, vault) = fresh("bytes");
    let not_utf8: &[u8] = &[0xFF, 0xFE, 0x00, 0xC3, 0x28, 0x80, b'\r', b'\n'];
    for (c, id, rel) in [
        ("notes", "pic.png", "notes/pic.png"),
        ("notes", "blob.bin", "notes/blob.bin"),
        ("config", "settings", "config/settings"),
        ("diary", "pic.png", ".isolated/diary/pic.png"),
    ] {
        for bytes in [PNG, not_utf8, b"", b"text\r\nwith\r\nCRLF"] {
            let made = vault.upsert_document(upsert(c, id, bytes)).unwrap();
            assert_eq!(made.raw.as_deref(), Some(bytes), "{c}");
            assert_eq!(read_file(&path, rel), bytes, "stored as given");
            let got = vault.get_document(key(c, id)).unwrap();
            assert_eq!(got.raw.as_deref(), Some(bytes), "{c}");
            assert_eq!(got.revision, Some(sha256(bytes)));
        }
    }
    settled(&path);
}

// ---------------------------------------------------------------------
// The checks before the lock.

#[test]
fn a_request_that_breaks_a_rule_fails_before_the_lock_is_taken() {
    let scratch = Scratch::new("prelock");
    let path = scratch.join("vault");
    let vault = Vault::open(&path, OpenOptions::default().with_write_timeout_ms(150)).unwrap();
    declare(&vault, json!({"name": "notes", "schema": {"standard": {}}}));
    declare(&vault, json!({"name": "log", "schema": {"journal": {}}}));
    declare(
        &vault,
        json!({"name": "config", "schema": {"singleton": {"id": "settings"}}}),
    );
    declare(
        &vault,
        json!({"name": "diary", "schema": {"standard": {}, "isolated": true}}),
    );
    declare(
        &vault,
        json!({"name": "sealed", "schema": {"standard": {}, "encrypted": true}}),
    );
    let before = commits(&path);
    // Another process holds the write lock for the whole test.
    let held = File::options()
        .read(true)
        .write(true)
        .open(path.join(".scheda/commit.lock"))
        .unwrap();
    held.lock().unwrap();

    let mut no_note = creation("notes", "a.md", b"x", "n");
    no_note.note = None;
    let mut empty_note = creation("notes", "a.md", b"x", "n");
    empty_note.note = Some(String::new());
    let mut with_revision = creation("notes", "a.md", b"x", "n");
    with_revision.document = Document {
        revision: Some("abc".to_owned()),
        ..document("notes", "a.md", b"x")
    }
    .into();
    let mut no_id = creation("notes", "a.md", b"x", "n");
    no_id.document = Document {
        id: None,
        ..document("notes", "a.md", b"x")
    }
    .into();
    let mut no_collection = creation("notes", "a.md", b"x", "n");
    no_collection.document = Document {
        collection: None,
        ..document("notes", "a.md", b"x")
    }
    .into();
    let mut no_document = creation("notes", "a.md", b"x", "n");
    no_document.document = Default::default();
    let cases: Vec<(&str, Error, Code)> = vec![
        (
            "no note",
            vault.create_document(no_note).unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "empty note",
            vault.create_document(empty_note).unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "a revision in the Document",
            vault.create_document(with_revision).unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "no id",
            vault.create_document(no_id).unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "no collection",
            vault.create_document(no_collection).unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "no Document",
            vault.create_document(no_document).unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "an unknown collection",
            vault
                .create_document(creation("nope", "a", b"x", "n"))
                .unwrap_err(),
            Code::NotFound,
        ),
        (
            "an update of a journal",
            vault
                .update_document(update("log", "a", b"x", None))
                .unwrap_err(),
            Code::Unimplemented,
        ),
        (
            "an upsert of a journal",
            vault.upsert_document(upsert("log", "a", b"x")).unwrap_err(),
            Code::Unimplemented,
        ),
        (
            "a delete of a journal",
            vault.delete_document(deletion("log", "a")).unwrap_err(),
            Code::Unimplemented,
        ),
        (
            "a path that steps out",
            vault
                .create_document(creation("notes", "../x", b"x", "n"))
                .unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "an id that is not a slug",
            vault
                .create_document(creation("notes", "a//b", b"x", "n"))
                .unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "the wrong id of a singleton",
            vault
                .create_document(creation("config", "other", b"x", "n"))
                .unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "a line break in a journal payload",
            vault
                .create_document(creation("log", "a", b"x\ny", "n"))
                .unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "a journal id that is not a slug",
            vault
                .create_document(creation("log", "a/b", b"x", "n"))
                .unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "an isolated and a main collection in one batch",
            batch(
                &vault,
                ok(vec![
                    create_op("notes", "a.md", b"x", "n"),
                    create_op("diary", "d", b"x", "n"),
                ]),
            )
            .unwrap_err(),
            Code::InvalidArgument,
        ),
        (
            "an encrypted collection without a secret",
            vault
                .create_document(creation("sealed", "a", b"x", "n"))
                .unwrap_err(),
            if cfg!(feature = "encrypt") {
                Code::Crypto
            } else {
                Code::Unimplemented
            },
        ),
    ];
    for (what, error, code) in &cases {
        assert!(error.is(*code), "{what}: {error:?}");
        assert!(
            !error.is(Code::Aborted),
            "{what}: the lock was not asked for"
        );
    }
    assert_eq!(commits(&path), before);
    // The same request, well formed, does wait for the lock and gives up.
    let start = Instant::now();
    let error = vault
        .create_document(creation("notes", "a.md", b"x", "n"))
        .unwrap_err();
    assert!(error.is(Code::Aborted));
    assert!(error.retryable());
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert_eq!(commits(&path), before);
    assert!(!path.join("notes/a.md").exists());
    held.unlock().unwrap();
    drop(held);
    vault
        .create_document(creation("notes", "a.md", b"x", "n"))
        .unwrap();
    assert_eq!(
        commits(&path),
        before + 1,
        "nothing was stored then, or later"
    );
    settled(&path);
}

#[test]
fn a_write_without_a_note_is_invalid_in_every_kind_of_collection() {
    let (_scratch, path, vault) = fresh("notes-required");
    let before = commits(&path);
    for (c, id) in [
        ("notes", "a.md"),
        ("log", "e1"),
        ("config", "settings"),
        ("diary", "d"),
        ("trail", "t"),
    ] {
        let mut write = creation(c, id, b"x", "n");
        write.note = None;
        assert!(
            vault
                .create_document(write)
                .unwrap_err()
                .is(Code::InvalidArgument)
        );
        let mut write = upsert(c, id, b"x");
        write.note = Some(String::new());
        assert!(
            vault
                .upsert_document(write)
                .unwrap_err()
                .is(Code::InvalidArgument)
        );
        let mut write = deletion(c, id);
        write.note = None;
        assert!(
            vault
                .delete_document(write)
                .unwrap_err()
                .is(Code::InvalidArgument)
        );
        let mut write = update(c, id, b"x", None);
        write.note = None;
        assert!(
            vault
                .update_document(write)
                .unwrap_err()
                .is(Code::InvalidArgument)
        );
    }
    assert_eq!(commits(&path), before);
}

#[test]
fn a_document_with_a_revision_in_a_request_is_invalid() {
    let (_scratch, path, vault) = fresh("revision-in-request");
    let made = vault
        .create_document(creation("notes", "a.md", b"x", "n"))
        .unwrap();
    let before = commits(&path);
    let mut write = update("notes", "a.md", b"y", None);
    write.document = made.clone().into();
    assert!(
        vault
            .update_document(write)
            .unwrap_err()
            .is(Code::InvalidArgument)
    );
    let mut write = upsert("notes", "a.md", b"y");
    write.document = made.into();
    assert!(
        vault
            .upsert_document(write)
            .unwrap_err()
            .is(Code::InvalidArgument)
    );
    assert_eq!(commits(&path), before);
}

#[test]
fn a_symlink_on_the_path_of_a_document_is_refused_and_nothing_is_stored() {
    #[cfg(unix)]
    {
        let (scratch, path, vault) = fresh("symlink");
        let outside = scratch.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(path.join("notes")).unwrap();
        std::os::unix::fs::symlink(&outside, path.join("notes/linked")).unwrap();
        let before = commits(&path);
        let error = vault
            .create_document(creation("notes", "linked/a.md", b"x", "n"))
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument));
        assert_eq!(commits(&path), before);
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(
            std::fs::read(path.join(".scheda/commit.lock")).unwrap(),
            Vec::<u8>::new()
        );
    }
}

// ---------------------------------------------------------------------
// Reads.

#[test]
fn get_document_names_what_is_missing() {
    let (_scratch, _path, vault) = fresh("get-missing");
    assert!(
        vault
            .get_document(key("nope", "a"))
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert!(
        vault
            .get_document(key("notes", "a.md"))
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert!(
        vault
            .get_document(key("config", "other"))
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert!(
        vault
            .get_document(key("log", "a"))
            .unwrap_err()
            .is(Code::NotFound)
    );
    let mut no_id = key("notes", "a.md");
    no_id.id = None;
    assert!(
        vault
            .get_document(no_id)
            .unwrap_err()
            .is(Code::InvalidArgument)
    );
    assert!(
        vault
            .get_document(key("notes", "../x"))
            .unwrap_err()
            .is(Code::InvalidArgument)
    );
}

#[test]
fn a_person_s_edit_of_a_file_is_what_get_document_reads() {
    let (_scratch, path, vault) = fresh("get-edit");
    vault
        .create_document(creation("notes", "a.md", b"stored", "n"))
        .unwrap();
    std::fs::write(path.join("notes/a.md"), b"edited by hand").unwrap();
    let got = vault.get_document(key("notes", "a.md")).unwrap();
    assert_eq!(got.raw.as_deref(), Some(&b"edited by hand"[..]));
    assert_eq!(got.revision, Some(sha256(b"edited by hand")));
}

#[test]
fn a_handle_reads_a_collection_another_handle_declared_and_writes_in_it() {
    let (_scratch, path, first) = fresh("two-handles");
    let second = Vault::open(&path, OpenOptions::default()).unwrap();
    declare(&first, json!({"name": "late", "schema": {"standard": {}}}));
    // The second handle has never seen `late`: it reads the collections
    // again, once, for the name it does not know.
    let error = second.get_document(key("late", "a")).unwrap_err();
    assert!(error.is(Code::NotFound));
    second
        .create_document(creation("late", "a", b"from the second", "n"))
        .unwrap();
    assert_eq!(
        first.get_document(key("late", "a")).unwrap().raw.as_deref(),
        Some(&b"from the second"[..])
    );
    first
        .upsert_document(upsert("late", "a", b"from the first"))
        .unwrap();
    assert_eq!(
        second
            .get_document(key("late", "a"))
            .unwrap()
            .raw
            .as_deref(),
        Some(&b"from the first"[..])
    );
    settled(&path);
}

#[test]
fn a_vault_built_by_hand_takes_document_writes_and_keeps_a_person_s_files() {
    let scratch = Scratch::new("by-hand");
    let path = scratch.join("vault");
    built_vault(&path);
    write(&path, "draft.txt", "a person's file, not committed");
    write(&path, "notes/scratch.md", "a person's file in a collection");
    let vault = Vault::open(&path, OpenOptions::default()).unwrap();
    let before = commits(&path);
    vault
        .create_document(creation("notes", "a.md", b"A", "n"))
        .unwrap();
    vault
        .create_document(creation("log", "e1", b"L", "n"))
        .unwrap();
    assert_eq!(commits(&path), before + 2);
    assert_eq!(
        read_file(&path, "draft.txt"),
        b"a person's file, not committed"
    );
    assert_eq!(
        vault
            .get_document(key("notes", "scratch.md"))
            .unwrap()
            .raw
            .as_deref(),
        Some(&b"a person's file in a collection"[..]),
        "a file put there by a person is a Document"
    );
    assert_settled(&path, &[".scheda/", "draft.txt", "notes/scratch.md"]);
}

#[test]
fn a_document_committed_by_a_person_is_read_replaced_and_deleted() {
    let scratch = Scratch::new("by-git");
    let path = scratch.join("vault");
    work_tree(
        &path,
        &[
            (".scheda.yaml", "version: 2\n"),
            (
                "notes/.collection.yaml",
                "name: notes\nschema:\n  standard: {}\n",
            ),
            ("notes/a.md", "by hand"),
        ],
    );
    let vault = Vault::open(&path, OpenOptions::default()).unwrap();
    let got = vault.get_document(key("notes", "a.md")).unwrap();
    assert_eq!(got.raw.as_deref(), Some(&b"by hand"[..]));
    let before = commits(&path);
    let error = vault
        .update_document(update("notes", "a.md", b"x", Some("stale")))
        .unwrap_err();
    assert!(error.is(Code::FailedPrecondition));
    vault
        .update_document(update(
            "notes",
            "a.md",
            b"replaced",
            got.revision.as_deref(),
        ))
        .unwrap();
    vault.delete_document(deletion("notes", "a.md")).unwrap();
    assert_eq!(commits(&path), before + 2);
    assert_settled(&path, &[".scheda/"]);
}

// ---------------------------------------------------------------------
// Batches.

fn stored(result: &scheda::v1::DocumentResult) -> &Document {
    match result.result.as_ref().unwrap() {
        ResultArm::Document(document) => document,
        ResultArm::Deleted(_) => panic!("the result is a delete"),
    }
}

#[test]
fn a_batch_is_one_commit_with_one_result_per_operation() {
    let (_scratch, path, vault) = fresh("batch-ok");
    vault
        .create_document(creation("notes", "old.md", b"old", "seed"))
        .unwrap();
    let before = commits(&path);
    let reply = batch(
        &vault,
        ok(vec![
            create_op("notes", "a.md", b"A", "note a"),
            create_op("log", "e1", b"line", "note log"),
            upsert_op("config", "settings", b"S", "note config"),
            delete_op("notes", "old.md", "note delete"),
        ]),
    )
    .unwrap();
    assert_eq!(commits(&path), before + 1);
    assert_eq!(reply.results.len(), 4);
    assert_eq!(stored(&reply.results[0]).raw.as_deref(), Some(&b"A"[..]));
    assert_eq!(stored(&reply.results[0]).revision, Some(sha256(b"A")));
    assert_eq!(stored(&reply.results[1]).raw.as_deref(), Some(&b"line"[..]));
    assert_eq!(stored(&reply.results[1]).revision, None);
    assert_eq!(stored(&reply.results[2]).id.as_deref(), Some("settings"));
    assert!(matches!(
        reply.results[3].result,
        Some(ResultArm::Deleted(_))
    ));
    assert!(!path.join("notes/old.md").exists());
    assert_eq!(read_file(&path, "notes/a.md"), b"A");
    assert_eq!(read_file(&path, "config/settings"), b"S");
    settled(&path);
    // An empty batch is nothing.
    let none = batch(&vault, Vec::new()).unwrap();
    assert!(none.results.is_empty());
    assert_eq!(commits(&path), before + 1);
}

#[test]
fn a_failing_write_in_the_middle_of_a_batch_stores_nothing_and_names_its_index() {
    let (_scratch, path, vault) = fresh("batch-fail");
    vault
        .create_document(creation("notes", "taken.md", b"t", "seed"))
        .unwrap();
    let before = commits(&path);
    let snap = snapshot(&path.join("notes"));
    let cases: Vec<(DocumentOperation, Code)> = vec![
        (
            create_op("notes", "taken.md", b"x", "n"),
            Code::AlreadyExists,
        ),
        (update_op("notes", "nope.md", b"x", "n"), Code::NotFound),
        (delete_op("notes", "nope.md", "n"), Code::NotFound),
        (create_op("nope", "a", b"x", "n"), Code::NotFound),
        (
            create_op("notes", "../bad", b"x", "n"),
            Code::InvalidArgument,
        ),
        (create_op("log", "e1", b"a\nb", "n"), Code::InvalidArgument),
        (update_op("log", "e1", b"x", "n"), Code::Unimplemented),
        (
            {
                let mut write = update("notes", "taken.md", b"x", Some("stale"));
                write.note = Some("n".to_owned());
                DocumentOperation {
                    operation: Some(Operation::Update(Box::new(write))),
                    ..Default::default()
                }
            },
            Code::FailedPrecondition,
        ),
    ];
    for (third, code) in cases {
        let items = ok(vec![
            create_op("notes", "fresh.md", b"f", "n"),
            create_op("log", "e1", b"line", "n"),
            third,
        ]);
        let error = batch(&vault, items).unwrap_err();
        assert!(error.is(code), "{code:?}: {error:?}");
        assert!(
            error.stack().iter().any(|frame| frame == "operation 2"),
            "the error names the operation: {:?}",
            error.stack()
        );
        assert_eq!(commits(&path), before);
        assert!(!path.join("notes/fresh.md").exists());
        assert_eq!(snapshot(&path.join("notes")), snap);
        assert!(segments_missing(&path, "log"));
        settled(&path);
    }
}

fn segments_missing(root: &Path, dir: &str) -> bool {
    !root.join(dir).exists() || segments(root, dir).is_empty()
}

#[test]
fn an_err_item_an_empty_item_and_a_stream_that_breaks_store_nothing() {
    let (_scratch, path, vault) = fresh("batch-err");
    let before = commits(&path);
    let first = || Ok(create_op("notes", "a.md", b"x", "n"));
    let error = batch(
        &vault,
        vec![
            first(),
            Err(Fault::new(Code::Unavailable, "the stream broke").into()),
            first(),
        ],
    )
    .unwrap_err();
    assert!(error.is(Code::Unavailable));
    assert!(error.stack().iter().any(|frame| frame == "operation 1"));
    // The stream ends right after an item that failed.
    let error = batch(
        &vault,
        vec![
            first(),
            first(),
            Err(Fault::new(Code::Unavailable, "the stream ended early").into()),
        ],
    )
    .unwrap_err();
    assert!(error.is(Code::Unavailable));
    assert!(error.stack().iter().any(|frame| frame == "operation 2"));
    // An item without a write.
    let error = batch(&vault, vec![first(), Ok(DocumentOperation::default())]).unwrap_err();
    assert!(error.is(Code::InvalidArgument));
    assert!(error.stack().iter().any(|frame| frame == "operation 1"));
    assert_eq!(commits(&path), before);
    assert!(!path.join("notes/a.md").exists());
    settled(&path);
}

#[test]
fn two_operations_on_one_document_see_each_other() {
    let (_scratch, path, vault) = fresh("batch-sees");
    vault
        .create_document(creation("notes", "seed.md", b"seed", "seed"))
        .unwrap();
    let before = commits(&path);
    // Create, then update, then delete-and-create again.
    let reply = batch(
        &vault,
        ok(vec![
            create_op("notes", "a.md", b"one", "create"),
            update_op("notes", "a.md", b"two", "update"),
            upsert_op("notes", "a.md", b"three", "upsert"),
            delete_op("notes", "seed.md", "delete"),
            create_op("notes", "seed.md", b"again", "create after delete"),
        ]),
    )
    .unwrap();
    assert_eq!(commits(&path), before + 1);
    assert_eq!(read_file(&path, "notes/a.md"), b"three");
    assert_eq!(read_file(&path, "notes/seed.md"), b"again");
    assert_eq!(stored(&reply.results[0]).raw.as_deref(), Some(&b"one"[..]));
    assert_eq!(stored(&reply.results[1]).raw.as_deref(), Some(&b"two"[..]));
    assert_eq!(
        stored(&reply.results[2]).raw.as_deref(),
        Some(&b"three"[..])
    );
    assert_eq!(
        stored(&reply.results[1]).revision,
        Some(sha256(b"two")),
        "each result is the Document as that operation stored it"
    );
    settled(&path);
    // A second create of a Document the batch made is ALREADY_EXISTS, at
    // its own index; a delete then an update is NOT_FOUND.
    let error = batch(
        &vault,
        ok(vec![
            create_op("notes", "b.md", b"x", "n"),
            create_op("notes", "b.md", b"y", "n"),
        ]),
    )
    .unwrap_err();
    assert!(error.is(Code::AlreadyExists));
    assert!(error.stack().iter().any(|frame| frame == "operation 1"));
    let error = batch(
        &vault,
        ok(vec![
            delete_op("notes", "a.md", "n"),
            update_op("notes", "a.md", b"y", "n"),
        ]),
    )
    .unwrap_err();
    assert!(error.is(Code::NotFound));
    assert!(error.stack().iter().any(|frame| frame == "operation 1"));
    // The same journal id twice in one batch.
    let error = batch(
        &vault,
        ok(vec![
            create_op("log", "e1", b"x", "n"),
            create_op("log", "e1", b"y", "n"),
        ]),
    )
    .unwrap_err();
    assert!(error.is(Code::AlreadyExists));
    assert_eq!(commits(&path), before + 1);
    // With `keep_existing` the second one returns what the first stored.
    let mut again = creation("log", "e1", b"y", "n");
    again.keep_existing = Some(true);
    let reply = batch(
        &vault,
        ok(vec![
            create_op("log", "e1", b"x", "n"),
            DocumentOperation {
                operation: Some(Operation::Create(Box::new(again))),
                ..Default::default()
            },
        ]),
    )
    .unwrap();
    assert_eq!(stored(&reply.results[1]).raw.as_deref(), Some(&b"x"[..]));
    assert_eq!(commits(&path), before + 2);
    settled(&path);
}

#[test]
fn a_batch_that_changes_nothing_makes_no_commit() {
    let (_scratch, path, vault) = fresh("batch-nothing");
    vault
        .create_document(creation("notes", "a.md", b"same", "seed"))
        .unwrap();
    vault
        .create_document(creation("log", "e1", b"line", "seed"))
        .unwrap();
    let before = commits(&path);
    let mut quiet = deletion("notes", "gone.md");
    quiet.ignore_missing = Some(true);
    let mut again = creation("log", "e1", b"other", "n");
    again.keep_existing = Some(true);
    let reply = batch(
        &vault,
        ok(vec![
            upsert_op("notes", "a.md", b"same", "identical"),
            update_op("notes", "a.md", b"same", "identical"),
            DocumentOperation {
                operation: Some(Operation::Delete(Box::new(quiet))),
                ..Default::default()
            },
            DocumentOperation {
                operation: Some(Operation::Create(Box::new(again))),
                ..Default::default()
            },
        ]),
    )
    .unwrap();
    assert_eq!(reply.results.len(), 4);
    assert_eq!(stored(&reply.results[0]).revision, Some(sha256(b"same")));
    assert_eq!(stored(&reply.results[3]).raw.as_deref(), Some(&b"line"[..]));
    assert_eq!(commits(&path), before);
    settled(&path);
    // A create and a delete that cancel each other leave the tree as it was.
    let reply = batch(
        &vault,
        ok(vec![
            create_op("notes", "tmp.md", b"x", "n"),
            delete_op("notes", "tmp.md", "n"),
        ]),
    )
    .unwrap();
    assert_eq!(reply.results.len(), 2);
    assert_eq!(commits(&path), before);
    assert!(!path.join("notes/tmp.md").exists());
    settled(&path);
}

#[test]
fn a_batch_of_isolated_collections_is_one_commit_of_the_nested_repository() {
    let (_scratch, path, vault) = fresh("batch-isolated");
    let main = commits(&path);
    let reply = batch(
        &vault,
        ok(vec![
            create_op("diary", "one", b"1", "diary one"),
            create_op("trail", "t1", b"line", "trail one"),
            create_op("diary", "two", b"2", "diary two"),
        ]),
    )
    .unwrap();
    assert_eq!(reply.results.len(), 3);
    assert_eq!(commits(&path), main, "the Vault repository did not move");
    assert_eq!(nested_commits(&path), 1);
    assert_eq!(read_file(&path, ".isolated/diary/one"), b"1");
    assert_eq!(read_file(&path, ".isolated/diary/two"), b"2");
    settled(&path);
    // A batch that names an isolated and a main-tree collection is refused
    // whole.
    let snap = snapshot(&path);
    let error = batch(
        &vault,
        ok(vec![
            create_op("diary", "three", b"3", "n"),
            create_op("notes", "a.md", b"x", "n"),
        ]),
    )
    .unwrap_err();
    assert!(error.is(Code::InvalidArgument));
    assert!(error.stack().iter().any(|frame| frame == "operation 1"));
    assert_eq!(snapshot(&path), snap);
}

#[test]
fn the_trailers_of_a_batch_name_each_document_with_its_own_note() {
    let (_scratch, path, vault) = fresh("trailers");
    batch(
        &vault,
        ok(vec![
            create_op("notes", "a.md", b"A", "first note\nwith a second line"),
            create_op("log", "e1", b"L", "note for the log"),
            upsert_op("notes", "b.md", b"B", "note \"quoted\""),
        ]),
    )
    .unwrap();
    let message = git_out(&path, &["log", "-1", "--format=%B"]);
    let lines: Vec<&str> = message.lines().collect();
    assert_eq!(lines[0], "first note", "the first line of the first note");
    assert_eq!(lines[1], "", "a blank line");
    assert_eq!(lines.len(), 5, "{message}");
    let values: Vec<Value> = lines[2..]
        .iter()
        .map(|line| {
            let json = line.strip_prefix("Scheda-Note: ").unwrap();
            serde_json::from_str(json).unwrap()
        })
        .collect();
    assert_eq!(
        values,
        vec![
            json!({"collection": "notes", "id": "a.md", "note": "first note\nwith a second line"}),
            json!({"collection": "log", "id": "e1", "note": "note for the log"}),
            json!({"collection": "notes", "id": "b.md", "note": "note \"quoted\""}),
        ]
    );
    // Git itself reads them as trailers.
    let trailers = git_out(
        &path,
        &[
            "log",
            "-1",
            "--format=%(trailers:key=Scheda-Note,valueonly,unfold)",
        ],
    );
    assert_eq!(
        trailers.lines().filter(|line| !line.is_empty()).count(),
        3,
        "{trailers}"
    );
    // A single write is one trailer under its note.
    vault
        .update_document(update("notes", "a.md", b"A2", None))
        .unwrap();
    let message = git_out(&path, &["log", "-1", "--format=%B"]);
    assert_eq!(
        message,
        "update\n\nScheda-Note: {\"collection\":\"notes\",\"id\":\"a.md\",\"note\":\"update\"}"
    );
}

// ---------------------------------------------------------------------
// The lock.

#[test]
fn a_call_that_cannot_take_the_lock_in_time_is_aborted_and_stores_nothing() {
    let scratch = Scratch::new("timeout");
    let path = scratch.join("vault");
    let vault = Vault::open(&path, OpenOptions::default().with_write_timeout_ms(150)).unwrap();
    declare(&vault, json!({"name": "notes", "schema": {"standard": {}}}));
    let before = commits(&path);
    let held = File::options()
        .read(true)
        .write(true)
        .open(path.join(".scheda/commit.lock"))
        .unwrap();
    held.lock().unwrap();
    for _ in 0..2 {
        let error = vault
            .create_document(creation("notes", "a.md", b"x", "n"))
            .unwrap_err();
        assert!(error.is(Code::Aborted));
        assert!(error.retryable());
        let error = batch(&vault, ok(vec![create_op("notes", "a.md", b"x", "n")])).unwrap_err();
        assert!(error.is(Code::Aborted));
    }
    assert_eq!(commits(&path), before);
    assert!(!path.join("notes/a.md").exists());
    held.unlock().unwrap();
    drop(held);
    assert!(
        vault
            .get_document(key("notes", "a.md"))
            .unwrap_err()
            .is(Code::NotFound)
    );
    vault
        .create_document(creation("notes", "a.md", b"x", "n"))
        .unwrap();
    assert_eq!(commits(&path), before + 1);
    settled(&path);
}

#[test]
fn sixty_four_document_writes_from_one_handle_are_each_stored_whole() {
    let (_scratch, path, vault) = fresh("threads");
    let vault = Arc::new(vault);
    let before = commits(&path);
    std::thread::scope(|scope| {
        for n in 0..64 {
            let vault = Arc::clone(&vault);
            scope.spawn(move || {
                let id = format!("doc-{n}.md");
                let raw = format!("document {n}");
                vault
                    .create_document(creation("notes", &id, raw.as_bytes(), "thread"))
                    .unwrap();
            });
        }
    });
    assert_eq!(commits(&path), before + 64);
    settled(&path);
    for n in 0..64 {
        let got = vault
            .get_document(key("notes", &format!("doc-{n}.md")))
            .unwrap();
        assert_eq!(got.raw.unwrap(), format!("document {n}").into_bytes());
    }
    // One commit each: no commit holds two Document files.
    let files = git_out(&path, &["log", "--format=", "--name-only", "-n", "64"]);
    assert_eq!(files.lines().filter(|line| !line.is_empty()).count(), 64);
}

#[test]
fn sixty_four_isolated_writes_from_one_handle_are_each_stored_whole() {
    let (_scratch, path, vault) = fresh("threads-isolated");
    let vault = Arc::new(vault);
    let before = nested_commits(&path);
    let main = commits(&path);
    std::thread::scope(|scope| {
        for n in 0..64 {
            let vault = Arc::clone(&vault);
            scope.spawn(move || {
                let id = format!("doc-{n}");
                vault
                    .create_document(creation("diary", &id, b"x", "thread"))
                    .unwrap();
            });
        }
    });
    assert_eq!(nested_commits(&path), before + 64);
    assert_eq!(commits(&path), main);
    settled(&path);
}

const CHILD_VAULT: &str = "SCHEDA_TEST_CHILD_VAULT";
const CHILD_INDEX: &str = "SCHEDA_TEST_CHILD_INDEX";
const PER_CHILD: usize = 22;

#[test]
fn sixty_six_document_writes_from_three_processes_are_each_stored_whole() {
    if let Ok(path) = std::env::var(CHILD_VAULT) {
        // The role of a child process: write into the Vault of the parent.
        let index = std::env::var(CHILD_INDEX).unwrap();
        let vault = Arc::new(
            Vault::open(&path, OpenOptions::default().with_write_timeout_ms(120_000)).unwrap(),
        );
        std::thread::scope(|scope| {
            for half in 0..2 {
                let vault = Arc::clone(&vault);
                let index = index.clone();
                scope.spawn(move || {
                    for n in (half * PER_CHILD / 2)..((half + 1) * PER_CHILD / 2) {
                        let id = format!("p{index}-{n}.md");
                        vault
                            .create_document(creation("notes", &id, id.as_bytes(), "process"))
                            .unwrap();
                    }
                });
            }
        });
        return;
    }
    let (_scratch, path, vault) = fresh("processes");
    let before = commits(&path);
    let exe = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for index in 0..3 {
        children.push(
            Command::new(&exe)
                .args([
                    "--exact",
                    "sixty_six_document_writes_from_three_processes_are_each_stored_whole",
                    "--test-threads=1",
                ])
                .env(CHILD_VAULT, &path)
                .env(CHILD_INDEX, index.to_string())
                .spawn()
                .unwrap(),
        );
    }
    let statuses: Vec<_> = children
        .iter_mut()
        .map(|child| child.wait().unwrap())
        .collect();
    assert!(
        statuses.iter().all(|status| status.success()),
        "{statuses:?}"
    );
    assert_eq!(commits(&path), before + 3 * PER_CHILD);
    settled(&path);
    for index in 0..3 {
        for n in 0..PER_CHILD {
            let id = format!("p{index}-{n}.md");
            let got = vault.get_document(key("notes", &id)).unwrap();
            assert_eq!(got.raw.unwrap(), id.into_bytes());
        }
    }
    assert!(
        command(&path, &["fsck", "--no-progress", "--strict"])
            .status()
            .unwrap()
            .success()
    );
}

// ---------------------------------------------------------------------
// Isolated collections.

#[test]
fn the_nested_repository_is_made_owner_only_by_the_first_write_that_needs_it() {
    let (_scratch, path, vault) = fresh("nested");
    assert!(
        !path.join(".isolated/.git").exists(),
        "declaring makes no repository"
    );
    let main = commits(&path);
    vault
        .create_document(creation("diary", "2026/day-1", b"secret", "first"))
        .unwrap();
    assert!(path.join(".isolated/.git").exists());
    assert_eq!(commits(&path), main);
    assert_eq!(nested_commits(&path), 1);
    assert_eq!(
        git_out(
            &path.join(".isolated"),
            &["show", "--name-only", "--format=", "HEAD"]
        ),
        ".gitignore\ndiary/2026/day-1"
    );
    assert_eq!(read_file(&path, ".isolated/.gitignore"), b".scheda/\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |rel: &str| {
            std::fs::metadata(path.join(rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(".isolated"), 0o700);
        assert_eq!(mode(".isolated/diary"), 0o700);
        assert_eq!(mode(".isolated/diary/2026"), 0o700);
        assert_eq!(mode(".isolated/diary/2026/day-1"), 0o600);
        assert_eq!(mode(".isolated/.gitignore"), 0o600);
    }
    // The Vault repository ignores the nested one and tracks none of it.
    assert_eq!(git_out(&path, &["ls-files", ".isolated"]), "");
    settled(&path);
    // The second write adds no second `.gitignore`.
    vault
        .create_document(creation("diary", "2026/day-2", b"more", "second"))
        .unwrap();
    assert_eq!(
        git_out(
            &path.join(".isolated"),
            &["show", "--name-only", "--format=", "HEAD"]
        ),
        "diary/2026/day-2"
    );
}

#[test]
fn a_nested_repository_with_an_origin_is_never_contacted() {
    let (_scratch, path, vault) = fresh("origin");
    vault
        .create_document(creation("diary", "a", b"1", "make the nested repository"))
        .unwrap();
    let nested = path.join(".isolated");
    common::git(
        &nested,
        &["remote", "add", "origin", "/nonexistent/scheda-origin.git"],
    );
    let start = Instant::now();
    vault
        .create_document(creation("diary", "b", b"2", "n"))
        .unwrap();
    vault
        .update_document(update("diary", "a", b"1b", None))
        .unwrap();
    vault.delete_document(deletion("diary", "b")).unwrap();
    batch(
        &vault,
        ok(vec![
            create_op("diary", "c", b"3", "n"),
            create_op("trail", "t1", b"line", "n"),
        ]),
    )
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(20));
    assert!(!nested.join(".git/FETCH_HEAD").exists(), "no fetch ran");
    assert_eq!(
        git_out(&nested, &["config", "--get", "remote.origin.url"]),
        "/nonexistent/scheda-origin.git"
    );
    assert_eq!(
        git_out(&nested, &["branch", "-r"]),
        "",
        "nothing was fetched"
    );
    settled(&path);
}

// ---------------------------------------------------------------------
// Encrypted collections.

#[cfg(feature = "encrypt")]
mod encrypted {
    use super::*;

    /// An age identity made for these tests alone.
    const IDENTITY: &str =
        "AGE-SECRET-KEY-1RN2D82PZQK97437LWJVQJK4Q6EQVHLQ9A8Z0QTAXZ424HV8XZZPS5Q9860";

    fn sealed(name: &str) -> (Scratch, PathBuf, Vault) {
        let scratch = Scratch::new(name);
        let path = scratch.join("vault");
        let options: OpenOptions = serde_json::from_value(json!({
            "encryption": {"identity": IDENTITY},
            "writeTimeoutMs": 60000
        }))
        .unwrap();
        let vault = Vault::open(&path, options).unwrap();
        declare(
            &vault,
            json!({"name": "secrets", "schema": {"standard": {}, "encrypted": true}}),
        );
        declare(
            &vault,
            json!({"name": "vault-config", "schema": {
                "singleton": {"id": "settings"}, "encrypted": true}}),
        );
        declare(
            &vault,
            json!({"name": "hidden", "schema": {
                "standard": {}, "encrypted": true, "isolated": true}}),
        );
        (scratch, path, vault)
    }

    #[test]
    fn the_table_holds_for_encrypted_collections() {
        let (_scratch, path, vault) = sealed("encrypted-table");
        let armor_of = |rel: &str| read_file(&path, rel);
        for (c, id, rel, isolated) in [
            ("secrets", "a/b.md", "secrets/a/b.md", false),
            ("vault-config", "settings", "vault-config/settings", false),
            ("hidden", "h", ".isolated/hidden/h", true),
        ] {
            let start = commits_of(&path, isolated);
            let made = vault
                .create_document(creation(c, id, b"plain text", "n"))
                .unwrap();
            assert_eq!(made.raw.as_deref(), Some(&b"plain text"[..]));
            let armor = armor_of(rel);
            assert!(
                armor.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----"),
                "{c}"
            );
            assert!(!armor.windows(10).any(|w| w == b"plain text"));
            assert_eq!(
                made.revision,
                Some(sha256(&armor)),
                "the revision is the hash of the sealed bytes"
            );
            assert_eq!(vault.get_document(key(c, id)).unwrap(), made);
            assert_eq!(commits_of(&path, isolated), start + 1);

            // The same plaintext stores nothing: the file is not sealed again.
            let same = vault.upsert_document(upsert(c, id, b"plain text")).unwrap();
            assert_eq!(same, made);
            let same = vault
                .update_document(update(c, id, b"plain text", None))
                .unwrap();
            assert_eq!(same, made);
            assert_eq!(armor_of(rel), armor, "{c}");
            assert_eq!(commits_of(&path, isolated), start + 1);

            // A different one does.
            let changed = vault
                .update_document(update(
                    c,
                    id,
                    b"changed",
                    Some(made.revision.as_deref().unwrap()),
                ))
                .unwrap();
            assert_ne!(changed.revision, made.revision);
            assert_ne!(armor_of(rel), armor);
            assert_eq!(commits_of(&path, isolated), start + 2);
            let error = vault
                .update_document(update(c, id, b"x", made.revision.as_deref()))
                .unwrap_err();
            assert!(error.is(Code::FailedPrecondition));

            let kept = vault.create_document(keeping(c, id, b"other")).unwrap();
            assert_eq!(kept, changed);
            vault.delete_document(deletion(c, id)).unwrap();
            assert!(!path.join(rel).exists());
            assert_eq!(commits_of(&path, isolated), start + 3);
            settled(&path);
        }
    }

    #[test]
    fn any_bytes_round_trip_in_an_encrypted_collection() {
        let (_scratch, path, vault) = sealed("encrypted-bytes");
        let not_utf8: &[u8] = &[0xFF, 0xFE, 0x00, 0xC3, 0x28, 0x80, b'\r', b'\n'];
        for bytes in [PNG, not_utf8, b""] {
            let made = vault
                .upsert_document(upsert("secrets", "bin", bytes))
                .unwrap();
            assert_eq!(made.raw.as_deref(), Some(bytes));
            let got = vault.get_document(key("secrets", "bin")).unwrap();
            assert_eq!(got.raw.as_deref(), Some(bytes));
        }
        settled(&path);
    }

    #[test]
    fn a_wrong_secret_is_crypto_and_stores_nothing() {
        let (_scratch, path, vault) = sealed("encrypted-wrong");
        vault
            .create_document(creation("secrets", "a", b"x", "n"))
            .unwrap();
        let before = commits(&path);
        let wrong: OpenOptions =
            serde_json::from_value(json!({"encryption": {"passphrase": "nope"}})).unwrap();
        let other = Vault::open(&path, wrong).unwrap();
        assert!(
            other
                .get_document(key("secrets", "a"))
                .unwrap_err()
                .is(Code::Crypto)
        );
        assert!(
            other
                .upsert_document(upsert("secrets", "a", b"y"))
                .unwrap_err()
                .is(Code::Crypto)
        );
        assert!(
            other
                .update_document(update("secrets", "a", b"y", None))
                .unwrap_err()
                .is(Code::Crypto)
        );
        assert_eq!(commits(&path), before);
        settled(&path);
    }
}

#[cfg(not(feature = "encrypt"))]
#[test]
fn an_encrypted_collection_in_a_build_without_the_feature_is_unimplemented() {
    let (_scratch, path, vault) = fresh("encrypted-thin");
    declare(
        &vault,
        json!({"name": "sealed", "schema": {"standard": {}, "encrypted": true}}),
    );
    let before = commits(&path);
    assert!(
        vault
            .create_document(creation("sealed", "a", b"x", "n"))
            .unwrap_err()
            .is(Code::Unimplemented)
    );
    assert!(
        vault
            .get_document(key("sealed", "a"))
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert_eq!(commits(&path), before);
}
