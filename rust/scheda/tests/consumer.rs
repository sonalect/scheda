//! The use of a consumer that reaches Scheda through one adapter, on the
//! object API: the shapes of its calls, run end to end against a Vault on
//! disk.
//!
//! The consumer opens a Vault with a secret from its environment, an
//! `author`, and a Projector that gives columns to one collection only (a
//! text for every leg and two keyword columns for `find`); it installs its
//! embedder after the open and passes no reranker. Its writes are
//! idempotent by design: a collection that is append-only keeps what is
//! stored, another is "add or replace", a delete of a missing Document is
//! nothing, and a group of writes is one commit. It ensures its
//! collections (creates the missing ones, rewrites a declaration that
//! differs, leaves a matching one alone), keeps one `singleton`
//! collection, reads through `get`, listings, history and a vector-only
//! search, waits on the Index, and `sync`s a mirror. Several processes
//! open one Vault, and calls come from sync code and from inside
//! `tokio::task::spawn_blocking`.
//!
//! Names here (`entries`, `notes`, `settings`, `trail`, `secrets`) are
//! neutral: nothing of a consumer's domain appears in this file.

#![cfg(feature = "search")]

mod common;

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda::v1::collection_result::Result as CollectionArm;
use scheda::v1::document_operation::Operation;
use scheda::v1::document_result::Result as ResultArm;
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::projector_ref::ProjectorRef;
use scheda::v1::search_item::Item;
use scheda::v1::search_summary::VectorLeg;
use scheda::v1::{
    CollectionDeclaration, CollectionKey, CollectionOperation, CollectionSync, ColumnKind,
    Document, DocumentCreation, DocumentDeletion, DocumentHistoryEntry, DocumentId,
    DocumentOperation, DocumentQuery, DocumentSearch, DocumentUpsert, IndexColumn, IndexDoc,
    IndexSchema, IndexSync, IndexValue, MirrorSetting, OpenMode, OpenOptions, ProjectorDescriptor,
    ProjectorSync, RawDocument, SchedaSync, SearchHit, SearchItem, VaultErrorCode as Code,
    VaultOpening, VaultSync, index_value,
};
use scheda::{Error, FixedEmbedder, Scheda};
use serde_json::{Value, json};

use common::{
    Scratch, command, git, open, opening, snapshot, vault as built_vault, work_tree, write,
};

const DEADLINE: Duration = Duration::from_secs(60);

// The Vault the consumer builds.

/// Open options as the consumer sets them: an author, the secret, and a
/// quick indexer so a test does not wait on the default poll.
fn options() -> OpenOptions {
    let mut value = json!({
        "author": {"name": "Test Author", "email": "author@example.test"},
        "indexPollMs": 10,
        "writeTimeoutMs": 60000,
    });
    #[cfg(feature = "encrypt")]
    {
        value["encryption"] = json!({"passphrase": "a passphrase from the environment"});
    }
    serde_json::from_value(value).unwrap()
}

/// A Projector that gives columns to `entries` only: `text` (TEXT; find,
/// fts, vector) and the keyword columns `topic` and `kind` (TEXT; find).
/// Every other collection is indexed by identity.
struct Columns;

