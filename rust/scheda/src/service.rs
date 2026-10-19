//! The services of the contract: [`Scheda`], the root a caller makes, and
//! the objects an open Vault gives, each behind a reference: the Vault
//! itself, one collection, and the mirror. They are async only. This version
//! of Scheda has no Index: `Vault.Index`, the queries, the counts, and the
//! searches answer `UNIMPLEMENTED`.
//!
//! The objects of one Vault share it and its gate. Every call enters the
//! gate first; `Vault.Close` shuts it, waits for the calls inside to end,
//! and stops the indexer. A stream is not a call: each item it makes enters
//! the gate on its own, so `Close` waits for an item being made, not for a
//! caller who holds a stream, and the next item after it is
//! `FAILED_PRECONDITION`. A Vault whose references are all dropped (its
//! collections', mirror's, and its streams, included) ends as `Close` would,
//! without waiting.
//!
//! A call waits without a thread (for the write lock, for `git`, for
//! another `open`) and does its work in blocking steps on the blocking pool
//! of the runtime of the call. A call that is dropped stops at its next
//! `await`: the step it is in, if any, runs to its end, and no step after it
//! starts.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use buffa_types::google::protobuf::Empty;
use protocontract::{Inline, IterStream, Stream};
use scheda_api::v1::collection_ref::CollectionRef;
use scheda_api::v1::embedder_ref::EmbedderRef;
use scheda_api::v1::index_ref::IndexRef;
use scheda_api::v1::mirror_ref::MirrorRef;
use scheda_api::v1::projector_ref::ProjectorRef;
use scheda_api::v1::reranker_ref::RerankerRef;
use scheda_api::v1::vault_ref::VaultRef;
use scheda_api::v1::{
    CheckReport, CollectionAsync, CollectionBatchResult, CollectionDeclaration, CollectionDeletion,
    CollectionKey, CollectionOperation, Collections, Count, Document, DocumentBatchResult,
    DocumentCreation, DocumentDeletion, DocumentHistoryEntry, DocumentId, DocumentOperation,
    DocumentQuery, DocumentSearch, DocumentSelection, DocumentUpdate, DocumentUpsert,
    DocumentWriteResult, Feature, MirrorAsync, MirrorInfo, MirrorSetting, QueryItem, SchedaAsync,
    SchedaInfo, SearchItem, VaultAsync, VaultDocumentOperation, VaultErrorCode, VaultInfo,
    VaultOpening, VaultQuery, VaultSearch,
};
use scheda_blocking::Blocking;

use crate::collections::Writes as CollectionWrites;
use crate::document_calls::Writes as DocumentWrites;
use crate::error::{Context, Error, Fault};
use crate::gate::Pass;
use crate::mirror;
use crate::ports::Ports;
use crate::stored;
use crate::vault::Vault;

/// Scheda: the root of the contract. A process makes one (it holds
/// nothing) and opens every Vault through it.
///
/// [`SchedaAsync::open`] returns a [`VaultRef`]: a reference to the open
/// Vault, from which the caller takes the form it needs
/// (`vault.to_async_inline()?` gives a `DynVaultAsync`). The Vault gives its
/// collections and its mirror the same way. The objects behind the
/// references are Scheda's own, and async only; a caller works through the
/// traits.
///
/// Scheda runs on the tokio runtime of its caller, with its time and I/O
/// drivers on (`Builder::enable_all`), of either flavor: it never blocks a
/// thread of the runtime. A Vault lives on the runtime that opened it: its
/// indexer is a task there. A caller with no async code of its own builds a
/// multi-thread runtime and takes the blocking form of a reference with
/// `to_sync()` inside it: protocontract's `Blocking` blocks the calling
/// thread until each call is done. On a current-thread runtime that form
/// refuses with `CANNOT_BLOCK` instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scheda;

