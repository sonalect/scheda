//! Making the call: opens the Vault, reaches the object, makes the one rpc,
//! prints the reply, and closes the Vault.

use std::future::poll_fn;
use std::pin::pin;

use buffa_types::google::protobuf::Empty;
use protocontract::{ErrorCode, IterStream, Stream};
use scheda::v1::rebuild::{Cause, Stage};
use scheda::v1::{
    CollectionAsync, CollectionKey, DynCollectionAsync, DynIndexAsync, DynMirrorAsync,
    DynVaultAsync, ErrorReport, IndexAsync, IndexState, MirrorAsync, SchedaAsync, VaultAsync,
    VaultErrorCode, VaultOpening,
};
use scheda::{Error, Fault, Scheda};

use crate::call::{Call, CollectionCall, IndexCall, MirrorCall};
use crate::stdio::{Flow, emit, emit_stream};

/// How a command failed: with an [`Error`], or with the report of a
/// failure the Index recorded, which is printed as the library gave it.
pub(crate) enum Failed {
    /// A failure of a call.
    Error(Error),
    /// A report carried by the state of the Index.
    Report(Box<ErrorReport>),
}

impl From<Error> for Failed {
    fn from(error: Error) -> Failed {
        Failed::Error(error)
    }
}

/// Makes `call`. `opening` is the request that opens the Vault (none for
/// `Scheda.Info`); `wait` is whether a query, a count, or a search waits
/// for the Index to reach the state of the Vault at open, and `index
/// reindex` for the rebuild to end.
///
/// The Vault is closed after the call whatever the call did. A failure of
/// the call is the command's failure; a failure of the close after a call
/// that succeeded is too.
///
/// # Errors
///
/// The failure of the call, or of closing the Vault.
pub(crate) async fn execute(
    call: Call,
    opening: Option<VaultOpening>,
    wait: bool,
) -> Result<(), Failed> {
    let Some(opening) = opening else {
        let info = Scheda.info(Empty::default()).await?;
        emit(&info)?;
        return Ok(());
    };
    let vault = Scheda.open(opening).await?.to_async_inline()?;
    let result = perform(&vault, call, wait).await;
    let closed = vault.close(Empty::default()).await;
    result?;
    closed?;
    Ok(())
}

async fn perform(vault: &DynVaultAsync, mut call: Call, wait: bool) -> Result<(), Failed> {
    if wait && call.reads_index() {
        // The state of the Vault now: the call answers once the Index has
        // reached it, unless the request names its own.
        if let Some(token) = vault.info(Empty::default()).await?.version {
            call.wait_for(token);
        }
    }
    let empty = Empty::default();
    match call {
        Call::SchedaInfo => {}
        Call::Info => {
            emit(&vault.info(empty).await?)?;
        }
        Call::Check => {
            emit(&vault.check(empty).await?)?;
        }
        Call::ListCollections => {
            emit(&vault.list_collections(empty).await?)?;
        }
        Call::CreateCollection(declaration) => {
            emit(&vault.create_collection(declaration).await?)?;
        }
        Call::UpdateCollection(declaration) => {
            emit(&vault.update_collection(declaration).await?)?;
        }
        Call::DeleteCollection(deletion) => {
            emit(&vault.delete_collection(deletion).await?)?;
        }
        Call::BatchCollections(operations) => {
            let operations = IterStream::new(operations.into_iter());
            emit(&vault.batch_collections(operations).await?)?;
        }
        Call::BatchDocuments(operations) => {
            let operations = IterStream::new(operations.into_iter());
            emit(&vault.batch_documents(operations).await?)?;
        }
        Call::QueryDocuments(query) => emit_stream(vault.query_documents(query).await?).await?,
        Call::CountDocuments(query) => {
            emit(&vault.count_documents(query).await?)?;
        }
        Call::SearchDocuments(search) => {
            emit_stream(vault.search_documents(search).await?).await?;
        }
        Call::Collection(name, call) => {
            let collection = vault
                .get_collection(CollectionKey::default().with_name(name))
                .await?
                .to_async_inline()?;
            collection_call(&collection, call).await?;
        }
        Call::Mirror(call) => {
            let mirror = vault.mirror(empty).await?.to_async_inline()?;
            mirror_call(&mirror, call).await?;
        }
        Call::Index(call) => {
            let index = vault.index(empty).await?.to_async_inline()?;
            index_call(&index, call, wait).await?;
        }
    }
    Ok(())
}

