//! The services of the contract: [`Scheda`], the root a caller makes, and
//! the objects an open Vault gives, each behind a reference: the Vault
//! itself, one collection, the mirror, and the Index.
//!
//! The objects of one Vault share its state and one gate. Every call enters
//! the gate first; `Vault.Close` shuts it, waits for the calls inside to
//! end, and stops the indexer. A stream is not a call: each item it makes
//! enters the gate on its own, so `Close` waits for an item being made, not
//! for a caller who holds a stream, and the next item after it is
//! `FAILED_PRECONDITION`. A Vault whose references are all dropped (its
//! collections', mirror's, and Index's, and its streams, included) ends as
//! `Close` would.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda_api::v1::collection_ref::CollectionRef;
use scheda_api::v1::embedder_ref::EmbedderRef;
use scheda_api::v1::index_ref::IndexRef;
use scheda_api::v1::mirror_ref::MirrorRef;
use scheda_api::v1::projector_ref::ProjectorRef;
use scheda_api::v1::reranker_ref::RerankerRef;
use scheda_api::v1::vault_ref::VaultRef;
use scheda_api::v1::{
    CheckReport, CollectionBatchResult, CollectionDeclaration, CollectionDeletion, CollectionKey,
    CollectionOperation, CollectionSync, Collections, Count, Document, DocumentBatchResult,
    DocumentCreation, DocumentDeletion, DocumentHistoryEntry, DocumentId, DocumentOperation,
    DocumentQuery, DocumentSearch, DocumentSelection, DocumentUpdate, DocumentUpsert, Feature,
    MirrorInfo, MirrorSetting, MirrorSync, SchedaInfo, SchedaSync, SearchItem,
    VaultDocumentOperation, VaultErrorCode, VaultInfo, VaultOpening, VaultQuery, VaultSearch,
    VaultSync,
};

use crate::error::{Context, Error, Fault};
use crate::ports::Ports;
use crate::stored;
use crate::vault::Vault;

/// Scheda: the root of the contract. A process makes one (it holds
/// nothing) and opens every Vault through it.
///
/// [`SchedaSync::open`] returns a [`VaultRef`]: a reference to the open
/// Vault, from which the caller takes the form it needs
/// (`vault.to_sync()?` gives an `Arc<dyn VaultSync>`). The Vault gives its
/// collections, its mirror, and its Index the same way. The objects behind
/// the references are Scheda's own; a caller works through the traits.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scheda;

impl SchedaSync for Scheda {
    fn info(&self, _empty: Empty) -> Result<SchedaInfo, Error> {
        let mut features = Vec::new();
        if cfg!(feature = "find") {
            features.push(Feature::FEATURE_FIND.into());
        }
        if cfg!(feature = "search") {
            features.push(Feature::FEATURE_SEARCH.into());
        }
        if cfg!(feature = "encrypt") {
            features.push(Feature::FEATURE_ENCRYPT.into());
        }
        Ok(SchedaInfo {
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            features,
            ..SchedaInfo::default()
        })
    }

    fn open(&self, mut vault_opening: VaultOpening) -> Result<VaultRef, Error> {
        stored::validate(&vault_opening).context("Scheda.Open: check the request")?;
        let path = vault_opening.path.take().unwrap_or_default();
        let options = vault_opening.options.take().unwrap_or_default();
        let ports = Ports {
            projector: vault_opening.projector.take(),
            embedder: vault_opening.embedder.take(),
            reranker: vault_opening.reranker.take(),
        };
        let vault = Vault::open(path, options, ports)?;
        Ok(VaultRef::from_sync(VaultObject {
            shared: Arc::new(Shared {
                vault,
                gate: Gate::default(),
                closing: Mutex::new(()),
            }),
        }))
    }
}

/// What the objects of one open Vault share.
struct Shared {
    vault: Vault,
    gate: Gate,
    /// Held by a `Close` while it runs, so a second one waits for it.
    closing: Mutex<()>,
}

