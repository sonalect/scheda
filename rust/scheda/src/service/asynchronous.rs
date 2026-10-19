//! The async form of the objects: what `Scheda` gives through
//! `SchedaAsync::open`.
//!
//! Each object shares the state, the gate, and the `Close` of the sync
//! objects of [`super`]; it differs in how a call waits.
//!
//! - A call that only works runs the sync call on the blocking pool of the
//!   runtime the caller is in, and a stream is pulled there and handed over
//!   through a channel that holds 16 items.
//! - A wait is native and holds no thread: a write waits for its turn and
//!   for the lock file with an `await` between two tries and writes on the
//!   blocking pool once it has the lock.
//! - A call made through a reference whose Vault the sync `Scheda.open`
//!   opened is not here at all: that Vault is native to the sync form, and
//!   protocontract's generic `Offload` is its async form.
//!
//! The mirror stays offloaded whole: `Pull` and `Sync` take the write lock
//! on a pool thread, and the network is theirs to wait on.

use std::sync::Arc;

use buffa_types::google::protobuf::Empty;
use protocontract::{Inline, Offload, RuntimeCode, Stream};
use scheda_api::v1::collection_ref::CollectionRef;
use scheda_api::v1::embedder_ref::EmbedderRef;
use scheda_api::v1::mirror_ref::MirrorRef;
use scheda_api::v1::projector_ref::ProjectorRef;
use scheda_api::v1::reranker_ref::RerankerRef;
use scheda_api::v1::vault_ref::VaultRef;
use scheda_api::v1::{
    CheckReport, CollectionAsync, CollectionBatchResult, CollectionDeclaration, CollectionDeletion,
    CollectionKey, CollectionOperation, Collections, Count, Document, DocumentBatchResult,
    DocumentCreation, DocumentDeletion, DocumentHistoryEntry, DocumentId, DocumentOperation,
    DocumentQuery, DocumentSearch, DocumentSelection, DocumentUpdate, DocumentUpsert,
    DocumentWriteResult, MirrorAsync, MirrorInfo, MirrorSetting, QueryItem, SchedaAsync,
    SchedaInfo, SearchItem, VaultAsync, VaultDocumentOperation, VaultInfo, VaultOpening,
    VaultQuery, VaultSearch,
};
use tokio::runtime::Handle;

use super::{CollectionObject, MirrorObject, Scheda, Shared, VaultObject, open_shared};
use crate::collections::Writes as CollectionWrites;
use crate::commit::Held;
use crate::document_calls::Writes as DocWrites;
use crate::error::{Context, Error};
use crate::vault::Vault;

/// The runtime the caller is in.
fn current() -> Result<Handle, Error> {
    Handle::try_current().map_err(|_| {
        Error::new(
            RuntimeCode::NoRuntime,
            "the async form of a Vault runs inside a Tokio runtime",
        )
    })
}

/// `service` run on the blocking pool of the runtime the caller is in now.
/// It is the runtime of the call, not the one that opened the Vault, so a
/// Vault outlives the runtime that opened it.
fn offload<T>(service: &Arc<T>) -> Result<Offload<T>, Error> {
    Ok(Offload::from_arc(Arc::clone(service), current()?))
}

/// Runs `step` on the blocking pool, inside the gate: `FAILED_PRECONDITION`
/// once the Vault is closed.
async fn in_gate<T: Send + 'static>(
    shared: &Arc<Shared>,
    step: impl FnOnce(&Vault) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    let pool = offload(shared)?;
    Offload::call(&pool, move |shared| shared.call(step)).await
}

/// Writes under the write lock, which is waited for in the task. First the
/// checks that need no lock run on the blocking pool (`check`), as the sync
/// call runs them before it waits: a request that can never succeed fails at
/// once with its own code, not `ABORTED` at the deadline. Then the turn among
/// the writes of the handle and the lock file are polled with an `await`
/// between two tries, so no thread is held while another writer has them.
/// The write itself (`run`, which checks again against the state in force
/// under the lock) runs on the blocking pool with the lock in hand.
async fn write<W: Send + 'static, T: Send + 'static>(
    shared: &Arc<Shared>,
    writes: W,
    check: impl FnOnce(&Vault, W) -> Result<W, Error> + Send + 'static,
    run: impl FnOnce(&Vault, Held, W) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    let writes = in_gate(shared, move |vault| check(vault, writes)).await?;
    let held = shared
        .vault
        .committer()
        .acquire_async()
        .await
        .context("take the write lock")?;
    in_gate(shared, move |vault| run(vault, held, writes)).await
}

