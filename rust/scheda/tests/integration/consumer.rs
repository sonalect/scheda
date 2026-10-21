//! The use of a consumer that reaches Scheda through one adapter: the shapes
//! of its calls, run end to end against a Vault on disk.
//!
//! The consumer is sync code. It holds a multi-thread tokio runtime and
//! takes the blocking form of each object inside it (protocontract's
//! `Blocking`), and it also calls from inside `tokio::task::spawn_blocking`.
//! It opens a Vault with a secret, an `author`, and a Projector that gives
//! columns to one collection only (a text for every leg, two keyword
//! columns for filters); it installs its embedder after the open and passes
//! no reranker. Its writes are idempotent by design: a collection that is
//! append-only keeps what is stored, another is "add or replace", a delete
//! of a missing Document is nothing, and a group of writes is one commit.
//! It ensures its collections (creates the missing ones, rewrites a
//! declaration that differs, leaves a matching one alone), keeps one
//! `singleton` collection, reads through `get`, listings, history, and a
//! vector-only search given the token of its write, rebuilds the Index
//! while it searches, and `sync`s a mirror. Several processes open one
//! Vault.
//!
//! Names here (`entries`, `notes`, `settings`, `trail`, `secrets`) are
//! neutral: nothing of a consumer's domain appears in this file.

#![cfg(feature = "index")]

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda::v1::collection_result::Result as CollectionArm;
use scheda::v1::document_operation::Operation;
use scheda::v1::document_result::Result as ResultArm;
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::projector_ref::ProjectorRef;
use scheda::v1::query_item::Item as QueryArm;
use scheda::v1::rebuild::{Cause, Stage};
use scheda::v1::search_item::Item;
use scheda::v1::search_summary::VectorLeg;
use scheda::v1::{
    CollectionDeclaration, CollectionKey, CollectionOperation, CollectionSync, ColumnKind,
    DocumentCreation, DocumentDeletion, DocumentHistoryEntry, DocumentId, DocumentOperation,
    DocumentQuery, DocumentSearch, DocumentUpsert, IndexColumn, IndexDoc, IndexSchema, IndexState,
    IndexSync, IndexValue, MirrorSetting, OpenMode, OpenOptions, ProjectorDescriptor,
    ProjectorSync, QueryItem, RawDocument, SchedaAsync, SearchHit, SearchItem,
    VaultErrorCode as Code, VaultOpening, VaultSync, index_value,
};
use scheda::{Error, FixedEmbedder, Scheda};
use serde_json::{Value, json};
use tokio::runtime::{Handle, Runtime};

use crate::common::{
    Scratch, command, file_url, git, opening, snapshot, vault as built_vault, work_tree, write,
};

const DEADLINE: Duration = Duration::from_secs(60);

// The runtime of the consumer.

/// The runtime a consumer without async code holds for Scheda.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    })
}

/// The runtime of the thread when it has one (a thread of
/// `spawn_blocking`), the consumer's own otherwise.
fn handle() -> Handle {
    Handle::try_current().unwrap_or_else(|_| runtime().handle().clone())
}

// The Vault the consumer builds.

/// Open options as the consumer sets them: an author, the secret, and a
/// quick indexer so a test does not wait on the default poll.
fn options() -> OpenOptions {
    let value = json!({
        "author": {"name": "Test Author", "email": "author@example.test"},
        "indexPollMs": 10,
        "writeTimeoutMs": 60000,
    });
    #[cfg(feature = "encrypt")]
    let value = {
        let mut value = value;
        value["encryption"] = json!({"passphrase": "a passphrase from the environment"});
        value
    };
    serde_json::from_value(value).unwrap()
}

/// A Projector that gives columns to `entries` only: `text` (fts, vector,
/// a scalar index) and the keyword columns `topic` and `kind` (scalar
/// indices). Every other collection is indexed by identity.
struct Columns;

fn column(name: &str, fts: bool, vector: bool) -> IndexColumn {
    IndexColumn::default()
        .with_name(name)
        .with_kind(ColumnKind::COLUMN_KIND_TEXT)
        .with_scalar_index(true)
        .with_fts(fts)
        .with_vector(vector)
}