impl Shared {
    /// Runs `call` inside the gate: `FAILED_PRECONDITION` once the Vault is
    /// closed.
    fn call<T>(&self, call: impl FnOnce(&Vault) -> Result<T, Error>) -> Result<T, Error> {
        let _pass = self.gate.enter()?;
        call(&self.vault)
    }
}

/// The gate of an open Vault: counts the calls inside it, and refuses new
/// ones once it is shut.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    idle: Condvar,
}

#[derive(Default)]
struct GateState {
    shut: bool,
    inside: usize,
}

/// One call inside the gate, until it is dropped.
struct Pass<'a>(&'a Gate);

impl Gate {
    /// The state of the gate. It holds two plain numbers that no panic can
    /// leave half written, so a poisoned lock is read as a clean one.
    fn state(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enter(&self) -> Result<Pass<'_>, Error> {
        let mut state = self.state();
        if state.shut {
            return Err(closed());
        }
        state.inside += 1;
        Ok(Pass(self))
    }

    fn is_shut(&self) -> bool {
        self.state().shut
    }

    /// Refuses new calls, then waits until no call is inside.
    fn shut(&self) {
        let mut state = self.state();
        state.shut = true;
        while state.inside > 0 {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.inside -= 1;
        if state.inside == 0 {
            self.0.idle.notify_all();
        }
    }
}

fn closed() -> Error {
    Fault::new(
        VaultErrorCode::FailedPrecondition,
        "the Vault is closed; open it again",
    )
    .into()
}

/// A stream of a Vault call, each item made inside the gate.
///
/// A stream that `holds` keeps its pass while it makes an item, so `Close`
/// waits for that item. One that waits for news (`Index.Watch`) holds none
/// while it waits; it ends when the indexer stops, and says that the Vault
/// is closed if that is why.
struct Guarded<I> {
    shared: Arc<Shared>,
    inner: Option<I>,
    holds: bool,
}

impl<I> Guarded<I> {
    #[cfg_attr(
        not(feature = "find"),
        expect(dead_code, reason = "only Index.Watch waits")
    )]
    fn waiting(shared: &Arc<Shared>, inner: I) -> Guarded<I> {
        Guarded {
            shared: Arc::clone(shared),
            inner: Some(inner),
            holds: false,
        }
    }
}

impl<T, I: Iterator<Item = Result<T, Error>>> Iterator for Guarded<I> {
    type Item = Result<T, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let inner = self.inner.as_mut()?;
        let pass = match self.shared.gate.enter() {
            Ok(pass) => pass,
            Err(error) => {
                self.inner = None;
                return Some(Err(error));
            }
        };
        if !self.holds {
            drop(pass);
            let item = inner.next();
            if item.is_none() {
                self.inner = None;
                if self.shared.gate.is_shut() {
                    return Some(Err(closed()));
                }
            }
            return item;
        }
        let item = inner.next();
        drop(pass);
        if item.is_none() {
            self.inner = None;
        }
        item
    }
}

/// The object of one open Vault.
struct VaultObject {
    shared: Arc<Shared>,
}

impl VaultSync for VaultObject {
    fn info(&self, _empty: Empty) -> Result<VaultInfo, Error> {
        self.shared.call(|vault| Ok(vault.info()))
    }

    fn check(&self, _empty: Empty) -> Result<CheckReport, Error> {
        todo!("R3: Vault.Check")
    }

    fn close(&self, _empty: Empty) -> Result<Empty, Error> {
        let _closing = self
            .shared
            .closing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.shared.gate.shut();
        self.shared.vault.close().context("Vault.Close")?;
        Ok(Empty::default())
    }

    fn mirror(&self, _empty: Empty) -> Result<MirrorRef, Error> {
        self.shared.call(|_| {
            Ok(MirrorRef::from_sync(MirrorObject {
                shared: Arc::clone(&self.shared),
            }))
        })
    }

