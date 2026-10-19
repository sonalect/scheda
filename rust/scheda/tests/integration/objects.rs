//! The objects of the contract: `Scheda` opens a Vault and gives a
//! reference to it; the Vault gives its collections and its mirror the same
//! way. `Vault.Close` ends every object of the Vault, and a Vault whose
//! references are all dropped ends by itself. This version of Scheda has no
//! Index to use a port, and refuses one.

use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::projector_ref::ProjectorRef;
use scheda::v1::reranker_ref::RerankerRef;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionDeletion, CollectionKey, DocumentCreation,
    DocumentId, DocumentUpsert, DynVaultAsync, EmbedderAsync, EmbedderDescriptor, Feature,
    MirrorAsync, MirrorSetting, OpenOptions, Query, SchedaAsync, Text, VaultAsync,
    VaultErrorCode as Code, Vector,
};
use scheda::{Error, Scheda};
use serde_json::json;

use crate::common::{
    Scratch, both, collection, mirror, open, opening, snapshot, vault as built_vault,
};

/// Opens a Vault as `request` asks.
async fn open_with(request: scheda::v1::VaultOpening) -> Result<DynVaultAsync, Error> {
    Scheda.open(request).await?.to_async_inline()
}

fn declaration(value: serde_json::Value) -> CollectionDeclaration {
    serde_json::from_value(value).unwrap()
}

async fn declare(vault: &DynVaultAsync, name: &str) {
    vault
        .create_collection(declaration(
            json!({"name": name, "schema": {"standard": {}}}),
        ))
        .await
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
    both(|| async {
        let info = Scheda.info(Empty::default()).await.unwrap();
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
    });
}

#[test]
fn open_without_a_path_is_invalid_argument() {
    both(|| async {
        let error = Scheda.open(Default::default()).await.err().unwrap();
        assert!(error.is(Code::InvalidArgument));
    });
}

#[test]
fn vault_info_gives_the_path_the_index_path_and_the_writer() {
    both(|| async {
        let scratch = Scratch::new("objects-info");
        let path = scratch.join("v");
        let vault = open(&path, OpenOptions::default().with_writer("writer-one"))
            .await
            .unwrap();
        let info = vault.info(Empty::default()).await.unwrap();
        let root = path.canonicalize().unwrap();
        assert_eq!(info.path.as_deref(), Some(root.to_str().unwrap()));
        assert_eq!(
            info.index_path.as_deref(),
            Some(root.join(".scheda/index").to_str().unwrap())
        );
        assert_eq!(info.writer.as_deref(), Some("writer-one"));
    });
}

#[test]
fn after_close_every_object_is_failed_precondition_and_a_second_close_is_nothing() {
    both(|| async {
        let scratch = Scratch::new("objects-closed");
        let path = scratch.join("v");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        declare(&vault, "notes").await;
        let notes = collection(&vault, "notes").await.unwrap();
        notes.upsert(upsert("a.md", "a")).await.unwrap();
        let the_mirror = mirror(&vault).await.unwrap();

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
        closed(notes.get(id("a.md")).await.unwrap_err());
        closed(notes.upsert(upsert("b.md", "b")).await.unwrap_err());
        closed(the_mirror.info(Empty::default()).await.unwrap_err());
        vault.close(Empty::default()).await.unwrap();

        // The write before the close is stored; another Vault reads it.
        let again = open(&path, OpenOptions::default()).await.unwrap();
        let raw = collection(&again, "notes")
            .await
            .unwrap()
            .get(id("a.md"))
            .await
            .unwrap()
            .raw;
        assert_eq!(raw.as_deref(), Some(&b"a"[..]));
    });
}