/// A document write: the checks, then the write.
async fn write_document(
    shared: &Arc<Shared>,
    writes: DocWrites,
) -> Result<DocumentWriteResult, Error> {
    write(
        shared,
        writes,
        |vault, writes| vault.precheck_documents(writes),
        |vault, held, writes| vault.write_document_held(held, writes),
    )
    .await
}

/// A document batch: the checks, then the write. A batch with no write
/// stores nothing and waits for no lock: it answers with the state now.
async fn write_documents(
    shared: &Arc<Shared>,
    writes: DocWrites,
) -> Result<DocumentBatchResult, Error> {
    if writes.is_empty() {
        return in_gate(shared, move |vault| vault.write_no_documents(writes)).await;
    }
    write(
        shared,
        writes,
        |vault, writes| vault.precheck_documents(writes),
        |vault, held, writes| vault.write_documents_held(held, writes),
    )
    .await
}

/// A collection write: the checks, then the write.
async fn write_collection(
    shared: &Arc<Shared>,
    writes: CollectionWrites,
) -> Result<CollectionDeclaration, Error> {
    write(
        shared,
        writes,
        |_, writes| writes.check(),
        |vault, held, writes| vault.write_collection_held(held, writes),
    )
    .await
}

/// The sync call `name` of an object, on the blocking pool.
macro_rules! offloaded {
    ($service:ident, $field:ident; $($name:ident($arg:ident: $request:ty) -> $reply:ty;)*) => {
        $(
            async fn $name(&self, $arg: $request) -> Result<$reply, Error> {
                $service::$name(&offload(&self.$field)?, $arg).await
            }
        )*
    };
}

/// The sync call `name` of an object that returns a stream, on the blocking
/// pool.
macro_rules! offloaded_streams {
    ($service:ident, $field:ident; $($name:ident($arg:ident: $request:ty) -> $item:ty;)*) => {
        $(
            async fn $name(
                &self,
                $arg: $request,
            ) -> Result<impl Stream<Item = Result<$item, Error>> + Send + use<>, Error> {
                $service::$name(&offload(&self.$field)?, $arg).await
            }
        )*
    };
}

impl SchedaAsync for Scheda {
    async fn info(&self, empty: Empty) -> Result<SchedaInfo, Error> {
        <Scheda as scheda_api::v1::SchedaSync>::info(self, empty)
    }

    async fn open(&self, vault_opening: VaultOpening) -> Result<VaultRef, Error> {
        let pool = Offload::new(*self, current()?);
        let shared = Offload::call(&pool, move |_| open_shared(vault_opening)).await?;
        Ok(VaultRef::from_async(AsyncVault::new(shared)))
    }
}

/// The async object of one open Vault.
struct AsyncVault {
    sync: Arc<VaultObject>,
}

impl AsyncVault {
    fn new(shared: Arc<Shared>) -> AsyncVault {
        AsyncVault {
            sync: Arc::new(VaultObject { shared }),
        }
    }

    fn shared(&self) -> &Arc<Shared> {
        &self.sync.shared
    }
}

impl VaultAsync for AsyncVault {
    offloaded! {
        VaultAsync, sync;
        info(empty: Empty) -> VaultInfo;
        check(empty: Empty) -> CheckReport;
        close(empty: Empty) -> Empty;
        set_projector(projector_ref: ProjectorRef) -> Empty;
        remove_projector(empty: Empty) -> Empty;
        set_embedder(embedder_ref: EmbedderRef) -> Empty;
        remove_embedder(empty: Empty) -> Empty;
        set_reranker(reranker_ref: RerankerRef) -> Empty;
        remove_reranker(empty: Empty) -> Empty;
        list_collections(empty: Empty) -> Collections;
        count_documents(vault_query: VaultQuery) -> Count;
    }

    async fn mirror(&self, _empty: Empty) -> Result<MirrorRef, Error> {
        self.shared().call(|_| {
            Ok(MirrorRef::from_async(AsyncMirror {
                sync: Arc::new(MirrorObject {
                    shared: Arc::clone(self.shared()),
                }),
            }))
        })
    }