impl SchedaAsync for Scheda {
    async fn info(&self, _empty: Empty) -> Result<SchedaInfo, Error> {
        // The Index is not listed: this version of Scheda answers none of its
        // calls, with or without the `index` feature.
        let mut features = Vec::new();
        if cfg!(feature = "encrypt") {
            features.push(Feature::FEATURE_ENCRYPT.into());
        }
        Ok(SchedaInfo {
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            features,
            ..SchedaInfo::default()
        })
    }

    async fn open(&self, vault_opening: VaultOpening) -> Result<VaultRef, Error> {
        Ok(VaultRef::from_async(VaultObject {
            vault: open(vault_opening).await?,
        }))
    }
}

/// Opens the Vault `vault_opening` asks for.
async fn open(mut vault_opening: VaultOpening) -> Result<Arc<Vault>, Error> {
    stored::validate(&vault_opening).context("Scheda.Open: check the request")?;
    let path = vault_opening.path.take().unwrap_or_default();
    let options = vault_opening.options.take().unwrap_or_default();
    let ports = Ports {
        projector: vault_opening.projector.take(),
        embedder: vault_opening.embedder.take(),
        reranker: vault_opening.reranker.take(),
    };
    Vault::open(PathBuf::from(path), options, ports).await
}

/// The failure of a call this version of Scheda does not have.
fn unavailable(call: &str) -> Error {
    Fault::new(
        VaultErrorCode::Unimplemented,
        format!("{call} is not available in this version of Scheda"),
    )
    .into()
}

/// The stream type of a call that always fails before it streams.
type NoStream<T> = IterStream<std::iter::Empty<Result<T, Error>>>;

/// Documents written under the write lock: the checks that need no lock
/// first, as one step, so a request that can never succeed fails at once
/// with its own code, not `ABORTED` at the deadline; then the wait for the
/// lock; then the write as one step, which checks again against the state
/// in force under the lock; then, once the lock is let go, the push of
/// `auto_sync`.
pub(crate) async fn write_document(
    pass: &Pass,
    writes: DocumentWrites,
) -> Result<DocumentWriteResult, Error> {
    let writes = pass
        .step(move |cx, vault| vault.precheck_documents(cx, writes))
        .await?;
    let held = pass
        .vault()
        .committer()
        .acquire()
        .await
        .context("take the write lock")?;
    let (result, pushable) = pass
        .step(move |cx, vault| vault.write_document_held(cx, held, writes))
        .await?;
    if pushable {
        mirror::push_after_write(pass).await;
    }
    Ok(result)
}

/// A batch of Documents, as [`write_document`]. A batch with no write
/// stores nothing and waits for no lock: it answers with the state now.
pub(crate) async fn write_documents(
    pass: &Pass,
    writes: DocumentWrites,
) -> Result<DocumentBatchResult, Error> {
    if writes.is_empty() {
        return pass
            .step(move |cx, vault| vault.write_no_documents(cx, writes))
            .await;
    }
    let writes = pass
        .step(move |cx, vault| vault.precheck_documents(cx, writes))
        .await?;
    let held = pass
        .vault()
        .committer()
        .acquire()
        .await
        .context("take the write lock")?;
    let (result, pushable) = pass
        .step(move |cx, vault| vault.write_documents_held(cx, held, writes))
        .await?;
    if pushable {
        mirror::push_after_write(pass).await;
    }
    Ok(result)
}

/// One collection written: the checks that need no state, the wait for the
/// lock, the write as one step, then the push of `auto_sync`.
pub(crate) async fn write_collection(
    pass: &Pass,
    writes: CollectionWrites,
) -> Result<CollectionDeclaration, Error> {
    let writes = writes.check()?;
    let held = pass
        .vault()
        .committer()
        .acquire()
        .await
        .context("take the write lock")?;
    let (result, pushable) = pass
        .step(move |cx, vault| vault.write_collection_held(cx, held, writes))
        .await?;
    if pushable {
        mirror::push_after_write(pass).await;
    }
    Ok(result)
}

