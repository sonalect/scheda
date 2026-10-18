//! The objects of the contract: `Scheda` opens a Vault and gives a
//! reference to it; the Vault gives its collections, its mirror, and its
//! Index the same way. `Vault.Close` ends every object of the Vault, and a
//! Vault whose references are all dropped ends by itself. Ports are given
//! as references too, and change at any time.

mod common;

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::embedder_ref::EmbedderRef;
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

/// The Index of `vault`.
#[cfg(feature = "find")]
fn index(vault: &Arc<dyn VaultSync>) -> Arc<dyn scheda::v1::IndexSync> {
    vault.index(Empty::default()).unwrap().to_sync().unwrap()
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
    assert_eq!(
        features.contains(&Feature::FEATURE_FIND),
        cfg!(feature = "find")
    );
    assert_eq!(
        features.contains(&Feature::FEATURE_SEARCH),
        cfg!(feature = "search")
    );
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
    #[cfg(feature = "find")]
    let index = index(&vault);

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
    #[cfg(feature = "find")]
    closed(index.status(Empty::default()).unwrap_err());
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
fn a_port_reference_that_cannot_be_used_is_invalid_argument() {
    let scratch = Scratch::new("objects-bad-port");
    let path = scratch.join("v");
    // Without `search` an embedder is refused first: the build has no use
    // for one.
    let code = if cfg!(feature = "search") {
        Code::InvalidArgument
    } else {
        Code::Unimplemented
    };
    for embedder in [EmbedderRef::default(), EmbedderRef::from_async(Remote)] {
        let mut request = opening(&path, OpenOptions::default());
        request.embedder = embedder.clone().into();
        let error = open_with(request).err().unwrap();
        assert!(error.is(code), "{error}");
        assert!(!path.exists(), "a refused port writes nothing");
    }
    // A Vault that exists: the refusal leaves every file as it was.
    built_vault(&path);
    let before = snapshot(&path);
    let mut request = opening(&path, OpenOptions::default());
    request.embedder = EmbedderRef::default().into();
    assert!(open_with(request).err().unwrap().is(code));
    assert_eq!(snapshot(&path), before);
    let vault = open(&path, OpenOptions::default()).unwrap();
    for embedder in [EmbedderRef::default(), EmbedderRef::from_async(Remote)] {
        let error = vault.set_embedder(embedder).unwrap_err();
        assert!(error.is(code), "{error}");
    }
}

#[cfg(feature = "find")]
mod indexing {
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::{
        CollectionKey, IndexDoc, IndexSchema, IndexState, ProjectorDescriptor, ProjectorSync,
        RawDocument,
    };

    use super::*;

    /// A Projector with no columns: every Document is indexed by identity.
    struct Bare;

    impl ProjectorSync for Bare {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("bare@1"))
        }

        fn columns(&self, _collection_key: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema::default())
        }

        fn project(&self, _raw_document: RawDocument) -> Result<IndexDoc, Error> {
            Ok(IndexDoc::default())
        }
    }

    fn fast() -> OpenOptions {
        OpenOptions::default().with_index_poll_ms(10)
    }

    /// Waits until `done` holds for the state of the Index of `vault`.
    fn wait_for(vault: &Arc<dyn VaultSync>, what: &str, done: impl Fn(&IndexState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let index = index(vault);
        loop {
            let state = index.status(Empty::default()).unwrap();
            if done(&state) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {state:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn version(vault: &Arc<dyn VaultSync>) -> Option<String> {
        index(vault).status(Empty::default()).unwrap().version
    }

    #[test]
    fn a_held_watch_does_not_stop_close_and_its_next_item_is_failed_precondition() {
        let scratch = Scratch::new("objects-watch-close");
        let mut request = opening(&scratch.join("v"), fast());
        request.projector = ProjectorRef::from_sync(Bare).into();
        let vault = open_with(request).unwrap();
        wait_for(&vault, "the first round", |state| state.version.is_some());

        // One watch blocked in `next`, one held and not read.
        let mut held = index(&vault).watch(Empty::default()).unwrap();
        held.next().unwrap().unwrap();
        let mut blocked = index(&vault).watch(Empty::default()).unwrap();
        blocked.next().unwrap().unwrap();
        let reader = thread::spawn(move || blocked.collect::<Vec<_>>());
        thread::sleep(Duration::from_millis(100));

        vault.close(Empty::default()).unwrap();

        let items = reader.join().unwrap();
        let last = items.last().expect("an item after close");
        assert!(last.as_ref().unwrap_err().is(Code::FailedPrecondition));
        let error = held.next().unwrap().unwrap_err();
        assert!(error.is(Code::FailedPrecondition));
        assert!(held.next().is_none(), "the stream ends after it");
    }

    #[test]
    fn a_vault_whose_references_are_all_dropped_stops_indexing_and_a_held_collection_keeps_it() {
        let scratch = Scratch::new("objects-dropped");
        let path = scratch.join("v");
        let open_indexing = || {
            let mut request = opening(&path, fast());
            request.projector = ProjectorRef::from_sync(Bare).into();
            open_with(request).unwrap()
        };
        let indexing = open_indexing();
        declare(&indexing, "notes");
        wait_for(&indexing, "the declaration indexed", |state| {
            state.version.is_some() && state.pending == Some(0)
        });
        // A Vault without ports never indexes; it reads what the other one
        // keeps.
        let reader = open(&path, fast()).unwrap();
        let notes = collection(&reader, "notes");

        // Held through a collection, the indexing Vault keeps indexing.
        let kept = collection(&indexing, "notes");
        drop(indexing);
        let before = version(&reader);
        notes.upsert(upsert("a.md", "a")).unwrap();
        wait_for(&reader, "the write indexed", |state| {
            state.version != before
        });

        // Dropped whole, it indexes nothing more.
        drop(kept);
        thread::sleep(Duration::from_millis(100));
        assert!(free(&path, "index.lock"));
        let before = version(&reader);
        notes.upsert(upsert("b.md", "b")).unwrap();
        thread::sleep(Duration::from_millis(500));
        assert_eq!(version(&reader), before, "no Vault indexes");

        // Given a Projector, the reader takes over.
        reader.set_projector(ProjectorRef::from_sync(Bare)).unwrap();
        wait_for(&reader, "the reader indexing", |state| {
            state.version != before
        });
        reader.remove_projector(Empty::default()).unwrap();
        let error = index(&reader).reindex(Empty::default()).unwrap_err();
        assert!(error.is(Code::FailedPrecondition), "{error}");
        assert!(error.to_string().contains("Projector"), "{error}");
    }

    #[cfg(feature = "search")]
    #[test]
    fn removing_the_embedder_a_vector_column_needs_stops_indexing_until_one_is_given() {
        use std::num::NonZeroUsize;

        use scheda::FixedEmbedder;
        use scheda::v1::{ColumnKind, IndexColumn, IndexValue};

        /// A Projector of one `vector` column: the bytes as text.
        struct Texts;

        impl ProjectorSync for Texts {
            fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
                Ok(ProjectorDescriptor::default().with_projection_id("texts@1"))
            }

            fn columns(&self, _collection_key: CollectionKey) -> Result<IndexSchema, Error> {
                Ok(IndexSchema {
                    columns: vec![
                        IndexColumn::default()
                            .with_name("text")
                            .with_kind(ColumnKind::COLUMN_KIND_TEXT)
                            .with_vector(true),
                    ],
                    ..IndexSchema::default()
                })
            }

            fn project(&self, raw_document: RawDocument) -> Result<IndexDoc, Error> {
                let text = String::from_utf8_lossy(raw_document.raw.as_deref().unwrap_or(&[]))
                    .into_owned();
                let mut doc = IndexDoc::default();
                doc.values.insert(
                    "text".to_owned(),
                    IndexValue {
                        value: Some(scheda::v1::index_value::Value::Text(text)),
                        ..IndexValue::default()
                    },
                );
                Ok(doc)
            }
        }

        let embedder = || {
            EmbedderRef::from_sync(FixedEmbedder::new(
                "model-a",
                2,
                NonZeroUsize::new(8).unwrap(),
                |texts: Vec<String>| {
                    Ok(texts
                        .iter()
                        .map(|text| vec![text.len() as f32, 1.0])
                        .collect())
                },
            ))
        };
        let scratch = Scratch::new("objects-embedder");
        let mut request = opening(&scratch.join("v"), fast());
        request.projector = ProjectorRef::from_sync(Texts).into();
        request.embedder = embedder().into();
        let vault = open_with(request).unwrap();
        declare(&vault, "notes");
        let notes = collection(&vault, "notes");
        notes.upsert(upsert("a.md", "one")).unwrap();
        wait_for(&vault, "the first write indexed", |state| {
            state.version.is_some() && state.pending == Some(0)
        });

        vault.remove_embedder(Empty::default()).unwrap();
        let error = index(&vault).reindex(Empty::default()).unwrap_err();
        assert!(error.is(Code::FailedPrecondition), "{error}");
        assert!(error.to_string().contains("embedder"), "{error}");
        let before = version(&vault);
        notes.upsert(upsert("b.md", "two")).unwrap();
        thread::sleep(Duration::from_millis(500));
        assert_eq!(version(&vault), before, "no round without the embedder");

        vault.set_embedder(embedder()).unwrap();
        wait_for(&vault, "the write indexed again", |state| {
            state.version != before
        });
    }
}