fn column(name: &str, find: bool, fts: bool, vector: bool) -> IndexColumn {
    IndexColumn::default()
        .with_name(name)
        .with_kind(ColumnKind::COLUMN_KIND_TEXT)
        .with_find(find)
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
                column("text", true, true, true),
                column("topic", true, false, false),
                column("kind", true, false, false),
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

/// A deterministic embedder of the three topics.
fn embedder() -> EmbedderRef {
    EmbedderRef::from_sync(FixedEmbedder::new(
        "topics@1",
        4,
        NonZeroUsize::new(8).unwrap(),
        |texts: Vec<String>| Ok(texts.iter().map(|text| topic_vector(text)).collect()),
    ))
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

fn open_vault(path: &Path, with_embedder: bool) -> Arc<dyn VaultSync> {
    Scheda
        .open(request(path, with_embedder, options()))
        .unwrap()
        .to_sync()
        .unwrap()
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
        json!({"name": "trail", "schema": {"standard": {}, "excludeVectorIndex": true}}),
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
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .unwrap()
        .to_sync()
        .unwrap()
}

fn index(vault: &Arc<dyn VaultSync>) -> Arc<dyn IndexSync> {
    vault.index(Empty::default()).unwrap().to_sync().unwrap()
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

/// Waits until the Index of `vault` reflects `HEAD` of the Vault at
/// `path`. The wait is on the Index's own state with a deadline.
fn wait_indexed(vault: &Arc<dyn VaultSync>, path: &Path) {
    let deadline = Instant::now() + DEADLINE;
    let status = index(vault);
    loop {
        let state = status.status(Empty::default()).unwrap();
        if state.version.as_deref() == Some(head(path).as_str()) && state.pending == Some(0) {
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

fn ids(stream: BoxIter<'static, Result<Document, Error>>) -> Vec<String> {
    stream
        .map(|document| document.unwrap().id.unwrap_or_default())
        .collect()
}

/// Every id of `target` the Index holds, or the failure of the query: a
/// Vault whose Index is not built yet answers `INDEX_REBUILDING`.
fn all_ids(target: &Arc<dyn CollectionSync>) -> Result<Vec<String>, Error> {
    let query: DocumentQuery = serde_json::from_value(json!({"filter": {"all": {}}})).unwrap();
    target.query(query).map(ids)
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

/// A Vault with the consumer's collections, the embedder installed at the
/// open.
struct Fixture {
    _scratch: Scratch,
    path: PathBuf,
    vault: Arc<dyn VaultSync>,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let scratch = Scratch::new(name);
        let path = scratch.join("vault");
        let vault = open_vault(&path, true);
        ensure(&vault, &wanted());
        let path = path.canonicalize().unwrap();
        Fixture {
            _scratch: scratch,
            path,
            vault,
        }
    }

    fn collection(&self, name: &str) -> Arc<dyn CollectionSync> {
        collection(&self.vault, name)
    }

    fn wait(&self) {
        wait_indexed(&self.vault, &self.path);
    }

    /// The corpus of `entries`, indexed.
    fn corpus(&self) {
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
        send(&entries, operations);
        self.wait();
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
    let got = entries.get(DocumentId::default().with_id("one")).unwrap();
    assert_eq!(got.raw.as_deref(), Some(&br#"{"text": "cat"}"#[..]));
    assert_eq!(got.revision, written.revision);
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
    let count = commits(path);
    let revisions = shape(&first);

    let second = send(target, make());
    assert_eq!(shape(&second), revisions, "the same results");
    assert_eq!(head(path), after_first, "HEAD does not move");
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
                "schema": {"standard": {}, "excludeVectorIndex": true},
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
    assert_eq!(second.revision, first.revision);
    assert_eq!(head(&path), stored);
    assert_eq!(history(&settings, "config.yaml").len(), 1);
}

// 4. A vector-only search.

#[test]
fn a_vector_only_search_runs_the_vector_leg_and_ranks_best_first() {
    let fx = Fixture::new("consumer-vector");
    fx.corpus();
    let entries = fx.collection("entries");
    let (leg, hits) = search(
        &entries,
        json!({"text": "feline", "k": 3, "lexical_weight": 0}),
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
}

// 5. SetEmbedder.

#[test]
fn a_vault_without_an_embedder_does_not_index_until_one_is_set() {
    let scratch = Scratch::new("consumer-set-embedder");
    let path = scratch.join("vault");
    let put = |vault: &Arc<dyn VaultSync>, id: &str| {
        collection(vault, "entries")
            .upsert(
                DocumentUpsert::default()
                    .with_id(id)
                    .with_raw(br#"{"text": "cat kitten feline"}"#.to_vec())
                    .with_note(id),
            )
            .unwrap();
    };

    // Before the Index has its first commit, a search is `INDEX_REBUILDING`
    // (retryable) or a summary that did not run the vector leg.
    let bare = open_vault(&path, false);
    ensure(&bare, &wanted());
    put(&bare, "cat0");
    let entries = collection(&bare, "entries");
    match entries.search(search_of(json!({"text": "feline", "lexical_weight": 0}))) {
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
    drop(entries);
    drop(bare);

    // An Index built by a Vault that has an embedder.
    let path = path.canonicalize().unwrap();
    let builder = open_vault(&path, true);
    wait_indexed(&builder, &path);
    drop(builder);

    // A Vault without one does not take over the indexing.
    let vault = open_vault(&path, false);
    put(&vault, "cat1");
    let quiet = Instant::now() + Duration::from_millis(300);
    while Instant::now() < quiet {
        let state = index(&vault).status(Empty::default()).unwrap();
        assert_eq!(state.role, Some(false));
        assert_ne!(state.version.as_deref(), Some(head(&path).as_str()));
        std::thread::sleep(Duration::from_millis(20));
    }
    let entries = collection(&vault, "entries");
    let (leg, _) = search(&entries, json!({"text": "feline", "lexical_weight": 0}));
    assert_eq!(leg, VectorLeg::VECTOR_LEG_NO_EMBEDDER);

    vault.set_embedder(embedder()).unwrap();
    wait_indexed(&vault, &path);
    // `role` is true only while a round runs: at rest it is false again.
    let (leg, hits) = search(
        &entries,
        json!({"text": "feline", "k": 2, "lexical_weight": 0}),
    );
    assert_eq!(leg, VectorLeg::VECTOR_LEG_RAN);
    let mut found: Vec<String> = hits.iter().map(hit_id).collect();
    found.sort();
    assert_eq!(found, ["cat0", "cat1"]);
}

// 6. Status and Watch.

#[test]
fn watch_reports_the_version_of_a_write_and_the_write_is_then_found() {
    let fx = Fixture::new("consumer-watch");
    fx.wait();
    let mut watch = index(&fx.vault).watch(Empty::default()).unwrap();
    let before = watch.next().unwrap().unwrap().version;
    assert_eq!(
        before,
        index(&fx.vault).status(Empty::default()).unwrap().version
    );

    let entries = fx.collection("entries");
    entries
        .upsert(
            DocumentUpsert::default()
                .with_id("fresh")
                .with_raw(br#"{"text": "kitten", "kind": "pet"}"#.to_vec())
                .with_note("fresh"),
        )
        .unwrap();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = watch.next().unwrap().unwrap();
        if state.version.is_some() && state.version != before && state.pending == Some(0) {
            break;
        }
        assert!(Instant::now() < deadline, "no version after the write");
    }
    let query: DocumentQuery = serde_json::from_value(json!({
        "filter": {"term": {"field": "kind", "value": "pet"}}
    }))
    .unwrap();
    assert_eq!(ids(entries.query(query).unwrap()), ["fresh"]);
}

// 7. Mirror.

#[test]
fn sync_to_an_empty_mirror_pushes_the_commit() {
    let fx = Fixture::new("consumer-mirror");
    let mirror = fx._scratch.join("mirror.git");
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
    let mirror_ref = fx
        .vault
        .mirror(Empty::default())
        .unwrap()
        .to_sync()
        .unwrap();
    mirror_ref
        .set(MirrorSetting::default().with_url(format!("file://{}", mirror.display())))
        .unwrap();
    mirror_ref.sync(Empty::default()).unwrap();
    assert_eq!(git_out(&mirror, &["rev-parse", "HEAD"]), head(&fx.path));
}

// 8. Two Vaults on one path.

#[test]
fn two_vaults_on_one_path_both_write_and_each_sees_the_other() {
    let scratch = Scratch::new("consumer-two");
    let path = scratch.join("vault");
    let first = open_vault(&path, true);
    ensure(&first, &wanted());
    let path = path.canonicalize().unwrap();
    let second = open_vault(&path, true);

    let write = |vault: &Arc<dyn VaultSync>, id: &str| {
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
            .unwrap();
    };
    write(&first, "from-first");
    write(&second, "from-second");

    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut seen = Vec::new();
        for vault in [&first, &second] {
            let mut found = match all_ids(&collection(vault, "entries")) {
                Ok(found) => found,
                Err(error) => {
                    assert!(
                        error.is(Code::IndexRebuilding) && error.retryable(),
                        "{error}"
                    );
                    Vec::new()
                }
            };
            found.sort();
            seen.push(found);
        }
        let roles = [&first, &second]
            .map(|vault| index(vault).status(Empty::default()).unwrap().role == Some(true));
        assert!(!(roles[0] && roles[1]), "one indexer at a time");
        if seen
            .iter()
            .all(|found| found == &["from-first", "from-second"])
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the Index did not catch up: {seen:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for vault in [&first, &second] {
        wait_indexed(vault, &path);
    }
}

// 9. From inside spawn_blocking.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_batches_and_the_search_run_inside_spawn_blocking() {
    tokio::task::spawn_blocking(|| {
        let fx = Fixture::new("consumer-blocking");
        the_batches_of_the_consumer(&fx.collection("notes"), &fx.path);
        fx.corpus();
        let (leg, hits) = search(
            &fx.collection("entries"),
            json!({"text": "feline", "k": 2, "lexical_weight": 0}),
        );
        assert_eq!(leg, VectorLeg::VECTOR_LEG_RAN);
        assert_eq!(hits.len(), 2);
    })
    .await
    .unwrap();
}

// 10. The codes.

#[test]
fn open_existing_on_a_missing_path_or_a_plain_folder_has_a_code() {
    let scratch = Scratch::new("consumer-codes");
    let existing = options().with_mode(OpenMode::OPEN_MODE_OPEN_EXISTING);

    let missing = scratch.join("nothing");
    let error = Scheda
        .open(request(&missing, true, existing.clone()))
        .err()
        .unwrap();
    assert!(error.is(Code::NotFound), "{error}");
    assert!(!missing.exists(), "a refused open creates nothing");

    let folder = scratch.join("folder");
    work_tree(&folder, &[("file.txt", "not a Vault")]);
    write(&folder, "draft.txt", "a person's file");
    let before = snapshot(&folder);
    let error = Scheda
        .open(request(&folder, true, existing.clone()))
        .err()
        .unwrap();
    assert!(error.is(Code::NotAVault), "{error}");
    assert_eq!(snapshot(&folder), before, "a refused open writes nothing");

    // A Vault built by hand opens as "existing only", with no port.
    let built = scratch.join("built");
    built_vault(&built);
    let opened = open(&built, existing).unwrap();
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
    let vault = Scheda
        .open(request(&path, true, options().with_write_timeout_ms(150)))
        .unwrap()
        .to_sync()
        .unwrap();
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
