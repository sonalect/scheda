//! The searches of the contract: `Collection.Search` and
//! `Vault.SearchDocuments`, over the Index on Lance with a Projector of
//! JSON Documents, an embedder of three topics, and a reranker that prefers
//! the ids it is given. A build without the Index answers both
//! `UNIMPLEMENTED`.

use scheda::v1::{DocumentSearch, VaultSearch};

fn search_of(value: serde_json::Value) -> DocumentSearch {
    serde_json::from_value(value).unwrap()
}

fn vault_search(names: serde_json::Value, search: serde_json::Value) -> VaultSearch {
    let mut request: VaultSearch =
        serde_json::from_value(serde_json::json!({"collections": {"names": names}})).unwrap();
    request.search = search_of(search).into();
    request
}

#[cfg(not(feature = "index"))]
#[test]
fn without_the_index_the_two_calls_are_unimplemented() {
    use scheda::v1::{CollectionAsync, VaultAsync, VaultErrorCode};

    use crate::common::{Scratch, both, collection, open, snapshot, vault as built_vault};

    both(|| async {
        let scratch = Scratch::new("search-unimplemented");
        let path = scratch.join("vault");
        built_vault(&path);
        let vault = open(&path, Default::default()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let before = snapshot(&path);
        let search = serde_json::json!({"text": "words"});
        let error = notes.search(search_of(search.clone())).await.err().unwrap();
        assert!(error.is(VaultErrorCode::Unimplemented), "{error}");
        let error = vault
            .search_documents(vault_search(serde_json::json!([]), search))
            .await
            .err()
            .unwrap();
        assert!(error.is(VaultErrorCode::Unimplemented), "{error}");
        assert_eq!(snapshot(&path), before, "a refused search writes nothing");
    });
}

#[cfg(feature = "index")]
mod on_lance {
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use protocontract::BoxIter;
    use scheda::v1::document_operation::Operation;
    use scheda::v1::embedder_ref::EmbedderRef;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::reranker_ref::RerankerRef;
    use scheda::v1::search_item::Item;
    use scheda::v1::search_summary::VectorLeg;
    use scheda::v1::{
        CollectionAsync, CollectionDeclaration, CollectionKey, ColumnKind, Document,
        DocumentCreation, DocumentDeletion, DocumentOperation, DocumentUpsert, DynCollectionAsync,
        DynVaultAsync, EmbedderDescriptor, EmbedderSync, IndexAsync, IndexColumn, IndexDoc,
        IndexSchema, IndexValue, OpenOptions, ProjectorDescriptor, ProjectorSync, Query,
        RawDocument, SchedaAsync, SearchHit, SearchItem, Text, VaultAsync, VaultErrorCode as Code,
        Vector, index_value,
    };
    use scheda::{Error, FixedReranker, Scheda};
    use serde_json::{Value, json};

    use super::{search_of, vault_search};
    use crate::common::{Scratch, both, collection, items, next, opening, snapshot, stream_of};

    const DEADLINE: Duration = Duration::from_secs(120);

    fn column(name: &str, kind: ColumnKind) -> IndexColumn {
        IndexColumn::default().with_name(name).with_kind(kind)
    }

    /// A Projector of JSON Documents with four text columns: `title` and
    /// `body` (searched), `topic` (embedded, when the Projector marks
    /// vectors), and `kind` (a scalar index, for filters).
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
                    column("title", text).with_fts(true),
                    column("body", text).with_fts(true),
                    column("topic", text).with_vector(self.vectors),
                    column("kind", text).with_scalar_index(true),
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

    /// What the embedder was asked.
    #[derive(Default)]
    struct Calls {
        queries: Mutex<Vec<String>>,
        embedded: AtomicUsize,
    }

    impl Calls {
        fn queries(&self) -> Vec<String> {
            self.queries.lock().unwrap().clone()
        }
    }

    /// An embedder of the three topics of a text.
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
            self.calls.queries.lock().unwrap().push(text.clone());
            Ok(Vector {
                values: topic_vector(&text),
                ..Vector::default()
            })
        }
    }

    /// What a reranker was asked: the query, and for each Document its id
    /// and whether it had its `raw`.
    type Asked = Arc<Mutex<Vec<(String, Vec<(String, bool)>)>>>;

    /// A reranker that prefers the ids it is given, in that order.
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
                asked.lock().unwrap().push((query.to_owned(), docs));
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

    const HYBRID: Ports = Ports {
        projection: "docs@1",
        vectors: true,
        model: Some("topics@1"),
    };

    async fn open_with(path: &Path, ports: &Ports, calls: &Arc<Calls>) -> DynVaultAsync {
        let mut request = opening(path, options());
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
        Scheda
            .open(request)
            .await
            .unwrap()
            .to_async_inline()
            .unwrap()
    }

    fn declaration(value: Value) -> CollectionDeclaration {
        serde_json::from_value(value).unwrap()
    }

    /// Waits until the Index of `vault` reflects the stored state now.
    async fn wait_indexed(vault: &DynVaultAsync) {
        let index = vault
            .index(Empty::default())
            .await
            .unwrap()
            .to_async_inline()
            .unwrap();
        let deadline = Instant::now() + DEADLINE;
        loop {
            let head = vault.info(Empty::default()).await.unwrap().version;
            let state = index.status(Empty::default()).await.unwrap();
            if state.version.is_some() && state.version == head {
                return;
            }
            assert!(Instant::now() < deadline, "not indexed: {state:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A Vault with the collections `notes` and `log` (a journal), the
    /// Index built.
    struct Fixture {
        _scratch: Scratch,
        path: PathBuf,
        vault: DynVaultAsync,
        calls: Arc<Calls>,
    }

    impl Fixture {
        async fn new(name: &str) -> Fixture {
            Fixture::with(name, &HYBRID).await
        }

        async fn with(name: &str, ports: &Ports) -> Fixture {
            let scratch = Scratch::new(name);
            let path = scratch.join("vault");
            crate::common::vault(&path);
            let calls = Arc::new(Calls::default());
            let vault = open_with(&path, ports, &calls).await;
            wait_indexed(&vault).await;
            Fixture {
                _scratch: scratch,
                path,
                vault,
                calls,
            }
        }

        async fn collection(&self, name: &str) -> DynCollectionAsync {
            collection(&self.vault, name).await.unwrap()
        }

        /// Writes the Document `id` of `collection` (appends a line to the
        /// journal `log`) without waiting for the Index; its state token.
        async fn put_unwaited(&self, collection: &str, id: &str, raw: &Value) -> String {
            let bytes = serde_json::to_vec(raw).unwrap();
            let target = self.collection(collection).await;
            let written = if collection == "log" {
                target
                    .create(
                        DocumentCreation::default()
                            .with_id(id)
                            .with_raw(bytes)
                            .with_note("test"),
                    )
                    .await
                    .unwrap()
            } else {
                target
                    .upsert(
                        DocumentUpsert::default()
                            .with_id(id)
                            .with_raw(bytes)
                            .with_note("test"),
                    )
                    .await
                    .unwrap()
            };
            written.version.unwrap()
        }

        async fn put(&self, collection: &str, id: &str, raw: &Value) {
            self.put_unwaited(collection, id, raw).await;
            wait_indexed(&self.vault).await;
        }

        /// The standard corpus of `notes`.
        async fn corpus(&self) {
            let operations: Vec<DocumentOperation> = [
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
            ]
            .into_iter()
            .map(|(id, title, body, topic, kind)| {
                let raw = json!({"title": title, "body": body, "topic": topic, "kind": kind});
                DocumentOperation {
                    operation: Some(Operation::Upsert(Box::new(
                        DocumentUpsert::default()
                            .with_id(id)
                            .with_raw(raw.to_string().into_bytes())
                            .with_note("test"),
                    ))),
                    ..DocumentOperation::default()
                }
            })
            .collect();
            self.collection("notes")
                .await
                .batch(stream_of(operations))
                .await
                .unwrap();
            wait_indexed(&self.vault).await;
        }

        async fn declare(&self, name: &str, mut schema: Value) {
            schema["standard"] = json!({});
            self.vault
                .create_collection(declaration(json!({"name": name, "schema": schema})))
                .await
                .unwrap();
            wait_indexed(&self.vault).await;
        }
    }

    /// The whole stream of a search of a collection.
    async fn run(target: &DynCollectionAsync, search: Value) -> Vec<SearchItem> {
        let stream = target.search(search_of(search)).await.unwrap();
        items(stream)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    /// The whole stream of a search of the Vault.
    async fn run_vault(vault: &DynVaultAsync, names: Value, search: Value) -> Vec<SearchItem> {
        let stream = vault
            .search_documents(vault_search(names, search))
            .await
            .unwrap();
        items(stream)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    /// The failure of a search of a collection.
    async fn refused(target: &DynCollectionAsync, search: Value) -> Error {
        target.search(search_of(search)).await.err().unwrap()
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
        let mut keys: Vec<(String, String)> = hits(items)
            .iter()
            .map(|hit| {
                let document = hit.document.as_option().unwrap();
                (
                    document.collection.clone().unwrap_or_default(),
                    document.id.clone().unwrap_or_default(),
                )
            })
            .collect();
        keys.sort();
        keys
    }

    fn key(collection: &str, id: &str) -> (String, String) {
        (collection.to_owned(), id.to_owned())
    }

    #[test]
    fn a_text_search_fuses_the_lexical_and_the_vector_leg() {
        both(|| async {
            let fx = Fixture::new("search-hybrid").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let found = run(&notes, json!({"text": "kitten", "k": 4})).await;
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_RAN);
            let all = hits(&found);
            assert_eq!(all.len(), 4, "the vector leg ranks every Document");
            // `cat2` has the word in its title and its body, and is the
            // nearest by topic.
            assert_eq!(id_of(&all[0]), "cat2");
            let cat = all.iter().find(|hit| id_of(hit) == "cat1").unwrap();
            assert!(cat.lexical_score.is_some_and(|score| score > 0.0));
            assert!(cat.vector_score.is_some());
            assert!(cat.rerank_score.is_none());
            // A Document only the vector leg finds has no lexical score.
            let car = all.iter().find(|hit| id_of(hit) == "car1").unwrap();
            assert!(car.lexical_score.is_none());
            assert!(car.vector_score.is_some());
            let scores: Vec<f64> = all.iter().filter_map(|hit| hit.score).collect();
            assert!(
                scores.windows(2).all(|pair| pair[0] >= pair[1]),
                "{scores:?}"
            );

            // A synonym no title or body holds: the vector leg alone.
            let found = run(
                &notes,
                json!({"text": "feline", "k": 2, "lexical_weight": 0}),
            )
            .await;
            let mut got = ids(&found);
            got.sort();
            assert_eq!(got, ["cat1", "cat2"]);
            assert!(hits(&found).iter().all(|hit| hit.lexical_score.is_none()));

            // A word of a column not marked `fts` is not searched.
            let found = run(&notes, json!({"text": "vehicle", "vector_weight": 0})).await;
            assert!(ids(&found).is_empty());
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NOT_ASKED);
        });
    }

    #[test]
    fn like_finds_the_neighbours_of_a_document_without_calling_the_embedder() {
        both(|| async {
            let fx = Fixture::new("search-like").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let asked = fx.calls.queries().len();
            let found = run(
                &notes,
                json!({"like": {"collection": "notes", "id": "cat1"}, "k": 3}),
            )
            .await;
            assert_eq!(fx.calls.queries().len(), asked, "like calls no model");
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_RAN);
            let all = hits(&found);
            // Not its own neighbour; its neighbour comes first, by both legs.
            assert!(!all.iter().any(|hit| id_of(hit) == "cat1"));
            assert_eq!(id_of(&all[0]), "cat2");
            assert!(all[0].lexical_score.is_some());
            assert!(all[0].vector_score.is_some());
            let found = run_vault(
                &fx.vault,
                json!(["notes"]),
                json!({"like": {"collection": "notes", "id": "cat1"}, "k": 1}),
            )
            .await;
            assert_eq!(ids(&found), ["cat2"]);

            // A Document the Index does not hold, or of no collection.
            let before = snapshot(&fx.path);
            for like in [
                json!({"collection": "notes", "id": "ghost"}),
                json!({"collection": "nowhere", "id": "x"}),
            ] {
                let error = refused(&notes, json!({"like": like})).await;
                assert!(error.is(Code::NotFound), "{error}");
            }
            assert_eq!(
                snapshot(&fx.path),
                before,
                "a refused search writes nothing"
            );
            let error = fx
                .vault
                .search_documents(vault_search(json!([]), json!({"like": {"id": "cat1"}})))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::InvalidArgument), "{error}");

            // A Document of another collection than the scope: a journal
            // line.
            fx.put(
                "log",
                "l1",
                &json!({"title": "kitten log", "topic": "kitten"}),
            )
            .await;
            let found = run(
                &notes,
                json!({"like": {"collection": "log", "id": "l1"}, "k": 1}),
            )
            .await;
            assert_eq!(ids(&found).len(), 1);
        });
    }

    #[test]
    fn k_and_offset_cut_one_ranking_and_the_first_item_is_the_summary() {
        both(|| async {
            let fx = Fixture::new("search-k").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let mut stream = notes
                .search(search_of(json!({"text": "kitten", "k": 4_000_000})))
                .await
                .unwrap();
            let first = next(&mut stream).await.unwrap().unwrap();
            assert!(matches!(first.item, Some(Item::Summary(_))));
            let rest = items(stream).await;
            assert_eq!(rest.len(), 4, "every Document there is");

            let whole = ids(&run(&notes, json!({"text": "kitten", "k": 4})).await);
            let first = ids(&run(&notes, json!({"text": "kitten", "k": 2})).await);
            let second = ids(&run(&notes, json!({"text": "kitten", "k": 2, "offset": 2})).await);
            assert_eq!(first, whole[..2]);
            assert_eq!(second, whole[2..]);
            let beyond = run(&notes, json!({"text": "kitten", "offset": 100})).await;
            assert_eq!(beyond.len(), 1);
            assert_eq!(summary(&beyond), VectorLeg::VECTOR_LEG_RAN);

            // `k` unset or 0 is 10.
            for n in 0..12 {
                fx.put_unwaited(
                    "notes",
                    &format!("e{n:02}"),
                    &json!({"title": "kitten again"}),
                )
                .await;
            }
            wait_indexed(&fx.vault).await;
            for search in [json!({"text": "kitten"}), json!({"text": "kitten", "k": 0})] {
                assert_eq!(ids(&run(&notes, search).await).len(), 10);
            }
        });
    }

    #[test]
    fn rerank_limit_needs_a_reranker_and_covers_the_page() {
        both(|| async {
            let fx = Fixture::new("search-rerank-rules").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let rerank = json!({"text": "kitten", "k": 2, "rerank_limit": 4});
            let error = refused(&notes, rerank.clone()).await;
            assert!(error.is(Code::InvalidArgument), "{error}");
            let asked = Asked::default();
            fx.vault.set_reranker(reranker(&[], &asked)).await.unwrap();
            assert!(notes.search(search_of(rerank.clone())).await.is_ok());
            let asked_once = asked.lock().unwrap().len();
            for below in [
                json!({"text": "kitten", "k": 3, "rerank_limit": 2}),
                // `k` is 10 when unset.
                json!({"text": "kitten", "rerank_limit": 9}),
                // Zero is no limit at all: a rule of the message.
                json!({"text": "kitten", "rerank_limit": 0}),
            ] {
                let error = refused(&notes, below).await;
                assert!(error.is(Code::InvalidArgument), "{error}");
            }
            assert_eq!(
                asked.lock().unwrap().len(),
                asked_once,
                "a refused search asks nothing"
            );
            fx.vault.remove_reranker(Empty::default()).await.unwrap();
            let error = refused(&notes, rerank).await;
            assert!(error.is(Code::InvalidArgument), "{error}");
            let error = fx
                .vault
                .search_documents(vault_search(
                    json!([]),
                    json!({"text": "kitten", "rerank_limit": 10}),
                ))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::InvalidArgument), "{error}");
        });
    }

    #[test]
    fn a_reranker_orders_the_first_hits_and_sees_the_query_first() {
        both(|| async {
            let fx = Fixture::new("search-rerank").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let plain = ids(&run(&notes, json!({"text": "kitten", "k": 4})).await);
            let worst = plain[3].clone();
            let asked = Asked::default();
            fx.vault
                .set_reranker(reranker(&[worst.as_str()], &asked))
                .await
                .unwrap();

            // The first `rerank_limit` fused hits, with their `raw`, ordered
            // by the reranker: the last of the four comes first.
            let all =
                hits(&run(&notes, json!({"text": "kitten", "k": 2, "rerank_limit": 4})).await);
            assert_eq!(all.len(), 2);
            assert_eq!(id_of(&all[0]), worst);
            assert_eq!(all[0].rerank_score, Some(100.0));
            assert_eq!(all[1].rerank_score, Some(0.0));
            assert!(all[0].score.is_some(), "the fused score stays");
            let asked_now = asked.lock().unwrap().clone();
            for (query, docs) in &asked_now {
                assert_eq!(query, "kitten", "the query comes first");
                assert!(docs.iter().all(|(_, has_raw)| *has_raw));
            }
            let mut seen: Vec<String> = asked_now
                .iter()
                .flat_map(|(_, docs)| docs.iter().map(|(id, _)| id.clone()))
                .collect();
            seen.sort();
            let mut want = plain.clone();
            want.sort();
            assert_eq!(seen, want);

            // Without `rerank_limit` the reranker is not called.
            asked.lock().unwrap().clear();
            let found = run(&notes, json!({"text": "kitten", "k": 2})).await;
            assert!(asked.lock().unwrap().is_empty());
            assert!(hits(&found).iter().all(|hit| hit.rerank_score.is_none()));

            // `like` scores against the `fts` text of its Document.
            fx.vault
                .set_reranker(reranker(&["dog1"], &asked))
                .await
                .unwrap();
            let found = run(
                &notes,
                json!({"like": {"collection": "notes", "id": "cat1"}, "k": 1, "rerank_limit": 3}),
            )
            .await;
            assert_eq!(ids(&found), ["dog1"]);
            let asked_now = asked.lock().unwrap().clone();
            assert_eq!(asked_now[0].0, "Cat care\nhow to feed a kitten");
        });
    }

    #[test]
    fn the_summary_names_why_the_vector_leg_did_not_run() {
        both(|| async {
            let fx = Fixture::new("search-legs").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let off = run(&notes, json!({"text": "kitten", "vector_weight": 0})).await;
            assert_eq!(summary(&off), VectorLeg::VECTOR_LEG_NOT_ASKED);
            assert_eq!(ids(&off).len(), 2, "the two with the word");
            assert!(hits(&off).iter().all(|hit| hit.vector_score.is_none()));

            // No embedder on this handle: it does not index, and searches
            // the Index another handle built with the lexical leg.
            let reader = open_with(
                &fx.path,
                &Ports {
                    model: None,
                    ..HYBRID
                },
                &fx.calls,
            )
            .await;
            let found = run(
                &collection(&reader, "notes").await.unwrap(),
                json!({"text": "kitten", "k": 4}),
            )
            .await;
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NO_EMBEDDER);
            assert_eq!(ids(&found).len(), 2, "the lexical leg answers");

            // Alone, it writes: its status says the Index is behind `HEAD`
            // and why, and a collection the Index does not hold yet is
            // still known to have a vector column.
            fx.vault.close(Empty::default()).await.unwrap();
            reader
                .create_collection(declaration(
                    json!({"name": "fresh", "schema": {"standard": {}}}),
                ))
                .await
                .unwrap();
            let fresh = collection(&reader, "fresh").await.unwrap();
            let head = fresh
                .upsert(
                    DocumentUpsert::default()
                        .with_id("f1")
                        .with_raw(json!({"title": "kitten"}).to_string().into_bytes())
                        .with_note("test"),
                )
                .await
                .unwrap()
                .version;
            let index = reader
                .index(Empty::default())
                .await
                .unwrap()
                .to_async_inline()
                .unwrap();
            let deadline = Instant::now() + DEADLINE;
            let state = loop {
                let state = index.status(Empty::default()).await.unwrap();
                if state.stored_version == head {
                    break state;
                }
                assert!(Instant::now() < deadline, "{state:?}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert_ne!(state.version, state.stored_version, "{state:?}");
            let report = state.last_error.as_option().unwrap();
            assert_eq!(
                report.code.as_deref(),
                Some("VAULT_ERROR_CODE_FAILED_PRECONDITION")
            );
            let found = run(&fresh, json!({"text": "kitten"})).await;
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NO_EMBEDDER);
            assert!(ids(&found).is_empty());
            reader.close(Empty::default()).await.unwrap();

            // Another model than the Index's: the Index is stale for it.
            let other = open_with(
                &fx.path,
                &Ports {
                    model: Some("topics@2"),
                    ..HYBRID
                },
                &fx.calls,
            )
            .await;
            let error = refused(
                &collection(&other, "notes").await.unwrap(),
                json!({"text": "feline"}),
            )
            .await;
            assert!(error.is(Code::IndexStale), "{error}");
            other.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn the_summary_names_a_vault_without_a_vector_collection_or_without_vectors() {
        both(|| async {
            let fx = Fixture::with(
                "search-no-vector-collection",
                &Ports {
                    vectors: false,
                    ..HYBRID
                },
            )
            .await;
            fx.corpus().await;
            let found = run(&fx.collection("notes").await, json!({"text": "kitten"})).await;
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NO_VECTOR_COLLECTION);
            assert_eq!(ids(&found).len(), 2);
            assert!(fx.calls.queries().is_empty(), "no model asked");

            // A vector column and an embedder, and not one text to embed.
            let fx = Fixture::new("search-no-vectors").await;
            fx.put("notes", "blank", &json!({"title": "kitten", "topic": "  "}))
                .await;
            let found = run(&fx.collection("notes").await, json!({"text": "kitten"})).await;
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_NO_VECTORS);
            assert_eq!(ids(&found), ["blank"]);
            assert!(fx.calls.queries().is_empty(), "no model asked");
        });
    }

    #[test]
    fn the_vault_form_spans_the_collections_it_names() {
        both(|| async {
            let fx = Fixture::new("search-vault").await;
            fx.corpus().await;
            fx.put(
                "log",
                "e1",
                &json!({"title": "kitten entry", "topic": "kitten"}),
            )
            .await;
            fx.vault
                .create_collection(declaration(json!({
                    "name": "secret",
                    "schema": {"standard": {}, "isolated": true}
                })))
                .await
                .unwrap();
            fx.put(
                "secret",
                "s1",
                &json!({"title": "kitten secret", "topic": "kitten"}),
            )
            .await;
            let lexical = json!({"text": "kitten", "k": 20, "vector_weight": 0});
            // No name: every collection that is not isolated.
            assert_eq!(
                keys(&run_vault(&fx.vault, json!([]), lexical.clone()).await),
                [key("log", "e1"), key("notes", "cat1"), key("notes", "cat2")]
            );
            // An isolated collection when it is named.
            assert_eq!(
                keys(&run_vault(&fx.vault, json!(["secret"]), lexical.clone()).await),
                [key("secret", "s1")]
            );
            assert_eq!(
                keys(&run_vault(&fx.vault, json!(["log", "secret"]), lexical.clone()).await),
                [key("log", "e1"), key("secret", "s1")]
            );
            // Both legs across the collections, one ranking.
            let both_legs =
                run_vault(&fx.vault, json!([]), json!({"text": "kitten", "k": 20})).await;
            assert_eq!(keys(&both_legs).len(), 5, "the four notes and the line");
            let error = fx
                .vault
                .search_documents(vault_search(json!(["nowhere"]), json!({"text": "kitten"})))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::NotFound), "{error}");
        });
    }

    #[test]
    fn a_collection_a_leg_leaves_out_is_ranked_by_the_other_and_one_both_leave_out_is_unimplemented()
     {
        both(|| async {
            let fx = Fixture::new("search-excluded").await;
            fx.declare("vec", json!({"exclude_lexical_index": true}))
                .await;
            fx.declare("words", json!({"exclude_vector_index": true}))
                .await;
            fx.declare(
                "quiet",
                json!({"exclude_lexical_index": true, "exclude_vector_index": true}),
            )
            .await;
            let embedded = fx.calls.embedded.load(Ordering::SeqCst);
            for (id, title, topic) in [
                ("v1", "kitten", "cat kitten feline"),
                ("v2", "kitten too", "kitten feline"),
                ("v3", "puppy", "dog puppy canine"),
            ] {
                fx.put_unwaited("vec", id, &json!({"title": title, "topic": topic}))
                    .await;
            }
            fx.put_unwaited(
                "words",
                "w1",
                &json!({"title": "kryptonite mineral", "topic": "cat"}),
            )
            .await;
            fx.put_unwaited("quiet", "q1", &json!({"title": "kitten", "topic": "cat"}))
                .await;
            fx.put("notes", "n1", &json!({"title": "kitten", "topic": "car"}))
                .await;
            assert_eq!(
                fx.calls.embedded.load(Ordering::SeqCst),
                embedded + 3 + 1,
                "the texts of vec and notes only"
            );

            // Without its columns: the vector leg alone ranks it.
            let vec = fx.collection("vec").await;
            let found = run(&vec, json!({"text": "kitten", "k": 3})).await;
            assert_eq!(summary(&found), VectorLeg::VECTOR_LEG_RAN);
            assert_eq!(ids(&found).len(), 3);
            assert!(hits(&found).iter().all(|hit| hit.lexical_score.is_none()));
            let found = run(
                &vec,
                json!({"like": {"collection": "vec", "id": "v1"}, "k": 1}),
            )
            .await;
            assert_eq!(ids(&found), ["v2"]);
            // A filter needs the columns.
            let filter = json!({"exists": {"column": "kind"}});
            let error = refused(&vec, json!({"text": "kitten", "filter": filter})).await;
            assert!(error.is(Code::Unimplemented), "{error}");

            // Without its vectors: the lexical leg alone.
            let found = run(&fx.collection("words").await, json!({"text": "kryptonite"})).await;
            assert_eq!(ids(&found), ["w1"]);
            assert!(hits(&found)[0].lexical_score.is_some());
            assert!(hits(&found)[0].vector_score.is_none());

            // Without both: refused when named, left out of the Vault's
            // scope.
            let error = refused(&fx.collection("quiet").await, json!({"text": "kitten"})).await;
            assert!(error.is(Code::Unimplemented), "{error}");
            let error = fx
                .vault
                .search_documents(vault_search(json!(["quiet"]), json!({"text": "kitten"})))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::Unimplemented), "{error}");
            let all =
                keys(&run_vault(&fx.vault, json!([]), json!({"text": "kitten", "k": 10})).await);
            assert!(all.contains(&key("vec", "v1")));
            assert!(all.contains(&key("notes", "n1")));
            assert!(all.iter().all(|(collection, _)| collection != "quiet"));
        });
    }

    #[test]
    fn the_filter_narrows_both_legs_before_they_rank() {
        both(|| async {
            let fx = Fixture::new("search-filter").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let vehicle = json!({"term": {"column": "kind", "value": {"text": "vehicle"}}});
            // The cats rank first; the filter keeps only the vehicle, which
            // the lexical leg cannot find and the vector leg ranks last.
            let found = run(&notes, json!({"text": "kitten", "filter": vehicle})).await;
            assert_eq!(ids(&found), ["car1"]);
            assert!(hits(&found)[0].vector_score.is_some());
            let pets = run(
                &notes,
                json!({"text": "kitten", "filter": {"term": {"column": "kind", "value": {"text": "pet"}}}}),
            )
            .await;
            assert_eq!(ids(&pets).len(), 3);
            // One filter for every collection of the Vault form.
            let found = run_vault(
                &fx.vault,
                json!([]),
                json!({"text": "kitten", "filter": vehicle}),
            )
            .await;
            assert_eq!(keys(&found), [key("notes", "car1")]);
            // A column no collection in scope declares.
            let error = refused(
                &notes,
                json!({"text": "kitten", "filter": {"exists": {"column": "colour"}}}),
            )
            .await;
            assert!(error.is(Code::InvalidArgument), "{error}");

            // The lexical pool is taken from the Documents the filter
            // admits, not cut first.
            let operations: Vec<DocumentOperation> = (0..80)
                .map(|n| DocumentOperation {
                    operation: Some(Operation::Upsert(Box::new(
                        DocumentUpsert::default()
                            .with_id(format!("loud{n:02}"))
                            .with_raw(
                                json!({"title": "alpha alpha", "kind": "x"})
                                    .to_string()
                                    .into_bytes(),
                            )
                            .with_note("test"),
                    ))),
                    ..DocumentOperation::default()
                })
                .collect();
            notes.batch(stream_of(operations)).await.unwrap();
            fx.put(
                "notes",
                "target",
                &json!({"title": "alpha among many other words here", "kind": "y"}),
            )
            .await;
            let found = run(
                &notes,
                json!({
                    "text": "alpha",
                    "k": 5,
                    "vector_weight": 0,
                    "filter": {"term": {"column": "kind", "value": {"text": "y"}}}
                }),
            )
            .await;
            assert_eq!(ids(&found), ["target"]);
            assert!(
                hits(&found)[0]
                    .lexical_score
                    .is_some_and(|score| score > 0.0)
            );
        });
    }

    #[test]
    fn the_weights_are_checked_and_zero_turns_a_leg_off() {
        both(|| async {
            let fx = Fixture::new("search-weights").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            for search in [
                json!({"text": "kitten", "lexical_weight": 0, "vector_weight": 0}),
                json!({"text": "kitten", "lexical_weight": -1}),
                json!({"text": "kitten", "vector_weight": -0.5}),
                json!({}),
                json!({"text": ""}),
            ] {
                let error = refused(&notes, search.clone()).await;
                assert!(error.is(Code::InvalidArgument), "{search}: {error}");
            }
            // With the vector leg far heavier, it decides the order.
            let heavy = run(
                &notes,
                json!({"text": "kitten", "vector_weight": 50, "k": 4}),
            )
            .await;
            let vector_only = run(
                &notes,
                json!({"text": "kitten", "lexical_weight": 0, "k": 4}),
            )
            .await;
            assert_eq!(ids(&heavy)[0], ids(&vector_only)[0]);
        });
    }

    #[test]
    fn the_documents_carry_their_raw_only_when_asked_and_a_deleted_one_is_no_hit() {
        both(|| async {
            let fx = Fixture::new("search-raw").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let bare = hits(&run(&notes, json!({"text": "dog", "k": 1})).await);
            let document = bare[0].document.as_option().unwrap();
            assert_eq!(document.id.as_deref(), Some("dog1"));
            assert_eq!(document.collection.as_deref(), Some("notes"));
            assert!(document.raw.is_none());
            assert!(document.revision.is_some());
            let full =
                hits(&run(&notes, json!({"text": "dog", "k": 1, "include_raw": true})).await);
            let document = full[0].document.as_option().unwrap();
            let raw: Value = serde_json::from_slice(document.raw.as_deref().unwrap()).unwrap();
            assert_eq!(raw["title"], "Dog training");

            notes
                .delete(DocumentDeletion::default().with_id("cat2").with_note("t"))
                .await
                .unwrap();
            wait_indexed(&fx.vault).await;
            let found = ids(&run(&notes, json!({"text": "kitten", "k": 10})).await);
            assert!(!found.contains(&"cat2".to_owned()), "{found:?}");
            assert!(found.contains(&"cat1".to_owned()));

            // A stream dropped early leaves the Vault usable.
            let mut stream = notes
                .search(search_of(json!({"text": "kitten", "include_raw": true})))
                .await
                .unwrap();
            assert!(next(&mut stream).await.is_some());
            assert!(next(&mut stream).await.is_some());
            drop(stream);
            assert_eq!(ids(&run(&notes, json!({"text": "kitten"})).await).len(), 3);
            fx.vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn a_search_given_the_token_of_a_write_finds_it() {
        both(|| async {
            let fx = Fixture::new("search-not-before").await;
            fx.corpus().await;
            let notes = fx.collection("notes").await;
            let token = fx
                .put_unwaited(
                    "notes",
                    "new",
                    &json!({"title": "zeppelin", "topic": "truck"}),
                )
                .await;
            // No sleep: the search waits for the Index to reach the write.
            let found = run(&notes, json!({"text": "zeppelin", "not_before": token})).await;
            assert_eq!(ids(&found)[0], "new");
            let found = run_vault(
                &fx.vault,
                json!([]),
                json!({"text": "zeppelin", "not_before": token}),
            )
            .await;
            let first = hits(&found)[0].document.as_option().unwrap().clone();
            assert_eq!(first.collection.as_deref(), Some("notes"));
            assert_eq!(first.id.as_deref(), Some("new"));
        });
    }

    /// The vector of a point of the recall fixture: a unit vector near the
    /// centre of its cluster, from a hash of its text.
    fn point(text: &str, dimensions: usize) -> Vec<f32> {
        let cluster: u64 = text
            .split('-')
            .next()
            .and_then(|head| head.trim_start_matches('c').parse().ok())
            .unwrap_or(0);
        let mut seed = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 2001) as f32 / 1000.0 - 1.0
        };
        let mut vector: Vec<f32> = (0..dimensions)
            .map(|axis| {
                let centre = if axis as u64 % 40 == cluster {
                    4.0
                } else {
                    0.0
                };
                centre + noise()
            })
            .collect();
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        vector.iter_mut().for_each(|value| *value /= norm);
        vector
    }

    const POINT_DIMENSIONS: usize = 64;

    /// An embedder of the points of the recall fixture.
    struct Points;

    impl EmbedderSync for Points {
        fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(EmbedderDescriptor::default()
                .with_model_id("points@1")
                .with_dimensions(POINT_DIMENSIONS as u32))
        }

        fn embed(
            &self,
            texts: BoxIter<'static, Result<Text, Error>>,
        ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
            Ok(Box::new(texts.map(|text| {
                Ok(Vector {
                    values: point(&text?.text.unwrap_or_default(), POINT_DIMENSIONS),
                    ..Vector::default()
                })
            })))
        }

        fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            Ok(Vector {
                values: point(&query.text.unwrap_or_default(), POINT_DIMENSIONS),
                ..Vector::default()
            })
        }
    }

    fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
        1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
    }

    /// Enough rows for the vector index (10 000), in 40 clusters: a search
    /// through the index finds at least 95 % of the exact 300 nearest, and
    /// a Document written after the index was built is found by its token.
    #[test]
    fn a_search_through_the_vector_index_finds_the_exact_nearest() {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                const ROWS: usize = 12_000;
                let scratch = Scratch::new("search-recall");
                let path = scratch.join("vault");
                crate::common::vault(&path);
                let mut request = opening(&path, options());
                request.projector = ProjectorRef::from_sync(Docs {
                    projection: "docs@1",
                    vectors: true,
                })
                .into();
                request.embedder = EmbedderRef::from_sync(Points).into();
                let vault: DynVaultAsync = Scheda
                    .open(request)
                    .await
                    .unwrap()
                    .to_async_inline()
                    .unwrap();
                let notes = collection(&vault, "notes").await.unwrap();
                let texts: Vec<String> = (0..ROWS).map(|n| format!("c{}-{n}", n % 40)).collect();
                let operations: Vec<DocumentOperation> = texts
                    .iter()
                    .enumerate()
                    .map(|(n, text)| DocumentOperation {
                        operation: Some(Operation::Upsert(Box::new(
                            DocumentUpsert::default()
                                .with_id(format!("p{n:05}"))
                                .with_raw(json!({"topic": text}).to_string().into_bytes())
                                .with_note("test"),
                        ))),
                        ..DocumentOperation::default()
                    })
                    .collect();
                let started = Instant::now();
                notes.batch(stream_of(operations)).await.unwrap();
                wait_indexed(&vault).await;
                eprintln!(
                    "recall fixture: {ROWS} rows indexed in {:?}",
                    started.elapsed()
                );
                let vectors: Vec<Vec<f32>> = texts
                    .iter()
                    .map(|text| point(text, POINT_DIMENSIONS))
                    .collect();
                let mut recall = Vec::new();
                for query in ["c3-q", "c17-q", "c29-q", "c38-q"] {
                    let wanted = point(query, POINT_DIMENSIONS);
                    let mut exact: Vec<(f32, usize)> = vectors
                        .iter()
                        .enumerate()
                        .map(|(n, vector)| (cosine_distance(&wanted, vector), n))
                        .collect();
                    exact.sort_by(|a, b| a.0.total_cmp(&b.0));
                    let exact: std::collections::BTreeSet<String> = exact[..300]
                        .iter()
                        .map(|(_, n)| format!("p{n:05}"))
                        .collect();
                    let found = run(
                        &notes,
                        json!({"text": query, "k": 300, "lexical_weight": 0}),
                    )
                    .await;
                    let found = ids(&found);
                    let shared = found.iter().filter(|id| exact.contains(*id)).count();
                    recall.push(shared as f64 / 300.0);
                }
                eprintln!("recall at k 300: {recall:?}");
                assert!(recall.iter().all(|value| *value >= 0.95), "{recall:?}");

                // A write after the index was built: found through its
                // token, before the index is brought over it.
                let token = notes
                    .upsert(
                        DocumentUpsert::default()
                            .with_id("late")
                            .with_raw(json!({"topic": "c3-q"}).to_string().into_bytes())
                            .with_note("test"),
                    )
                    .await
                    .unwrap()
                    .version
                    .unwrap();
                let found = run(
                    &notes,
                    json!({"text": "c3-q", "k": 1, "lexical_weight": 0, "not_before": token}),
                )
                .await;
                assert_eq!(ids(&found), ["late"]);
                vault.close(Empty::default()).await.unwrap();
            });
    }
}