/// A batch of collections, as [`write_collection`]. A batch with no write
/// stores nothing and waits for no lock.
pub(crate) async fn write_collections(
    pass: &Pass,
    writes: CollectionWrites,
) -> Result<CollectionBatchResult, Error> {
    let writes = writes.check()?;
    if writes.is_empty() {
        return Ok(CollectionBatchResult::default());
    }
    let held = pass
        .vault()
        .committer()
        .acquire()
        .await
        .context("take the write lock")?;
    let (result, pushable) = pass
        .step(move |cx, vault| vault.write_collections_held(cx, held, writes))
        .await?;
    if pushable {
        mirror::push_after_write(pass).await;
    }
    Ok(result)
}

/// The next item of a stream of `S`, made in a blocking step.
type Next<S, T> = fn(&Blocking, &Vault, &mut S) -> Option<Result<T, Error>>;

/// The step a stream waits for: the state back, and the item it made.
type Pending<S, T> =
    Pin<Box<dyn Future<Output = Result<(S, Option<Result<T, Error>>), Error>> + Send>>;

/// A stream whose items are made one blocking step each, nothing read
/// ahead of the reader. Each item enters the gate of the Vault, so after
/// `Close` the next item is `FAILED_PRECONDITION` and the stream ends.
struct Steps<S, T> {
    vault: Arc<Vault>,
    /// The state, between two items; `None` once the stream has ended.
    state: Option<S>,
    pending: Option<Pending<S, T>>,
    next: Next<S, T>,
}

impl<S: Send + 'static, T: Send + 'static> Steps<S, T> {
    fn new(vault: &Arc<Vault>, state: S, next: Next<S, T>) -> Steps<S, T> {
        Steps {
            vault: Arc::clone(vault),
            state: Some(state),
            pending: None,
            next,
        }
    }
}

impl<S: Send + Unpin + 'static, T: Send + 'static> Stream for Steps<S, T> {
    type Item = Result<T, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.pending.is_none() {
            let Some(mut state) = this.state.take() else {
                return Poll::Ready(None);
            };
            let pass = match Pass::enter(&this.vault) {
                Ok(pass) => pass,
                Err(error) => return Poll::Ready(Some(Err(error))),
            };
            let next = this.next;
            this.pending = Some(Box::pin(async move {
                pass.step(move |cx, vault| {
                    let item = next(cx, vault, &mut state);
                    Ok((state, item))
                })
                .await
            }));
        }
        let Some(pending) = this.pending.as_mut() else {
            return Poll::Ready(None);
        };
        let Poll::Ready(made) = pending.as_mut().poll(cx) else {
            return Poll::Pending;
        };
        this.pending = None;
        Poll::Ready(match made {
            Ok((state, item)) => {
                if item.is_some() {
                    this.state = Some(state);
                }
                item
            }
            // The state went with the step: the stream ends after this.
            Err(error) => Some(Err(error)),
        })
    }
}

/// The object of one open Vault.
struct VaultObject {
    vault: Arc<Vault>,
}

impl VaultObject {
    fn enter(&self) -> Result<Pass, Error> {
        Pass::enter(&self.vault)
    }
}

impl VaultAsync for VaultObject {
    async fn info(&self, _empty: Empty) -> Result<VaultInfo, Error> {
        self.enter()?.step(|cx, vault| vault.info(cx)).await
    }

    async fn check(&self, _empty: Empty) -> Result<CheckReport, Error> {
        self.enter()?.step(|cx, vault| vault.check(cx)).await
    }

    async fn close(&self, _empty: Empty) -> Result<Empty, Error> {
        self.vault.close().await.context("Vault.Close")?;
        Ok(Empty::default())
    }

    async fn mirror(&self, _empty: Empty) -> Result<MirrorRef, Error> {
        let _pass = self.enter()?;
        Ok(MirrorRef::from_async(MirrorObject {
            vault: Arc::clone(&self.vault),
        }))
    }

