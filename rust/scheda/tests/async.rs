//! The async form of the objects (feature `tokio`): `Scheda` opens a Vault
//! through `SchedaAsync`, and the references it gives hold the async form
//! of the Vault, its collections, and its mirror. They run their calls on
//! the blocking pool and wait natively where a thread would only stand
//! idle: the turn and the lock file of a write.
//!
//! What counts blob reads in the async form is only reachable from inside
//! the library: that is a unit test in `src/service/asynchronous.rs`.
#![cfg(feature = "tokio")]

mod common;

use std::fs::File;
use std::future::{Future, poll_fn};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use protocontract::{IterStream, Stream};
use scheda::v1::document_operation::Operation;
use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionDeletion, CollectionKey, DocumentCreation,
    DocumentId, DocumentOperation, DocumentSelection, DocumentUpdate, DocumentUpsert,
    DynCollectionAsync, DynVaultAsync, Listing, MirrorAsync, OpenOptions, SchedaAsync, SchedaSync,
    VaultAsync, VaultDocumentOperation, VaultErrorCode as Code, VaultOpening,
};
use scheda::{Error, Scheda};
use serde_json::json;
use tokio::runtime::{Builder, Runtime};

use common::{Scratch, open, opening, snapshot, vault as built_vault};

/// How long a step may take before the test gives up on it.
const DEADLINE: Duration = Duration::from_secs(60);

/// A runtime of `workers` worker threads (a current-thread runtime for 0)
/// whose blocking pool has `pool` threads.
fn runtime(workers: usize, pool: usize) -> Runtime {
    let mut builder = if workers == 0 {
        Builder::new_current_thread()
    } else {
        let mut builder = Builder::new_multi_thread();
        builder.worker_threads(workers);
        builder
    };
    builder
        .max_blocking_threads(pool)
        .enable_all()
        .build()
        .unwrap()
}

fn options() -> OpenOptions {
    OpenOptions::default()
        .with_index_poll_ms(10)
        .with_write_timeout_ms(20_000)
}

/// A scratch directory with the Vault of the other tests built in it.
fn fresh(name: &str) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(name);
    let path = scratch.join("v");
    built_vault(&path);
    (scratch, path)
}

/// Opens the Vault `request` asks for through `SchedaAsync`.
async fn open_request(request: VaultOpening) -> DynVaultAsync {
    SchedaAsync::open(&Scheda, request)
        .await
        .unwrap()
        .to_async_offload()
        .unwrap()
}

async fn open_async(path: &Path, options: OpenOptions) -> DynVaultAsync {
    open_request(opening(path, options)).await
}

async fn collection(vault: &DynVaultAsync, name: &str) -> DynCollectionAsync {
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .await
        .unwrap()
        .to_async_offload()
        .unwrap()
}

/// The next item of `stream`.
async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
}

/// Every item of `stream`, unwrapped.
async fn all<T>(stream: impl Stream<Item = Result<T, Error>>) -> Vec<T> {
    let mut stream = Box::pin(stream);
    let mut items = Vec::new();
    while let Some(item) = next(&mut stream).await {
        items.push(item.unwrap());
    }
    items
}

fn creation(id: &str, raw: &str) -> DocumentCreation {
    DocumentCreation::default()
        .with_id(id)
        .with_raw(raw.as_bytes().to_vec())
        .with_note("test")
}

fn upsert(id: &str, raw: &str) -> DocumentUpsert {
    DocumentUpsert::default()
        .with_id(id)
        .with_raw(raw.as_bytes().to_vec())
        .with_note("test")
}

fn id(id: &str) -> DocumentId {
    DocumentId::default().with_id(id)
}

fn listing() -> DocumentSelection {
    DocumentSelection {
        include_raw: Some(true),
        selection: Some(Selection::Listing(Box::<Listing>::default())),
        ..DocumentSelection::default()
    }
}

fn declaration(name: &str) -> CollectionDeclaration {
    serde_json::from_value(json!({"name": name, "schema": {"standard": {}}})).unwrap()
}

/// A batch of operations as the stream a call takes.
fn stream_of<T: Send + 'static>(items: Vec<T>) -> IterStream<std::vec::IntoIter<Result<T, Error>>> {
    IterStream::new(items.into_iter().map(Ok).collect::<Vec<_>>().into_iter())
}

fn operation(operation: Operation) -> DocumentOperation {
    DocumentOperation {
        operation: Some(operation),
        ..DocumentOperation::default()
    }
}

