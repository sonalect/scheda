//! The Document calls: `Collection.Get`, the four writes, and the batches
//! of a collection and of a Vault, against a Vault on disk. Every kind of collection takes the rows
//! of the table: `standard`, `singleton`, `journal`, isolated (the nested
//! repository under `.isolated/`), and encrypted.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use protocontract::IterStream;
use scheda::v1::document_operation::Operation;
use scheda::v1::document_result::Result as ResultArm;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionKey, Document, DocumentCreation,
    DocumentDeletion, DocumentId, DocumentKey, DocumentOperation, DocumentUpdate, DocumentUpsert,
    DocumentWriteResult, DynCollectionAsync, DynVaultAsync, OpenOptions, VaultAsync,
    VaultDocumentOperation, VaultErrorCode as Code,
};
use scheda::{Error, Fault};
use serde_json::{Value, json};

use crate::common::{
    Scratch, both, command, git, open, snapshot, vault as built_vault, work_tree, write,
};
use crate::probe::{assert_settled, commits, git_out};

fn collection(value: Value) -> CollectionDeclaration {
    serde_json::from_value(value).unwrap()
}

const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82, 0xFF, 0xFE, 0xC3,
    0x28,
];

/// A request together with the collection it is for: the requests of a
/// collection carry the Document id only.
struct To<T>(String, T);

impl<T> std::ops::Deref for To<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.1
    }
}

impl<T> std::ops::DerefMut for To<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.1
    }
}

fn creation(collection: &str, id: &str, raw: &[u8], note: &str) -> To<DocumentCreation> {
    To(
        collection.to_owned(),
        DocumentCreation {
            id: Some(id.to_owned()),
            raw: Some(raw.to_vec()),
            note: Some(note.to_owned()),
            ..Default::default()
        },
    )
}

fn keeping(collection: &str, id: &str, raw: &[u8]) -> To<DocumentCreation> {
    let mut write = creation(collection, id, raw, "keep");
    write.1.keep_existing = Some(true);
    write
}

fn update(collection: &str, id: &str, raw: &[u8], revision: Option<&str>) -> To<DocumentUpdate> {
    To(
        collection.to_owned(),
        DocumentUpdate {
            id: Some(id.to_owned()),
            raw: Some(raw.to_vec()),
            revision: revision.map(str::to_owned),
            note: Some("update".to_owned()),
            ..Default::default()
        },
    )
}

fn upsert(collection: &str, id: &str, raw: &[u8]) -> To<DocumentUpsert> {
    To(
        collection.to_owned(),
        DocumentUpsert {
            id: Some(id.to_owned()),
            raw: Some(raw.to_vec()),
            note: Some("upsert".to_owned()),
            ..Default::default()
        },
    )
}

fn deletion(collection: &str, id: &str) -> To<DocumentDeletion> {
    To(
        collection.to_owned(),
        DocumentDeletion {
            id: Some(id.to_owned()),
            note: Some("delete".to_owned()),
            ..Default::default()
        },
    )
}

fn key(collection: &str, id: &str) -> DocumentKey {
    DocumentKey {
        collection: Some(collection.to_owned()),
        id: Some(id.to_owned()),
        ..Default::default()
    }
}

/// The collection `name` of `vault`: `NOT_FOUND` when it is not declared.
async fn collection_of(vault: &DynVaultAsync, name: &str) -> Result<DynCollectionAsync, Error> {
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .await?
        .to_async_inline()
}

/// The single Document calls of `Collection`, made through a Vault: each
/// request names its collection, and the call goes to that collection's
/// object.
///
/// A write answers with the Document it stored, as the tests read it; the
/// token of the write is in `*_result`. A deletion answers with no
/// Document, which `delete` checks.
trait Calls {
    async fn get(&self, key: DocumentKey) -> Result<Document, Error>;
    async fn create(&self, write: To<DocumentCreation>) -> Result<Document, Error>;
    async fn update(&self, write: To<DocumentUpdate>) -> Result<Document, Error>;
    async fn upsert(&self, write: To<DocumentUpsert>) -> Result<Document, Error>;
    async fn delete(&self, write: To<DocumentDeletion>) -> Result<Empty, Error>;
    async fn create_result(
        &self,
        write: To<DocumentCreation>,
    ) -> Result<DocumentWriteResult, Error>;
    async fn update_result(&self, write: To<DocumentUpdate>) -> Result<DocumentWriteResult, Error>;
    async fn upsert_result(&self, write: To<DocumentUpsert>) -> Result<DocumentWriteResult, Error>;
    async fn delete_result(
        &self,
        write: To<DocumentDeletion>,
    ) -> Result<DocumentWriteResult, Error>;
}

/// The Document a write stored, and the token of the write: both are set.
fn written(result: DocumentWriteResult) -> Document {
    assert!(
        result.version.as_deref().is_some_and(|v| !v.is_empty()),
        "a write returns its token"
    );
    result.document.into_option().unwrap()
}

impl Calls for DynVaultAsync {
    async fn get(&self, key: DocumentKey) -> Result<Document, Error> {
        let name = key.collection.clone().unwrap_or_default();
        let id = DocumentId {
            id: key.id,
            ..Default::default()
        };
        collection_of(self, &name).await?.get(id).await
    }

    async fn create(&self, write: To<DocumentCreation>) -> Result<Document, Error> {
        self.create_result(write).await.map(written)
    }

    async fn update(&self, write: To<DocumentUpdate>) -> Result<Document, Error> {
        self.update_result(write).await.map(written)
    }

    async fn upsert(&self, write: To<DocumentUpsert>) -> Result<Document, Error> {
        self.upsert_result(write).await.map(written)
    }

    async fn delete(&self, write: To<DocumentDeletion>) -> Result<Empty, Error> {
        self.delete_result(write).await.map(|result| {
            assert!(result.document.is_unset(), "a deletion returns no Document");
            assert!(
                result.version.as_deref().is_some_and(|v| !v.is_empty()),
                "a write returns its token"
            );
            Empty::default()
        })
    }

    async fn create_result(
        &self,
        write: To<DocumentCreation>,
    ) -> Result<DocumentWriteResult, Error> {
        collection_of(self, &write.0).await?.create(write.1).await
    }

    async fn update_result(&self, write: To<DocumentUpdate>) -> Result<DocumentWriteResult, Error> {
        collection_of(self, &write.0).await?.update(write.1).await
    }

    async fn upsert_result(&self, write: To<DocumentUpsert>) -> Result<DocumentWriteResult, Error> {
        collection_of(self, &write.0).await?.upsert(write.1).await
    }

    async fn delete_result(
        &self,
        write: To<DocumentDeletion>,
    ) -> Result<DocumentWriteResult, Error> {
        collection_of(self, &write.0).await?.delete(write.1).await
    }
}