    fn index(&self, _empty: Empty) -> Result<IndexRef, Error> {
        self.shared.call(|_| {
            #[cfg(feature = "find")]
            {
                Ok(IndexRef::from_sync(IndexObject {
                    shared: Arc::clone(&self.shared),
                }))
            }
            #[cfg(not(feature = "find"))]
            {
                Err(Fault::new(
                    VaultErrorCode::Unimplemented,
                    "the Index needs a build with the `find` feature",
                )
                .into())
            }
        })
    }

    fn set_projector(&self, projector_ref: ProjectorRef) -> Result<Empty, Error> {
        self.shared
            .call(|vault| vault.set_projector(Some(projector_ref)))
            .context("Vault.SetProjector")?;
        Ok(Empty::default())
    }

    fn remove_projector(&self, _empty: Empty) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.set_projector(None))?;
        Ok(Empty::default())
    }

    fn set_embedder(&self, embedder_ref: EmbedderRef) -> Result<Empty, Error> {
        self.shared
            .call(|vault| vault.set_embedder(Some(embedder_ref)))
            .context("Vault.SetEmbedder")?;
        Ok(Empty::default())
    }

    fn remove_embedder(&self, _empty: Empty) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.set_embedder(None))?;
        Ok(Empty::default())
    }

    fn set_reranker(&self, reranker_ref: RerankerRef) -> Result<Empty, Error> {
        self.shared
            .call(|vault| vault.set_reranker(Some(reranker_ref)))
            .context("Vault.SetReranker")?;
        Ok(Empty::default())
    }

    fn remove_reranker(&self, _empty: Empty) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.set_reranker(None))?;
        Ok(Empty::default())
    }

    fn list_collections(&self, empty: Empty) -> Result<Collections, Error> {
        self.shared.call(|vault| vault.list_collections(empty))
    }

    fn get_collection(&self, collection_key: CollectionKey) -> Result<CollectionRef, Error> {
        let declaration = self
            .shared
            .call(|vault| vault.get_collection(collection_key))?;
        Ok(CollectionRef::from_sync(CollectionObject {
            shared: Arc::clone(&self.shared),
            name: declaration.name.unwrap_or_default(),
        }))
    }

    fn create_collection(
        &self,
        collection_declaration: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        self.shared
            .call(|vault| vault.create_collection(collection_declaration))
    }

    fn update_collection(
        &self,
        collection_declaration: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        self.shared
            .call(|vault| vault.update_collection(collection_declaration))
    }

    fn delete_collection(&self, collection_deletion: CollectionDeletion) -> Result<Empty, Error> {
        self.shared
            .call(|vault| vault.delete_collection(collection_deletion))
    }

    fn batch_collections(
        &self,
        collection_operations: BoxIter<'static, Result<CollectionOperation, Error>>,
    ) -> Result<CollectionBatchResult, Error> {
        self.shared
            .call(|vault| vault.batch_collections(collection_operations))
    }

    fn batch_documents(
        &self,
        vault_document_operations: BoxIter<'static, Result<VaultDocumentOperation, Error>>,
    ) -> Result<DocumentBatchResult, Error> {
        self.shared
            .call(|vault| vault.batch_documents(vault_document_operations))
    }

    fn query_documents(
        &self,
        _vault_query: VaultQuery,
    ) -> Result<BoxIter<'static, Result<Document, Error>>, Error> {
        todo!("R4: Vault.QueryDocuments")
    }

    fn count_documents(&self, _vault_query: VaultQuery) -> Result<Count, Error> {
        todo!("R4: Vault.CountDocuments")
    }

    fn search_documents(
        &self,
        _vault_search: VaultSearch,
    ) -> Result<BoxIter<'static, Result<SearchItem, Error>>, Error> {
        todo!("R4: Vault.SearchDocuments")
    }
}

/// The object of one collection of an open Vault. It holds the name only
/// and finds the collection again on each call, so after the collection is
/// deleted its calls are `NOT_FOUND`, and after the name is declared again
/// they work again.
struct CollectionObject {
    shared: Arc<Shared>,
    name: String,
}

impl CollectionSync for CollectionObject {
    fn describe(&self, _empty: Empty) -> Result<CollectionDeclaration, Error> {
        self.shared.call(|vault| {
            vault.get_collection(CollectionKey::default().with_name(self.name.clone()))
        })
    }

