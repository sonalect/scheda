//! The async form of the objects (feature `tokio`): `Scheda` opens a Vault
//! through `SchedaAsync`, and the references it gives hold the async form
//! of the Vault, its collections, its mirror, and its Index. They run their
//! calls on the blocking pool and wait natively where a thread would only
//! stand idle: `Index.Watch`, the turn and the lock file of a write, and
//! the embedder and the reranker of a search.
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

/// Whether nobody holds the lock file `name` under `.scheda/`.
#[cfg(feature = "find")]
fn free(path: &Path, name: &str) -> bool {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".scheda").join(name))
        .unwrap();
    file.try_lock().is_ok()
}

/// Waits until the Index counts `wanted` Documents in `notes`.
#[cfg(feature = "find")]
async fn until_counted(notes: &DynCollectionAsync, wanted: u64) {
    let query: scheda::v1::DocumentQuery =
        serde_json::from_value(json!({"filter": {"all": {}}})).unwrap();
    let started = Instant::now();
    loop {
        // An Index not built yet refuses the count: look again.
        if let Ok(count) = notes.count(query.clone()).await
            && count.count == Some(wanted)
        {
            return;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "the Index never counted {wanted}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

/// A Projector of one text column, and an Index with nothing in it.
#[cfg(feature = "find")]
mod find {
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::{
        CollectionKey, IndexAsync, IndexDoc, IndexSchema, IndexState, ProjectorDescriptor,
        ProjectorSync, RawDocument, VaultSync,
    };

    use super::*;

    /// A Projector of no column: the Index holds the Documents' keys.
    pub struct Bare;

    impl ProjectorSync for Bare {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("bare@1"))
        }

        fn columns(&self, _collection_key: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema::default())
        }

        fn project(&self, _raw_document: RawDocument) -> Result<IndexDoc, Error> {
            Ok(IndexDoc::default())
        }
    }

    pub fn indexing(path: &Path) -> VaultOpening {
        let mut request = opening(path, options());
        request.projector = ProjectorRef::from_sync(Bare).into();
        request
    }

    pub async fn index_of(vault: &DynVaultAsync) -> scheda::v1::DynIndexAsync {
        vault
            .index(Empty::default())
            .await
            .unwrap()
            .to_async_offload()
            .unwrap()
    }

    /// Reads the Index through `watch` until `done` holds.
    pub async fn watch_until(
        index: &scheda::v1::DynIndexAsync,
        what: &str,
        done: impl Fn(&IndexState) -> bool,
    ) {
        let mut watch = Box::pin(index.watch(Empty::default()).await.unwrap());
        loop {
            let state = within(what, next(&mut watch))
                .await
                .expect("the watch does not end")
                .unwrap();
            if done(&state) {
                return;
            }
        }
    }

    pub fn built(state: &IndexState) -> bool {
        state.version.is_some() && state.pending == Some(0)
    }

    /// The Index's `version` as a Vault that indexes nothing reads it.
    pub fn version(vault: &std::sync::Arc<dyn VaultSync>) -> Option<String> {
        vault
            .index(Empty::default())
            .unwrap()
            .to_sync()
            .unwrap()
            .status(Empty::default())
            .unwrap()
            .version
    }

    /// The version once it has not moved for a while: the reader sees what
    /// other handles reach a poll late, so a single read may lag.
    pub fn settled(vault: &std::sync::Arc<dyn VaultSync>) -> Option<String> {
        let mut last = version(vault);
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_millis(300) {
            std::thread::sleep(Duration::from_millis(20));
            let now = version(vault);
            if now != last {
                last = now;
                since = Instant::now();
            }
        }
        last
    }

    pub fn wait_version_changes(
        vault: &std::sync::Arc<dyn VaultSync>,
        before: &Option<String>,
        what: &str,
    ) {
        let deadline = Instant::now() + DEADLINE;
        while version(vault) == *before {
            assert!(Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(feature = "find")]
mod watching {
    use scheda::v1::{DocumentQuery, IndexAsync, VaultSync};

    use super::find::{
        built, index_of, indexing, settled, version, wait_version_changes, watch_until,
    };
    use super::*;

    #[test]
    fn an_open_watch_holds_no_pool_thread_and_wakes_on_a_change() {
        let (_scratch, path) = fresh("async-watch");
        let rt = runtime(2, 1);
        rt.block_on(async {
            let vault = open_request(indexing(&path)).await;
            let index = index_of(&vault).await;
            let mut watch = Box::pin(index.watch(Empty::default()).await.unwrap());
            let first = within("the first item", next(&mut watch))
                .await
                .unwrap()
                .unwrap();
            // The stream waits for news: another call still gets the one
            // pool thread.
            let notes = collection(&vault, "notes").await;
            within("a write beside a watch", notes.upsert(upsert("a.md", "a")))
                .await
                .unwrap();
            // The watch is woken by the indexer, with no thread waiting.
            loop {
                let state = within("a change", next(&mut watch)).await.unwrap().unwrap();
                if built(&state) && state.version != first.version {
                    break;
                }
            }
            // The write reaches the Index, and a count over it still runs on
            // the one pool thread.
            within("the Index counting", until_counted(&notes, 1)).await;
            // A query is a stream the pool pulls.
            let found = all(notes
                .query(
                    serde_json::from_value::<DocumentQuery>(json!({"filter": {"all": {}}}))
                        .unwrap(),
                )
                .await
                .unwrap())
            .await;
            assert_eq!(found.len(), 1);
            vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn a_held_watch_gets_failed_precondition_after_close_and_then_ends() {
        let (_scratch, path) = fresh("async-watch-close");
        runtime(2, 1).block_on(async {
            let vault = open_request(indexing(&path)).await;
            let index = index_of(&vault).await;
            watch_until(&index, "the first round", |state| state.version.is_some()).await;

            // One watch held and not read, one parked in a task.
            let mut held = Box::pin(index.watch(Empty::default()).await.unwrap());
            next(&mut held).await.unwrap().unwrap();
            let mut parked = Box::pin(index.watch(Empty::default()).await.unwrap());
            next(&mut parked).await.unwrap().unwrap();
            let reader = tokio::spawn(async move {
                let mut last = None;
                while let Some(item) = next(&mut parked).await {
                    last = Some(item);
                }
                last
            });
            tokio::time::sleep(Duration::from_millis(100)).await;

            vault.close(Empty::default()).await.unwrap();

            let last = within("the parked watch", reader).await.unwrap();
            let error = last.expect("an item after close").unwrap_err();
            assert!(error.is(Code::FailedPrecondition));
            let error = next(&mut held).await.unwrap().unwrap_err();
            assert!(error.is(Code::FailedPrecondition));
            assert!(next(&mut held).await.is_none(), "the stream ends after it");
            assert!(
                index
                    .status(Empty::default())
                    .await
                    .unwrap_err()
                    .is(Code::FailedPrecondition)
            );
        });
    }

    /// Open `path` in a runtime that is dropped afterwards, with the Index
    /// of the Vault built; returns the reference, a sync reader of the
    /// same Vault that indexes nothing, and the runtime it will be used in.
    fn orphan(
        path: &Path,
    ) -> (
        scheda::v1::vault_ref::VaultRef,
        std::sync::Arc<dyn VaultSync>,
    ) {
        let first = runtime(2, 2);
        let reference = first.block_on(async {
            let reference = SchedaAsync::open(&Scheda, indexing(path)).await.unwrap();
            let vault = reference.to_async_offload().unwrap();
            vault.create_collection(declaration("extra")).await.unwrap();
            watch_until(&index_of(&vault).await, "the first round", built).await;
            reference
        });
        // A Vault without ports never indexes; it reads what the other one
        // keeps.
        let reader = open(path, options()).unwrap();
        // The runtime that opened the Vault ends first.
        drop(first);
        (reference, reader)
    }

    fn write_by_reader(reader: &std::sync::Arc<dyn VaultSync>, id_: &str) {
        reader
            .get_collection(CollectionKey::default().with_name("notes"))
            .unwrap()
            .to_sync()
            .unwrap()
            .upsert(upsert(id_, id_))
            .unwrap();
    }

    #[test]
    fn the_indexer_outlives_the_runtime_that_opened_the_vault_and_ends_with_close() {
        let (_scratch, path) = fresh("async-indexer-close");
        let (reference, reader) = orphan(&path);

        // The thread does not belong to the runtime: it still indexes.
        let before = settled(&reader);
        write_by_reader(&reader, "a.md");
        wait_version_changes(&reader, &before, "the write indexed");
        settled(&reader);

        // `Close` from a plain thread, through the sync form, in a runtime
        // made after the first one went.
        let second = runtime(2, 2);
        let sync = {
            let _inside = second.enter();
            reference.to_sync().unwrap()
        };
        sync.close(Empty::default()).unwrap();
        assert!(free(&path, "index.lock"), "the indexer holds no lock");
        let before = settled(&reader);
        write_by_reader(&reader, "b.md");
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(version(&reader), before, "a closed Vault indexes nothing");
    }

    #[test]
    fn the_indexer_ends_when_every_reference_is_dropped_after_the_runtime_went() {
        let (_scratch, path) = fresh("async-indexer-dropped");
        let (reference, reader) = orphan(&path);
        let before = settled(&reader);
        write_by_reader(&reader, "a.md");
        wait_version_changes(&reader, &before, "the write indexed");
        settled(&reader);

        drop(reference);
        std::thread::sleep(Duration::from_millis(100));
        assert!(free(&path, "index.lock"));
        let before = settled(&reader);
        write_by_reader(&reader, "b.md");
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(version(&reader), before, "no Vault indexes");
    }
}

#[cfg(feature = "search")]
mod searching {
    use protocontract::BoxIter;
    use protocontract::RuntimeCode;
    use scheda::v1::embedder_ref::EmbedderRef;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::search_item::Item;
    use scheda::v1::{
        ColumnKind, DocumentSearch, EmbedderAsync, EmbedderDescriptor, EmbedderSync, IndexColumn,
        IndexDoc, IndexSchema, IndexValue, ProjectorDescriptor, ProjectorSync, Query, RawDocument,
        SearchItem, Text, Vector, index_value,
    };

    use super::find::{built, index_of, watch_until};
    use super::*;

    /// A Projector of JSON Documents with a searched `title` and an
    /// embedded `topic`.
    struct Docs;

    impl ProjectorSync for Docs {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("docs@1"))
        }

        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            let text = ColumnKind::COLUMN_KIND_TEXT;
            Ok(IndexSchema {
                columns: vec![
                    IndexColumn::default()
                        .with_name("title")
                        .with_kind(text)
                        .with_find(true)
                        .with_fts(true),
                    IndexColumn::default()
                        .with_name("topic")
                        .with_kind(text)
                        .with_vector(true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let json: serde_json::Value =
                serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
            let mut doc = IndexDoc::default();
            for name in ["title", "topic"] {
                if let Some(text) = json.get(name).and_then(serde_json::Value::as_str) {
                    doc.values.insert(
                        name.to_owned(),
                        IndexValue {
                            value: Some(index_value::Value::Text(text.to_owned())),
                            ..IndexValue::default()
                        },
                    );
                }
            }
            Ok(doc)
        }
    }

    /// The vector of a text: how many words of two topics it holds.
    fn topic_vector(text: &str) -> Vec<f32> {
        let topics: [&[&str]; 2] = [&["cat", "kitten"], &["car", "engine"]];
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut vector: Vec<f32> = topics
            .iter()
            .map(|topic| words.iter().filter(|word| topic.contains(word)).count() as f32)
            .collect();
        vector.push(0.05);
        vector
    }

    fn descriptor() -> EmbedderDescriptor {
        EmbedderDescriptor::default()
            .with_model_id("topics@1")
            .with_dimensions(3)
    }

    /// A blocking embedder; it panics on a query when told to.
    struct Blocking {
        panics: bool,
    }

    impl EmbedderSync for Blocking {
        fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(descriptor())
        }

        fn embed(
            &self,
            texts: BoxIter<'static, Result<Text, Error>>,
        ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
            Ok(Box::new(texts.map(|text| {
                Ok(Vector {
                    values: topic_vector(&text?.text.unwrap_or_default()),
                    ..Vector::default()
                })
            })))
        }

        fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            assert!(!self.panics, "the embedder panics");
            Ok(Vector {
                values: topic_vector(&query.text.unwrap_or_default()),
                ..Vector::default()
            })
        }
    }

    /// An async embedder that answers a query after `delay`, or panics.
    struct Remote {
        delay: Duration,
        panics: bool,
    }

    impl EmbedderAsync for Remote {
        async fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(descriptor())
        }

        async fn embed<R>(
            &self,
            texts: R,
        ) -> Result<impl Stream<Item = Result<Vector, Error>> + Send + use<R>, Error>
        where
            R: Stream<Item = Result<Text, Error>> + Send + 'static,
        {
            let mut texts = Box::pin(texts);
            let mut vectors = Vec::new();
            while let Some(text) = next(&mut texts).await {
                vectors.push(text.map(|text| Vector {
                    values: topic_vector(&text.text.unwrap_or_default()),
                    ..Vector::default()
                }));
            }
            Ok(IterStream::new(vectors.into_iter()))
        }

        async fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            tokio::time::sleep(self.delay).await;
            assert!(!self.panics, "the remote embedder panics");
            Ok(Vector {
                values: topic_vector(&query.text.unwrap_or_default()),
                ..Vector::default()
            })
        }
    }

    /// A Vault with the Index of three notes built, a blocking embedder
    /// installed.
    async fn corpus(path: &Path) -> (DynVaultAsync, DynCollectionAsync) {
        let mut request = opening(path, options());
        request.projector = ProjectorRef::from_sync(Docs).into();
        request.embedder = EmbedderRef::from_sync(Blocking { panics: false }).into();
        let vault = open_request(request).await;
        let notes = collection(&vault, "notes").await;
        for (name, title, topic) in [
            ("cat1", "Cat care", "cat kitten"),
            ("cat2", "Kitten food", "kitten"),
            ("car1", "Car repair", "car engine"),
        ] {
            let raw = json!({"title": title, "topic": topic}).to_string();
            notes.upsert(upsert(name, &raw)).await.unwrap();
        }
        watch_until(&index_of(&vault).await, "the Index built", built).await;
        until_counted(&notes, 3).await;
        (vault, notes)
    }

    fn search(text: &str) -> DocumentSearch {
        serde_json::from_value(json!({"text": text, "k": 3})).unwrap()
    }

    fn hit_ids(items: &[SearchItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item.item.as_ref() {
                Some(Item::Hit(hit)) => hit.document.as_option().and_then(|d| d.id.clone()),
                _ => None,
            })
            .collect()
    }

    async fn search_all(notes: &DynCollectionAsync, text: &str) -> Vec<SearchItem> {
        all(notes.search(search(text)).await.unwrap()).await
    }

    #[test]
    fn a_search_runs_on_both_runtime_flavors() {
        for workers in [0, 2] {
            let (_scratch, path) = fresh("async-search");
            runtime(workers, 4).block_on(async {
                let (vault, notes) = corpus(&path).await;
                let items = search_all(&notes, "kitten").await;
                let ids = hit_ids(&items);
                assert!(ids.contains(&"cat1".to_owned()), "{ids:?}");
                assert!(!ids.contains(&"car1".to_owned()) || ids.len() == 3);
                // Across the Vault, too.
                let request = serde_json::from_value(json!({
                    "collections": {"names": ["notes"]},
                    "search": {"text": "engine", "k": 1},
                }))
                .unwrap();
                let items = all(vault.search_documents(request).await.unwrap()).await;
                assert_eq!(hit_ids(&items), ["car1"]);
                vault.close(Empty::default()).await.unwrap();
            });
        }
    }

    #[test]
    fn an_async_embedder_that_takes_a_second_is_awaited_not_blocked_on() {
        let (_scratch, path) = fresh("async-search-slow");
        // One pool thread: a search that held it while the model answers
        // would stop the read beside it.
        let rt = runtime(2, 1);
        rt.block_on(async {
            let (vault, notes) = corpus(&path).await;
            vault
                .set_embedder(EmbedderRef::from_async(Remote {
                    delay: Duration::from_secs(1),
                    panics: false,
                }))
                .await
                .unwrap();
            let started = Instant::now();
            let searching = tokio::spawn({
                let notes = notes.clone();
                async move { search_all(&notes, "kitten").await }
            });
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!searching.is_finished(), "the embedder is still answering");
            let read_started = Instant::now();
            within("a read beside the search", notes.get(id("cat1")))
                .await
                .unwrap();
            assert!(
                read_started.elapsed() < Duration::from_millis(500),
                "the read waited for the search"
            );
            let items = within("the search", searching).await.unwrap();
            assert!(started.elapsed() >= Duration::from_secs(1));
            assert!(hit_ids(&items).contains(&"cat1".to_owned()));
            // `Close` does not wait for a model either.
            let again = tokio::spawn({
                let notes = notes.clone();
                async move { notes.search(search("kitten")).await.map(|_| ()) }
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
            let closing = Instant::now();
            vault.close(Empty::default()).await.unwrap();
            assert!(closing.elapsed() < Duration::from_millis(700));
            let error = within("the search over a closed Vault", again)
                .await
                .unwrap()
                .unwrap_err();
            assert!(error.is(Code::FailedPrecondition), "{error}");
        });
    }

    #[test]
    fn a_panicking_port_is_panicked_whether_it_is_async_or_blocking() {
        let (_scratch, path) = fresh("async-search-panic");
        runtime(2, 2).block_on(async {
            let (vault, notes) = corpus(&path).await;
            for port in [
                EmbedderRef::from_async(Remote {
                    delay: Duration::ZERO,
                    panics: true,
                }),
                EmbedderRef::from_sync(Blocking { panics: true }),
            ] {
                vault.set_embedder(port).await.unwrap();
                let error = notes.search(search("kitten")).await.err().unwrap();
                assert!(error.is(RuntimeCode::Panicked), "{error}");
            }
            // The Vault works after the panics.
            vault
                .set_embedder(EmbedderRef::from_sync(Blocking { panics: false }))
                .await
                .unwrap();
            assert!(hit_ids(&search_all(&notes, "kitten").await).contains(&"cat1".to_owned()));
            vault.close(Empty::default()).await.unwrap();
        });
    }
}
