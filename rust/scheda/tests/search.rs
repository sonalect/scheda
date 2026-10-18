//! The searches of the contract: `Collection.Search` and
//! `Vault.SearchDocuments`. A search ranks: the Documents that best fit a
//! text, or a Document, by the lexical and the vector leg fused, with the
//! summary of the legs first and the hits after it as a lazy stream.
//!
//! What counts blob reads (a stream opens a file as it reaches a hit,
//! `offset` opens none) and what stops a write half way is only reachable
//! from inside the library: those are unit tests in `src/search.rs`. A
//! build without `search` answers both calls with `UNIMPLEMENTED`.

mod common;

#[cfg(not(feature = "search"))]
mod thin {
    use buffa_types::google::protobuf::Empty;
    use scheda::v1::{
        CollectionKey, DocumentSearch, Feature, SchedaSync, VaultErrorCode, VaultSearch,
    };
    use scheda::{Error, Scheda};

    use crate::common::{Scratch, open, snapshot, vault as built_vault};

    fn search() -> DocumentSearch {
        serde_json::from_value(serde_json::json!({"text": "words"})).unwrap()
    }

    /// `UNIMPLEMENTED`, and the message names the feature.
    fn names_search(error: &Error) -> bool {
        error.is(VaultErrorCode::Unimplemented) && error.to_string().contains("`search`")
    }

    #[test]
    fn without_search_the_two_calls_are_unimplemented_naming_the_feature() {
        let scratch = Scratch::new("search-thin");
        let path = scratch.join("vault");
        built_vault(&path);
        let vault = open(&path, Default::default()).unwrap();
        let notes = vault
            .get_collection(CollectionKey::default().with_name("notes"))
            .unwrap()
            .to_sync()
            .unwrap();
        let before = snapshot(&path);

        assert!(names_search(&notes.search(search()).err().unwrap()));
        let request = VaultSearch {
            search: search().into(),
            ..VaultSearch::default()
        };
        assert!(names_search(
            &vault.search_documents(request).err().unwrap()
        ));
        assert_eq!(snapshot(&path), before, "a refused search writes nothing");
    }

    #[test]
    fn without_search_scheda_info_lists_no_search() {
        let info = Scheda.info(Empty::default()).unwrap();
        let features: Vec<Feature> = info
            .features
            .iter()
            .filter_map(|feature| feature.as_known())
            .collect();
        assert!(!features.contains(&Feature::FEATURE_SEARCH));
    }
}