/// Takes the write lock of the Vault at `path` as another process would.
fn hold_write_lock(path: &Path) -> File {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".scheda/commit.lock"))
        .unwrap();
    file.lock().unwrap();
    file
}

/// Runs `future` with the time the test allows it.
async fn within<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, future)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"))
}

/// The write path, the reads, and the streams of the Vault, the collection,
/// and the mirror in the async form.
async fn round_trip() {
    let (_scratch, path) = fresh("async-round-trip");
    let vault = open_async(&path, options()).await;
    let info = vault.info(Empty::default()).await.unwrap();
    assert_eq!(
        info.path.as_deref(),
        Some(path.canonicalize().unwrap().to_str().unwrap())
    );
    let notes = collection(&vault, "notes").await;
    assert_eq!(
        notes
            .describe(Empty::default())
            .await
            .unwrap()
            .name
            .as_deref(),
        Some("notes")
    );

    notes.create(creation("a.md", "one")).await.unwrap();
    notes.upsert(upsert("b.md", "two")).await.unwrap();
    let updated = notes
        .update(
            DocumentUpdate::default()
                .with_id("a.md")
                .with_raw(b"uno".to_vec())
                .with_note("test"),
        )
        .await
        .unwrap();
    assert_eq!(updated.raw.as_deref(), Some(&b"uno"[..]));
    assert_eq!(
        notes.get(id("a.md")).await.unwrap().raw.as_deref(),
        Some(&b"uno"[..])
    );
    assert!(
        notes
            .get(id("zzz.md"))
            .await
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert!(
        notes
            .create(creation("a.md", "again"))
            .await
            .unwrap_err()
            .is(Code::AlreadyExists)
    );

    let batch = notes
        .batch(stream_of(vec![
            operation(Operation::Upsert(Box::new(upsert("c.md", "three")))),
            operation(Operation::Delete(Box::new(
                scheda::v1::DocumentDeletion::default()
                    .with_id("b.md")
                    .with_note("test"),
            ))),
        ]))
        .await
        .unwrap();
    assert_eq!(batch.results.len(), 2);

    let documents = all(notes.get_many(listing()).await.unwrap()).await;
    let ids: Vec<_> = documents.iter().map(|d| d.id.clone().unwrap()).collect();
    assert_eq!(ids, ["a.md", "c.md"]);
    let history = all(notes.history(id("a.md")).await.unwrap()).await;
    assert_eq!(history.len(), 2, "the creation and the update");

    // The Vault writes too: a collection, then Documents in two.
    vault.create_collection(declaration("extra")).await.unwrap();
    assert_eq!(
        vault
            .list_collections(Empty::default())
            .await
            .unwrap()
            .collections
            .len(),
        3
    );
    let across = vault
        .batch_documents(stream_of(vec![
            VaultDocumentOperation {
                collection: Some("notes".to_owned()),
                operation: operation(Operation::Upsert(Box::new(upsert("d.md", "four")))).into(),
                ..VaultDocumentOperation::default()
            },
            VaultDocumentOperation {
                collection: Some("extra".to_owned()),
                operation: operation(Operation::Upsert(Box::new(upsert("e.md", "five")))).into(),
                ..VaultDocumentOperation::default()
            },
        ]))
        .await
        .unwrap();
    assert_eq!(across.results.len(), 2);
    let extra = collection(&vault, "extra").await;
    assert_eq!(
        extra.get(id("e.md")).await.unwrap().raw.as_deref(),
        Some(&b"five"[..])
    );
    assert!(
        vault
            .delete_collection(CollectionDeletion::default().with_name("extra"))
            .await
            .unwrap_err()
            .is(Code::FailedPrecondition),
        "a collection that holds Documents stays"
    );
    assert!(
        vault
            .get_collection(CollectionKey::default().with_name("missing"))
            .await
            .err()
            .unwrap()
            .is(Code::NotFound)
    );

    let mirror = vault
        .mirror(Empty::default())
        .await
        .unwrap()
        .to_async_offload()
        .unwrap();
    assert_eq!(
        mirror
            .info(Empty::default())
            .await
            .unwrap()
            .url
            .as_deref()
            .unwrap_or(""),
        ""
    );
    assert!(
        mirror
            .push(Empty::default())
            .await
            .unwrap_err()
            .is(Code::NotFound),
        "no mirror to push to"
    );

    assert!(
        vault
            .check(Empty::default())
            .await
            .unwrap()
            .problems
            .is_empty()
    );
    vault.close(Empty::default()).await.unwrap();
}

#[test]
fn the_async_objects_work_on_a_multi_thread_runtime() {
    runtime(2, 4).block_on(round_trip());
}

#[test]
fn the_async_objects_work_on_a_current_thread_runtime() {
    runtime(0, 4).block_on(round_trip());
}

#[test]
fn a_refused_open_keeps_its_code() {
    runtime(0, 4).block_on(async {
        let error = SchedaAsync::open(&Scheda, VaultOpening::default())
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument));
        let info = SchedaAsync::info(&Scheda, Empty::default()).await.unwrap();
        assert_eq!(info.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    });
}