fn in_vault(collection: &str, operation: Operation) -> VaultDocumentOperation {
    VaultDocumentOperation {
        collection: Some(collection.to_owned()),
        operation: DocumentOperation {
            operation: Some(operation),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// A write in the collection a request names, as the batch of a Vault takes it.
fn op<T>(write: To<T>, wrap: fn(Box<T>) -> Operation) -> VaultDocumentOperation {
    in_vault(&write.0, wrap(Box::new(write.1)))
}

fn create_op(collection: &str, id: &str, raw: &[u8], note: &str) -> VaultDocumentOperation {
    let write = creation(collection, id, raw, note);
    in_vault(collection, Operation::Create(Box::new(write.1)))
}

fn update_op(collection: &str, id: &str, raw: &[u8], note: &str) -> VaultDocumentOperation {
    let mut write = update(collection, id, raw, None);
    write.1.note = Some(note.to_owned());
    in_vault(collection, Operation::Update(Box::new(write.1)))
}

fn upsert_op(collection: &str, id: &str, raw: &[u8], note: &str) -> VaultDocumentOperation {
    let mut write = upsert(collection, id, raw);
    write.1.note = Some(note.to_owned());
    in_vault(collection, Operation::Upsert(Box::new(write.1)))
}

fn delete_op(collection: &str, id: &str, note: &str) -> VaultDocumentOperation {
    let mut write = deletion(collection, id);
    write.1.note = Some(note.to_owned());
    in_vault(collection, Operation::Delete(Box::new(write.1)))
}

async fn batch(
    vault: &DynVaultAsync,
    items: Vec<Result<VaultDocumentOperation, Error>>,
) -> Result<scheda::v1::DocumentBatchResult, Error> {
    vault
        .batch_documents(IterStream::new(items.into_iter()))
        .await
}

fn ok(items: Vec<VaultDocumentOperation>) -> Vec<Result<VaultDocumentOperation, Error>> {
    items.into_iter().map(Ok).collect()
}

/// The id git gives a blob of `bytes`: what `git hash-object` prints, and
/// what a Document's revision is.
fn blob_id(bytes: &[u8]) -> String {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

async fn declare(vault: &DynVaultAsync, value: Value) {
    vault.create_collection(collection(value)).await.unwrap();
}

/// A Vault with `notes` (standard), `log` (journal), `config` (a singleton
/// with the id `settings`), `diary` (standard, isolated), and `trail`
/// (journal, isolated).
async fn fresh(name: &str) -> (Scratch, PathBuf, DynVaultAsync) {
    let scratch = Scratch::new(name);
    let path = scratch.join("vault");
    let vault = open(&path, OpenOptions::default().with_write_timeout_ms(30_000))
        .await
        .unwrap();
    declare(&vault, json!({"name": "notes", "schema": {"standard": {}}})).await;
    declare(&vault, json!({"name": "log", "schema": {"journal": {}}})).await;
    declare(
        &vault,
        json!({"name": "config", "schema": {"singleton": {"id": "settings"}}}),
    )
    .await;
    declare(
        &vault,
        json!({"name": "diary", "schema": {"standard": {}, "isolated": true}}),
    )
    .await;
    declare(
        &vault,
        json!({"name": "trail", "schema": {"journal": {}, "isolated": true}}),
    )
    .await;
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
async fn file_table(vault: &DynVaultAsync, root: &Path, target: &Target) {
    let (c, id) = (target.collection, target.id);
    let repo_commits = || commits_of(root, target.isolated);
    let main_head = git_out(root, &["rev-parse", "HEAD"]);
    let start = repo_commits();

    // Absent: update and delete are NOT_FOUND, `ignore_missing` is nothing.
    let error = vault.update(update(c, id, b"x", None)).await.unwrap_err();
    assert!(error.is(Code::NotFound), "{c}");
    let error = vault.delete(deletion(c, id)).await.unwrap_err();
    assert!(error.is(Code::NotFound), "{c}");
    let mut quiet = deletion(c, id);
    quiet.ignore_missing = Some(true);
    vault.delete(quiet).await.unwrap();
    let error = vault.get(key(c, id)).await.unwrap_err();
    assert!(error.is(Code::NotFound), "{c}");
    assert_eq!(repo_commits(), start, "{c}: nothing was stored");

    // Create.
    let made = vault
        .create(creation(c, id, b"one", "first"))
        .await
        .unwrap();
    assert_eq!(made.collection.as_deref(), Some(c));
    assert_eq!(made.id.as_deref(), Some(id));
    assert_eq!(made.raw.as_deref(), Some(&b"one"[..]));
    let first = made.revision.clone().unwrap();
    assert_eq!(
        first,
        blob_id(b"one"),
        "the revision is the hash of the file"
    );
    assert_eq!(read_file(root, target.rel), b"one");
    assert_eq!(repo_commits(), start + 1, "{c}");
    settled(root);
    assert_eq!(vault.get(key(c, id)).await.unwrap(), made);
    if target.isolated {
        assert_eq!(git_out(root, &["rev-parse", "HEAD"]), main_head);
    }

    // Create again: ALREADY_EXISTS, or the stored Document with
    // `keep_existing`.
    let error = vault
        .create(creation(c, id, b"other", "again"))
        .await
        .unwrap_err();
    assert!(error.is(Code::AlreadyExists), "{c}");
    let kept = vault.create(keeping(c, id, b"other")).await.unwrap();
    assert_eq!(kept, made, "the Document that was already stored");
    assert_eq!(read_file(root, target.rel), b"one");
    assert_eq!(repo_commits(), start + 1);

    // Update: replaces, with a new revision.
    let changed = vault
        .update(update(c, id, b"two", Some(&first)))
        .await
        .unwrap();
    let second = changed.revision.clone().unwrap();
    assert_ne!(second, first);
    assert_eq!(second, blob_id(b"two"));
    assert_eq!(changed.raw.as_deref(), Some(&b"two"[..]));
    assert_eq!(read_file(root, target.rel), b"two");
    assert_eq!(repo_commits(), start + 2);
    settled(root);

    // A stale revision fails and stores nothing.
    let error = vault
        .update(update(c, id, b"three", Some(&first)))
        .await
        .unwrap_err();
    assert!(error.is(Code::FailedPrecondition), "{c}");
    let mut stale = deletion(c, id);
    stale.revision = Some(first.clone());
    let error = vault.delete(stale).await.unwrap_err();
    assert!(error.is(Code::FailedPrecondition), "{c}");
    assert_eq!(read_file(root, target.rel), b"two");
    assert_eq!(repo_commits(), start + 2);

    // Identical bytes store nothing and keep the revision.
    let same = vault.update(update(c, id, b"two", None)).await.unwrap();
    assert_eq!(same, changed);
    let same = vault.upsert(upsert(c, id, b"two")).await.unwrap();
    assert_eq!(same, changed);
    assert_eq!(repo_commits(), start + 2, "{c}: identical bytes, no commit");
    settled(root);

    // Upsert replaces a present Document.
    let upserted = vault.upsert(upsert(c, id, b"three")).await.unwrap();
    assert_eq!(
        upserted.revision.as_deref(),
        Some(blob_id(b"three").as_str())
    );
    assert_eq!(read_file(root, target.rel), b"three");
    assert_eq!(repo_commits(), start + 3);

    // Delete with the right revision.
    let mut exact = deletion(c, id);
    exact.revision = upserted.revision.clone();
    vault.delete(exact).await.unwrap();
    assert!(!root.join(target.rel).exists());
    assert!(vault.get(key(c, id)).await.unwrap_err().is(Code::NotFound));
    assert_eq!(repo_commits(), start + 4);
    settled(root);

    // Upsert creates an absent Document, and a plain delete removes it.
    let again = vault.upsert(upsert(c, id, b"four")).await.unwrap();
    assert_eq!(again.revision.as_deref(), Some(blob_id(b"four").as_str()));
    assert_eq!(repo_commits(), start + 5);
    vault.delete(deletion(c, id)).await.unwrap();
    assert!(!root.join(target.rel).exists());
    assert_eq!(repo_commits(), start + 6);
    settled(root);
    if target.isolated {
        assert_eq!(git_out(root, &["rev-parse", "HEAD"]), main_head);
    }
}

#[test]
fn the_table_holds_for_a_standard_collection() {
    both(|| async {
        let (_scratch, path, vault) = fresh("table-standard").await;
        file_table(&vault, &path, &FILE_TARGETS[0]).await;
    });
}

#[test]
fn the_table_holds_for_a_singleton() {
    both(|| async {
        let (_scratch, path, vault) = fresh("table-singleton").await;
        file_table(&vault, &path, &FILE_TARGETS[1]).await;
    });
}

#[test]
fn the_table_holds_for_an_isolated_collection() {
    both(|| async {
        let (_scratch, path, vault) = fresh("table-isolated").await;
        file_table(&vault, &path, &FILE_TARGETS[2]).await;
    });
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
async fn journal_table(vault: &DynVaultAsync, root: &Path, c: &str, dir: &str, isolated: bool) {
    let repo_commits = || commits_of(root, isolated);
    let start = repo_commits();
    let one = vault
        .create(creation(c, "e1", b"first", "one"))
        .await
        .unwrap();
    assert_eq!(one.raw.as_deref(), Some(&b"first"[..]));
    assert_eq!(one.revision, None, "a journal line has no revision");
    let two = vault
        .create(creation(c, "e2", b"second", "two"))
        .await
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

    let got = vault.get(key(c, "e2")).await.unwrap();
    assert_eq!(got, two);
    assert!(
        vault
            .get(key(c, "e3"))
            .await
            .unwrap_err()
            .is(Code::NotFound)
    );

    let error = vault
        .create(creation(c, "e1", b"other", "again"))
        .await
        .unwrap_err();
    assert!(error.is(Code::AlreadyExists));
    let kept = vault.create(keeping(c, "e1", b"other")).await.unwrap();
    assert_eq!(kept, one, "the line that was already stored");
    assert_eq!(repo_commits(), start + 2);
    assert_eq!(
        std::fs::read(&files[0]).unwrap(),
        b"e1: first\ne2: second\n"
    );

    for error in [
        vault.update(update(c, "e1", b"x", None)).await.unwrap_err(),
        vault.upsert(upsert(c, "e1", b"x")).await.unwrap_err(),
        vault.delete(deletion(c, "e1")).await.unwrap_err(),
    ] {
        assert!(error.is(Code::Unimplemented));
    }
    let mut quiet = deletion(c, "e9");
    quiet.ignore_missing = Some(true);
    assert!(
        vault
            .delete(quiet)
            .await
            .unwrap_err()
            .is(Code::Unimplemented)
    );
    assert_eq!(repo_commits(), start + 2);

    // A payload holds no line break; any other bytes are stored.
    for payload in [&b"a\nb"[..], b"a\rb", b"\n"] {
        let error = vault
            .create(creation(c, "bad", payload, "bad"))
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{payload:?}");
    }
    let bytes = [0xFF, 0xFE, 0x00, 0xC3, 0x28, b' ', b':'];
    vault
        .create(creation(c, "binary", &bytes, "bytes"))
        .await
        .unwrap();
    assert_eq!(
        vault.get(key(c, "binary")).await.unwrap().raw.as_deref(),
        Some(&bytes[..])
    );
    assert_eq!(repo_commits(), start + 3);
    settled(root);
}

#[test]
fn the_table_holds_for_a_journal() {
    both(|| async {
        let (_scratch, path, vault) = fresh("table-journal").await;
        journal_table(&vault, &path, "log", "log", false).await;
    });
}

#[test]
fn the_table_holds_for_an_isolated_journal() {
    both(|| async {
        let (_scratch, path, vault) = fresh("table-trail").await;
        let main = commits(&path);
        journal_table(&vault, &path, "trail", ".isolated/trail", true).await;
        assert_eq!(commits(&path), main, "the Vault repository did not move");
    });
}

#[test]
fn a_journal_rolls_over_to_a_new_segment_inside_one_batch() {
    both(|| async {
        let (_scratch, path, vault) = fresh("rotate").await;
        declare(
            &vault,
            json!({"name": "small", "schema": {"journal": {"segmentMaxLines": 2}}}),
        )
        .await;
        let before = commits(&path);
        let items: Vec<VaultDocumentOperation> = (0..5)
            .map(|n| create_op("small", &format!("l{n}"), b"x", "lines"))
            .collect();
        batch(&vault, ok(items)).await.unwrap();
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
    });
}

// ---------------------------------------------------------------------
// Bytes.

#[test]
fn any_bytes_round_trip_in_standard_and_singleton_collections() {
    both(|| async {
        let (_scratch, path, vault) = fresh("bytes").await;
        let not_utf8: &[u8] = &[0xFF, 0xFE, 0x00, 0xC3, 0x28, 0x80, b'\r', b'\n'];
        for (c, id, rel) in [
            ("notes", "pic.png", "notes/pic.png"),
            ("notes", "blob.bin", "notes/blob.bin"),
            ("config", "settings", "config/settings"),
            ("diary", "pic.png", ".isolated/diary/pic.png"),
        ] {
            for bytes in [PNG, not_utf8, b"", b"text\r\nwith\r\nCRLF"] {
                let made = vault.upsert(upsert(c, id, bytes)).await.unwrap();
                assert_eq!(made.raw.as_deref(), Some(bytes), "{c}");
                assert_eq!(read_file(&path, rel), bytes, "stored as given");
                let got = vault.get(key(c, id)).await.unwrap();
                assert_eq!(got.raw.as_deref(), Some(bytes), "{c}");
                assert_eq!(got.revision, Some(blob_id(bytes)));
            }
        }
        settled(&path);
    });
}

// ---------------------------------------------------------------------
// The checks before the lock.

#[test]
fn a_request_that_breaks_a_rule_fails_before_the_lock_is_taken() {
    both(|| async {
        let scratch = Scratch::new("prelock");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default().with_write_timeout_ms(150))
            .await
            .unwrap();
        declare(&vault, json!({"name": "notes", "schema": {"standard": {}}})).await;
        declare(&vault, json!({"name": "log", "schema": {"journal": {}}})).await;
        declare(
            &vault,
            json!({"name": "config", "schema": {"singleton": {"id": "settings"}}}),
        )
        .await;
        declare(
            &vault,
            json!({"name": "diary", "schema": {"standard": {}, "isolated": true}}),
        )
        .await;
        declare(
            &vault,
            json!({"name": "sealed", "schema": {"standard": {}, "encrypted": true}}),
        )
        .await;
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
        let mut no_id = creation("notes", "a.md", b"x", "n");
        no_id.id = None;
        let mut no_collection = creation("notes", "a.md", b"x", "n");
        no_collection.0 = String::new();
        let mut no_document = creation("notes", "a.md", b"x", "n");
        no_document.id = None;
        no_document.raw = None;
        let cases: Vec<(&str, Error, Code)> = vec![
            (
                "no note",
                vault.create(no_note).await.unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "empty note",
                vault.create(empty_note).await.unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "no id",
                vault.create(no_id).await.unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "no collection",
                vault.create(no_collection).await.unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "no Document",
                vault.create(no_document).await.unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "an unknown collection",
                vault
                    .create(creation("nope", "a", b"x", "n"))
                    .await
                    .unwrap_err(),
                Code::NotFound,
            ),
            (
                "an update of a journal",
                vault
                    .update(update("log", "a", b"x", None))
                    .await
                    .unwrap_err(),
                Code::Unimplemented,
            ),
            (
                "an upsert of a journal",
                vault.upsert(upsert("log", "a", b"x")).await.unwrap_err(),
                Code::Unimplemented,
            ),
            (
                "a delete of a journal",
                vault.delete(deletion("log", "a")).await.unwrap_err(),
                Code::Unimplemented,
            ),
            (
                "a path that steps out",
                vault
                    .create(creation("notes", "../x", b"x", "n"))
                    .await
                    .unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "an id that is not a slug",
                vault
                    .create(creation("notes", "a//b", b"x", "n"))
                    .await
                    .unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "the wrong id of a singleton",
                vault
                    .create(creation("config", "other", b"x", "n"))
                    .await
                    .unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "a line break in a journal payload",
                vault
                    .create(creation("log", "a", b"x\ny", "n"))
                    .await
                    .unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "a journal id that is not a slug",
                vault
                    .create(creation("log", "a/b", b"x", "n"))
                    .await
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
                .await
                .unwrap_err(),
                Code::InvalidArgument,
            ),
            (
                "an encrypted collection without a secret",
                vault
                    .create(creation("sealed", "a", b"x", "n"))
                    .await
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
            .create(creation("notes", "a.md", b"x", "n"))
            .await
            .unwrap_err();
        assert!(error.is(Code::Aborted));
        assert!(error.retryable());
        assert!(start.elapsed() >= Duration::from_millis(100));
        assert_eq!(commits(&path), before);
        assert!(!path.join("notes/a.md").exists());
        held.unlock().unwrap();
        drop(held);
        vault
            .create(creation("notes", "a.md", b"x", "n"))
            .await
            .unwrap();
        assert_eq!(
            commits(&path),
            before + 1,
            "nothing was stored then, or later"
        );
        settled(&path);
    });
}

#[test]
fn a_write_without_a_note_is_invalid_in_every_kind_of_collection() {
    both(|| async {
        let (_scratch, path, vault) = fresh("notes-required").await;
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
                    .create(write)
                    .await
                    .unwrap_err()
                    .is(Code::InvalidArgument)
            );
            let mut write = upsert(c, id, b"x");
            write.note = Some(String::new());
            assert!(
                vault
                    .upsert(write)
                    .await
                    .unwrap_err()
                    .is(Code::InvalidArgument)
            );
            let mut write = deletion(c, id);
            write.note = None;
            assert!(
                vault
                    .delete(write)
                    .await
                    .unwrap_err()
                    .is(Code::InvalidArgument)
            );
            let mut write = update(c, id, b"x", None);
            write.note = None;
            assert!(
                vault
                    .update(write)
                    .await
                    .unwrap_err()
                    .is(Code::InvalidArgument)
            );
        }
        assert_eq!(commits(&path), before);
    });
}

#[test]
fn a_symlink_on_the_path_of_a_document_is_refused_and_nothing_is_stored() {
    both(|| async {
        #[cfg(unix)]
        {
            let (scratch, path, vault) = fresh("symlink").await;
            let outside = scratch.join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::create_dir_all(path.join("notes")).unwrap();
            std::os::unix::fs::symlink(&outside, path.join("notes/linked")).unwrap();
            let before = commits(&path);
            let error = vault
                .create(creation("notes", "linked/a.md", b"x", "n"))
                .await
                .unwrap_err();
            assert!(error.is(Code::InvalidArgument));
            assert_eq!(commits(&path), before);
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
            assert_eq!(
                std::fs::read(path.join(".scheda/commit.lock")).unwrap(),
                Vec::<u8>::new()
            );
        }
    });
}

// ---------------------------------------------------------------------
// Reads.

#[test]
fn get_document_names_what_is_missing() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("get-missing").await;
        assert!(
            vault
                .get(key("nope", "a"))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        assert!(
            vault
                .get(key("notes", "a.md"))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        assert!(
            vault
                .get(key("config", "other"))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        assert!(
            vault
                .get(key("log", "a"))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        let mut no_id = key("notes", "a.md");
        no_id.id = None;
        assert!(
            vault
                .get(no_id)
                .await
                .unwrap_err()
                .is(Code::InvalidArgument)
        );
        assert!(
            vault
                .get(key("notes", "../x"))
                .await
                .unwrap_err()
                .is(Code::InvalidArgument)
        );
    });
}

#[test]
fn a_persons_uncommitted_edit_is_seen_by_no_get_document_and_a_commit_by_hand_is() {
    both(|| async {
        let (_scratch, path, vault) = fresh("get-edit").await;
        vault
            .create(creation("notes", "a.md", b"stored", "n"))
            .await
            .unwrap();
        std::fs::write(path.join("notes/a.md"), b"edited by hand").unwrap();
        std::fs::remove_file(path.join(".scheda.yaml")).unwrap();
        let got = vault.get(key("notes", "a.md")).await.unwrap();
        assert_eq!(got.raw.as_deref(), Some(&b"stored"[..]));
        assert_eq!(got.revision, Some(blob_id(b"stored")));
        git(&path, &["checkout", "--", ".scheda.yaml"]);
        // The same bytes, committed with plain git, are read at once.
        git(&path, &["add", "notes/a.md"]);
        git(&path, &["commit", "--quiet", "--message", "by hand"]);
        let got = vault.get(key("notes", "a.md")).await.unwrap();
        assert_eq!(got.raw.as_deref(), Some(&b"edited by hand"[..]));
        assert_eq!(got.revision, Some(blob_id(b"edited by hand")));
        // A new file and a deleted one in the work tree are as unseen.
        write(&path, "notes/new.md", "never committed");
        std::fs::remove_file(path.join("notes/a.md")).unwrap();
        assert!(vault.get(key("notes", "a.md")).await.is_ok());
        assert!(
            vault
                .get(key("notes", "new.md"))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
    });
}

#[test]
fn a_persons_edit_is_committed_by_no_write_of_another_document_and_survives_one_of_its_own() {
    both(|| async {
        let (_scratch, path, vault) = fresh("edit-survives").await;
        vault
            .create(creation("notes", "mine.md", b"stored", "n"))
            .await
            .unwrap();
        vault
            .create(creation("notes", "other.md", b"other", "n"))
            .await
            .unwrap();
        std::fs::write(path.join("notes/mine.md"), b"edited by hand").unwrap();
        write(&path, "notes/new.md", "never committed");
        // A write of another Document commits neither the edit nor the new file.
        let before = commits(&path);
        vault
            .upsert(upsert("notes", "other.md", b"other, again"))
            .await
            .unwrap();
        assert_eq!(commits(&path), before + 1);
        assert_eq!(
            git_out(&path, &["show", "--name-only", "--format=", "HEAD"]),
            "notes/other.md"
        );
        assert_eq!(
            git_out(&path, &["show", "HEAD:notes/mine.md"]),
            "stored",
            "the edit is not in HEAD"
        );
        assert_eq!(
            git_out(&path, &["status", "--porcelain"]),
            " M notes/mine.md\n?? notes/new.md"
        );
        // A write of the same Document lands in HEAD; the person's file stays and
        // git shows it as changed against the new HEAD.
        let reply = vault
            .upsert(upsert("notes", "mine.md", b"written by the API"))
            .await
            .unwrap();
        assert_eq!(
            git_out(&path, &["show", "HEAD:notes/mine.md"]),
            "written by the API"
        );
        assert_eq!(read_file(&path, "notes/mine.md"), b"edited by hand");
        assert_eq!(
            git_out(&path, &["status", "--porcelain"]),
            " M notes/mine.md\n?? notes/new.md"
        );
        assert_eq!(
            reply.revision,
            Some(git_out(&path, &["rev-parse", "HEAD:notes/mine.md"]))
        );
        assert_eq!(
            vault
                .get(key("notes", "mine.md"))
                .await
                .unwrap()
                .raw
                .as_deref(),
            Some(&b"written by the API"[..])
        );
        assert!(read_file(&path, ".scheda/commit.lock").is_empty());
        // A delete of it removes it from HEAD and leaves the person's file.
        vault.delete(deletion("notes", "mine.md")).await.unwrap();
        assert_eq!(read_file(&path, "notes/mine.md"), b"edited by hand");
        assert_eq!(
            git_out(&path, &["status", "--porcelain"]),
            "?? notes/mine.md\n?? notes/new.md"
        );
    });
}

#[test]
fn a_documents_revision_is_the_blob_id_of_the_document_in_head() {
    both(|| async {
        let (_scratch, path, vault) = fresh("revision-blob").await;
        for (collection, id, raw) in [
            ("notes", "a.md", &b"alpha"[..]),
            ("notes", "dir/b.bin", PNG),
            ("config", "settings", b"on"),
        ] {
            let made = vault
                .create(creation(collection, id, raw, "n"))
                .await
                .unwrap();
            let rel = format!("{collection}/{id}");
            let head = git_out(&path, &["rev-parse", &format!("HEAD:{rel}")]);
            assert_eq!(made.revision.as_deref(), Some(head.as_str()), "{rel}");
            let got = vault.get(key(collection, id)).await.unwrap();
            assert_eq!(got.revision.as_deref(), Some(head.as_str()), "{rel}");
        }
        let replaced = vault
            .update(update("notes", "a.md", b"beta", None))
            .await
            .unwrap();
        assert_eq!(
            replaced.revision,
            Some(git_out(&path, &["rev-parse", "HEAD:notes/a.md"]))
        );
        // The nested repository: the id is the blob of its own HEAD.
        let nested = vault
            .create(creation("diary", "d", b"secret", "n"))
            .await
            .unwrap();
        assert_eq!(
            nested.revision,
            Some(git_out(
                &path.join(".isolated"),
                &["rev-parse", "HEAD:diary/d"]
            ))
        );
    });
}

#[test]
fn a_repository_with_gpgsign_gets_unsigned_commits_and_the_write_succeeds() {
    both(|| async {
        let (_scratch, path, vault) = fresh("gpgsign").await;
        git(&path, &["config", "commit.gpgSign", "true"]);
        git(&path, &["config", "gpg.program", "/nonexistent/gpg"]);
        git(&path, &["config", "user.signingkey", "ABCDEF"]);
        let made = vault
            .create(creation("notes", "signed.md", b"x", "n"))
            .await
            .unwrap();
        assert!(made.revision.is_some());
        assert_eq!(git_out(&path, &["show", "HEAD:notes/signed.md"]), "x");
        let raw = git_out(&path, &["cat-file", "commit", "HEAD"]);
        assert!(!raw.contains("gpgsig"), "{raw}");
        settled(&path);
    });
}

#[test]
fn a_handle_reads_a_collection_another_handle_declared_and_writes_in_it() {
    both(|| async {
        let (_scratch, path, first) = fresh("two-handles").await;
        let second = open(&path, OpenOptions::default()).await.unwrap();
        declare(&first, json!({"name": "late", "schema": {"standard": {}}})).await;
        // The second handle has never seen `late`: it reads the collections
        // again, once, for the name it does not know.
        let error = second.get(key("late", "a")).await.unwrap_err();
        assert!(error.is(Code::NotFound));
        second
            .create(creation("late", "a", b"from the second", "n"))
            .await
            .unwrap();
        assert_eq!(
            first.get(key("late", "a")).await.unwrap().raw.as_deref(),
            Some(&b"from the second"[..])
        );
        first
            .upsert(upsert("late", "a", b"from the first"))
            .await
            .unwrap();
        assert_eq!(
            second.get(key("late", "a")).await.unwrap().raw.as_deref(),
            Some(&b"from the first"[..])
        );
        settled(&path);
    });
}

#[test]
fn a_vault_built_by_hand_takes_document_writes_and_keeps_a_person_s_files() {
    both(|| async {
        let scratch = Scratch::new("by-hand");
        let path = scratch.join("vault");
        built_vault(&path);
        write(&path, "draft.txt", "a person's file, not committed");
        write(&path, "notes/scratch.md", "a person's file in a collection");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let before = commits(&path);
        vault
            .create(creation("notes", "a.md", b"A", "n"))
            .await
            .unwrap();
        vault
            .create(creation("log", "e1", b"L", "n"))
            .await
            .unwrap();
        assert_eq!(commits(&path), before + 2);
        assert_eq!(
            read_file(&path, "draft.txt"),
            b"a person's file, not committed"
        );
        assert!(
            vault
                .get(key("notes", "scratch.md"))
                .await
                .unwrap_err()
                .is(Code::NotFound),
            "a file a person did not commit is not a Document"
        );
        assert_eq!(
            read_file(&path, "notes/scratch.md"),
            b"a person's file in a collection"
        );
        assert_settled(&path, &[".scheda/", "draft.txt", "notes/scratch.md"]);
    });
}

#[test]
fn a_document_committed_by_a_person_is_read_replaced_and_deleted() {
    both(|| async {
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
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let got = vault.get(key("notes", "a.md")).await.unwrap();
        assert_eq!(got.raw.as_deref(), Some(&b"by hand"[..]));
        let before = commits(&path);
        let error = vault
            .update(update("notes", "a.md", b"x", Some("stale")))
            .await
            .unwrap_err();
        assert!(error.is(Code::FailedPrecondition));
        vault
            .update(update(
                "notes",
                "a.md",
                b"replaced",
                got.revision.as_deref(),
            ))
            .await
            .unwrap();
        vault.delete(deletion("notes", "a.md")).await.unwrap();
        assert_eq!(commits(&path), before + 2);
        assert_settled(&path, &[".scheda/"]);
    });
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
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-ok").await;
        vault
            .create(creation("notes", "old.md", b"old", "seed"))
            .await
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
        .await
        .unwrap();
        assert_eq!(commits(&path), before + 1);
        assert_eq!(reply.results.len(), 4);
        assert_eq!(stored(&reply.results[0]).raw.as_deref(), Some(&b"A"[..]));
        assert_eq!(stored(&reply.results[0]).revision, Some(blob_id(b"A")));
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
        let none = batch(&vault, Vec::new()).await.unwrap();
        assert!(none.results.is_empty());
        assert_eq!(commits(&path), before + 1);
    });
}

#[test]
fn the_batch_of_one_collection_is_one_commit() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-one").await;
        let before = commits(&path);
        let notes = collection_of(&vault, "notes").await.unwrap();
        let items: Vec<Result<DocumentOperation, Error>> = vec![
            Ok(create_op("notes", "a.md", b"A", "a").operation.unwrap()),
            Ok(create_op("notes", "b.md", b"B", "b").operation.unwrap()),
        ];
        let reply = notes
            .batch(IterStream::new(items.into_iter()))
            .await
            .unwrap();
        assert_eq!(reply.results.len(), 2);
        assert_eq!(commits(&path), before + 1, "one batch is one commit");
        assert_eq!(read_file(&path, "notes/a.md"), b"A");
        assert_eq!(read_file(&path, "notes/b.md"), b"B");
        settled(&path);
    });
}

#[test]
fn a_failing_write_in_the_middle_of_a_batch_stores_nothing_and_names_its_index() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-fail").await;
        vault
            .create(creation("notes", "taken.md", b"t", "seed"))
            .await
            .unwrap();
        let before = commits(&path);
        let snap = snapshot(&path.join("notes"));
        let cases: Vec<(VaultDocumentOperation, Code)> = vec![
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
                    op(write, Operation::Update)
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
            let error = batch(&vault, items).await.unwrap_err();
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
    });
}

