//! Making the call: opens the Vault, reaches the object, makes the one rpc,
//! prints the reply, and closes the Vault.

use buffa_types::google::protobuf::Empty;
use protocontract::IterStream;
use scheda::v1::{
    CollectionAsync, CollectionKey, DynCollectionAsync, DynMirrorAsync, DynVaultAsync, MirrorAsync,
    SchedaAsync, VaultAsync, VaultOpening,
};
use scheda::{Error, Scheda};

use crate::call::{Call, CollectionCall, MirrorCall};
use crate::stdio::{emit, emit_stream};

/// Makes `call`. `opening` is the request that opens the Vault (none for
/// `Scheda.Info`).
///
/// The Vault is closed after the call whatever the call did. A failure of
/// the call is the command's failure; a failure of the close after a call
/// that succeeded is too.
///
/// # Errors
///
/// The failure of the call, or of closing the Vault.
pub(crate) async fn execute(call: Call, opening: Option<VaultOpening>) -> Result<(), Error> {
    let Some(opening) = opening else {
        let info = Scheda.info(Empty::default()).await?;
        emit(&info)?;
        return Ok(());
    };
    let vault = Scheda.open(opening).await?.to_async_inline()?;
    let result = perform(&vault, call).await;
    let closed = vault.close(Empty::default()).await;
    result?;
    closed?;
    Ok(())
}

async fn perform(vault: &DynVaultAsync, call: Call) -> Result<(), Error> {
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