async fn collection_call(
    collection: &DynCollectionAsync,
    call: CollectionCall,
) -> Result<(), Error> {
    match call {
        CollectionCall::Describe => {
            emit(&collection.describe(Empty::default()).await?)?;
        }
        CollectionCall::Get(id) => {
            emit(&collection.get(id).await?)?;
        }
        CollectionCall::GetMany(selection) => {
            emit_stream(collection.get_many(selection).await?).await?;
        }
        CollectionCall::History(id) => emit_stream(collection.history(id).await?).await?,
        CollectionCall::Create(creation) => {
            emit(&collection.create(creation).await?)?;
        }
        CollectionCall::Update(update) => {
            emit(&collection.update(update).await?)?;
        }
        CollectionCall::Upsert(upsert) => {
            emit(&collection.upsert(upsert).await?)?;
        }
        CollectionCall::Delete(deletion) => {
            emit(&collection.delete(deletion).await?)?;
        }
        CollectionCall::Batch(operations) => {
            let operations = IterStream::new(operations.into_iter());
            emit(&collection.batch(operations).await?)?;
        }
        CollectionCall::Query(query) => emit_stream(collection.query(query).await?).await?,
        CollectionCall::Count(query) => {
            emit(&collection.count(query).await?)?;
        }
        CollectionCall::Search(search) => emit_stream(collection.search(search).await?).await?,
        CollectionCall::GetVectors(selection) => {
            emit_stream(collection.get_vectors(selection).await?).await?;
        }
    }
    Ok(())
}

async fn mirror_call(mirror: &DynMirrorAsync, call: MirrorCall) -> Result<(), Error> {
    let empty = Empty::default();
    match call {
        MirrorCall::Info => emit(&mirror.info(empty).await?)?,
        MirrorCall::Set(setting) => emit(&mirror.set(setting).await?)?,
        MirrorCall::Remove => emit(&mirror.remove(empty).await?)?,
        MirrorCall::Push => emit(&mirror.push(empty).await?)?,
        MirrorCall::Pull => emit(&mirror.pull(empty).await?)?,
        MirrorCall::Sync => emit(&mirror.sync(empty).await?)?,
    };
    Ok(())
}

async fn index_call(index: &DynIndexAsync, call: IndexCall, wait: bool) -> Result<(), Failed> {
    match call {
        IndexCall::Status => {
            emit(&index.status(Empty::default()).await?)?;
        }
        IndexCall::Watch { until_settled } => {
            let mut watch = pin!(index.watch(Empty::default()).await?);
            while let Some(state) = next(&mut watch).await {
                let state = state?;
                if emit(&state)? == Flow::Closed || (until_settled && settled(&state)) {
                    break;
                }
            }
        }
        IndexCall::Reindex => reindex(index, wait).await?,
    }
    Ok(())
}

/// The next item of `stream`.
async fn next<S: Stream>(stream: &mut std::pin::Pin<&mut S>) -> Option<S::Item> {
    poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

/// Whether the Index has nothing left to do: it reflects the stored state
/// and no rebuild runs; or it recorded a failure, which will not pass by
/// waiting.
fn settled(state: &IndexState) -> bool {
    state.last_error.is_set()
        || (state.version.is_some()
            && state.version == state.stored_version
            && stage(state) != Some(Stage::STAGE_RUNNING))
}

fn stage(state: &IndexState) -> Option<Stage> {
    state
        .rebuild
        .as_option()
        .and_then(|rebuild| rebuild.stage.as_ref())
        .and_then(|stage| stage.as_known())
}

fn cause(state: &IndexState) -> Option<Cause> {
    state
        .rebuild
        .as_option()
        .and_then(|rebuild| rebuild.cause.as_ref())
        .and_then(|cause| cause.as_known())
}

/// Rebuilds the Index and, with `wait`, waits for the rebuild to end,
/// reading the state of the Index from a watch opened before the rebuild
/// is asked for.
async fn reindex(index: &DynIndexAsync, wait: bool) -> Result<(), Failed> {
    if !wait {
        index.reindex(Empty::default()).await?;
        emit(&Empty::default())?;
        return Ok(());
    }
    let mut watch = pin!(index.watch(Empty::default()).await?);
    // The state now: whatever it shows is from before the rebuild.
    if let Some(first) = next(&mut watch).await {
        first?;
    }
    index.reindex(Empty::default()).await?;
    let mut running = false;
    while let Some(state) = next(&mut watch).await {
        let state = state?;
        let asked = cause(&state) == Some(Cause::CAUSE_REINDEX);
        match stage(&state) {
            Some(Stage::STAGE_RUNNING) if asked => running = true,
            Some(Stage::STAGE_DONE) if running && asked => {
                emit(&Empty::default())?;
                return Ok(());
            }
            Some(Stage::STAGE_FAILED) if running && asked => {
                let error = state
                    .rebuild
                    .as_option()
                    .and_then(|rebuild| rebuild.error.as_option().cloned());
                return Err(Failed::Report(Box::new(error.unwrap_or_else(|| {
                    ErrorReport::default()
                        .with_code("VAULT_ERROR_CODE_INDEX_BROKEN")
                        .with_message("the rebuild failed")
                }))));
            }
            _ => {}
        }
        // `INDEX_STALE` is what the rebuild ends: a round that looked
        // before the request found it.
        if !running
            && let Some(error) = state.last_error.as_option()
            && error.code.as_deref() != Some(ErrorCode::name(VaultErrorCode::IndexStale))
        {
            return Err(Failed::Report(Box::new(error.clone())));
        }
    }
    Err(Error::from(Fault::new(
        VaultErrorCode::Aborted,
        "the Index stopped before the rebuild ended",
    ))
    .into())
}
