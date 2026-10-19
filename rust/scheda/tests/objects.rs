//! The objects of the contract: `Scheda` opens a Vault and gives a
//! reference to it; the Vault gives its collections and its mirror the same
//! way. `Vault.Close` ends every object of the Vault, and a Vault whose
//! references are all dropped ends by itself. This version of Scheda has no
//! Index to use a port, and refuses one.

mod common;

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::projector_ref::ProjectorRef;
use scheda::v1::reranker_ref::RerankerRef;
use scheda::v1::{
    CollectionDeclaration, CollectionDeletion, CollectionKey, DocumentCreation, DocumentId,
    DocumentUpsert, EmbedderAsync, EmbedderDescriptor, Feature, MirrorSetting, OpenOptions, Query,
    SchedaSync, Text, VaultErrorCode as Code, VaultSync, Vector,
};
use scheda::{Error, Scheda};
use serde_json::json;

use common::{Scratch, open, opening, snapshot, vault as built_vault};

/// Opens a Vault as `request` asks.
fn open_with(request: scheda::v1::VaultOpening) -> Result<Arc<dyn VaultSync>, Error> {
    Scheda.open(request)?.to_sync()
}

/// The collection `name` of `vault`, which must be declared.
fn collection(vault: &Arc<dyn VaultSync>, name: &str) -> Arc<dyn scheda::v1::CollectionSync> {
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .unwrap()
        .to_sync()
        .unwrap()
}

/// The mirror of `vault`.
fn mirror(vault: &Arc<dyn VaultSync>) -> Arc<dyn scheda::v1::MirrorSync> {
    vault.mirror(Empty::default()).unwrap().to_sync().unwrap()
}

fn declaration(value: serde_json::Value) -> CollectionDeclaration {
    serde_json::from_value(value).unwrap()
}