fn segments_missing(root: &Path, dir: &str) -> bool {
    !root.join(dir).exists() || segments(root, dir).is_empty()
}

#[test]
fn an_err_item_an_empty_item_and_a_stream_that_breaks_store_nothing() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-err").await;
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
        .await
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
        .await
        .unwrap_err();
        assert!(error.is(Code::Unavailable));
        assert!(error.stack().iter().any(|frame| frame == "operation 2"));
        // An item without a write.
        let error = batch(
            &vault,
            vec![
                first(),
                Ok(VaultDocumentOperation {
                    collection: Some("notes".to_owned()),
                    ..Default::default()
                }),
            ],
        )
        .await
        .unwrap_err();
        assert!(error.is(Code::InvalidArgument));
        assert!(error.stack().iter().any(|frame| frame == "operation 1"));
        assert_eq!(commits(&path), before);
        assert!(!path.join("notes/a.md").exists());
        settled(&path);
    });
}

#[test]
fn two_operations_on_one_document_see_each_other() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-sees").await;
        vault
            .create(creation("notes", "seed.md", b"seed", "seed"))
            .await
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
        .await
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
            Some(blob_id(b"two")),
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
        .await
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
        .await
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
        .await
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
                op(again, Operation::Create),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(stored(&reply.results[1]).raw.as_deref(), Some(&b"x"[..]));
        assert_eq!(commits(&path), before + 2);
        settled(&path);
    });
}