    async fn index(&self, _empty: Empty) -> Result<scheda_api::v1::index_ref::IndexRef, Error> {
        VaultAsync::index(&offload(&self.sync)?, Empty::default()).await
    }

    async fn get_collection(&self, collection_key: CollectionKey) -> Result<CollectionRef, Error> {
        let declaration = in_gate(self.shared(), move |vault| {
            vault.get_collection(collection_key)
        })
        .await?;
        Ok(CollectionRef::from_async(AsyncCollection {
            sync: Arc::new(CollectionObject {
                shared: Arc::clone(self.shared()),
                name: declaration.name.unwrap_or_default(),
            }),
        }))
    }

    async fn create_collection(
        &self,
        collection_declaration: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        write_collection(
            self.shared(),
            CollectionWrites::create(collection_declaration),
        )
        .await
    }

    async fn update_collection(
        &self,
        collection_declaration: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        write_collection(
            self.shared(),
            CollectionWrites::update(collection_declaration),
        )
        .await
    }

    async fn delete_collection(
        &self,
        collection_deletion: CollectionDeletion,
    ) -> Result<Empty, Error> {
        write_collection(self.shared(), CollectionWrites::delete(collection_deletion))
            .await
            .map(|_| Empty::default())
    }

    async fn batch_collections<R>(
        &self,
        collection_operations: R,
    ) -> Result<CollectionBatchResult, Error>
    where
        R: Stream<Item = Result<CollectionOperation, Error>> + Send + 'static,
    {
        let operations = Inline::<()>::buffer(collection_operations).await;
        write(
            self.shared(),
            CollectionWrites::of_operations(operations)?,
            |_, writes| writes.check(),
            |vault, held, writes| vault.write_collections_held(held, writes),
        )
        .await
    }

    async fn batch_documents<R>(
        &self,
        vault_document_operations: R,
    ) -> Result<DocumentBatchResult, Error>
    where
        R: Stream<Item = Result<VaultDocumentOperation, Error>> + Send + 'static,
    {
        let operations = Inline::<()>::buffer(vault_document_operations).await;
        write_documents(self.shared(), DocWrites::of_vault(operations)?).await
    }

    offloaded_streams! {
        VaultAsync, sync;
        query_documents(vault_query: VaultQuery) -> QueryItem;
        search_documents(vault_search: VaultSearch) -> SearchItem;
    }
}

/// The async object of one collection of an open Vault.
struct AsyncCollection {
    sync: Arc<CollectionObject>,
}

impl AsyncCollection {
    fn shared(&self) -> &Arc<Shared> {
        &self.sync.shared
    }
}

impl CollectionAsync for AsyncCollection {
    offloaded! {
        CollectionAsync, sync;
        describe(empty: Empty) -> CollectionDeclaration;
        get(document_id: DocumentId) -> Document;
        count(document_query: DocumentQuery) -> Count;
    }

    offloaded_streams! {
        CollectionAsync, sync;
        get_many(document_selection: DocumentSelection) -> Document;
        history(document_id: DocumentId) -> DocumentHistoryEntry;
        query(document_query: DocumentQuery) -> QueryItem;
        search(document_search: DocumentSearch) -> SearchItem;
    }

    async fn create(
        &self,
        document_creation: DocumentCreation,
    ) -> Result<DocumentWriteResult, Error> {
        let name = self.sync.name.clone();
        write_document(self.shared(), DocWrites::one(&name, document_creation)).await
    }

    async fn update(&self, document_update: DocumentUpdate) -> Result<DocumentWriteResult, Error> {
        let name = self.sync.name.clone();
        write_document(self.shared(), DocWrites::one(&name, document_update)).await
    }

    async fn upsert(&self, document_upsert: DocumentUpsert) -> Result<DocumentWriteResult, Error> {
        let name = self.sync.name.clone();
        write_document(self.shared(), DocWrites::one(&name, document_upsert)).await
    }

    async fn delete(
        &self,
        document_deletion: DocumentDeletion,
    ) -> Result<DocumentWriteResult, Error> {
        let name = self.sync.name.clone();
        write_document(self.shared(), DocWrites::one(&name, document_deletion)).await
    }