fn declare(vault: &Arc<dyn VaultSync>, name: &str) {
    vault
        .create_collection(declaration(
            json!({"name": name, "schema": {"standard": {}}}),
        ))
        .unwrap();
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

#[test]
fn scheda_info_gives_the_version_and_the_features_of_the_build() {
    let info = Scheda.info(Empty::default()).unwrap();
    assert_eq!(info.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    let features: Vec<Feature> = info
        .features
        .iter()
        .filter_map(|feature| feature.as_known())
        .collect();
    // The Index is listed once it exists, whether or not the build has the
    // feature `index`.
    assert!(!features.contains(&Feature::FEATURE_INDEX));
    assert_eq!(
        features.contains(&Feature::FEATURE_ENCRYPT),
        cfg!(feature = "encrypt")
    );
}

#[test]
fn open_without_a_path_is_invalid_argument() {
    let error = Scheda.open(Default::default()).err().unwrap();
    assert!(error.is(Code::InvalidArgument));
}

#[test]
fn vault_info_gives_the_path_the_index_path_and_the_writer() {
    let scratch = Scratch::new("objects-info");
    let path = scratch.join("v");
    let vault = open(&path, OpenOptions::default().with_writer("writer-one")).unwrap();
    let info = vault.info(Empty::default()).unwrap();
    let root = path.canonicalize().unwrap();
    assert_eq!(info.path.as_deref(), Some(root.to_str().unwrap()));
    assert_eq!(
        info.index_path.as_deref(),
        Some(root.join(".scheda/index").to_str().unwrap())
    );
    assert_eq!(info.writer.as_deref(), Some("writer-one"));
}

#[test]
fn after_close_every_object_is_failed_precondition_and_a_second_close_is_nothing() {
    let scratch = Scratch::new("objects-closed");
    let path = scratch.join("v");
    let vault = open(&path, OpenOptions::default()).unwrap();
    declare(&vault, "notes");
    let notes = collection(&vault, "notes");
    notes.upsert(upsert("a.md", "a")).unwrap();
    let the_mirror = mirror(&vault);

    vault.close(Empty::default()).unwrap();

    let closed = |error: Error| assert!(error.is(Code::FailedPrecondition), "{error}");
    closed(vault.info(Empty::default()).unwrap_err());
    closed(
        vault
            .get_collection(CollectionKey::default().with_name("notes"))
            .err()
            .unwrap(),
    );
    closed(vault.mirror(Empty::default()).err().unwrap());
    closed(notes.get(id("a.md")).unwrap_err());
    closed(notes.upsert(upsert("b.md", "b")).unwrap_err());
    closed(the_mirror.info(Empty::default()).unwrap_err());
    vault.close(Empty::default()).unwrap();

    // The write before the close is stored; another Vault reads it.
    let again = open(&path, OpenOptions::default()).unwrap();
    let raw = collection(&again, "notes").get(id("a.md")).unwrap().raw;
    assert_eq!(raw.as_deref(), Some(&b"a"[..]));
}

#[test]
fn close_waits_for_a_running_write_and_holds_no_lock_after() {
    let scratch = Scratch::new("objects-close-waits");
    let path = scratch.join("v");
    let vault = open(&path, OpenOptions::default().with_write_timeout_ms(20_000)).unwrap();
    declare(&vault, "notes");
    let notes = collection(&vault, "notes");
    let held = hold_write_lock(&path);

    let (wrote, write_done) = mpsc::channel();
    let writer = thread::spawn(move || {
        let outcome = notes.upsert(upsert("a.md", "while closing"));
        wrote.send(Instant::now()).unwrap();
        outcome
    });
    // The write waits for the lock this test holds.
    thread::sleep(Duration::from_millis(200));
    let closer = {
        let vault = Arc::clone(&vault);
        thread::spawn(move || {
            vault.close(Empty::default()).unwrap();
            Instant::now()
        })
    };
    thread::sleep(Duration::from_millis(300));
    assert!(!closer.is_finished(), "close waits for the running write");
    drop(held);

    let written = write_done.recv().unwrap();
    let closed = closer.join().unwrap();
    writer.join().unwrap().unwrap();
    assert!(written <= closed, "the write ended before close returned");
    assert!(free(&path, "commit.lock"), "close holds no write lock");
    assert!(free(&path, "index.lock"), "close holds no index lock");
    let again = open(&path, OpenOptions::default()).unwrap();
    let raw = collection(&again, "notes").get(id("a.md")).unwrap().raw;
    assert_eq!(raw.as_deref(), Some(&b"while closing"[..]));
}

#[test]
fn a_collection_ref_finds_its_collection_again_on_each_call() {
    let scratch = Scratch::new("objects-collection-ref");
    let vault = open(&scratch.join("v"), OpenOptions::default()).unwrap();
    declare(&vault, "notes");
    let notes = collection(&vault, "notes");
    assert_eq!(
        notes.describe(Empty::default()).unwrap().name.as_deref(),
        Some("notes")
    );
    vault
        .delete_collection(CollectionDeletion::default().with_name("notes"))
        .unwrap();
    assert!(
        notes
            .describe(Empty::default())
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert!(notes.get(id("a.md")).unwrap_err().is(Code::NotFound));
    assert!(
        notes
            .create(
                DocumentCreation::default()
                    .with_id("a.md")
                    .with_raw(b"a".to_vec())
                    .with_note("test")
            )
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert!(
        vault
            .get_collection(CollectionKey::default().with_name("notes"))
            .err()
            .unwrap()
            .is(Code::NotFound)
    );
    declare(&vault, "notes");
    notes.upsert(upsert("a.md", "again")).unwrap();
    assert_eq!(
        notes.get(id("a.md")).unwrap().raw.as_deref(),
        Some(&b"again"[..])
    );
}

#[test]
fn mirror_info_gives_the_url_auto_sync_and_the_last_failure_of_a_sync_by_itself() {
    let scratch = Scratch::new("objects-mirror-info");
    let vault = open(
        &scratch.join("v"),
        OpenOptions::default().with_auto_sync(true),
    )
    .unwrap();
    let the_mirror = mirror(&vault);
    let info = the_mirror.info(Empty::default()).unwrap();
    assert_eq!(info.url.as_deref().unwrap_or(""), "");
    assert_eq!(info.auto_sync, Some(true));
    assert!(!info.last_auto_sync_error.is_set());

    // A mirror nobody can reach: the push after a write fails, the write
    // is stored, and the failure shows here.
    let url = format!(
        "file://{}",
        scratch.join("nowhere").to_string_lossy().replace('\\', "/")
    );
    the_mirror
        .set(MirrorSetting::default().with_url(url.clone()))
        .unwrap();
    declare(&vault, "notes");
    let info = the_mirror.info(Empty::default()).unwrap();
    assert_eq!(info.url.as_deref(), Some(url.as_str()));
    let failure = info.last_auto_sync_error.as_option().expect("a failure");
    assert!(failure.code.as_deref().is_some_and(|code| !code.is_empty()));
    assert!(
        collection(&vault, "notes")
            .describe(Empty::default())
            .is_ok(),
        "the write that pushed is stored"
    );
}

/// An async embedder: given outside a runtime it cannot be called.
struct Remote;

impl EmbedderAsync for Remote {
    async fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
        Ok(EmbedderDescriptor::default()
            .with_model_id("remote")
            .with_dimensions(3))
    }

    async fn embed<R>(
        &self,
        _texts: R,
    ) -> Result<impl protocontract::Stream<Item = Result<Vector, Error>> + Send + use<R>, Error>
    where
        R: protocontract::Stream<Item = Result<Text, Error>> + Send + 'static,
    {
        Ok(protocontract::IterStream::new(std::iter::empty()))
    }

    async fn embed_query(&self, _query: Query) -> Result<Vector, Error> {
        Ok(Vector::default())
    }
}

#[test]
fn a_port_is_unimplemented_whatever_its_reference_and_removing_one_is_nothing() {
    let scratch = Scratch::new("objects-bad-port");
    let path = scratch.join("v");
    for embedder in [EmbedderRef::default(), EmbedderRef::from_async(Remote)] {
        let mut request = opening(&path, OpenOptions::default());
        request.embedder = embedder.clone().into();
        let error = open_with(request).err().unwrap();
        assert!(error.is(Code::Unimplemented), "{error}");
        assert!(!path.exists(), "a refused port writes nothing");
    }
    let mut request = opening(&path, OpenOptions::default());
    request.projector = ProjectorRef::default().into();
    assert!(open_with(request).err().unwrap().is(Code::Unimplemented));
    let mut request = opening(&path, OpenOptions::default());
    request.reranker = RerankerRef::default().into();
    assert!(open_with(request).err().unwrap().is(Code::Unimplemented));
    assert!(!path.exists(), "a refused port writes nothing");
    // A Vault that exists: the refusal leaves every file as it was.
    built_vault(&path);
    let before = snapshot(&path);
    let mut request = opening(&path, OpenOptions::default());
    request.embedder = EmbedderRef::default().into();
    assert!(open_with(request).err().unwrap().is(Code::Unimplemented));
    assert_eq!(snapshot(&path), before);
    let vault = open(&path, OpenOptions::default()).unwrap();
    for embedder in [EmbedderRef::default(), EmbedderRef::from_async(Remote)] {
        let error = vault.set_embedder(embedder).unwrap_err();
        assert!(error.is(Code::Unimplemented), "{error}");
    }
    let error = vault.set_projector(ProjectorRef::default()).unwrap_err();
    assert!(error.is(Code::Unimplemented), "{error}");
    let error = vault.set_reranker(RerankerRef::default()).unwrap_err();
    assert!(error.is(Code::Unimplemented), "{error}");
    vault.remove_embedder(Empty::default()).unwrap();
    vault.remove_projector(Empty::default()).unwrap();
    vault.remove_reranker(Empty::default()).unwrap();
}
