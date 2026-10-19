//! Making the call: opens the Vault, reaches the object, makes the one rpc,
//! prints the reply, and closes the Vault.

use std::sync::Arc;

use buffa_types::google::protobuf::Empty;
use scheda::v1::{CollectionKey, CollectionSync, MirrorSync, SchedaSync, VaultOpening, VaultSync};
use scheda::{Error, Scheda, parse_command};

use crate::call::{Call, CollectionCall, MirrorCall};
use crate::stdio::{emit, emit_stream, json_lines};

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
pub(crate) fn execute(call: Call, opening: Option<VaultOpening>) -> Result<(), Error> {
    let Some(opening) = opening else {
        let info = Scheda.info(Empty::default())?;
        emit(&info)?;
        return Ok(());
    };
    let vault = Scheda.open(opening)?.to_sync()?;
    let result = perform(&vault, call);
    let closed = vault.close(Empty::default());
    result?;
    closed?;
    Ok(())
}

fn perform(vault: &Arc<dyn VaultSync>, call: Call) -> Result<(), Error> {
    let empty = Empty::default();
    match call {
        Call::SchedaInfo => {}
        Call::Info => {
            emit(&vault.info(empty)?)?;
        }
        Call::Check => {
            emit(&vault.check(empty)?)?;
        }
        Call::ListCollections => {
            emit(&vault.list_collections(empty)?)?;
        }
        Call::CreateCollection(declaration) => {
            emit(&vault.create_collection(declaration)?)?;
        }
        Call::UpdateCollection(declaration) => {
            emit(&vault.update_collection(declaration)?)?;
        }
        Call::DeleteCollection(deletion) => {
            emit(&vault.delete_collection(deletion)?)?;
        }
        Call::BatchCollections => {
            let operations = json_lines(parse_command);
            emit(&vault.batch_collections(operations)?)?;
        }
        Call::BatchDocuments => {
            let operations = json_lines(parse_command);
            emit(&vault.batch_documents(operations)?)?;
        }
        Call::Collection(name, call) => {
            let collection = vault
                .get_collection(CollectionKey::default().with_name(name))?
                .to_sync()?;
            collection_call(&collection, call)?;
        }
        Call::Mirror(call) => mirror_call(&vault.mirror(empty)?.to_sync()?, call)?,
    }
    Ok(())
}

fn collection_call(
    collection: &Arc<dyn CollectionSync>,
    call: CollectionCall,
) -> Result<(), Error> {
    match call {
        CollectionCall::Describe => {
            emit(&collection.describe(Empty::default())?)?;
        }
        CollectionCall::Get(id) => {
            emit(&collection.get(id)?)?;
        }
        CollectionCall::GetMany(selection) => emit_stream(collection.get_many(selection)?)?,
        CollectionCall::History(id) => emit_stream(collection.history(id)?)?,
        CollectionCall::Create(creation) => {
            emit(&collection.create(creation)?)?;
        }
        CollectionCall::Update(update) => {
            emit(&collection.update(update)?)?;
        }
        CollectionCall::Upsert(upsert) => {
            emit(&collection.upsert(upsert)?)?;
        }
        CollectionCall::Delete(deletion) => {
            emit(&collection.delete(deletion)?)?;
        }
        CollectionCall::Batch => {
            emit(&collection.batch(json_lines(parse_command))?)?;
        }
    }
    Ok(())
}

fn mirror_call(mirror: &Arc<dyn MirrorSync>, call: MirrorCall) -> Result<(), Error> {
    let empty = Empty::default();
    match call {
        MirrorCall::Info => emit(&mirror.info(empty)?)?,
        MirrorCall::Set(setting) => emit(&mirror.set(setting)?)?,
        MirrorCall::Remove => emit(&mirror.remove(empty)?)?,
        MirrorCall::Push => emit(&mirror.push(empty)?)?,
        MirrorCall::Pull => emit(&mirror.pull(empty)?)?,
        MirrorCall::Sync => emit(&mirror.sync(empty)?)?,
    };
    Ok(())
}