#[test]
fn a_batch_that_changes_nothing_makes_no_commit() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-nothing").await;
        vault
            .create(creation("notes", "a.md", b"same", "seed"))
            .await
            .unwrap();
        vault
            .create(creation("log", "e1", b"line", "seed"))
            .await
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
                op(quiet, Operation::Delete),
                op(again, Operation::Create),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(reply.results.len(), 4);
        assert_eq!(stored(&reply.results[0]).revision, Some(blob_id(b"same")));
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
        .await
        .unwrap();
        assert_eq!(reply.results.len(), 2);
        assert_eq!(commits(&path), before);
        assert!(!path.join("notes/tmp.md").exists());
        settled(&path);
    });
}

#[test]
fn a_batch_of_isolated_collections_is_one_commit_of_the_nested_repository() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-isolated").await;
        let main = commits(&path);
        let reply = batch(
            &vault,
            ok(vec![
                create_op("diary", "one", b"1", "diary one"),
                create_op("trail", "t1", b"line", "trail one"),
                create_op("diary", "two", b"2", "diary two"),
            ]),
        )
        .await
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
        .await
        .unwrap_err();
        assert!(error.is(Code::InvalidArgument));
        assert!(error.stack().iter().any(|frame| frame == "operation 1"));
        assert_eq!(snapshot(&path), snap);
    });
}