    async fn index(&self, _empty: Empty) -> Result<IndexRef, Error> {
        let _pass = self.enter()?;
        Err(unavailable("Vault.Index"))
    }

    async fn set_projector(&self, projector_ref: ProjectorRef) -> Result<Empty, Error> {
        let _pass = self.enter()?;
        self.vault
            .set_projector(Some(projector_ref))
            .context("Vault.SetProjector")?;
        Ok(Empty::default())
    }

    async fn remove_projector(&self, _empty: Empty) -> Result<Empty, Error> {
        let _pass = self.enter()?;
        self.vault.set_projector(None)?;
        Ok(Empty::default())
    }

    async fn set_embedder(&self, embedder_ref: EmbedderRef) -> Result<Empty, Error> {
        let _pass = self.enter()?;
        self.vault
            .set_embedder(Some(embedder_ref))
            .context("Vault.SetEmbedder")?;
        Ok(Empty::default())
    }

    async fn remove_embedder(&self, _empty: Empty) -> Result<Empty, Error> {
        let _pass = self.enter()?;
        self.vault.set_embedder(None)?;
        Ok(Empty::default())
    }

    async fn set_reranker(&self, reranker_ref: RerankerRef) -> Result<Empty, Error> {
        let _pass = self.enter()?;
        self.vault
            .set_reranker(Some(reranker_ref))
            .context("Vault.SetReranker")?;
        Ok(Empty::default())
    }

    async fn remove_reranker(&self, _empty: Empty) -> Result<Empty, Error> {
        let _pass = self.enter()?;
        self.vault.set_reranker(None)?;
        Ok(Empty::default())
    }

    async fn list_collections(&self, empty: Empty) -> Result<Collections, Error> {
        self.enter()?
            .step(move |cx, vault| vault.list_collections(cx, empty))
            .await
    }

    async fn get_collection(&self, collection_key: CollectionKey) -> Result<CollectionRef, Error> {
        let declaration = self
            .enter()?
            .step(move |cx, vault| vault.get_collection(cx, collection_key))
            .await?;
        Ok(CollectionRef::from_async(CollectionObject {
            vault: Arc::clone(&self.vault),
            name: declaration.name.unwrap_or_default(),
        }))
    }

    async fn create_collection(
        &self,
        collection_declaration: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        write_collection(
            &self.enter()?,
            CollectionWrites::create(collection_declaration),
        )
        .await
    }

    async fn update_collection(
        &self,
        collection_declaration: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        write_collection(
            &self.enter()?,
            CollectionWrites::update(collection_declaration),
        )
        .await
    }

    async fn delete_collection(
        &self,
        collection_deletion: CollectionDeletion,
    ) -> Result<Empty, Error> {
        write_collection(
            &self.enter()?,
            CollectionWrites::delete(collection_deletion),
        )
        .await?;
        Ok(Empty::default())
    }

    async fn batch_collections<R>(
        &self,
        collection_operations: R,
    ) -> Result<CollectionBatchResult, Error>
    where
        R: Stream<Item = Result<CollectionOperation, Error>> + Send + 'static,
    {
        let pass = self.enter()?;
        // The stream is read whole first, and no lock is held while it is.
        let operations = Inline::<()>::buffer(collection_operations).await;
        write_collections(&pass, CollectionWrites::of_operations(operations)?).await
    }

    async fn batch_documents<R>(
        &self,
        vault_document_operations: R,
    ) -> Result<DocumentBatchResult, Error>
    where
        R: Stream<Item = Result<VaultDocumentOperation, Error>> + Send + 'static,
    {
        let pass = self.enter()?;
        // The stream is read whole first, and no lock is held while it is.
        let operations = Inline::<()>::buffer(vault_document_operations).await;
        write_documents(&pass, DocumentWrites::of_vault(operations)?).await
    }

    async fn query_documents(
        &self,
        _vault_query: VaultQuery,
    ) -> Result<impl Stream<Item = Result<QueryItem, Error>> + Send + use<>, Error> {
        let _pass = self.enter()?;
        Err::<NoStream<QueryItem>, _>(unavailable("Vault.QueryDocuments"))
    }