#[test]
fn close_waits_for_a_running_write_and_holds_no_lock_after() {
    both(|| async {
        let scratch = Scratch::new("objects-close-waits");
        let path = scratch.join("v");
        let vault = open(&path, OpenOptions::default().with_write_timeout_ms(20_000))
            .await
            .unwrap();
        declare(&vault, "notes").await;
        let notes = collection(&vault, "notes").await.unwrap();
        let held = hold_write_lock(&path);

        let writer = tokio::spawn(async move {
            let outcome = notes.upsert(upsert("a.md", "while closing")).await;
            (Instant::now(), outcome)
        });
        // The write waits for the lock this test holds.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let closer = {
            let vault = vault.clone();
            tokio::spawn(async move {
                vault.close(Empty::default()).await.unwrap();
                Instant::now()
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!closer.is_finished(), "close waits for the running write");
        drop(held);

        let (written, outcome) = writer.await.unwrap();
        let closed = closer.await.unwrap();
        outcome.unwrap();
        assert!(written <= closed, "the write ended before close returned");
        assert!(free(&path, "commit.lock"), "close holds no write lock");
        assert!(free(&path, "index.lock"), "close holds no index lock");
        let again = open(&path, OpenOptions::default()).await.unwrap();
        let raw = collection(&again, "notes")
            .await
            .unwrap()
            .get(id("a.md"))
            .await
            .unwrap()
            .raw;
        assert_eq!(raw.as_deref(), Some(&b"while closing"[..]));
    });
}

#[test]
fn a_collection_ref_finds_its_collection_again_on_each_call() {
    both(|| async {
        let scratch = Scratch::new("objects-collection-ref");
        let vault = open(&scratch.join("v"), OpenOptions::default())
            .await
            .unwrap();
        declare(&vault, "notes").await;
        let notes = collection(&vault, "notes").await.unwrap();
        assert_eq!(
            notes
                .describe(Empty::default())
                .await
                .unwrap()
                .name
                .as_deref(),
            Some("notes")
        );
        vault
            .delete_collection(CollectionDeletion::default().with_name("notes"))
            .await
            .unwrap();
        assert!(
            notes
                .describe(Empty::default())
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        assert!(notes.get(id("a.md")).await.unwrap_err().is(Code::NotFound));
        assert!(
            notes
                .create(
                    DocumentCreation::default()
                        .with_id("a.md")
                        .with_raw(b"a".to_vec())
                        .with_note("test")
                )
                .await
                .unwrap_err()
                .is(Code::NotFound)
        );
        assert!(
            vault
                .get_collection(CollectionKey::default().with_name("notes"))
                .await
                .err()
                .unwrap()
                .is(Code::NotFound)
        );
        declare(&vault, "notes").await;
        notes.upsert(upsert("a.md", "again")).await.unwrap();
        assert_eq!(
            notes.get(id("a.md")).await.unwrap().raw.as_deref(),
            Some(&b"again"[..])
        );
    });
}

#[test]
fn mirror_info_gives_the_url_auto_sync_and_the_last_failure_of_a_sync_by_itself() {
    both(|| async {
        let scratch = Scratch::new("objects-mirror-info");
        let vault = open(
            &scratch.join("v"),
            OpenOptions::default().with_auto_sync(true),
        )
        .await
        .unwrap();
        let the_mirror = mirror(&vault).await.unwrap();
        let info = the_mirror.info(Empty::default()).await.unwrap();
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
            .await
            .unwrap();
        declare(&vault, "notes").await;
        let info = the_mirror.info(Empty::default()).await.unwrap();
        assert_eq!(info.url.as_deref(), Some(url.as_str()));
        let failure = info.last_auto_sync_error.as_option().expect("a failure");
        assert!(failure.code.as_deref().is_some_and(|code| !code.is_empty()));
        assert!(
            collection(&vault, "notes")
                .await
                .unwrap()
                .describe(Empty::default())
                .await
                .is_ok(),
            "the write that pushed is stored"
        );
    });
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
    both(|| async {
        let scratch = Scratch::new("objects-bad-port");
        let path = scratch.join("v");
        for embedder in [EmbedderRef::default(), EmbedderRef::from_async(Remote)] {
            let mut request = opening(&path, OpenOptions::default());
            request.embedder = embedder.clone().into();
            let error = open_with(request).await.err().unwrap();
            assert!(error.is(Code::Unimplemented), "{error}");
            assert!(!path.exists(), "a refused port writes nothing");
        }
        let mut request = opening(&path, OpenOptions::default());
        request.projector = ProjectorRef::default().into();
        assert!(
            open_with(request)
                .await
                .err()
                .unwrap()
                .is(Code::Unimplemented)
        );
        let mut request = opening(&path, OpenOptions::default());
        request.reranker = RerankerRef::default().into();
        assert!(
            open_with(request)
                .await
                .err()
                .unwrap()
                .is(Code::Unimplemented)
        );
        assert!(!path.exists(), "a refused port writes nothing");
        // A Vault that exists: the refusal leaves every file as it was.
        built_vault(&path);
        let before = snapshot(&path);
        let mut request = opening(&path, OpenOptions::default());
        request.embedder = EmbedderRef::default().into();
        assert!(
            open_with(request)
                .await
                .err()
                .unwrap()
                .is(Code::Unimplemented)
        );
        assert_eq!(snapshot(&path), before);
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        for embedder in [EmbedderRef::default(), EmbedderRef::from_async(Remote)] {
            let error = vault.set_embedder(embedder).await.unwrap_err();
            assert!(error.is(Code::Unimplemented), "{error}");
        }
        let error = vault
            .set_projector(ProjectorRef::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::Unimplemented), "{error}");
        let error = vault
            .set_reranker(RerankerRef::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::Unimplemented), "{error}");
        vault.remove_embedder(Empty::default()).await.unwrap();
        vault.remove_projector(Empty::default()).await.unwrap();
        vault.remove_reranker(Empty::default()).await.unwrap();
    });
}