#[test]
fn the_calls_this_version_does_not_have_are_unimplemented_in_the_async_form() {
    let (_scratch, path) = fresh("async-unimplemented");
    runtime(2, 4).block_on(async {
        let vault = open_async(&path, options()).await;
        let notes = collection(&vault, "notes").await;
        let before = snapshot(&path);
        let unimplemented = |error: Error| assert!(error.is(Code::Unimplemented), "{error}");
        unimplemented(notes.query(Default::default()).await.err().unwrap());
        unimplemented(notes.count(Default::default()).await.unwrap_err());
        unimplemented(notes.search(Default::default()).await.err().unwrap());
        unimplemented(
            vault
                .query_documents(Default::default())
                .await
                .err()
                .unwrap(),
        );
        unimplemented(vault.count_documents(Default::default()).await.unwrap_err());
        unimplemented(
            vault
                .search_documents(Default::default())
                .await
                .err()
                .unwrap(),
        );
        unimplemented(vault.index(Empty::default()).await.err().unwrap());
        assert_eq!(snapshot(&path), before, "a refused call writes nothing");
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn a_sync_caller_takes_the_sync_form_of_an_async_reference_and_the_reverse() {
    let (_scratch, path) = fresh("async-forms");
    let rt = runtime(2, 4);
    // An async reference, called from a plain thread in its sync form.
    let reference = rt.block_on(async {
        SchedaAsync::open(&Scheda, opening(&path, options()))
            .await
            .unwrap()
    });
    let sync = {
        let _inside = rt.enter();
        reference.to_sync().unwrap()
    };
    let notes = sync
        .get_collection(CollectionKey::default().with_name("notes"))
        .unwrap();
    let notes = {
        let _inside = rt.enter();
        notes.to_sync().unwrap()
    };
    notes.upsert(upsert("a.md", "sync")).unwrap();
    assert_eq!(
        notes.get(id("a.md")).unwrap().raw.as_deref(),
        Some(&b"sync"[..])
    );
    sync.close(Empty::default()).unwrap();

    // A Vault the sync open made, called in the async form: protocontract's
    // generic form, which runs every call on the blocking pool.
    let sync = open(&path, options()).unwrap();
    rt.block_on(async {
        let vault = SchedaSync::open(&Scheda, opening(&path, options()))
            .unwrap()
            .to_async_offload()
            .unwrap();
        let notes = collection(&vault, "notes").await;
        assert_eq!(
            notes.get(id("a.md")).await.unwrap().raw.as_deref(),
            Some(&b"sync"[..])
        );
        vault.close(Empty::default()).await.unwrap();
    });
    sync.close(Empty::default()).unwrap();
}

#[test]
fn after_close_every_async_object_is_failed_precondition_and_a_second_close_is_nothing() {
    let (_scratch, path) = fresh("async-closed");
    runtime(2, 4).block_on(async {
        let vault = open_async(&path, options()).await;
        let notes = collection(&vault, "notes").await;
        notes.upsert(upsert("a.md", "a")).await.unwrap();
        let mirror = vault
            .mirror(Empty::default())
            .await
            .unwrap()
            .to_async_offload()
            .unwrap();

        vault.close(Empty::default()).await.unwrap();

        let closed = |error: Error| assert!(error.is(Code::FailedPrecondition), "{error}");
        closed(vault.info(Empty::default()).await.unwrap_err());
        closed(
            vault
                .get_collection(CollectionKey::default().with_name("notes"))
                .await
                .err()
                .unwrap(),
        );
        closed(vault.mirror(Empty::default()).await.err().unwrap());
        closed(
            vault
                .create_collection(declaration("more"))
                .await
                .unwrap_err(),
        );
        closed(notes.get(id("a.md")).await.unwrap_err());
        closed(notes.upsert(upsert("b.md", "b")).await.unwrap_err());
        closed(mirror.info(Empty::default()).await.unwrap_err());
        vault.close(Empty::default()).await.unwrap();
    });
    // The write before the close is stored; another Vault reads it.
    let again = open(&path, options()).unwrap();
    let notes = again
        .get_collection(CollectionKey::default().with_name("notes"))
        .unwrap()
        .to_sync()
        .unwrap();
    assert_eq!(
        notes.get(id("a.md")).unwrap().raw.as_deref(),
        Some(&b"a"[..])
    );
}

#[test]
fn a_write_waiting_for_the_write_lock_holds_no_pool_thread_and_aborts_at_the_deadline() {
    let (_scratch, path) = fresh("async-lock");
    // One thread in the blocking pool: whatever waits for the lock on it
    // would stop every other call.
    let rt = runtime(2, 1);
    rt.block_on(async {
        let vault = open_async(&path, options()).await;
        let notes = collection(&vault, "notes").await;
        let held = hold_write_lock(&path);
        let waiting = tokio::spawn({
            let notes = notes.clone();
            async move { notes.upsert(upsert("a.md", "late")).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiting.is_finished(), "the write waits for the lock");
        // The single pool thread is free: a read gets through.
        let started = Instant::now();
        assert!(
            within("a read while a write waits", notes.get(id("none.md")))
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        // So does a write to another handle of the Vault... which waits for
        // the same lock: the vault-level write holds no thread either.
        let other = tokio::spawn({
            let vault = vault.clone();
            async move { vault.create_collection(declaration("more")).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!other.is_finished());
        assert!(
            within("info while two writes wait", vault.info(Empty::default()))
                .await
                .is_ok()
        );

        drop(held);
        within("the first write", waiting).await.unwrap().unwrap();
        within("the second write", other).await.unwrap().unwrap();
        assert_eq!(
            notes.get(id("a.md")).await.unwrap().raw.as_deref(),
            Some(&b"late"[..])
        );
        vault.close(Empty::default()).await.unwrap();
    });

    // At the deadline the write is ABORTED, and nothing is written.
    let rt = runtime(2, 1);
    let before = rt.block_on(async {
        let vault = open_async(&path, options().with_write_timeout_ms(300)).await;
        let notes = collection(&vault, "notes").await;
        let held = hold_write_lock(&path);
        let before = snapshot(&path);
        let error = notes.upsert(upsert("b.md", "never")).await.unwrap_err();
        assert!(error.is(Code::Aborted), "{error}");
        drop(held);
        vault.close(Empty::default()).await.unwrap();
        before
    });
    assert_eq!(snapshot(&path), before, "an aborted write stores nothing");
}

#[test]
fn a_request_that_can_never_succeed_fails_at_once_while_the_write_lock_is_held() {
    let (_scratch, path) = fresh("async-prechecks");
    runtime(2, 2).block_on(async {
        let vault = open_async(&path, options().with_write_timeout_ms(5_000)).await;
        let notes = collection(&vault, "notes").await;
        vault.create_collection(declaration("gone")).await.unwrap();
        let gone = collection(&vault, "gone").await;
        vault
            .delete_collection(CollectionDeletion::default().with_name("gone"))
            .await
            .unwrap();

        let held = hold_write_lock(&path);
        let before = snapshot(&path);
        // Each of these would wait the five seconds out and come back
        // `ABORTED` (retryable) if the checks ran after the wait.
        let started = Instant::now();
        let no_note = DocumentCreation::default()
            .with_id("a.md")
            .with_raw(b"a".to_vec());
        let error = notes.create(no_note).await.unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let error = gone.upsert(upsert("a.md", "a")).await.unwrap_err();
        assert!(error.is(Code::NotFound), "{error}");
        let bad = DocumentUpsert::default()
            .with_id("b.md")
            .with_raw(b"b".to_vec());
        let error = notes
            .batch(stream_of(vec![
                operation(Operation::Upsert(Box::new(upsert("a.md", "a")))),
                operation(Operation::Upsert(Box::new(bad))),
            ]))
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let error = vault
            .create_collection(declaration("Not A Slug!"))
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the refusals waited for the lock"
        );
        drop(held);
        assert_eq!(snapshot(&path), before, "a refused write stores nothing");
        vault.close(Empty::default()).await.unwrap();
    });
}