    async fn batch<R>(&self, document_operations: R) -> Result<DocumentBatchResult, Error>
    where
        R: Stream<Item = Result<DocumentOperation, Error>> + Send + 'static,
    {
        let name = self.sync.name.clone();
        let operations = Inline::<()>::buffer(document_operations).await;
        write_documents(self.shared(), DocWrites::of_collection(&name, operations)?).await
    }
}

/// The async object of the mirror of an open Vault.
struct AsyncMirror {
    sync: Arc<MirrorObject>,
}

impl MirrorAsync for AsyncMirror {
    offloaded! {
        MirrorAsync, sync;
        info(empty: Empty) -> MirrorInfo;
        set(mirror_setting: MirrorSetting) -> Empty;
        remove(empty: Empty) -> Empty;
        push(empty: Empty) -> Empty;
        pull(empty: Empty) -> Empty;
        sync(empty: Empty) -> Empty;
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::path::PathBuf;
    use std::pin::Pin;

    use scheda_api::v1::document_selection::Selection;
    use scheda_api::v1::{
        DocumentId, DocumentSelection, DocumentUpsert, Listing, OpenOptions, SchedaAsync,
        VaultOpening,
    };
    use tokio::runtime::Builder;

    use super::*;
    use crate::repo::blob_reads;

    /// A scratch directory removed when the test ends.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// How many Documents (or versions) the streams run through: more than
    /// the 16 a stream moved between threads holds ahead of its reader.
    const MANY: usize = 60;

    /// Blobs read ahead of the reader at most: the first item, the 16 the
    /// channel holds, and the one the pulling thread has in hand.
    const AHEAD: u64 = 18;

    async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
        poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
    }

    /// The blobs read so far by the one thread of the blocking pool, which
    /// runs every call and pulls every stream of the Vault.
    async fn pool_reads() -> u64 {
        tokio::task::spawn_blocking(blob_reads)
            .await
            .unwrap_or(u64::MAX)
    }

    #[test]
    fn a_stream_dropped_after_its_first_item_reads_no_more_in_the_async_form() {
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let scratch = Scratch(base.join(format!("scheda-async-lazy-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&scratch.0);
        let runtime = Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let opening = VaultOpening {
                path: Some(scratch.0.to_string_lossy().into_owned()),
                options: OpenOptions::default().with_write_timeout_ms(20_000).into(),
                ..VaultOpening::default()
            };
            let vault = SchedaAsync::open(&Scheda, opening)
                .await
                .unwrap()
                .to_async_offload()
                .unwrap();
            vault
                .create_collection(
                    serde_json::from_value(
                        serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            let notes = vault
                .get_collection(CollectionKey::default().with_name("notes"))
                .await
                .unwrap()
                .to_async_offload()
                .unwrap();
            for n in 0..MANY {
                let upsert = |id: &str, text: String| {
                    DocumentUpsert::default()
                        .with_id(id)
                        .with_raw(text.into_bytes())
                        .with_note("test")
                };
                notes
                    .upsert(upsert(&format!("d{n:03}.md"), format!("document {n}")))
                    .await
                    .unwrap();
                notes
                    .upsert(upsert("long.md", format!("version {n}")))
                    .await
                    .unwrap();
            }

            // A listing: one Document is read for the first item, and a
            // dropped stream lets the pulling thread go with a few more.
            let selection = DocumentSelection {
                include_raw: Some(true),
                selection: Some(Selection::Listing(Box::<Listing>::default())),
                ..DocumentSelection::default()
            };
            let before = pool_reads().await;
            let mut stream = Box::pin(notes.get_many(selection).await.unwrap());
            let first = next(&mut stream).await.unwrap().unwrap();
            assert_eq!(first.id.as_deref(), Some("d000.md"));
            drop(stream);
            // The read queues behind the pulling task, which ends when its
            // reader is gone.
            let read = pool_reads().await - before;
            assert!(read >= 1, "the first Document was read");
            assert!(read <= AHEAD, "a dropped listing read {read} Documents");

            // A history: the same, a blob per version.
            let before = pool_reads().await;
            let mut stream = Box::pin(
                notes
                    .history(DocumentId::default().with_id("long.md"))
                    .await
                    .unwrap(),
            );
            next(&mut stream).await.unwrap().unwrap();
            drop(stream);
            let read = pool_reads().await - before;
            assert!(read >= 1, "the first version was read");
            assert!(read <= AHEAD, "a dropped history read {read} versions");
            vault.close(Empty::default()).await.unwrap();
        });
    }
}
