//! What the unit tests do to an open Vault: open it, and write and read
//! through the same path the objects take (the gate, the blocking steps, the
//! write lock), so a test sees what a caller sees.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocontract::BoxIter;
use scheda_api::v1::{
    CollectionDeclaration, DocumentBatchResult, DocumentCreation, DocumentDeletion,
    DocumentOperation, DocumentUpsert, DocumentWriteResult, OpenOptions,
};
use scheda_blocking::Blocking;

use crate::collections::Writes as CollectionWrites;
use crate::document_calls::Writes as DocumentWrites;
use crate::error::Error;
use crate::gate::Pass;
use crate::ports::Ports;
use crate::service;
use crate::vault::Vault;

thread_local! {
    /// The scratch folders of the test this thread runs.
    static LATER: Later = const { Later(RefCell::new(Vec::new())) };
}

/// Folders removed when the thread that holds them ends.
struct Later(RefCell<Vec<PathBuf>>);

impl Drop for Later {
    fn drop(&mut self) {
        for dir in self.0.get_mut().drain(..) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Removes the scratch folder `dir` when the thread of the test ends,
/// whether the test passed or failed.
///
/// A Vault dropped without `Close` lets its indexer finish the round it is
/// in, on the blocking pool of its runtime, and that round writes under
/// `.scheda/index/`. Removed when the test's last value goes, the folder
/// races the round: a file the round adds stops the removal, and the rest
/// of the folder stays. The thread of a test ends after the runtime the
/// test built was dropped, and dropping a runtime waits for its blocking
/// steps, so the folder goes after the last round. The test harness runs
/// each test on a thread of its own and joins it before it reports it.
pub(crate) fn remove_when_the_test_ends(dir: &Path) {
    let dir = dir.to_path_buf();
    LATER.with(|later| later.0.borrow_mut().push(dir));
}

/// Opens the Vault at `path` with `options` and no port.
pub(crate) async fn open(path: &Path, options: OpenOptions) -> Result<Arc<Vault>, Error> {
    Vault::open(path.to_path_buf(), options, Ports::default()).await
}

/// Opens the Vault at `path` with `options` and `ports`.
#[cfg(feature = "index")]
pub(crate) async fn open_with(
    path: &Path,
    options: OpenOptions,
    ports: Ports,
) -> Result<Arc<Vault>, Error> {
    Vault::open(path.to_path_buf(), options, ports).await
}

/// The state token of `HEAD` of the Vault now.
#[cfg(feature = "index")]
pub(crate) async fn head(vault: &Arc<Vault>) -> String {
    step(vault, |cx, vault| Ok(vault.state_now(cx)?.token()))
        .await
        .unwrap_or_default()
}

/// Waits until the Index of `vault` reflects `HEAD`, or fails the test
/// after 60 seconds.
#[cfg(feature = "index")]
pub(crate) async fn wait_indexed(vault: &Arc<Vault>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let head = head(vault).await;
        let state = vault.indexer().status();
        if state.version.as_deref() == Some(head.as_str()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the Index did not reach {head}: {state:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// A journal `name` of `lines` lines `l00`, `l01`, ... in one segment, with
/// its Index built.
#[cfg(feature = "index")]
pub(crate) async fn journal_of_lines(vault: &Arc<Vault>, name: &str, lines: usize) {
    create_collection(
        vault,
        serde_json::json!({
            "name": name,
            "schema": {"journal": {"segment_max_bytes": 1_000_000}}
        }),
    )
    .await
    .unwrap();
    for n in 0..lines {
        create(
            vault,
            name,
            DocumentCreation::default()
                .with_id(format!("l{n:02}"))
                .with_raw(format!("{{\"n\":{n}}}").into_bytes())
                .with_note("test"),
        )
        .await
        .unwrap();
    }
    wait_indexed(vault).await;
}

/// Runs `step` on `vault` as a call of an object runs one: inside the gate,
/// on the blocking pool.
pub(crate) async fn step<T, F>(vault: &Arc<Vault>, step: F) -> Result<T, Error>
where
    F: FnOnce(&Blocking, &Vault) -> Result<T, Error> + Send + 'static,
    T: Send + 'static,
{
    Pass::enter(vault)?.step(step).await
}

/// The collection the JSON `declaration` declares, created.
pub(crate) async fn create_collection(
    vault: &Arc<Vault>,
    declaration: serde_json::Value,
) -> Result<CollectionDeclaration, Error> {
    let declaration: CollectionDeclaration =
        serde_json::from_value(declaration).map_err(|err| {
            Error::new(
                scheda_api::v1::VaultErrorCode::InvalidArgument,
                err.to_string(),
            )
        })?;
    service::write_collection(&Pass::enter(vault)?, CollectionWrites::create(declaration)).await
}

/// The collection the JSON `declaration` declares, updated.
#[cfg(feature = "index")]
pub(crate) async fn update_collection(
    vault: &Arc<Vault>,
    declaration: serde_json::Value,
) -> Result<CollectionDeclaration, Error> {
    let declaration: CollectionDeclaration =
        serde_json::from_value(declaration).map_err(|err| {
            Error::new(
                scheda_api::v1::VaultErrorCode::InvalidArgument,
                err.to_string(),
            )
        })?;
    service::write_collection(&Pass::enter(vault)?, CollectionWrites::update(declaration)).await
}

/// `Collection.Create` in `collection`.
pub(crate) async fn create(
    vault: &Arc<Vault>,
    collection: &str,
    request: DocumentCreation,
) -> Result<DocumentWriteResult, Error> {
    service::write_document(
        &Pass::enter(vault)?,
        DocumentWrites::one(collection, request),
    )
    .await
}

/// `Collection.Upsert` in `collection`.
pub(crate) async fn upsert(
    vault: &Arc<Vault>,
    collection: &str,
    request: DocumentUpsert,
) -> Result<DocumentWriteResult, Error> {
    service::write_document(
        &Pass::enter(vault)?,
        DocumentWrites::one(collection, request),
    )
    .await
}

/// `Collection.Delete` in `collection`.
pub(crate) async fn delete(
    vault: &Arc<Vault>,
    collection: &str,
    request: DocumentDeletion,
) -> Result<DocumentWriteResult, Error> {
    service::write_document(
        &Pass::enter(vault)?,
        DocumentWrites::one(collection, request),
    )
    .await
}

/// `Collection.Batch` in `collection`.
pub(crate) async fn batch(
    vault: &Arc<Vault>,
    collection: &str,
    operations: Vec<DocumentOperation>,
) -> Result<DocumentBatchResult, Error> {
    let operations: BoxIter<'static, Result<DocumentOperation, Error>> =
        Box::new(operations.into_iter().map(Ok));
    service::write_documents(
        &Pass::enter(vault)?,
        DocumentWrites::of_collection(collection, operations)?,
    )
    .await
}

/// A Document write request with `id`, `raw`, and a note.
pub(crate) fn upsert_of(id: &str, raw: &[u8]) -> DocumentUpsert {
    DocumentUpsert::default()
        .with_id(id)
        .with_raw(raw.to_vec())
        .with_note("test")
}