    fn get(&self, document_id: DocumentId) -> Result<Document, Error> {
        self.shared.call(|vault| vault.get(&self.name, document_id))
    }

    fn get_many(
        &self,
        _document_selection: DocumentSelection,
    ) -> Result<BoxIter<'static, Result<Document, Error>>, Error> {
        todo!("R3: Collection.GetMany")
    }

    fn history(
        &self,
        _document_id: DocumentId,
    ) -> Result<BoxIter<'static, Result<DocumentHistoryEntry, Error>>, Error> {
        todo!("R3: Collection.History")
    }

    fn create(&self, document_creation: DocumentCreation) -> Result<Document, Error> {
        self.shared
            .call(|vault| vault.create(&self.name, document_creation))
    }

    fn update(&self, document_update: DocumentUpdate) -> Result<Document, Error> {
        self.shared
            .call(|vault| vault.update(&self.name, document_update))
    }

    fn upsert(&self, document_upsert: DocumentUpsert) -> Result<Document, Error> {
        self.shared
            .call(|vault| vault.upsert(&self.name, document_upsert))
    }

    fn delete(&self, document_deletion: DocumentDeletion) -> Result<Empty, Error> {
        self.shared
            .call(|vault| vault.delete(&self.name, document_deletion))
    }

    fn batch(
        &self,
        document_operations: BoxIter<'static, Result<DocumentOperation, Error>>,
    ) -> Result<DocumentBatchResult, Error> {
        self.shared
            .call(|vault| vault.batch(&self.name, document_operations))
    }

    fn query(
        &self,
        _document_query: DocumentQuery,
    ) -> Result<BoxIter<'static, Result<Document, Error>>, Error> {
        todo!("R4: Collection.Query")
    }

    fn count(&self, _document_query: DocumentQuery) -> Result<Count, Error> {
        todo!("R4: Collection.Count")
    }

    fn search(
        &self,
        _document_search: DocumentSearch,
    ) -> Result<BoxIter<'static, Result<SearchItem, Error>>, Error> {
        todo!("R4: Collection.Search")
    }
}

/// The object of the mirror of an open Vault. A Vault without a mirror has
/// it too: `Info` says so, and the calls that need one are `NOT_FOUND`.
struct MirrorObject {
    shared: Arc<Shared>,
}

impl MirrorSync for MirrorObject {
    fn info(&self, _empty: Empty) -> Result<MirrorInfo, Error> {
        self.shared.call(Vault::mirror_info)
    }

    fn set(&self, mirror_setting: MirrorSetting) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.set_mirror(mirror_setting))
    }

    fn remove(&self, empty: Empty) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.remove_mirror(empty))
    }

    fn push(&self, empty: Empty) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.push(empty))
    }

    fn pull(&self, empty: Empty) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.pull(empty))
    }

    fn sync(&self, empty: Empty) -> Result<Empty, Error> {
        self.shared.call(|vault| vault.sync(empty))
    }
}

/// The object of the Index of an open Vault. A build without `find` has
/// none.
#[cfg(feature = "find")]
struct IndexObject {
    shared: Arc<Shared>,
}

#[cfg(feature = "find")]
impl scheda_api::v1::IndexSync for IndexObject {
    fn status(&self, _empty: Empty) -> Result<scheda_api::v1::IndexState, Error> {
        self.shared.call(|vault| Ok(vault.indexer().status()))
    }

    fn watch(
        &self,
        _empty: Empty,
    ) -> Result<BoxIter<'static, Result<scheda_api::v1::IndexState, Error>>, Error> {
        let watch = self.shared.call(|vault| Ok(vault.indexer().watch()))?;
        Ok(Box::new(Guarded::waiting(&self.shared, watch)))
    }

    fn reindex(&self, _empty: Empty) -> Result<Empty, Error> {
        self.shared
            .call(|vault| vault.indexer().reindex())
            .context("Index.Reindex")?;
        Ok(Empty::default())
    }
}