impl ProjectorSync for Columns {
    fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
        Ok(ProjectorDescriptor::default().with_projection_id("columns@1"))
    }

    fn columns(&self, collection: CollectionKey) -> Result<IndexSchema, Error> {
        if collection.name.as_deref() != Some("entries") {
            return Ok(IndexSchema::default());
        }
        Ok(IndexSchema {
            columns: vec![
                column("text", true, true),
                column("topic", false, false),
                column("kind", false, false),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
        let json: Value =
            serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
        let mut doc = IndexDoc::default();
        for name in ["text", "topic", "kind"] {
            if let Some(text) = json.get(name).and_then(Value::as_str) {
                doc.values.insert(
                    name.to_owned(),
                    IndexValue {
                        value: Some(index_value::Value::Text(text.to_owned())),
                        ..IndexValue::default()
                    },
                );
            }
        }
        Ok(doc)
    }
}

/// The vector of a text: how many words of three topics it holds, and a
/// little of a fourth so no vector is empty.
fn topic_vector(text: &str) -> Vec<f32> {
    let topics: [&[&str]; 3] = [
        &["cat", "kitten", "feline"],
        &["dog", "puppy", "canine"],
        &["car", "truck", "engine"],
    ];
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut vector: Vec<f32> = topics
        .iter()
        .map(|topic| words.iter().filter(|word| topic.contains(word)).count() as f32)
        .collect();
    vector.push(0.05);
    vector
}

/// A deterministic embedder of the three topics, as slow per portion as
/// `pause` says.
fn embedder_with(pause: Duration) -> EmbedderRef {
    EmbedderRef::from_sync(FixedEmbedder::new(
        "topics@1",
        4,
        NonZeroUsize::new(8).unwrap(),
        move |texts: Vec<String>| {
            std::thread::sleep(pause);
            Ok(texts.iter().map(|text| topic_vector(text)).collect())
        },
    ))
}

fn embedder() -> EmbedderRef {
    embedder_with(Duration::ZERO)
}

/// The request that opens `path` with the Projector, and the embedder
/// when `with_embedder`. No reranker.
fn request(path: &Path, with_embedder: bool, options: OpenOptions) -> VaultOpening {
    let mut request = opening(path, options);
    request.projector = ProjectorRef::from_sync(Columns).into();
    if with_embedder {
        request.embedder = embedder().into();
    }
    request
}

/// A Vault the consumer opened, closed when it is dropped, whether the test
/// passed or failed. The consumer's runtime outlives every test: a Vault
/// that is only dropped lets its indexer finish its round there, in the
/// folder the test removes.
struct Opened(Arc<dyn VaultSync>);

impl std::ops::Deref for Opened {
    type Target = Arc<dyn VaultSync>;

    fn deref(&self) -> &Arc<dyn VaultSync> {
        &self.0
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // A Vault the test closed already: a second `Close` is nothing.
        let _ = self.0.close(Empty::default());
    }
}

/// Opens with `request`, in the blocking form.
fn open_request(request: VaultOpening) -> Result<Opened, Error> {
    let handle = handle();
    let reference = handle.block_on(Scheda.open(request))?;
    let _inside = handle.enter();
    reference.to_sync().map(Opened)
}

fn open_vault(path: &Path, with_embedder: bool) -> Opened {
    open_request(request(path, with_embedder, options())).unwrap()
}

fn declaration(value: Value) -> CollectionDeclaration {
    serde_json::from_value(value).unwrap()
}

/// The collections the consumer ensures.
fn wanted() -> Vec<CollectionDeclaration> {
    let mut all = vec![
        json!({"name": "entries", "schema": {"standard": {}}}),
        json!({"name": "notes", "schema": {"standard": {}}}),
        json!({"name": "settings", "schema": {"singleton": {"id": "config.yaml"}}}),
        json!({"name": "trail", "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_NO_VECTORS"}}),
    ];
    if cfg!(feature = "encrypt") {
        all.push(json!({"name": "secrets", "schema": {"standard": {}, "encrypted": true}}));
    }
    all.into_iter().map(declaration).collect()
}

/// What the consumer's ensure step does: list, create what is missing,
/// update what differs, leave a match alone. Returns the operations it
/// sent; when there are none it sends nothing.
fn ensure(vault: &Arc<dyn VaultSync>, wanted: &[CollectionDeclaration]) -> usize {
    let stored = vault
        .list_collections(Empty::default())
        .unwrap()
        .collections;
    let mut operations = Vec::new();
    for want in wanted {
        match stored.iter().find(|have| have.name == want.name) {
            None => operations.push(CollectionOperation {
                operation: Some(scheda::v1::collection_operation::Operation::Create(
                    Box::new(want.clone()),
                )),
                ..CollectionOperation::default()
            }),
            Some(have) if have != want => operations.push(CollectionOperation {
                operation: Some(scheda::v1::collection_operation::Operation::Update(
                    Box::new(want.clone()),
                )),
                ..CollectionOperation::default()
            }),
            Some(_) => {}
        }
    }
    let count = operations.len();
    if count > 0 {
        let reply = vault
            .batch_collections(Box::new(operations.into_iter().map(Ok)))
            .unwrap();
        assert_eq!(reply.results.len(), count);
        for result in &reply.results {
            assert!(matches!(result.result, Some(CollectionArm::Collection(_))));
        }
    }
    count
}

fn collection(vault: &Arc<dyn VaultSync>, name: &str) -> Arc<dyn CollectionSync> {
    let reference = vault
        .get_collection(CollectionKey::default().with_name(name))
        .unwrap();
    let _inside = handle().enter();
    reference.to_sync().unwrap()
}

fn index(vault: &Arc<dyn VaultSync>) -> Arc<dyn IndexSync> {
    let reference = vault.index(Empty::default()).unwrap();
    let _inside = handle().enter();
    reference.to_sync().unwrap()
}

// What the test looks at.

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = command(dir, args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn head(dir: &Path) -> String {
    git_out(dir, &["rev-parse", "HEAD"])
}

fn commits(dir: &Path) -> usize {
    git_out(dir, &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

/// Waits until the Index of `vault` reflects the stored state of the
/// Vault, on the Index's own state, with a deadline.
fn wait_indexed(vault: &Arc<dyn VaultSync>) {
    let deadline = Instant::now() + DEADLINE;
    let status = index(vault);
    loop {
        let stored = vault.info(Empty::default()).unwrap().version;
        let state = status.status(Empty::default()).unwrap();
        if state.version.is_some() && state.version == stored {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the Index: {state:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn upsert_op(id: &str, raw: &[u8]) -> Operation {
    Operation::Upsert(Box::new(
        DocumentUpsert::default()
            .with_id(id)
            .with_raw(raw.to_vec())
            .with_note("upsert"),
    ))
}

fn create_op(id: &str, raw: &[u8]) -> Operation {
    Operation::Create(Box::new(
        DocumentCreation::default()
            .with_id(id)
            .with_raw(raw.to_vec())
            .with_keep_existing(true)
            .with_note("create"),
    ))
}

fn delete_op(id: &str) -> Operation {
    Operation::Delete(Box::new(
        DocumentDeletion::default()
            .with_id(id)
            .with_ignore_missing(true)
            .with_note("delete"),
    ))
}

fn send(
    target: &Arc<dyn CollectionSync>,
    operations: Vec<Operation>,
) -> scheda::v1::DocumentBatchResult {
    let items: Vec<Result<DocumentOperation, Error>> = operations
        .into_iter()
        .map(|operation| {
            Ok(DocumentOperation {
                operation: Some(operation),
                ..DocumentOperation::default()
            })
        })
        .collect();
    target.batch(Box::new(items.into_iter())).unwrap()
}

/// The Document or the delete a batch result holds, as comparable text.
fn shape(reply: &scheda::v1::DocumentBatchResult) -> Vec<String> {
    reply
        .results
        .iter()
        .map(|result| match result.result.as_ref().unwrap() {
            ResultArm::Document(document) => format!(
                "{}:{}:{:?}",
                document.id.clone().unwrap_or_default(),
                document.revision.clone().unwrap_or_default(),
                document.raw
            ),
            ResultArm::Deleted(_) => "deleted".to_owned(),
        })
        .collect()
}

fn history(target: &Arc<dyn CollectionSync>, id: &str) -> Vec<DocumentHistoryEntry> {
    target
        .history(DocumentId::default().with_id(id))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The ids of the Documents a query streams after its summary.
fn ids(stream: BoxIter<'static, Result<QueryItem, Error>>) -> Vec<String> {
    stream
        .filter_map(|item| match item.unwrap().item {
            Some(QueryArm::Document(document)) => document.id,
            _ => None,
        })
        .collect()
}

/// Every id of `target` the Index holds, or the failure of the query: a
/// Vault whose Index is not built yet answers `INDEX_REBUILDING`.
fn all_ids(target: &Arc<dyn CollectionSync>) -> Result<Vec<String>, Error> {
    target.query(DocumentQuery::default()).map(ids)
}

fn search_of(value: Value) -> DocumentSearch {
    serde_json::from_value(value).unwrap()
}

/// The whole stream of a search: its summary, then its hits.
fn search(target: &Arc<dyn CollectionSync>, value: Value) -> (VectorLeg, Vec<SearchHit>) {
    let items: Vec<SearchItem> = target
        .search(search_of(value))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let leg = match items.first().and_then(|item| item.item.as_ref()) {
        Some(Item::Summary(summary)) => summary
            .vector_leg
            .as_ref()
            .and_then(|leg| leg.as_known())
            .unwrap(),
        other => panic!("the first item is not the summary: {other:?}"),
    };
    let hits = items[1..]
        .iter()
        .map(|item| match item.item.as_ref() {
            Some(Item::Hit(hit)) => (**hit).clone(),
            other => panic!("not a hit: {other:?}"),
        })
        .collect();
    (leg, hits)
}

fn hit_id(hit: &SearchHit) -> String {
    hit.document
        .as_option()
        .and_then(|document| document.id.clone())
        .unwrap_or_default()
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

/// A Vault with the consumer's collections, the embedder installed at the
/// open.
struct Fixture {
    scratch: Scratch,
    path: PathBuf,
    vault: Opened,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let scratch = Scratch::new(name);
        let path = scratch.join("vault");
        let vault = open_vault(&path, true);
        ensure(&vault, &wanted());
        let path = path.canonicalize().unwrap();
        Fixture {
            scratch,
            path,
            vault,
        }
    }

    fn collection(&self, name: &str) -> Arc<dyn CollectionSync> {
        collection(&self.vault, name)
    }

    /// The corpus of `entries`; the token of the state that holds it.
    fn corpus(&self) -> String {
        let entries = self.collection("entries");
        let docs = [
            ("cat1", "cat kitten feline", "pet"),
            ("cat2", "kitten feline", "pet"),
            ("dog1", "dog puppy canine", "pet"),
            ("car1", "car engine truck", "vehicle"),
        ];
        let operations = docs
            .iter()
            .map(|(id, text, kind)| {
                let raw = json!({"text": text, "topic": "t", "kind": kind});
                upsert_op(id, raw.to_string().as_bytes())
            })
            .collect();
        send(&entries, operations).version.unwrap()
    }
}

// 1. Open, ports, encryption.

#[test]
fn open_with_a_projector_an_embedder_an_author_and_a_secret_writes_and_reads_back() {
    let fx = Fixture::new("consumer-open");
    let entries = fx.collection("entries");
    let written = entries
        .upsert(
            DocumentUpsert::default()
                .with_id("one")
                .with_raw(br#"{"text": "cat"}"#.to_vec())
                .with_note("first"),
        )
        .unwrap();
    assert_eq!(written.version.as_deref(), Some(head(&fx.path).as_str()));
    let got = entries.get(DocumentId::default().with_id("one")).unwrap();
    assert_eq!(got.raw.as_deref(), Some(&br#"{"text": "cat"}"#[..]));
    assert_eq!(got.revision, written.document.as_option().unwrap().revision);
    let versions = history(&entries, "one");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].note.as_deref(), Some("first"));
    let author = versions[0].author.as_option().unwrap();
    assert_eq!(author.name.as_deref(), Some("Test Author"));
    assert_eq!(author.email.as_deref(), Some("author@example.test"));

    #[cfg(feature = "encrypt")]
    {
        let secrets = fx.collection("secrets");
        secrets
            .upsert(
                DocumentUpsert::default()
                    .with_id("s1")
                    .with_raw(b"a secret body".to_vec())
                    .with_note("sealed"),
            )
            .unwrap();
        let got = secrets.get(DocumentId::default().with_id("s1")).unwrap();
        assert_eq!(got.raw.as_deref(), Some(&b"a secret body"[..]));
        let on_disk = std::fs::read(fx.path.join("secrets/s1")).unwrap();
        assert!(
            !String::from_utf8_lossy(&on_disk).contains("a secret body"),
            "the file holds armor, not the body"
        );
        assert_eq!(
            history(&secrets, "s1")[0].raw.as_deref(),
            Some(&b"a secret body"[..])
        );
    }
}

// 2. Idempotent batches.

fn the_batches_of_the_consumer(target: &Arc<dyn CollectionSync>, path: &Path) {
    // `gone` is stored first so the delete has something to remove once.
    target
        .upsert(
            DocumentUpsert::default()
                .with_id("gone")
                .with_raw(b"old".to_vec())
                .with_note("seed"),
        )
        .unwrap();
    let make = || {
        vec![
            upsert_op("replaced", b"new"),
            create_op("kept", b"first"),
            delete_op("gone"),
        ]
    };
    let first = send(target, make());
    let after_first = head(path);
    assert_eq!(first.version.as_deref(), Some(after_first.as_str()));
    let count = commits(path);
    let revisions = shape(&first);
    // One write: every version it made, the deletion included, has its id.
    let write = history(target, "replaced")[0].write_id.clone();
    assert!(write.as_deref().is_some_and(|id| !id.is_empty()));
    assert_eq!(history(target, "kept")[0].write_id, write);
    assert_eq!(history(target, "gone")[0].write_id, write);
    assert_ne!(history(target, "gone")[1].write_id, write, "the seed");

    let second = send(target, make());
    assert_eq!(shape(&second), revisions, "the same results");
    assert_eq!(head(path), after_first, "HEAD does not move");
    assert_eq!(
        second.version, first.version,
        "the token of the state it found"
    );
    assert_eq!(commits(path), count);
    assert_eq!(history(target, "replaced").len(), 1);
    assert_eq!(history(target, "kept").len(), 1);

    // An id the append-only write finds stored is left as it is.
    let again = send(target, vec![create_op("kept", b"changed")]);
    assert!(shape(&again)[0].contains(&format!("{:?}", Some(b"first".to_vec()))));
    assert_eq!(head(path), after_first);
    assert_eq!(
        target
            .get(DocumentId::default().with_id("kept"))
            .unwrap()
            .raw
            .as_deref(),
        Some(&b"first"[..])
    );
}

#[test]
fn each_batch_sent_twice_changes_nothing_the_second_time() {
    let fx = Fixture::new("consumer-batch");
    the_batches_of_the_consumer(&fx.collection("notes"), &fx.path);
}

// 3. Ensure the collections.

#[test]
fn ensuring_the_collections_again_finds_nothing_to_do() {
    let scratch = Scratch::new("consumer-ensure");
    let path = scratch.join("vault");
    let vault = open_vault(&path, true);
    let wanted = wanted();
    assert_eq!(ensure(&vault, &wanted), wanted.len());
    let stored = head(&path);
    let count = commits(&path);
    assert_eq!(ensure(&vault, &wanted), 0, "a matching declaration is left");
    assert_eq!(head(&path), stored);
    assert_eq!(commits(&path), count);

    // A declaration that differs is rewritten, alone.
    let mut changed = wanted.clone();
    for item in &mut changed {
        if item.name.as_deref() == Some("notes") {
            *item = declaration(json!({
                "name": "notes",
                "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_NO_VECTORS"},
            }));
        }
    }
    assert_eq!(ensure(&vault, &changed), 1);
    assert_eq!(commits(&path), count + 1);
    assert_eq!(ensure(&vault, &changed), 0);

    // The singleton collection is written with an upsert, and the second
    // upsert of the same bytes changes nothing.
    let settings = collection(&vault, "settings");
    let put = || {
        settings
            .upsert(
                DocumentUpsert::default()
                    .with_id("config.yaml")
                    .with_raw(b"a: 1\n".to_vec())
                    .with_note("settings"),
            )
            .unwrap()
    };
    let first = put();
    let stored = head(&path);
    let second = put();
    assert_eq!(
        second.document.as_option().unwrap().revision,
        first.document.as_option().unwrap().revision
    );
    assert_eq!(head(&path), stored);
    assert_eq!(history(&settings, "config.yaml").len(), 1);
}

// 4. A vector-only search given the token of a write.

#[test]
fn a_vector_only_search_given_the_token_of_a_write_finds_it_and_ranks_best_first() {
    let fx = Fixture::new("consumer-vector");
    // No wait on the Index: the token of the write is the search's.
    let token = fx.corpus();
    let entries = fx.collection("entries");
    let (leg, hits) = search(
        &entries,
        json!({"text": "feline", "k": 3, "lexicalWeight": 0, "notBefore": token}),
    );
    assert_eq!(leg, VectorLeg::VECTOR_LEG_RAN);
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().all(|hit| hit.lexical_score.is_none()));
    assert!(hits.iter().all(|hit| hit.vector_score.is_some()));
    let first_two = [hit_id(&hits[0]), hit_id(&hits[1])];
    assert!(
        first_two.iter().all(|id| id.starts_with("cat")),
        "{first_two:?}"
    );
    let scores: Vec<f64> = hits.iter().filter_map(|hit| hit.score).collect();
    assert_eq!(scores.len(), 3);
    assert!(
        scores.windows(2).all(|pair| pair[0] >= pair[1]),
        "{scores:?}"
    );

    // One more write, found by the next search with its token.
    let token = entries
        .upsert(
            DocumentUpsert::default()
                .with_id("cat3")
                .with_raw(br#"{"text": "feline"}"#.to_vec())
                .with_note("cat3"),
        )
        .unwrap()
        .version
        .unwrap();
    let (_, hits) = search(
        &entries,
        json!({"text": "feline", "k": 1, "lexicalWeight": 0, "notBefore": token}),
    );
    assert_eq!(hit_id(&hits[0]), "cat3");
}

// 5. Index.Reindex while searches go on.

#[test]
fn searches_go_on_while_the_index_is_rebuilt() {
    let scratch = Scratch::new("consumer-reindex");
    let path = scratch.join("vault");
    let mut request = request(&path, false, options());
    // An embedder slow enough that the searches meet the rebuild.
    request.embedder = embedder_with(Duration::from_millis(5)).into();
    let vault = open_request(request).unwrap();
    ensure(&vault, &wanted());
    let entries = collection(&vault, "entries");
    let operations = (0..400)
        .map(|n| {
            let text = ["cat kitten", "dog puppy", "car truck"][n % 3];
            upsert_op(
                &format!("e{n:03}"),
                json!({"text": text, "kind": "k"}).to_string().as_bytes(),
            )
        })
        .collect();
    let token = send(&entries, operations).version.unwrap();
    let probe = json!({"text": "kitten", "k": 5, "lexicalWeight": 0, "notBefore": token});
    let (_, before) = search(&entries, probe.clone());
    assert_eq!(before.len(), 5);

    let rebuilt = Arc::new(AtomicBool::new(false));
    let during = Arc::new(AtomicUsize::new(0));
    let searcher = {
        let entries = Arc::clone(&entries);
        let index = index(&vault);
        let rebuilt = Arc::clone(&rebuilt);
        let during = Arc::clone(&during);
        let probe = probe.clone();
        std::thread::spawn(move || {
            while !rebuilt.load(Ordering::SeqCst) {
                let running =
                    stage(&index.status(Empty::default()).unwrap()) == Some(Stage::STAGE_RUNNING);
                let (leg, hits) = search(&entries, probe.clone());
                assert_eq!(leg, VectorLeg::VECTOR_LEG_RAN);
                assert_eq!(hits.len(), 5, "a search during the rebuild answers");
                if running {
                    during.fetch_add(1, Ordering::SeqCst);
                }
            }
        })
    };
    let index = index(&vault);
    index.reindex(Empty::default()).unwrap();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = index.status(Empty::default()).unwrap();
        if cause(&state) == Some(Cause::CAUSE_REINDEX) && stage(&state) == Some(Stage::STAGE_DONE) {
            break;
        }
        assert!(Instant::now() < deadline, "{state:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    rebuilt.store(true, Ordering::SeqCst);
    searcher.join().unwrap();
    assert!(
        during.load(Ordering::SeqCst) > 0,
        "the searches met the rebuild"
    );
    let (_, after) = search(&entries, probe);
    assert_eq!(
        after.iter().map(hit_id).collect::<Vec<_>>().len(),
        before.len()
    );
}

// 6. SetEmbedder.

#[test]
fn a_vault_without_an_embedder_does_not_index_until_one_is_set() {
    let scratch = Scratch::new("consumer-set-embedder");
    let path = scratch.join("vault");
    let put = |vault: &Arc<dyn VaultSync>, id: &str| -> String {
        collection(vault, "entries")
            .upsert(
                DocumentUpsert::default()
                    .with_id(id)
                    .with_raw(br#"{"text": "cat kitten feline"}"#.to_vec())
                    .with_note(id),
            )
            .unwrap()
            .version
            .unwrap()
    };

    // A Vault that lacks the embedder its vector column needs does not
    // index it. A search is `INDEX_REBUILDING` (retryable) when no Index
    // was built before the column was declared, or a summary that did not
    // run the vector leg.
    let bare = open_vault(&path, false);
    ensure(&bare, &wanted());
    put(&bare, "cat0");
    match collection(&bare, "entries")
        .search(search_of(json!({"text": "feline", "lexicalWeight": 0})))
    {
        Ok(stream) => {
            let first = stream.map(Result::unwrap).next().unwrap();
            let Some(Item::Summary(summary)) = first.item else {
                panic!("the first item is not the summary");
            };
            assert_ne!(
                summary.vector_leg.as_ref().and_then(|leg| leg.as_known()),
                Some(VectorLeg::VECTOR_LEG_RAN)
            );
        }
        Err(error) => assert!(
            error.is(Code::IndexRebuilding) && error.retryable(),
            "{error}"
        ),
    }
    bare.close(Empty::default()).unwrap();

    // An Index built by a Vault that has an embedder.
    let path = path.canonicalize().unwrap();
    let builder = open_vault(&path, true);
    wait_indexed(&builder);
    builder.close(Empty::default()).unwrap();

    // A Vault without one does not index; its status says the Index is
    // behind and why, and its searches run without the vector leg.
    let vault = open_vault(&path, false);
    let stored = put(&vault, "cat1");
    let index_of = index(&vault);
    let deadline = Instant::now() + DEADLINE;
    let state = loop {
        let state = index_of.status(Empty::default()).unwrap();
        if state.stored_version.as_deref() == Some(stored.as_str()) {
            break state;
        }
        assert!(Instant::now() < deadline, "{state:?}");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_ne!(state.role, Some(true));
    assert_ne!(state.version, state.stored_version);
    assert_eq!(
        state.last_error.as_option().unwrap().code.as_deref(),
        Some("VAULT_ERROR_CODE_FAILED_PRECONDITION")
    );
    let entries = collection(&vault, "entries");
    let (leg, _) = search(&entries, json!({"text": "feline", "lexicalWeight": 0}));
    assert_eq!(leg, VectorLeg::VECTOR_LEG_NO_EMBEDDER);

    vault.set_embedder(embedder()).unwrap();
    let (leg, hits) = search(
        &entries,
        json!({"text": "feline", "k": 2, "lexicalWeight": 0, "notBefore": stored}),
    );
    assert_eq!(leg, VectorLeg::VECTOR_LEG_RAN);
    let mut found: Vec<String> = hits.iter().map(hit_id).collect();
    found.sort();
    assert_eq!(found, ["cat0", "cat1"]);
}

// 7. Status and Watch.

#[test]
fn watch_reports_the_version_of_a_write_and_the_write_is_then_found() {
    let fx = Fixture::new("consumer-watch");
    wait_indexed(&fx.vault);
    let mut watch = index(&fx.vault).watch(Empty::default()).unwrap();
    let before = watch.next().unwrap().unwrap().version;
    assert_eq!(
        before,
        index(&fx.vault).status(Empty::default()).unwrap().version
    );

    let entries = fx.collection("entries");
    let token = entries
        .upsert(
            DocumentUpsert::default()
                .with_id("fresh")
                .with_raw(br#"{"text": "kitten", "kind": "pet"}"#.to_vec())
                .with_note("fresh"),
        )
        .unwrap()
        .version;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = watch.next().unwrap().unwrap();
        if state.version == token {
            break;
        }
        assert!(Instant::now() < deadline, "no version after the write");
    }
    let query: DocumentQuery = serde_json::from_value(json!({
        "filter": {"term": {"column": "kind", "value": {"text": "pet"}}}
    }))
    .unwrap();
    assert_eq!(ids(entries.query(query).unwrap()), ["fresh"]);
}

// 8. Mirror.

#[test]
fn sync_to_an_empty_mirror_pushes_the_commit() {
    let fx = Fixture::new("consumer-mirror");
    let mirror = fx.scratch.join("mirror.git");
    std::fs::create_dir_all(&mirror).unwrap();
    git(
        &mirror,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    fx.collection("notes")
        .upsert(
            DocumentUpsert::default()
                .with_id("a")
                .with_raw(b"A".to_vec())
                .with_note("a"),
        )
        .unwrap();
    let mirror_ref = {
        let reference = fx.vault.mirror(Empty::default()).unwrap();
        let _inside = handle().enter();
        reference.to_sync().unwrap()
    };
    mirror_ref
        .set(MirrorSetting::default().with_url(file_url(&mirror)))
        .unwrap();
    mirror_ref.sync(Empty::default()).unwrap();
    assert_eq!(git_out(&mirror, &["rev-parse", "HEAD"]), head(&fx.path));
}

// 9. Two Vaults on one path.

#[test]
fn two_vaults_on_one_path_both_write_and_each_sees_the_other() {
    let scratch = Scratch::new("consumer-two");
    let path = scratch.join("vault");
    let first = open_vault(&path, true);
    ensure(&first, &wanted());
    let path = path.canonicalize().unwrap();
    let second = open_vault(&path, true);

    let write = |vault: &Arc<dyn VaultSync>, id: &str| -> String {
        collection(vault, "entries")
            .upsert(
                DocumentUpsert::default()
                    .with_id(id)
                    .with_raw(
                        json!({"text": "kitten", "kind": id})
                            .to_string()
                            .into_bytes(),
                    )
                    .with_note(id),
            )
            .unwrap()
            .version
            .unwrap()
    };
    write(&first, "from-first");
    let last = write(&second, "from-second");
    // Each sees both, given the token of the later write.
    for vault in [&first, &second] {
        let query: DocumentQuery = serde_json::from_value(json!({"notBefore": last})).unwrap();
        let mut found = ids(collection(vault, "entries").query(query).unwrap());
        found.sort();
        assert_eq!(found, ["from-first", "from-second"]);
    }
    let roles = [&first, &second]
        .map(|vault| index(vault).status(Empty::default()).unwrap().role == Some(true));
    assert!(!(roles[0] && roles[1]), "one indexer at a time");
    for vault in [&first, &second] {
        assert!(all_ids(&collection(vault, "entries")).is_ok());
    }
}

// 10. From inside spawn_blocking.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_batches_and_the_search_run_inside_spawn_blocking() {
    tokio::task::spawn_blocking(|| {
        let fx = Fixture::new("consumer-blocking");
        the_batches_of_the_consumer(&fx.collection("notes"), &fx.path);
        let token = fx.corpus();
        let (leg, hits) = search(
            &fx.collection("entries"),
            json!({"text": "feline", "k": 2, "lexicalWeight": 0, "notBefore": token}),
        );
        assert_eq!(leg, VectorLeg::VECTOR_LEG_RAN);
        assert_eq!(hits.len(), 2);
        fx.vault.close(Empty::default()).unwrap();
    })
    .await
    .unwrap();
}

// 11. The codes.

#[test]
fn open_existing_on_a_missing_path_or_a_plain_folder_has_a_code() {
    let scratch = Scratch::new("consumer-codes");
    let existing = options().with_mode(OpenMode::OPEN_MODE_OPEN_EXISTING);

    let missing = scratch.join("nothing");
    let error = open_request(request(&missing, true, existing.clone()))
        .err()
        .unwrap();
    assert!(error.is(Code::NotFound), "{error}");
    assert!(!missing.exists(), "a refused open creates nothing");

    let folder = scratch.join("folder");
    work_tree(&folder, &[("file.txt", "not a Vault")]);
    write(&folder, "draft.txt", "a person's file");
    let before = snapshot(&folder);
    let error = open_request(request(&folder, true, existing.clone()))
        .err()
        .unwrap();
    assert!(error.is(Code::NotAVault), "{error}");
    assert_eq!(snapshot(&folder), before, "a refused open writes nothing");

    // A Vault built by hand opens as "existing only", with no port.
    let built = scratch.join("built");
    built_vault(&built);
    let opened = open_request(opening(&built, existing)).unwrap();
    assert_eq!(
        opened
            .list_collections(Empty::default())
            .unwrap()
            .collections
            .len(),
        2
    );
}

#[test]
fn a_write_while_another_holds_the_lock_past_the_timeout_is_aborted_and_retryable() {
    let scratch = Scratch::new("consumer-lock");
    let path = scratch.join("vault");
    let vault = open_request(request(&path, true, options().with_write_timeout_ms(150))).unwrap();
    ensure(&vault, &wanted());
    let before = commits(&path);
    let held = File::options()
        .read(true)
        .write(true)
        .open(path.join(".scheda/commit.lock"))
        .unwrap();
    held.lock().unwrap();
    let error = collection(&vault, "notes")
        .upsert(
            DocumentUpsert::default()
                .with_id("a")
                .with_raw(b"A".to_vec())
                .with_note("blocked"),
        )
        .unwrap_err();
    assert!(error.is(Code::Aborted), "{error}");
    assert!(error.retryable());
    assert_eq!(commits(&path), before);
    held.unlock().unwrap();
    drop(held);
    collection(&vault, "notes")
        .upsert(
            DocumentUpsert::default()
                .with_id("a")
                .with_raw(b"A".to_vec())
                .with_note("later"),
        )
        .unwrap();
    assert_eq!(commits(&path), before + 1);
}
