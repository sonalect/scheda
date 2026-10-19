//! Making the call: opens the Vault, reaches the object, makes the one rpc,
//! prints the reply, and closes the Vault.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda::v1::rebuild::Stage;
use scheda::v1::{
    CollectionKey, CollectionSync, ErrorReport, IndexState, IndexSync, MirrorSync, SchedaSync,
    VaultErrorCode, VaultOpening, VaultSync,
};
use scheda::{Error, Fault, Scheda, parse_command};

use crate::call::{Call, CollectionCall, IndexCall, MirrorCall};
use crate::stdio::{Flow, emit, emit_stream, json_lines};

/// How long a query waits for the Index to settle.
const SETTLE_WAIT: Duration = Duration::from_secs(60);

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
/// `Scheda.Info`); `wait` is whether a query waits for the Index.
///
/// The Vault is closed after the call whatever the call did. A failure of
/// the call is the command's failure; a failure of the close after a call
/// that succeeded is too.
///
/// # Errors
///
/// The failure of the call, or of closing the Vault.
pub(crate) fn execute(call: Call, opening: Option<VaultOpening>, wait: bool) -> Result<(), Failed> {
    let Some(opening) = opening else {
        let info = Scheda.info(Empty::default())?;
        emit(&info)?;
        return Ok(());
    };
    let vault = Scheda.open(opening)?.to_sync()?;
    let result = perform(&vault, call, wait);
    let closed = vault.close(Empty::default());
    result?;
    closed?;
    Ok(())
}

fn perform(vault: &Arc<dyn VaultSync>, call: Call, wait: bool) -> Result<(), Failed> {
    if wait && call.reads_index() {
        wait_settled(vault)?;
    }
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
        Call::QueryDocuments(query) => emit_stream(vault.query_documents(query)?)?,
        Call::CountDocuments(query) => {
            emit(&vault.count_documents(query)?)?;
        }
        Call::SearchDocuments(search) => emit_stream(vault.search_documents(search)?)?,
        Call::Collection(name, call) => {
            let collection = vault
                .get_collection(CollectionKey::default().with_name(name))?
                .to_sync()?;
            collection_call(&collection, call)?;
        }
        Call::Mirror(call) => mirror_call(&vault.mirror(empty)?.to_sync()?, call)?,
        Call::Index(call) => index_call(&vault.index(empty)?.to_sync()?, call, wait)?,
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
        CollectionCall::Query(query) => emit_stream(collection.query(query)?)?,
        CollectionCall::Count(query) => {
            emit(&collection.count(query)?)?;
        }
        CollectionCall::Search(search) => emit_stream(collection.search(search)?)?,
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

fn index_call(index: &Arc<dyn IndexSync>, call: IndexCall, wait: bool) -> Result<(), Failed> {
    match call {
        IndexCall::Status => {
            emit(&index.status(Empty::default())?)?;
        }
        IndexCall::Watch { until_settled } => {
            for state in index.watch(Empty::default())? {
                let state = state?;
                if emit(&state)? == Flow::Closed || (until_settled && settled(&state)) {
                    break;
                }
            }
        }
        IndexCall::Reindex => reindex(index, wait)?,
    }
    Ok(())
}

/// Whether the Index holds nothing left to do: it has a version, no
/// document waits, and no rebuild runs; or it recorded a failure, which
/// will not pass by waiting.
fn settled(state: &IndexState) -> bool {
    state.last_error.is_set()
        || (state.version.is_some() && state.pending == Some(0) && !rebuilding(state))
}

fn rebuilding(state: &IndexState) -> bool {
    stage(state) == Some(Stage::STAGE_RUNNING)
}

fn stage(state: &IndexState) -> Option<Stage> {
    state
        .rebuild
        .as_option()
        .and_then(|rebuild| rebuild.stage.as_ref())
        .and_then(|stage| stage.as_known())
}

/// Waits up to [`SETTLE_WAIT`] until the Index is [`settled`]. Past the
/// deadline the query goes ahead: it answers from what the Index holds, or
/// fails with its own code.
///
/// The watch is read on a helper thread so the deadline holds while the
/// iterator blocks.
fn wait_settled(vault: &Arc<dyn VaultSync>) -> Result<(), Error> {
    let index = vault.index(Empty::default())?.to_sync()?;
    let watch = index.watch(Empty::default())?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for item in watch {
            if sender.send(item).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + SETTLE_WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left) {
            Ok(Ok(state)) if settled(&state) => return Ok(()),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(()),
        }
    }
}

/// Rebuilds the Index and waits for the rebuild to end, reading the state
/// of the Index from a watch opened before the rebuild is asked for.
fn reindex(index: &Arc<dyn IndexSync>, wait: bool) -> Result<(), Failed> {
    if !wait {
        index.reindex(Empty::default())?;
        emit(&Empty::default())?;
        return Ok(());
    }
    let mut watch: BoxIter<'static, Result<IndexState, Error>> = index.watch(Empty::default())?;
    // The state now: whatever it shows is from before the rebuild.
    if let Some(first) = watch.next() {
        first?;
    }
    index.reindex(Empty::default())?;
    let mut running = false;
    for state in watch {
        let state = state?;
        match stage(&state) {
            Some(Stage::STAGE_RUNNING) => running = true,
            Some(Stage::STAGE_DONE) if running => {
                emit(&Empty::default())?;
                return Ok(());
            }
            Some(Stage::STAGE_FAILED) if running => {
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
        if !running && let Some(error) = state.last_error.as_option() {
            return Err(Failed::Report(Box::new(error.clone())));
        }
    }
    Err(Error::from(Fault::new(
        VaultErrorCode::Aborted,
        "the Index stopped before the rebuild ended",
    ))
    .into())
}