    async fn count_documents(&self, _vault_query: VaultQuery) -> Result<Count, Error> {
        let _pass = self.enter()?;
        Err(unavailable("Vault.CountDocuments"))
    }

    async fn search_documents(
        &self,
        _vault_search: VaultSearch,
    ) -> Result<impl Stream<Item = Result<SearchItem, Error>> + Send + use<>, Error> {
        let _pass = self.enter()?;
        Err::<NoStream<SearchItem>, _>(unavailable("Vault.SearchDocuments"))
    }
}

/// The object of one collection of an open Vault. It holds the name only
/// and finds the collection again on each call, so after the collection is
/// deleted its calls are `NOT_FOUND`, and after the name is declared again
/// they work again.
struct CollectionObject {
    vault: Arc<Vault>,
    name: String,
}

impl CollectionObject {
    fn enter(&self) -> Result<Pass, Error> {
        Pass::enter(&self.vault)
    }
}

impl CollectionAsync for CollectionObject {
    async fn describe(&self, _empty: Empty) -> Result<CollectionDeclaration, Error> {
        let key = CollectionKey::default().with_name(self.name.clone());
        self.enter()?
            .step(move |cx, vault| vault.get_collection(cx, key))
            .await
    }

    async fn get(&self, document_id: DocumentId) -> Result<Document, Error> {
        let name = self.name.clone();
        self.enter()?
            .step(move |cx, vault| vault.get(cx, &name, document_id))
            .await
    }

    async fn get_many(
        &self,
        document_selection: DocumentSelection,
    ) -> Result<impl Stream<Item = Result<Document, Error>> + Send + use<>, Error> {
        let name = self.name.clone();
        let state = self
            .enter()?
            .step(move |cx, vault| vault.get_many(cx, &name, document_selection))
            .await?;
        Ok(Steps::new(&self.vault, state, |cx, vault, stream| {
            stream.next(cx, vault)
        }))
    }

    async fn history(
        &self,
        document_id: DocumentId,
    ) -> Result<impl Stream<Item = Result<DocumentHistoryEntry, Error>> + Send + use<>, Error> {
        let name = self.name.clone();
        let state = self
            .enter()?
            .step(move |cx, vault| vault.history(cx, &name, document_id))
            .await?;
        Ok(Steps::new(&self.vault, state, |cx, vault, stream| {
            stream.next(cx, vault)
        }))
    }

    async fn create(
        &self,
        document_creation: DocumentCreation,
    ) -> Result<DocumentWriteResult, Error> {
        write_document(
            &self.enter()?,
            DocumentWrites::one(&self.name, document_creation),
        )
        .await
    }

    async fn update(&self, document_update: DocumentUpdate) -> Result<DocumentWriteResult, Error> {
        write_document(
            &self.enter()?,
            DocumentWrites::one(&self.name, document_update),
        )
        .await
    }

    async fn upsert(&self, document_upsert: DocumentUpsert) -> Result<DocumentWriteResult, Error> {
        write_document(
            &self.enter()?,
            DocumentWrites::one(&self.name, document_upsert),
        )
        .await
    }

    async fn delete(
        &self,
        document_deletion: DocumentDeletion,
    ) -> Result<DocumentWriteResult, Error> {
        write_document(
            &self.enter()?,
            DocumentWrites::one(&self.name, document_deletion),
        )
        .await
    }

    async fn batch<R>(&self, document_operations: R) -> Result<DocumentBatchResult, Error>
    where
        R: Stream<Item = Result<DocumentOperation, Error>> + Send + 'static,
    {
        let pass = self.enter()?;
        // The stream is read whole first, and no lock is held while it is.
        let operations = Inline::<()>::buffer(document_operations).await;
        write_documents(
            &pass,
            DocumentWrites::of_collection(&self.name, operations)?,
        )
        .await
    }