#[cfg(feature = "search")]
mod search {
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, ThreadId};
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use protocontract::BoxIter;
    use scheda::v1::embedder_ref::EmbedderRef;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::reranker_ref::RerankerRef;
    use scheda::v1::search_item::Item;
    use scheda::v1::search_summary::VectorLeg;
    use scheda::v1::{
        CollectionDeclaration, CollectionKey, CollectionSync, ColumnKind, Document,
        DocumentCreation, DocumentDeletion, DocumentSearch, DocumentUpsert, EmbedderDescriptor,
        EmbedderSync, IndexColumn, IndexDoc, IndexSchema, IndexSync, IndexValue, OpenOptions,
        ProjectorDescriptor, ProjectorSync, Query, RawDocument, SchedaSync, SearchHit, SearchItem,
        Text, VaultErrorCode as Code, VaultOpening, VaultSearch, VaultSync, Vector, index_value,
    };
    use scheda::{Error, FixedReranker, Scheda};
    use serde_json::{Value, json};

    use crate::common::{Scratch, command, opening, snapshot};

    const DEADLINE: Duration = Duration::from_secs(60);

    fn column(name: &str, kind: ColumnKind, find: bool, fts: bool, vector: bool) -> IndexColumn {
        IndexColumn::default()
            .with_name(name)
            .with_kind(kind)
            .with_find(find)
            .with_fts(fts)
            .with_vector(vector)
    }

    /// A Projector of JSON Documents with four text columns: `title`
    /// (filtered and searched), `body` (searched), `topic` (embedded, when
    /// the Projector marks vectors), and `kind` (filtered).
    struct Docs {
        projection: &'static str,
        vectors: bool,
    }

    impl ProjectorSync for Docs {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id(self.projection))
        }

        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            let text = ColumnKind::COLUMN_KIND_TEXT;
            Ok(IndexSchema {
                columns: vec![
                    column("title", text, true, true, false),
                    column("body", text, false, true, false),
                    column("topic", text, false, false, self.vectors),
                    column("kind", text, true, false, false),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let json: Value =
                serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
            let mut doc = IndexDoc::default();
            for name in ["title", "body", "topic", "kind"] {
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

    /// What the embedder was asked, and on which thread.
    #[derive(Default)]
    struct Calls {
        queries: Mutex<Vec<(String, ThreadId)>>,
        embedded: AtomicUsize,
    }

    /// An embedder of the three topics of a text; a blocking one.
    struct Topics {
        model: &'static str,
        calls: Arc<Calls>,
    }

    impl EmbedderSync for Topics {
        fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(EmbedderDescriptor::default()
                .with_model_id(self.model)
                .with_dimensions(4))
        }

        fn embed(
            &self,
            texts: BoxIter<'static, Result<Text, Error>>,
        ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
            let calls = Arc::clone(&self.calls);
            Ok(Box::new(texts.map(move |text| {
                calls.embedded.fetch_add(1, Ordering::SeqCst);
                Ok(Vector {
                    values: topic_vector(&text?.text.unwrap_or_default()),
                    ..Vector::default()
                })
            })))
        }

        fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            let text = query.text.unwrap_or_default();
            if let Ok(mut queries) = self.calls.queries.lock() {
                queries.push((text.clone(), thread::current().id()));
            }
            Ok(Vector {
                values: topic_vector(&text),
                ..Vector::default()
            })
        }
    }

    /// What a reranker was asked: the query, and for each Document its id
    /// and whether it had its `raw`.
    type Asked = Arc<Mutex<Vec<(String, Vec<(String, bool)>, ThreadId)>>>;

    /// A reranker that prefers the ids it is given, in that order, and
    /// records what it was asked.
    fn reranker(prefer: &[&str], asked: &Asked) -> RerankerRef {
        let prefer: Vec<String> = prefer.iter().map(|id| (*id).to_owned()).collect();
        let asked = Arc::clone(asked);
        RerankerRef::from_sync(FixedReranker::new(
            NonZeroUsize::MIN,
            move |query: &str, documents: Vec<Document>| {
                let docs: Vec<(String, bool)> = documents
                    .iter()
                    .map(|document| {
                        (
                            document.id.clone().unwrap_or_default(),
                            document.raw.is_some(),
                        )
                    })
                    .collect();
                let scores = docs
                    .iter()
                    .map(|(id, _)| {
                        prefer
                            .iter()
                            .position(|wanted| wanted == id)
                            .map_or(0.0, |at| 100.0 - at as f64)
                    })
                    .collect();
                if let Ok(mut asked) = asked.lock() {
                    asked.push((query.to_owned(), docs, thread::current().id()));
                }
                Ok(scores)
            },
        ))
    }

    fn options() -> OpenOptions {
        OpenOptions::default()
            .with_index_poll_ms(10)
            .with_write_timeout_ms(60_000)
    }

    /// The ports of a test Vault.
    struct Ports {
        projection: &'static str,
        /// The Projector marks `topic` a vector column.
        vectors: bool,
        /// The model of the embedder; none: no embedder.
        model: Option<&'static str>,
    }

    impl Ports {
        fn hybrid() -> Ports {
            Ports {
                projection: "docs@1",
                vectors: true,
                model: Some("topics@1"),
            }
        }
    }

    fn open_with(path: &Path, ports: &Ports, calls: &Arc<Calls>) -> Arc<dyn VaultSync> {
        let mut request: VaultOpening = opening(path, options());
        request.projector = ProjectorRef::from_sync(Docs {
            projection: ports.projection,
            vectors: ports.vectors,
        })
        .into();
        if let Some(model) = ports.model {
            request.embedder = EmbedderRef::from_sync(Topics {
                model,
                calls: Arc::clone(calls),
            })
            .into();
        }
        Scheda.open(request).unwrap().to_sync().unwrap()
    }

    fn declaration(value: Value) -> CollectionDeclaration {
        serde_json::from_value(value).unwrap()
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

    /// The `IndexState.version` the Index has when it reflects `HEAD` of
    /// the Vault at `path`.
    fn head(path: &Path) -> String {
        let rev = |dir: &Path| {
            let out = command(dir, &["rev-parse", "HEAD"]).output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        let main = rev(path);
        if path.join(".isolated/.git").exists() {
            format!("{main}+{}", rev(&path.join(".isolated")))
        } else {
            main
        }
    }

    /// Waits until the Index of `vault` reflects `HEAD` of the Vault at
    /// `path`.
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
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// A Vault with the collections `notes` and `log` (a journal) declared,
    /// the Index built.
    struct Fixture {
        _scratch: Scratch,
        path: PathBuf,
        vault: Arc<dyn VaultSync>,
        calls: Arc<Calls>,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            Fixture::with(name, &Ports::hybrid())
        }

        fn with(name: &str, ports: &Ports) -> Fixture {
            let scratch = Scratch::new(name);
            let path = scratch.join("vault");
            let calls = Arc::new(Calls::default());
            let vault = open_with(&path, ports, &calls);
            for value in [
                json!({"name": "notes", "schema": {"standard": {}}}),
                json!({"name": "log", "schema": {"journal": {}}}),
            ] {
                vault.create_collection(declaration(value)).unwrap();
            }
            let path = path.canonicalize().unwrap();
            let fixture = Fixture {
                _scratch: scratch,
                path,
                vault,
                calls,
            };
            fixture.wait();
            fixture
        }

        fn wait(&self) {
            wait_indexed(&self.vault, &self.path);
        }

        fn collection(&self, name: &str) -> Arc<dyn CollectionSync> {
            collection(&self.vault, name)
        }

        /// Writes the Document `id` of `collection` (appends a line to the
        /// journal `log`) without waiting for the Index.
        fn put_unwaited(&self, collection: &str, id: &str, raw: &Value) {
            let bytes = serde_json::to_vec(raw).unwrap();
            let target = self.collection(collection);
            if collection == "log" {
                target
                    .create(
                        DocumentCreation::default()
                            .with_id(id)
                            .with_raw(bytes)
                            .with_note("test"),
                    )
                    .unwrap();
            } else {
                target
                    .upsert(
                        DocumentUpsert::default()
                            .with_id(id)
                            .with_raw(bytes)
                            .with_note("test"),
                    )
                    .unwrap();
            }
        }

        fn put(&self, collection: &str, id: &str, raw: &Value) {
            self.put_unwaited(collection, id, raw);
            self.wait();
        }

        /// The standard corpus of `notes`.
        fn corpus(&self) -> &Fixture {
            for (id, title, body, topic, kind) in [
                (
                    "cat1",
                    "Cat care",
                    "how to feed a kitten",
                    "cat kitten feline",
                    "pet",
                ),
                (
                    "cat2",
                    "Kitten food",
                    "feed your kitten twice a day",
                    "kitten feline",
                    "pet",
                ),
                (
                    "dog1",
                    "Dog training",
                    "puppy and canine basics",
                    "dog puppy canine",
                    "pet",
                ),
                (
                    "car1",
                    "Car repair",
                    "engine and truck parts",
                    "car engine truck",
                    "vehicle",
                ),
            ] {
                self.put_unwaited(
                    "notes",
                    id,
                    &json!({"title": title, "body": body, "topic": topic, "kind": kind}),
                );
            }
            self.wait();
            self
        }
    }

    fn search_of(value: Value) -> DocumentSearch {
        serde_json::from_value(value).unwrap()
    }

    fn vault_search(names: Value, search: Value) -> VaultSearch {
        let mut request: VaultSearch = serde_json::from_value(json!({
            "collections": {"names": names},
        }))
        .unwrap();
        request.search = search_of(search).into();
        request
    }

    /// The whole stream of a search.
    fn items(stream: BoxIter<'static, Result<SearchItem, Error>>) -> Vec<SearchItem> {
        stream.map(|item| item.unwrap()).collect()
    }

    fn run(target: &Arc<dyn CollectionSync>, search: Value) -> Vec<SearchItem> {
        items(target.search(search_of(search)).unwrap())
    }

    /// The summary, which must be the first item.
    fn summary(items: &[SearchItem]) -> VectorLeg {
        match items.first().and_then(|item| item.item.as_ref()) {
            Some(Item::Summary(summary)) => summary
                .vector_leg
                .as_ref()
                .and_then(|leg| leg.as_known())
                .unwrap(),
            other => panic!("the first item is not the summary: {other:?}"),
        }
    }

    /// The hits, after the summary, which must be the only one.
    fn hits(items: &[SearchItem]) -> Vec<SearchHit> {
        let _ = summary(items);
        items[1..]
            .iter()
            .map(|item| match item.item.as_ref() {
                Some(Item::Hit(hit)) => (**hit).clone(),
                other => panic!("not a hit: {other:?}"),
            })
            .collect()
    }

    fn id_of(hit: &SearchHit) -> String {
        hit.document
            .as_option()
            .and_then(|document| document.id.clone())
            .unwrap_or_default()
    }

    fn ids(items: &[SearchItem]) -> Vec<String> {
        hits(items).iter().map(id_of).collect()
    }

    fn keys(items: &[SearchItem]) -> Vec<(String, String)> {
        hits(items)
            .iter()
            .map(|hit| {
                let document = hit.document.as_option().unwrap();
                (
                    document.collection.clone().unwrap_or_default(),
                    document.id.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn a_text_search_fuses_the_lexical_and_the_vector_leg() {
        let fx = Fixture::new("search-hybrid");
        fx.corpus();
        let notes = fx.collection("notes");
        let found = run(&notes, json!({"text": "kitten", "k": 4}));
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_RAN);
        let all = hits(&found);
        assert_eq!(all.len(), 4, "the vector leg ranks every Document");
        // `cat2` matches the word in its title and its body, and `cat1` in
        // its body; both are cats.
        assert_eq!(id_of(&all[0]), "cat2");
        let cat = all.iter().find(|hit| id_of(hit) == "cat1").unwrap();
        assert!(cat.lexical_score.is_some_and(|score| score > 0.0));
        assert!(cat.vector_score.is_some());
        assert!(cat.score.is_some());
        assert!(cat.rerank_score.is_none());
        // A Document only the vector leg finds has no lexical score.
        let car = all.iter().find(|hit| id_of(hit) == "car1").unwrap();
        assert!(car.lexical_score.is_none());
        assert!(car.vector_score.is_some());
        // The fused order is the order of the stream.
        let scores: Vec<f64> = all.iter().filter_map(|hit| hit.score).collect();
        assert!(
            scores.windows(2).all(|pair| pair[0] >= pair[1]),
            "{scores:?}"
        );
    }

    #[test]
    fn a_synonym_with_no_shared_word_is_found_by_the_vector_leg() {
        let fx = Fixture::new("search-synonym");
        fx.corpus();
        let notes = fx.collection("notes");
        // "feline" is in the topic of the cats only: no word of any title or
        // body.
        let found = run(
            &notes,
            json!({"text": "feline", "k": 2, "lexical_weight": 0}),
        );
        assert_eq!(ids(&found).len(), 2);
        assert!(ids(&found).iter().all(|id| id.starts_with("cat")));
        for hit in hits(&found) {
            assert!(hit.vector_score.is_some());
            assert!(hit.lexical_score.is_none());
        }
    }

    #[test]
    fn a_word_in_an_unmarked_column_is_not_found() {
        let fx = Fixture::new("search-unmarked");
        fx.put("notes", "n1", &json!({"title": "alpha", "kind": "zebra"}));
        let notes = fx.collection("notes");
        // `kind` is filtered on, not searched.
        let found = run(&notes, json!({"text": "zebra", "vector_weight": 0}));
        assert!(ids(&found).is_empty());
        let found = run(&notes, json!({"text": "alpha", "vector_weight": 0}));
        assert_eq!(ids(&found), ["n1"]);
    }

    #[test]
    fn like_finds_the_neighbours_of_a_document_without_calling_the_embedder() {
        let fx = Fixture::new("search-like");
        fx.corpus();
        let notes = fx.collection("notes");
        let asked = |fx: &Fixture| fx.calls.queries.lock().unwrap().len();
        assert_eq!(asked(&fx), 0);
        let found = run(
            &notes,
            json!({"like": {"collection": "notes", "id": "cat1"}, "k": 3}),
        );
        assert_eq!(asked(&fx), 0, "like calls no model");
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_RAN);
        let all = hits(&found);
        // The Document itself is not its own neighbour; its neighbour comes
        // first, by both legs.
        assert!(!all.iter().any(|hit| id_of(hit) == "cat1"));
        assert_eq!(id_of(&all[0]), "cat2");
        assert!(all[0].lexical_score.is_some());
        assert!(all[0].vector_score.is_some());
        // The same through the Vault, naming the collection.
        let found = items(
            fx.vault
                .search_documents(vault_search(
                    json!(["notes"]),
                    json!({"like": {"collection": "notes", "id": "cat1"}, "k": 1}),
                ))
                .unwrap(),
        );
        assert_eq!(ids(&found), ["cat2"]);
        assert_eq!(asked(&fx), 0);
    }

    #[test]
    fn like_of_a_document_the_index_does_not_hold_is_not_found() {
        let fx = Fixture::new("search-like-missing");
        fx.corpus();
        let notes = fx.collection("notes");
        let before = snapshot(&fx.path);
        let error = notes
            .search(search_of(
                json!({"like": {"collection": "notes", "id": "ghost"}}),
            ))
            .err()
            .unwrap();
        assert!(error.is(Code::NotFound), "{error}");
        assert_eq!(
            snapshot(&fx.path),
            before,
            "a refused search writes nothing"
        );
        let error = notes
            .search(search_of(
                json!({"like": {"collection": "nowhere", "id": "x"}}),
            ))
            .err()
            .unwrap();
        assert!(error.is(Code::NotFound), "{error}");
        // The key of the Document names its collection, in both forms.
        let error = fx
            .vault
            .search_documents(vault_search(json!([]), json!({"like": {"id": "cat1"}})))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        // `like` may name a Document of another collection than the scope.
        fx.put(
            "log",
            "l1",
            &json!({"title": "kitten log", "topic": "kitten"}),
        );
        let found = run(
            &notes,
            json!({"like": {"collection": "log", "id": "l1"}, "k": 1}),
        );
        assert_eq!(ids(&found).len(), 1);
    }

    #[test]
    fn an_index_never_built_is_index_rebuilding_for_a_search() {
        let scratch = Scratch::new("search-rebuilding");
        let path = scratch.join("vault");
        crate::common::vault(&path);
        // No Projector: this handle never indexes, so the Index has no commit.
        let vault = crate::common::open(&path, options()).unwrap();
        let notes = collection(&vault, "notes");
        notes
            .upsert(
                DocumentUpsert::default()
                    .with_id("a")
                    .with_raw(b"{}".to_vec())
                    .with_note("t"),
            )
            .unwrap();
        let rebuilding = |error: &Error| error.is(Code::IndexRebuilding) && error.retryable();
        assert!(rebuilding(
            &notes
                .search(search_of(json!({"text": "words"})))
                .err()
                .unwrap()
        ));
        assert!(rebuilding(
            &vault
                .search_documents(vault_search(json!([]), json!({"text": "words"})))
                .err()
                .unwrap()
        ));
    }

    #[test]
    fn a_vault_of_another_projection_is_index_stale_for_a_search() {
        let scratch = Scratch::new("search-projection");
        let path = scratch.join("vault");
        let calls = Arc::new(Calls::default());
        let old = open_with(
            &path,
            &Ports {
                projection: "docs@1",
                vectors: false,
                model: None,
            },
            &calls,
        );
        old.create_collection(declaration(
            json!({"name": "notes", "schema": {"standard": {}}}),
        ))
        .unwrap();
        let path = path.canonicalize().unwrap();
        collection(&old, "notes")
            .upsert(
                DocumentUpsert::default()
                    .with_id("a")
                    .with_raw(br#"{"title": "alpha"}"#.to_vec())
                    .with_note("t"),
            )
            .unwrap();
        wait_indexed(&old, &path);
        old.close(Empty::default()).unwrap();

        let new = open_with(
            &path,
            &Ports {
                projection: "docs@2",
                vectors: false,
                model: None,
            },
            &calls,
        );
        let stale = |error: &Error| error.is(Code::IndexStale) && !error.retryable();
        let error = collection(&new, "notes")
            .search(search_of(json!({"text": "alpha"})))
            .err()
            .unwrap();
        assert!(stale(&error), "{error}");
        let error = new
            .search_documents(vault_search(json!([]), json!({"text": "alpha"})))
            .err()
            .unwrap();
        assert!(stale(&error), "{error}");
    }

    #[test]
    fn k_has_no_upper_limit_and_the_first_item_is_the_summary() {
        let fx = Fixture::new("search-k");
        fx.corpus();
        let notes = fx.collection("notes");
        let mut stream = notes
            .search(search_of(json!({"text": "kitten", "k": 4_000_000})))
            .unwrap();
        let first = stream.next().unwrap().unwrap();
        assert!(matches!(first.item, Some(Item::Summary(_))));
        let rest: Vec<SearchItem> = stream.map(|item| item.unwrap()).collect();
        assert_eq!(rest.len(), 4, "every Document there is");
        // `k` unset or 0 is 10.
        fx.put("notes", "extra", &json!({"title": "kitten"}));
        let more: Vec<_> = (0..12).map(|n| format!("e{n:02}")).collect();
        for id in &more {
            fx.put_unwaited("notes", id, &json!({"title": "kitten again"}));
        }
        fx.wait();
        for k in [json!({}), json!({"k": 0})] {
            let mut search = json!({"text": "kitten"});
            search
                .as_object_mut()
                .unwrap()
                .extend(k.as_object().unwrap().clone());
            assert_eq!(ids(&run(&notes, search)).len(), 10);
        }
    }

    #[test]
    fn offset_skips_hits_and_pages_agree_with_one_large_search() {
        let fx = Fixture::new("search-offset");
        fx.corpus();
        let notes = fx.collection("notes");
        let whole = ids(&run(&notes, json!({"text": "kitten", "k": 4})));
        assert_eq!(whole.len(), 4);
        let first = ids(&run(&notes, json!({"text": "kitten", "k": 2})));
        let second = ids(&run(&notes, json!({"text": "kitten", "k": 2, "offset": 2})));
        assert_eq!(first, whole[..2]);
        assert_eq!(second, whole[2..]);
        assert!(ids(&run(&notes, json!({"text": "kitten", "offset": 4}))).is_empty());
        // The summary comes first whatever the page.
        let beyond = run(&notes, json!({"text": "kitten", "offset": 100}));
        assert_eq!(beyond.len(), 1);
        assert_eq!(summary(&beyond), VectorLeg::VECTOR_LEG_RAN);
    }

    #[test]
    fn rerank_limit_needs_a_reranker_also_after_it_was_removed() {
        let fx = Fixture::new("search-rerank-none");
        fx.corpus();
        let notes = fx.collection("notes");
        let rerank = json!({"text": "kitten", "k": 2, "rerank_limit": 4});
        let error = notes.search(search_of(rerank.clone())).err().unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let asked = Asked::default();
        fx.vault.set_reranker(reranker(&["car1"], &asked)).unwrap();
        assert!(notes.search(search_of(rerank.clone())).is_ok());
        fx.vault.remove_reranker(Empty::default()).unwrap();
        let error = notes.search(search_of(rerank)).err().unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        // The same for the Vault form.
        let error = fx
            .vault
            .search_documents(vault_search(
                json!([]),
                json!({"text": "kitten", "rerank_limit": 10}),
            ))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
    }

    #[test]
    fn rerank_limit_below_k_is_invalid() {
        let fx = Fixture::new("search-rerank-below");
        fx.corpus();
        let asked = Asked::default();
        fx.vault.set_reranker(reranker(&[], &asked)).unwrap();
        let notes = fx.collection("notes");
        for search in [
            json!({"text": "kitten", "k": 3, "rerank_limit": 2}),
            // k is 10 when unset.
            json!({"text": "kitten", "rerank_limit": 9}),
        ] {
            let error = notes.search(search_of(search)).err().unwrap();
            assert!(error.is(Code::InvalidArgument), "{error}");
        }
        // Nothing was asked of the reranker.
        assert!(asked.lock().unwrap().is_empty());
        // Zero is no limit at all: a rule of the message.
        let error = notes
            .search(search_of(json!({"text": "kitten", "rerank_limit": 0})))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
    }

    #[test]
    fn a_reranker_orders_the_first_hits_and_sees_the_query_first() {
        let fx = Fixture::new("search-rerank");
        fx.corpus();
        let notes = fx.collection("notes");
        let plain = ids(&run(&notes, json!({"text": "kitten", "k": 4})));
        let worst = plain[3].clone();
        let asked = Asked::default();
        fx.vault
            .set_reranker(reranker(&[worst.as_str()], &asked))
            .unwrap();

        // The reranker sees the first `rerank_limit` fused hits, with their
        // `raw`, and orders them: the last of the four comes first.
        let found = run(&notes, json!({"text": "kitten", "k": 2, "rerank_limit": 4}));
        let all = hits(&found);
        assert_eq!(all.len(), 2);
        assert_eq!(id_of(&all[0]), worst);
        assert_eq!(all[0].rerank_score, Some(100.0));
        assert_eq!(all[1].rerank_score, Some(0.0));
        assert!(all[0].score.is_some(), "the fused score stays");
        let asked_now = asked.lock().unwrap().clone();
        assert!(!asked_now.is_empty());
        for (query, docs, _) in &asked_now {
            assert_eq!(query, "kitten", "the query comes first");
            assert!(docs.iter().all(|(_, has_raw)| *has_raw));
        }
        let mut seen: Vec<String> = asked_now
            .iter()
            .flat_map(|(_, docs, _)| docs.iter().map(|(id, _)| id.clone()))
            .collect();
        seen.sort();
        let mut want = plain.clone();
        want.sort();
        assert_eq!(seen, want);

        // Fewer to rerank than there are: the rest are not scored, and the
        // reranked ones come first in the order of the reranker.
        asked.lock().unwrap().clear();
        let found = run(&notes, json!({"text": "kitten", "k": 2, "rerank_limit": 2}));
        let seen: usize = asked
            .lock()
            .unwrap()
            .iter()
            .map(|(_, docs, _)| docs.len())
            .sum();
        assert_eq!(seen, 2);
        let all = hits(&found);
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|hit| hit.rerank_score.is_some()));

        // Without `rerank_limit` the Vault's reranker is not called.
        asked.lock().unwrap().clear();
        let found = run(&notes, json!({"text": "kitten", "k": 2}));
        assert!(asked.lock().unwrap().is_empty());
        assert!(hits(&found).iter().all(|hit| hit.rerank_score.is_none()));
    }

    #[test]
    fn a_reranked_like_search_scores_against_the_text_of_its_document() {
        let fx = Fixture::new("search-rerank-like");
        fx.corpus();
        let notes = fx.collection("notes");
        let asked = Asked::default();
        fx.vault.set_reranker(reranker(&["dog1"], &asked)).unwrap();
        let found = run(
            &notes,
            json!({"like": {"collection": "notes", "id": "cat1"}, "k": 1, "rerank_limit": 3}),
        );
        assert_eq!(ids(&found), ["dog1"]);
        let asked_now = asked.lock().unwrap().clone();
        // The `fts` columns of the Document, as indexed.
        assert_eq!(asked_now[0].0, "Cat care\nhow to feed a kitten");
    }

    #[test]
    fn a_sync_embedder_and_reranker_are_called_on_the_thread_of_the_caller() {
        let fx = Fixture::new("search-thread");
        fx.corpus();
        let notes = fx.collection("notes");
        let asked = Asked::default();
        fx.vault.set_reranker(reranker(&[], &asked)).unwrap();
        let here = thread::current().id();
        run(&notes, json!({"text": "kitten", "rerank_limit": 10}));
        let queries = fx.calls.queries.lock().unwrap().clone();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].0, "kitten");
        assert_eq!(queries[0].1, here);
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .all(|(_, _, thread)| *thread == here)
        );
        // The same from another thread: it is that thread.
        let other = thread::scope(|scope| {
            scope
                .spawn(|| {
                    run(&notes, json!({"text": "feline"}));
                    thread::current().id()
                })
                .join()
                .unwrap()
        });
        assert_ne!(other, here);
        let queries = fx.calls.queries.lock().unwrap().clone();
        assert_eq!(queries[1], ("feline".to_owned(), other));
    }

    #[test]
    fn the_summary_names_why_the_vector_leg_did_not_run() {
        // Ran, and not asked.
        let fx = Fixture::new("search-legs");
        fx.corpus();
        let notes = fx.collection("notes");
        assert_eq!(
            summary(&run(&notes, json!({"text": "kitten"}))),
            VectorLeg::VECTOR_LEG_RAN
        );
        let off = run(&notes, json!({"text": "kitten", "vector_weight": 0}));
        assert_eq!(summary(&off), VectorLeg::VECTOR_LEG_NOT_ASKED);
        assert_eq!(
            ids(&off).len(),
            2,
            "only the lexical leg: the two with the word"
        );
        assert!(hits(&off).iter().all(|hit| hit.vector_score.is_none()));
        // The lexical leg off: the vector leg alone.
        let only = run(
            &notes,
            json!({"text": "kitten", "lexical_weight": 0, "k": 2}),
        );
        assert_eq!(summary(&only), VectorLeg::VECTOR_LEG_RAN);
        assert!(hits(&only).iter().all(|hit| hit.lexical_score.is_none()));

        // No embedder on this handle: a reader of the Index another handle
        // built.
        let reader = open_with(
            &fx.path,
            &Ports {
                projection: "docs@1",
                vectors: true,
                model: None,
            },
            &fx.calls,
        );
        let found = run(
            &collection(&reader, "notes"),
            json!({"text": "kitten", "k": 4}),
        );
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NO_EMBEDDER);
        assert_eq!(ids(&found).len(), 2, "the lexical leg answers");

        // Another model than the Index's.
        let other = open_with(
            &fx.path,
            &Ports {
                projection: "docs@1",
                vectors: true,
                model: Some("topics@2"),
            },
            &fx.calls,
        );
        let found = run(&collection(&other, "notes"), json!({"text": "feline"}));
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_MODEL_MISMATCH);
        assert!(
            ids(&found).is_empty(),
            "no word 'feline' in a title or body"
        );
    }

    #[test]
    fn the_summary_names_a_vault_without_a_vector_collection_or_without_vectors() {
        // The Projector marks no vector column.
        let fx = Fixture::with(
            "search-no-vector-collection",
            &Ports {
                projection: "docs@1",
                vectors: false,
                model: Some("topics@1"),
            },
        );
        fx.corpus();
        let found = run(&fx.collection("notes"), json!({"text": "kitten"}));
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NO_VECTOR_COLLECTION);
        assert_eq!(ids(&found).len(), 2);
        assert_eq!(fx.calls.queries.lock().unwrap().len(), 0, "no model asked");

        // A vector column and an embedder, and not one text to embed.
        let fx = Fixture::new("search-no-vectors");
        fx.put("notes", "blank", &json!({"title": "kitten", "topic": "  "}));
        let found = run(&fx.collection("notes"), json!({"text": "kitten"}));
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NO_VECTORS);
        assert_eq!(ids(&found), ["blank"]);
        assert_eq!(fx.calls.queries.lock().unwrap().len(), 0, "no model asked");
    }

    #[test]
    fn the_vault_form_spans_the_collections_it_names() {
        let fx = Fixture::new("search-vault");
        fx.corpus();
        fx.put(
            "log",
            "e1",
            &json!({"title": "kitten entry", "topic": "kitten"}),
        );
        fx.vault
            .create_collection(declaration(json!({
                "name": "secret",
                "schema": {"standard": {}, "isolated": true}
            })))
            .unwrap();
        fx.put(
            "secret",
            "s1",
            &json!({"title": "kitten secret", "topic": "kitten"}),
        );
        let vault_run = |names: Value, extra: Value| {
            let mut search = json!({"text": "kitten", "k": 20, "vector_weight": 0});
            search
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            items(
                fx.vault
                    .search_documents(vault_search(names, search))
                    .unwrap(),
            )
        };
        // No name: every collection that is not isolated.
        let mut all = keys(&vault_run(json!([]), json!({})));
        all.sort();
        assert_eq!(
            all,
            [
                ("log".to_owned(), "e1".to_owned()),
                ("notes".to_owned(), "cat1".to_owned()),
                ("notes".to_owned(), "cat2".to_owned()),
            ]
        );
        // An isolated collection is searched when it is named.
        assert_eq!(
            keys(&vault_run(json!(["secret"]), json!({}))),
            [("secret".to_owned(), "s1".to_owned())]
        );
        let mut named = keys(&vault_run(json!(["log", "secret"]), json!({})));
        named.sort();
        assert_eq!(named.len(), 2);
        // A name that is not declared.
        let error = fx
            .vault
            .search_documents(vault_search(json!(["nowhere"]), json!({"text": "kitten"})))
            .err()
            .unwrap();
        assert!(error.is(Code::NotFound), "{error}");
        // A line of a journal comes with its collection and id.
        let line = keys(&vault_run(json!(["log"]), json!({})));
        assert_eq!(line, [("log".to_owned(), "e1".to_owned())]);
    }

    fn declare(fx: &Fixture, name: &str, schema: Value) {
        let mut schema = schema;
        schema["standard"] = json!({});
        fx.vault
            .create_collection(declaration(json!({"name": name, "schema": schema})))
            .unwrap();
        fx.wait();
    }

    #[test]
    fn a_collection_that_leaves_the_lexical_index_out_is_searched_by_the_vector_leg() {
        let fx = Fixture::new("search-vector-only");
        declare(&fx, "vec", json!({"exclude_lexical_index": true}));
        for (id, title, topic) in [
            ("v1", "kitten", "cat kitten feline"),
            ("v2", "kitten too", "kitten feline"),
            ("v3", "puppy", "dog puppy canine"),
        ] {
            fx.put_unwaited("vec", id, &json!({"title": title, "topic": topic}));
        }
        fx.put("notes", "n1", &json!({"title": "kitten", "topic": "car"}));
        let vec = fx.collection("vec");

        // The vector leg alone.
        let found = run(&vec, json!({"text": "feline", "lexical_weight": 0, "k": 2}));
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_RAN);
        let mut got = ids(&found);
        got.sort();
        assert_eq!(got, ["v1", "v2"]);
        assert!(hits(&found).iter().all(|hit| hit.lexical_score.is_none()));
        assert!(hits(&found).iter().all(|hit| hit.vector_score.is_some()));

        // Default weights: the lexical leg finds nothing there.
        let found = run(&vec, json!({"text": "kitten", "k": 3}));
        assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_RAN);
        assert_eq!(ids(&found).len(), 3);
        assert!(hits(&found).iter().all(|hit| hit.lexical_score.is_none()));

        // The lexical leg alone finds nothing, and says nothing is wrong.
        let found = run(&vec, json!({"text": "kitten", "vector_weight": 0}));
        assert!(ids(&found).is_empty());

        // `like` takes the stored vectors of its Document.
        let before = fx.calls.queries.lock().unwrap().len();
        let found = run(
            &vec,
            json!({"like": {"collection": "vec", "id": "v1"}, "k": 1}),
        );
        assert_eq!(ids(&found), ["v2"]);
        assert_eq!(fx.calls.queries.lock().unwrap().len(), before);

        // The default scope of the Vault form holds it next to the others.
        let all = items(
            fx.vault
                .search_documents(vault_search(json!([]), json!({"text": "kitten", "k": 10})))
                .unwrap(),
        );
        let found = keys(&all);
        assert!(found.contains(&("vec".to_owned(), "v1".to_owned())));
        assert!(found.contains(&("notes".to_owned(), "n1".to_owned())));

        // A Filter is checked in the lexical Index: the default scope leaves
        // the collection out, and naming it is refused.
        let filter = json!({"term": {"field": "kind", "value": "x", "collection": "notes"}});
        let filtered = items(
            fx.vault
                .search_documents(vault_search(
                    json!([]),
                    json!({"text": "kitten", "k": 10, "filter": filter}),
                ))
                .unwrap(),
        );
        assert!(keys(&filtered).iter().all(|(c, _)| c != "vec"));
        let error = fx
            .vault
            .search_documents(vault_search(
                json!(["vec"]),
                json!({"text": "kitten", "filter": filter}),
            ))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let error = vec
            .search(search_of(json!({
                "text": "kitten",
                "filter": {"term": {"field": "kind", "value": "x"}}
            })))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        // `all` is no Filter.
        let plain = run(&vec, json!({"text": "kitten", "filter": {"all": {}}}));
        assert_eq!(ids(&plain).len(), 3);
    }

    #[test]
    fn a_collection_that_leaves_the_vector_index_out_is_searched_by_the_lexical_leg() {
        let fx = Fixture::new("search-lexical-only");
        declare(&fx, "words", json!({"exclude_vector_index": true}));
        let embedded = fx.calls.embedded.load(Ordering::SeqCst);
        fx.put(
            "words",
            "w1",
            &json!({"title": "kryptonite mineral", "topic": "cat"}),
        );
        assert_eq!(
            fx.calls.embedded.load(Ordering::SeqCst),
            embedded,
            "nothing of it is embedded"
        );
        let found = run(&fx.collection("words"), json!({"text": "kryptonite"}));
        assert_eq!(ids(&found), ["w1"]);
        assert!(hits(&found)[0].lexical_score.is_some());
        assert!(hits(&found)[0].vector_score.is_none());
    }

    #[test]
    fn a_collection_with_both_legs_left_out_is_unimplemented_when_named() {
        let fx = Fixture::new("search-both-excluded");
        declare(
            &fx,
            "quiet",
            json!({"exclude_lexical_index": true, "exclude_vector_index": true}),
        );
        fx.put("quiet", "q1", &json!({"title": "kitten", "topic": "cat"}));
        fx.put("notes", "n1", &json!({"title": "kitten"}));
        let error = fx
            .vault
            .search_documents(vault_search(json!(["quiet"]), json!({"text": "kitten"})))
            .err()
            .unwrap();
        assert!(error.is(Code::Unimplemented), "{error}");
        assert!(error.to_string().contains("quiet"), "{error}");
        let error = fx
            .collection("quiet")
            .search(search_of(json!({"text": "kitten"})))
            .err()
            .unwrap();
        assert!(error.is(Code::Unimplemented), "{error}");
        // Left out of the default scope.
        let all = items(
            fx.vault
                .search_documents(vault_search(json!([]), json!({"text": "kitten"})))
                .unwrap(),
        );
        assert!(keys(&all).iter().all(|(c, _)| c != "quiet"));
        assert!(!keys(&all).is_empty());
    }

    #[test]
    fn the_filter_narrows_both_legs_before_they_rank() {
        let fx = Fixture::new("search-filter");
        fx.corpus();
        let notes = fx.collection("notes");
        // The cats rank first; the filter keeps only the vehicle, which the
        // lexical leg cannot find and the vector leg ranks last.
        let found = run(
            &notes,
            json!({
                "text": "kitten",
                "filter": {"term": {"field": "kind", "value": "vehicle"}}
            }),
        );
        assert_eq!(ids(&found), ["car1"]);
        let one = &hits(&found)[0];
        assert!(one.vector_score.is_some());
        let pets = run(
            &notes,
            json!({
                "text": "kitten",
                "filter": {"term": {"field": "kind", "value": "pet"}}
            }),
        );
        assert_eq!(ids(&pets).len(), 3);
        assert!(!ids(&pets).contains(&"car1".to_owned()));
        // A leaf of a Vault search names the collection of its column.
        let error = fx
            .vault
            .search_documents(vault_search(
                json!([]),
                json!({"text": "kitten", "filter": {"term": {"field": "kind", "value": "pet"}}}),
            ))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let named = items(
            fx.vault
                .search_documents(vault_search(
                    json!([]),
                    json!({
                        "text": "kitten",
                        "filter": {"term": {"field": "kind", "value": "vehicle", "collection": "notes"}}
                    }),
                ))
                .unwrap(),
        );
        assert_eq!(ids(&named), ["car1"]);
    }

    #[test]
    fn a_filter_selects_before_the_lexical_pool_is_cut() {
        let fx = Fixture::new("search-filter-pool");
        for n in 0..80 {
            fx.put_unwaited(
                "notes",
                &format!("loud{n:02}"),
                &json!({"title": "alpha alpha", "kind": "x"}),
            );
        }
        fx.put(
            "notes",
            "target",
            &json!({"title": "alpha among many other words here", "kind": "y"}),
        );
        let found = run(
            &fx.collection("notes"),
            json!({
                "text": "alpha",
                "k": 5,
                "vector_weight": 0,
                "filter": {"term": {"field": "kind", "value": "y"}}
            }),
        );
        assert_eq!(ids(&found), ["target"]);
        assert!(
            hits(&found)[0]
                .lexical_score
                .is_some_and(|score| score > 0.0)
        );
    }

    #[test]
    fn the_weights_are_checked_and_zero_turns_a_leg_off() {
        let fx = Fixture::new("search-weights");
        fx.corpus();
        let notes = fx.collection("notes");
        for search in [
            // Both off: nothing could run.
            json!({"text": "kitten", "lexical_weight": 0, "vector_weight": 0}),
            // A rule of the message.
            json!({"text": "kitten", "lexical_weight": -1}),
            json!({"text": "kitten", "vector_weight": -0.5}),
            // Exactly one subject.
            json!({}),
            json!({"text": ""}),
        ] {
            let error = notes.search(search_of(search.clone())).err().unwrap();
            assert!(error.is(Code::InvalidArgument), "{search}: {error}");
        }
        // A weight shifts the fusion: with the vector leg far heavier, the
        // vector leg decides the order.
        let heavy = run(
            &notes,
            json!({"text": "kitten", "vector_weight": 50, "k": 4}),
        );
        let vector_only = run(
            &notes,
            json!({"text": "kitten", "lexical_weight": 0, "k": 4}),
        );
        assert_eq!(ids(&heavy)[0], ids(&vector_only)[0]);
    }

    #[test]
    fn the_documents_carry_their_raw_only_when_asked() {
        let fx = Fixture::new("search-raw");
        fx.corpus();
        let notes = fx.collection("notes");
        let bare = hits(&run(&notes, json!({"text": "dog", "k": 1})));
        let document = bare[0].document.as_option().unwrap();
        assert_eq!(document.id.as_deref(), Some("dog1"));
        assert_eq!(document.collection.as_deref(), Some("notes"));
        assert!(document.raw.is_none());
        assert!(document.revision.is_some());
        let full = hits(&run(
            &notes,
            json!({"text": "dog", "k": 1, "include_raw": true}),
        ));
        let document = full[0].document.as_option().unwrap();
        let raw: Value = serde_json::from_slice(document.raw.as_deref().unwrap()).unwrap();
        assert_eq!(raw["title"], "Dog training");
    }

    #[test]
    fn a_document_deleted_after_the_index_saw_it_is_not_a_hit_once_the_index_catches_up() {
        let fx = Fixture::new("search-deleted");
        fx.corpus();
        let notes = fx.collection("notes");
        notes
            .delete(DocumentDeletion::default().with_id("cat2").with_note("t"))
            .unwrap();
        fx.wait();
        let found = ids(&run(&notes, json!({"text": "kitten", "k": 10})));
        assert!(!found.contains(&"cat2".to_owned()), "{found:?}");
        assert!(found.contains(&"cat1".to_owned()));
    }

    #[test]
    fn a_stream_dropped_early_leaves_the_vault_usable() {
        let fx = Fixture::new("search-drop");
        fx.corpus();
        let notes = fx.collection("notes");
        let mut stream = notes
            .search(search_of(json!({"text": "kitten", "include_raw": true})))
            .unwrap();
        assert!(stream.next().is_some());
        assert!(stream.next().is_some());
        drop(stream);
        assert_eq!(ids(&run(&notes, json!({"text": "kitten"}))).len(), 4);
        fx.vault.close(Empty::default()).unwrap();
    }
}