#[test]
fn the_trailers_of_a_batch_name_each_document_with_its_own_note() {
    both(|| async {
        let (_scratch, path, vault) = fresh("trailers").await;
        batch(
            &vault,
            ok(vec![
                create_op("notes", "a.md", b"A", "first note\nwith a second line"),
                create_op("log", "e1", b"L", "note for the log"),
                upsert_op("notes", "b.md", b"B", "note \"quoted\""),
            ]),
        )
        .await
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
            .update(update("notes", "a.md", b"A2", None))
            .await
            .unwrap();
        let message = git_out(&path, &["log", "-1", "--format=%B"]);
        assert_eq!(
            message,
            "update\n\nScheda-Note: {\"collection\":\"notes\",\"id\":\"a.md\",\"note\":\"update\"}"
        );
    });
}

// ---------------------------------------------------------------------
// The lock.

#[test]
fn a_call_that_cannot_take_the_lock_in_time_is_aborted_and_stores_nothing() {
    both(|| async {
        let scratch = Scratch::new("timeout");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default().with_write_timeout_ms(150))
            .await
            .unwrap();
        declare(&vault, json!({"name": "notes", "schema": {"standard": {}}})).await;
        let before = commits(&path);
        let held = File::options()
            .read(true)
            .write(true)
            .open(path.join(".scheda/commit.lock"))
            .unwrap();
        held.lock().unwrap();
        for _ in 0..2 {
            let error = vault
                .create(creation("notes", "a.md", b"x", "n"))
                .await
                .unwrap_err();
            assert!(error.is(Code::Aborted));
            assert!(error.retryable());
            let error = batch(&vault, ok(vec![create_op("notes", "a.md", b"x", "n")]))
                .await
                .unwrap_err();
            assert!(error.is(Code::Aborted));
        }
        assert_eq!(commits(&path), before);
        assert!(!path.join("notes/a.md").exists());
        held.unlock().unwrap();
        drop(held);
        assert!(
            vault
                .get(key("notes", "a.md"))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        vault
            .create(creation("notes", "a.md", b"x", "n"))
            .await
            .unwrap();
        assert_eq!(commits(&path), before + 1);
        settled(&path);
    });
}