    async fn query(
        &self,
        _document_query: DocumentQuery,
    ) -> Result<impl Stream<Item = Result<QueryItem, Error>> + Send + use<>, Error> {
        let _pass = self.enter()?;
        Err::<NoStream<QueryItem>, _>(unavailable("Collection.Query"))
    }

    async fn count(&self, _document_query: DocumentQuery) -> Result<Count, Error> {
        let _pass = self.enter()?;
        Err(unavailable("Collection.Count"))
    }

    async fn search(
        &self,
        _document_search: DocumentSearch,
    ) -> Result<impl Stream<Item = Result<SearchItem, Error>> + Send + use<>, Error> {
        let _pass = self.enter()?;
        Err::<NoStream<SearchItem>, _>(unavailable("Collection.Search"))
    }
}

/// The object of the mirror of an open Vault. A Vault without a mirror has
/// it too: `Info` says so, and the calls that need one are `NOT_FOUND`.
struct MirrorObject {
    vault: Arc<Vault>,
}

impl MirrorObject {
    fn enter(&self) -> Result<Pass, Error> {
        Pass::enter(&self.vault)
    }
}

impl MirrorAsync for MirrorObject {
    async fn info(&self, _empty: Empty) -> Result<MirrorInfo, Error> {
        self.enter()?.step(|cx, vault| vault.mirror_info(cx)).await
    }

    async fn set(&self, mirror_setting: MirrorSetting) -> Result<Empty, Error> {
        mirror::set(&self.enter()?, mirror_setting).await
    }

    async fn remove(&self, _empty: Empty) -> Result<Empty, Error> {
        mirror::remove(&self.enter()?).await
    }

    async fn push(&self, _empty: Empty) -> Result<Empty, Error> {
        mirror::push(&self.enter()?).await
    }

    async fn pull(&self, _empty: Empty) -> Result<Empty, Error> {
        mirror::pull(&self.enter()?).await
    }

    async fn sync(&self, _empty: Empty) -> Result<Empty, Error> {
        mirror::sync(&self.enter()?).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use scheda_api::v1::{DocumentId, OpenOptions};

    use crate::commit::hooks;
    use crate::testing::{create_collection, open, step, upsert, upsert_of};

    /// A scratch directory removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            crate::git::isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-service-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_dropped_during_its_commit_is_stored_and_close_waits_for_it() {
        let scratch = Scratch::new("dropped-commit");
        let vault = open(&scratch.0, OpenOptions::default()).await.unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        // The commit stops between its read of `HEAD` and its swap until the
        // test lets it go.
        let (entered, at_swap) = mpsc::channel::<()>();
        let (go, wait_go) = mpsc::channel::<()>();
        hooks::run_before_swap(vault.path(), move |_root| {
            let _ = entered.send(());
            let _ = wait_go.recv();
        });
        let writing = tokio::spawn({
            let vault = Arc::clone(&vault);
            async move { upsert(&vault, "notes", upsert_of("a.md", b"kept")).await }
        });
        tokio::task::spawn_blocking(move || at_swap.recv_timeout(Duration::from_secs(30)))
            .await
            .unwrap()
            .unwrap();
        writing.abort();
        assert!(writing.await.unwrap_err().is_cancelled());

        // `Close` waits for the step of the dropped call; once it returns, the
        // write is stored.
        let closing = tokio::spawn({
            let vault = Arc::clone(&vault);
            async move { vault.close().await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!closing.is_finished(), "Close waits for the commit");
        go.send(()).unwrap();
        closing.await.unwrap().unwrap();

        let again = open(&scratch.0, OpenOptions::default()).await.unwrap();
        let document = step(&again, |cx, vault| {
            vault.get(cx, "notes", DocumentId::default().with_id("a.md"))
        })
        .await
        .unwrap();
        assert_eq!(document.raw.as_deref(), Some(&b"kept"[..]));
        again.close().await.unwrap();
    }
}