#[test]
fn sixty_four_document_writes_from_one_handle_are_each_stored_whole() {
    both(|| async {
        let (_scratch, path, vault) = fresh("threads").await;
        let before = commits(&path);
        let handles: Vec<_> = (0..64)
            .map(|n| {
                let vault = vault.clone();
                tokio::spawn(async move {
                    let id = format!("doc-{n}.md");
                    let raw = format!("document {n}");
                    vault
                        .create(creation("notes", &id, raw.as_bytes(), "thread"))
                        .await
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(commits(&path), before + 64);
        settled(&path);
        for n in 0..64 {
            let got = vault
                .get(key("notes", &format!("doc-{n}.md")))
                .await
                .unwrap();
            assert_eq!(got.raw.unwrap(), format!("document {n}").into_bytes());
        }
        // One commit each: no commit holds two Document files.
        let files = git_out(&path, &["log", "--format=", "--name-only", "-n", "64"]);
        assert_eq!(files.lines().filter(|line| !line.is_empty()).count(), 64);
    });
}

#[test]
fn sixty_four_isolated_writes_from_one_handle_are_each_stored_whole() {
    both(|| async {
        let (_scratch, path, vault) = fresh("threads-isolated").await;
        let before = nested_commits(&path);
        let main = commits(&path);
        let handles: Vec<_> = (0..64)
            .map(|n| {
                let vault = vault.clone();
                tokio::spawn(async move {
                    let id = format!("doc-{n}");
                    vault
                        .create(creation("diary", &id, b"x", "thread"))
                        .await
                        .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(nested_commits(&path), before + 64);
        assert_eq!(commits(&path), main);
        settled(&path);
    });
}

const CHILD_VAULT: &str = "SCHEDA_TEST_CHILD_VAULT";
const CHILD_INDEX: &str = "SCHEDA_TEST_CHILD_INDEX";
const PER_CHILD: usize = 22;

/// The role of a child process: write into the Vault of the parent from
/// two tasks of one multi-thread runtime.
async fn child(path: String, index: String) {
    let vault = open(
        Path::new(&path),
        OpenOptions::default().with_write_timeout_ms(120_000),
    )
    .await
    .unwrap();
    let handles: Vec<_> = (0..2)
        .map(|half| {
            let vault = vault.clone();
            let index = index.clone();
            tokio::spawn(async move {
                for n in (half * PER_CHILD / 2)..((half + 1) * PER_CHILD / 2) {
                    let id = format!("p{index}-{n}.md");
                    vault
                        .create(creation("notes", &id, id.as_bytes(), "process"))
                        .await
                        .unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.await.unwrap();
    }
}

#[test]
fn sixty_six_document_writes_from_three_processes_are_each_stored_whole() {
    if let Ok(path) = std::env::var(CHILD_VAULT) {
        // The child runs the body once: a second run would write the same
        // Documents again.
        let index = std::env::var(CHILD_INDEX).unwrap();
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(child(path, index));
        return;
    }
    both(|| async {
        let (_scratch, path, vault) = fresh("processes").await;
        let before = commits(&path);
        let exe = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for index in 0..3 {
            children.push(
                Command::new(&exe)
                    .args([
                        "--exact",
                        "documents::sixty_six_document_writes_from_three_processes_are_each_stored_whole",
                        "--test-threads=1",
                    ])
                    .env(CHILD_VAULT, &path)
                    .env(CHILD_INDEX, index.to_string())
                    .spawn()
                    .unwrap(),
            );
        }
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
        assert_eq!(commits(&path), before + 3 * PER_CHILD);
        settled(&path);
        for index in 0..3 {
            for n in 0..PER_CHILD {
                let id = format!("p{index}-{n}.md");
                let got = vault.get(key("notes", &id)).await.unwrap();
                assert_eq!(got.raw.unwrap(), id.into_bytes());
            }
        }
        assert!(
            command(&path, &["fsck", "--no-progress", "--strict"])
                .status()
                .unwrap()
                .success()
        );
    });
}

// ---------------------------------------------------------------------
// Isolated collections.

#[test]
fn the_nested_repository_is_made_owner_only_by_the_first_write_that_needs_it() {
    both(|| async {
        let (_scratch, path, vault) = fresh("nested").await;
        assert!(
            !path.join(".isolated/.git").exists(),
            "declaring makes no repository"
        );
        let main = commits(&path);
        vault
            .create(creation("diary", "2026/day-1", b"secret", "first"))
            .await
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
            .create(creation("diary", "2026/day-2", b"more", "second"))
            .await
            .unwrap();
        assert_eq!(
            git_out(
                &path.join(".isolated"),
                &["show", "--name-only", "--format=", "HEAD"]
            ),
            "diary/2026/day-2"
        );
    });
}

#[test]
fn a_nested_repository_with_an_origin_is_never_contacted() {
    both(|| async {
        let (_scratch, path, vault) = fresh("origin").await;
        vault
            .create(creation("diary", "a", b"1", "make the nested repository"))
            .await
            .unwrap();
        let nested = path.join(".isolated");
        git(
            &nested,
            &["remote", "add", "origin", "/nonexistent/scheda-origin.git"],
        );
        let start = Instant::now();
        vault
            .create(creation("diary", "b", b"2", "n"))
            .await
            .unwrap();
        vault
            .update(update("diary", "a", b"1b", None))
            .await
            .unwrap();
        vault.delete(deletion("diary", "b")).await.unwrap();
        batch(
            &vault,
            ok(vec![
                create_op("diary", "c", b"3", "n"),
                create_op("trail", "t1", b"line", "n"),
            ]),
        )
        .await
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
    });
}

// ---------------------------------------------------------------------
// Encrypted collections.

#[cfg(feature = "encrypt")]
mod encrypted {
    use super::*;

    /// An age identity made for these tests alone.
    const IDENTITY: &str =
        "AGE-SECRET-KEY-1RN2D82PZQK97437LWJVQJK4Q6EQVHLQ9A8Z0QTAXZ424HV8XZZPS5Q9860";

    async fn sealed(name: &str) -> (Scratch, PathBuf, DynVaultAsync) {
        let scratch = Scratch::new(name);
        let path = scratch.join("vault");
        let options: OpenOptions = serde_json::from_value(json!({
            "encryption": {"identity": IDENTITY},
            "writeTimeoutMs": 60000
        }))
        .unwrap();
        let vault = open(&path, options).await.unwrap();
        declare(
            &vault,
            json!({"name": "secrets", "schema": {"standard": {}, "encrypted": true}}),
        )
        .await;
        declare(
            &vault,
            json!({"name": "vault-config", "schema": {
                "singleton": {"id": "settings"}, "encrypted": true}}),
        )
        .await;
        declare(
            &vault,
            json!({"name": "hidden", "schema": {
                "standard": {}, "encrypted": true, "isolated": true}}),
        )
        .await;
        (scratch, path, vault)
    }

    #[test]
    fn the_table_holds_for_encrypted_collections() {
        both(|| async {
            let (_scratch, path, vault) = sealed("encrypted-table").await;
            let armor_of = |rel: &str| read_file(&path, rel);
            for (c, id, rel, isolated) in [
                ("secrets", "a/b.md", "secrets/a/b.md", false),
                ("vault-config", "settings", "vault-config/settings", false),
                ("hidden", "h", ".isolated/hidden/h", true),
            ] {
                let start = commits_of(&path, isolated);
                let made = vault
                    .create(creation(c, id, b"plain text", "n"))
                    .await
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
                    Some(blob_id(&armor)),
                    "the revision is the hash of the sealed bytes"
                );
                assert_eq!(vault.get(key(c, id)).await.unwrap(), made);
                assert_eq!(commits_of(&path, isolated), start + 1);

                // The same plaintext stores nothing: the file is not sealed again.
                let same = vault.upsert(upsert(c, id, b"plain text")).await.unwrap();
                assert_eq!(same, made);
                let same = vault
                    .update(update(c, id, b"plain text", None))
                    .await
                    .unwrap();
                assert_eq!(same, made);
                assert_eq!(armor_of(rel), armor, "{c}");
                assert_eq!(commits_of(&path, isolated), start + 1);

                // A different one does.
                let changed = vault
                    .update(update(
                        c,
                        id,
                        b"changed",
                        Some(made.revision.as_deref().unwrap()),
                    ))
                    .await
                    .unwrap();
                assert_ne!(changed.revision, made.revision);
                assert_ne!(armor_of(rel), armor);
                assert_eq!(commits_of(&path, isolated), start + 2);
                let error = vault
                    .update(update(c, id, b"x", made.revision.as_deref()))
                    .await
                    .unwrap_err();
                assert!(error.is(Code::FailedPrecondition));

                let kept = vault.create(keeping(c, id, b"other")).await.unwrap();
                assert_eq!(kept, changed);
                vault.delete(deletion(c, id)).await.unwrap();
                assert!(!path.join(rel).exists());
                assert_eq!(commits_of(&path, isolated), start + 3);
                settled(&path);
            }
        });
    }

    #[test]
    fn any_bytes_round_trip_in_an_encrypted_collection() {
        both(|| async {
            let (_scratch, path, vault) = sealed("encrypted-bytes").await;
            let not_utf8: &[u8] = &[0xFF, 0xFE, 0x00, 0xC3, 0x28, 0x80, b'\r', b'\n'];
            for bytes in [PNG, not_utf8, b""] {
                let made = vault.upsert(upsert("secrets", "bin", bytes)).await.unwrap();
                assert_eq!(made.raw.as_deref(), Some(bytes));
                let got = vault.get(key("secrets", "bin")).await.unwrap();
                assert_eq!(got.raw.as_deref(), Some(bytes));
            }
            settled(&path);
        });
    }

    #[test]
    fn a_wrong_secret_is_crypto_and_stores_nothing() {
        both(|| async {
            let (_scratch, path, vault) = sealed("encrypted-wrong").await;
            vault
                .create(creation("secrets", "a", b"x", "n"))
                .await
                .unwrap();
            let before = commits(&path);
            let wrong: OpenOptions =
                serde_json::from_value(json!({"encryption": {"passphrase": "nope"}})).unwrap();
            let other = open(&path, wrong).await.unwrap();
            assert!(
                other
                    .get(key("secrets", "a"))
                    .await
                    .unwrap_err()
                    .is(Code::Crypto)
            );
            assert!(
                other
                    .upsert(upsert("secrets", "a", b"y"))
                    .await
                    .unwrap_err()
                    .is(Code::Crypto)
            );
            assert!(
                other
                    .update(update("secrets", "a", b"y", None))
                    .await
                    .unwrap_err()
                    .is(Code::Crypto)
            );
            assert_eq!(commits(&path), before);
            settled(&path);
        });
    }
}

#[cfg(not(feature = "encrypt"))]
#[test]
fn an_encrypted_collection_in_a_build_without_the_feature_is_unimplemented() {
    both(|| async {
        let (_scratch, path, vault) = fresh("encrypted-thin").await;
        declare(
            &vault,
            json!({"name": "sealed", "schema": {"standard": {}, "encrypted": true}}),
        )
        .await;
        let before = commits(&path);
        assert!(
            vault
                .create(creation("sealed", "a", b"x", "n"))
                .await
                .unwrap_err()
                .is(Code::Unimplemented)
        );
        assert!(
            vault
                .get(key("sealed", "a"))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        assert_eq!(commits(&path), before);
    });
}

// ---------------------------------------------------------------------
// The state token of a write, and of `Vault.Info`.

/// `HEAD` of the repository at `dir`; `None` when it is not there or holds
/// no commit.
fn head_of(dir: &Path) -> Option<String> {
    if !dir.join(".git").exists() {
        return None;
    }
    let out = command(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

/// The state token as git shows it: `HEAD` of the Vault, then `+` and `HEAD`
/// of the nested repository once that holds a commit.
fn token_of(root: &Path) -> String {
    let main = head_of(root).unwrap();
    match head_of(&root.join(".isolated")) {
        Some(nested) => format!("{main}+{nested}"),
        None => main,
    }
}

/// The token a reply carries.
fn version_of(result: &DocumentWriteResult) -> String {
    result.version.clone().unwrap()
}

/// Whether the commit `commit` of the repository at `dir` holds the file
/// `rel`.
fn holds(dir: &Path, commit: &str, rel: &str) -> bool {
    command(dir, &["cat-file", "-e", &format!("{commit}:{rel}")])
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn a_write_returns_the_token_of_the_commit_it_made_in_the_main_repository() {
    both(|| async {
        let (_scratch, path, vault) = fresh("token-main").await;
        let mut seen = vec![token_of(&path)];
        let mut check = |result: DocumentWriteResult| {
            let version = version_of(&result);
            assert_eq!(version, token_of(&path), "the token names the commit made");
            assert_eq!(version, git_out(&path, &["rev-parse", "HEAD"]));
            assert!(
                !seen.contains(&version),
                "a write that stores moves the state"
            );
            seen.push(version);
            result
        };
        let first = check(
            vault
                .create_result(creation("notes", "a.md", b"one", "n"))
                .await
                .unwrap(),
        );
        assert_eq!(
            first.document.as_option().unwrap().raw,
            Some(b"one".to_vec())
        );
        check(
            vault
                .update_result(update("notes", "a.md", b"two", None))
                .await
                .unwrap(),
        );
        check(
            vault
                .upsert_result(upsert("notes", "b.md", b"b"))
                .await
                .unwrap(),
        );
        check(
            vault
                .upsert_result(upsert("config", "settings", b"S"))
                .await
                .unwrap(),
        );
        check(
            vault
                .create_result(creation("log", "l1", b"line", "n"))
                .await
                .unwrap(),
        );
        let deleted = check(
            vault
                .delete_result(deletion("notes", "a.md"))
                .await
                .unwrap(),
        );
        assert!(
            deleted.document.is_unset(),
            "a deletion returns no Document"
        );
        settled(&path);
    });
}

#[test]
fn a_write_to_an_isolated_collection_changes_the_nested_part_of_the_token_only() {
    both(|| async {
        let (_scratch, path, vault) = fresh("token-isolated").await;
        let split = |token: &str| -> (String, Option<String>) {
            match token.split_once('+') {
                Some((main, nested)) => (main.to_owned(), Some(nested.to_owned())),
                None => (token.to_owned(), None),
            }
        };
        let main_write = vault
            .create_result(creation("notes", "a.md", b"a", "n"))
            .await
            .unwrap();
        let (main_one, nested_none) = split(&version_of(&main_write));
        assert_eq!(nested_none, None, "no nested repository yet");
        assert_eq!(version_of(&main_write), token_of(&path));

        // The first isolated write makes the nested repository.
        let isolated = vault
            .create_result(creation("diary", "d1", b"d", "n"))
            .await
            .unwrap();
        assert_eq!(version_of(&isolated), token_of(&path));
        let (main_two, nested_one) = split(&version_of(&isolated));
        assert_eq!(main_two, main_one, "the Vault repository did not move");
        assert_eq!(
            nested_one.as_deref(),
            head_of(&path.join(".isolated")).as_deref()
        );

        // An isolated journal, in the same repository.
        let trail = vault
            .create_result(creation("trail", "t1", b"line", "n"))
            .await
            .unwrap();
        let (main_three, nested_two) = split(&version_of(&trail));
        assert_eq!(main_three, main_one);
        assert_ne!(nested_two, nested_one);
        assert_eq!(version_of(&trail), token_of(&path));

        // A write to the main repository keeps the nested part of the token.
        let again = vault
            .create_result(creation("notes", "b.md", b"b", "n"))
            .await
            .unwrap();
        let (main_four, nested_three) = split(&version_of(&again));
        assert_ne!(main_four, main_one);
        assert_eq!(nested_three, nested_two);
        assert_eq!(version_of(&again), token_of(&path));

        // An update and a deletion in the nested repository.
        let updated = vault
            .update_result(update("diary", "d1", b"dd", None))
            .await
            .unwrap();
        assert_eq!(version_of(&updated), token_of(&path));
        let deleted = vault.delete_result(deletion("diary", "d1")).await.unwrap();
        assert!(deleted.document.is_unset());
        assert_eq!(version_of(&deleted), token_of(&path));
        assert_ne!(version_of(&deleted), version_of(&updated));
        settled(&path);
    });
}

#[test]
fn a_write_that_stores_nothing_returns_the_token_of_the_state_it_found() {
    both(|| async {
        let (_scratch, path, vault) = fresh("token-nothing").await;
        vault
            .create_result(creation("notes", "a.md", b"one", "n"))
            .await
            .unwrap();
        vault
            .create_result(creation("diary", "d1", b"d", "n"))
            .await
            .unwrap();
        let found = token_of(&path);
        let commits_main = commits(&path);
        let commits_nested = nested_commits(&path);

        let kept = vault
            .create_result(keeping("notes", "a.md", b"other"))
            .await
            .unwrap();
        assert_eq!(version_of(&kept), found);
        assert_eq!(
            kept.document.as_option().unwrap().raw,
            Some(b"one".to_vec())
        );
        let same = vault
            .update_result(update("notes", "a.md", b"one", None))
            .await
            .unwrap();
        assert_eq!(version_of(&same), found);
        let upserted = vault
            .upsert_result(upsert("notes", "a.md", b"one"))
            .await
            .unwrap();
        assert_eq!(version_of(&upserted), found);
        let missing = {
            let mut write = deletion("notes", "absent.md");
            write.1.ignore_missing = Some(true);
            vault.delete_result(write).await.unwrap()
        };
        assert_eq!(version_of(&missing), found);
        assert!(missing.document.is_unset());
        // The same in the nested repository, and with it the token holds both parts.
        let kept = vault
            .create_result(keeping("diary", "d1", b"other"))
            .await
            .unwrap();
        assert_eq!(version_of(&kept), found);
        let same = vault
            .upsert_result(upsert("diary", "d1", b"d"))
            .await
            .unwrap();
        assert_eq!(version_of(&same), found);
        assert!(
            found.contains('+'),
            "the token holds the nested commit: {found}"
        );
        assert_eq!(commits(&path), commits_main);
        assert_eq!(nested_commits(&path), commits_nested);

        // A commit made by hand is part of the state a later no-op finds.
        git(
            &path,
            &["commit", "--quiet", "--allow-empty", "--message", "by hand"],
        );
        let by_hand = token_of(&path);
        assert_ne!(by_hand, found);
        let after = vault
            .upsert_result(upsert("notes", "a.md", b"one"))
            .await
            .unwrap();
        assert_eq!(version_of(&after), by_hand);
        assert_eq!(commits(&path), commits_main + 1);
    });
}

#[test]
fn a_batch_returns_the_token_of_its_commit_and_an_empty_one_the_state_now() {
    both(|| async {
        let (_scratch, path, vault) = fresh("token-batch").await;
        let made = batch(
            &vault,
            ok(vec![
                create_op("notes", "a.md", b"A", "a"),
                create_op("log", "l1", b"line", "b"),
                upsert_op("config", "settings", b"S", "c"),
            ]),
        )
        .await
        .unwrap();
        assert_eq!(made.version.as_deref(), Some(token_of(&path).as_str()));
        assert_eq!(
            made.version.as_deref(),
            Some(git_out(&path, &["rev-parse", "HEAD"]).as_str())
        );

        // Nothing to store: the token found, and no commit.
        let before = commits(&path);
        let same = batch(&vault, ok(vec![upsert_op("notes", "a.md", b"A", "again")]))
            .await
            .unwrap();
        assert_eq!(same.version, made.version);
        // No write at all: the state now, read as `Vault.Info` reads it.
        let empty = batch(&vault, Vec::new()).await.unwrap();
        assert!(empty.results.is_empty());
        assert_eq!(empty.version.as_deref(), Some(token_of(&path).as_str()));
        assert_eq!(empty.version, made.version);
        git(
            &path,
            &["commit", "--quiet", "--allow-empty", "--message", "by hand"],
        );
        let moved = batch(&vault, Vec::new()).await.unwrap();
        assert_eq!(moved.version.as_deref(), Some(token_of(&path).as_str()));
        assert_ne!(moved.version, made.version);
        let before = before + 1;
        assert_eq!(commits(&path), before);

        // A batch of one collection.
        let notes = collection_of(&vault, "notes").await.unwrap();
        let items: Vec<Result<DocumentOperation, Error>> = vec![Ok(DocumentOperation {
            operation: Some(Operation::Create(Box::new(
                creation("notes", "b.md", b"B", "b").1,
            ))),
            ..Default::default()
        })];
        let one = notes
            .batch(IterStream::new(items.into_iter()))
            .await
            .unwrap();
        assert_eq!(one.version.as_deref(), Some(token_of(&path).as_str()));
        assert_ne!(one.version, made.version);

        // A batch of isolated collections: the nested commit is in the token.
        let isolated = batch(
            &vault,
            ok(vec![
                create_op("diary", "one", b"1", "n"),
                create_op("trail", "t1", b"line", "n"),
            ]),
        )
        .await
        .unwrap();
        let token = isolated.version.unwrap();
        assert_eq!(token, token_of(&path));
        let (main, nested) = token.split_once('+').unwrap();
        assert_eq!(Some(main.to_owned()), head_of(&path));
        assert_eq!(Some(nested.to_owned()), head_of(&path.join(".isolated")));
    });
}

/// The token `Vault.Info` gives.
async fn info_token(vault: &DynVaultAsync) -> String {
    vault.info(Empty::default()).await.unwrap().version.unwrap()
}

#[test]
fn vault_info_gives_the_token_of_head_now() {
    both(|| async {
        let (_scratch, path, vault) = fresh("token-info").await;
        assert_eq!(info_token(&vault).await, token_of(&path));
        let made = vault
            .create_result(creation("notes", "a.md", b"A", "n"))
            .await
            .unwrap();
        assert_eq!(info_token(&vault).await, version_of(&made));

        // A commit made by hand moves the token, with no write of the library.
        git(
            &path,
            &["commit", "--quiet", "--allow-empty", "--message", "by hand"],
        );
        assert_eq!(info_token(&vault).await, token_of(&path));
        assert_ne!(info_token(&vault).await, version_of(&made));

        // The nested repository joins it once it has a commit.
        let isolated = vault
            .create_result(creation("diary", "d1", b"d", "n"))
            .await
            .unwrap();
        assert!(info_token(&vault).await.contains('+'));
        assert_eq!(info_token(&vault).await, version_of(&isolated));
        assert_eq!(info_token(&vault).await, token_of(&path));

        // Another handle on the path reads the same state.
        let other = open(&path, OpenOptions::default()).await.unwrap();
        assert_eq!(info_token(&other).await, info_token(&vault).await);
    });
}

/// Each token of a write names the commit that write made, and no later
/// one: the commit it names holds the file and its parent does not.
#[test]
fn the_token_of_a_write_names_its_own_commit_among_many_writers() {
    both(|| async {
        let (_scratch, path, vault) = fresh("token-threads").await;
        const EACH: usize = 8;
        let mut workers = Vec::new();
        for n in 0..EACH {
            for isolated in [false, true] {
                let vault = vault.clone();
                workers.push(tokio::spawn(async move {
                    let (collection, id) = if isolated {
                        ("diary", format!("d{n}"))
                    } else {
                        ("notes", format!("n{n}.md"))
                    };
                    let result = vault
                        .create_result(creation(collection, &id, b"x", "thread"))
                        .await
                        .unwrap();
                    (version_of(&result), id, isolated)
                }));
            }
        }
        let mut tokens: Vec<(String, String, bool)> = Vec::new();
        for worker in workers {
            tokens.push(worker.await.unwrap());
        }
        let nested = path.join(".isolated");
        for (token, id, isolated) in &tokens {
            let (main, nested_part) = match token.split_once('+') {
                Some((main, nested)) => (main.to_owned(), Some(nested.to_owned())),
                None => (token.clone(), None),
            };
            if *isolated {
                let commit = nested_part
                    .clone()
                    .expect("an isolated write has a nested part");
                assert!(holds(&nested, &commit, &format!("diary/{id}")));
                assert!(
                    !holds(&nested, &format!("{commit}^"), &format!("diary/{id}")),
                    "{id} was made by the commit its token names"
                );
            } else {
                assert!(holds(&path, &main, &format!("notes/{id}")));
                assert!(
                    !holds(&path, &format!("{main}^"), &format!("notes/{id}")),
                    "{id} was made by the commit its token names"
                );
            }
            // Every part of every token is a commit that is still in the history.
            let reaches = |dir: &Path, commit: &str| {
                command(dir, &["merge-base", "--is-ancestor", commit, "HEAD"])
                    .status()
                    .unwrap()
                    .success()
            };
            assert!(reaches(&path, &main));
            if let Some(part) = &nested_part {
                assert!(reaches(&nested, part));
            }
        }
        settled(&path);
    });
}

#[test]
fn an_empty_batch_waits_for_no_lock_and_answers_with_the_state_now() {
    both(|| async {
        let scratch = Scratch::new("token-empty-locked");
        let path = scratch.join("vault");
        // A short deadline: a batch that waited for the lock would be
        // `ABORTED` after two seconds, and the test would fail on it.
        let vault = open(&path, OpenOptions::default().with_write_timeout_ms(2_000))
            .await
            .unwrap();
        declare(&vault, json!({"name": "notes", "schema": {"standard": {}}})).await;
        let before = commits(&path);
        let held = File::options()
            .read(true)
            .write(true)
            .open(path.join(".scheda/commit.lock"))
            .unwrap();
        held.lock().unwrap();

        let started = Instant::now();
        let in_vault = batch(&vault, Vec::new()).await.unwrap();
        let notes = collection_of(&vault, "notes").await.unwrap();
        let in_collection = notes
            .batch(IterStream::new(
                Vec::<Result<DocumentOperation, Error>>::new().into_iter(),
            ))
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(1_500),
            "an empty batch does not wait for the lock"
        );
        for reply in [in_vault, in_collection] {
            assert!(reply.results.is_empty());
            assert_eq!(reply.version.as_deref(), Some(token_of(&path).as_str()));
        }
        assert_eq!(commits(&path), before);

        // A batch that writes still waits for the lock, and is refused at the
        // deadline.
        let error = batch(&vault, ok(vec![create_op("notes", "a.md", b"x", "n")]))
            .await
            .unwrap_err();
        assert!(error.is(Code::Aborted));
        held.unlock().unwrap();
        drop(held);
    });
}
