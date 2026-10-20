//! A search over Documents with several texts each: a `vector` column of
//! kind `text list`, whose every text has a vector. The vector leg ranks a
//! Document once, by its text closest to the query (for `like`, by its
//! closest pair of texts), and names that text in the hit.

#[cfg(feature = "index")]
mod on_lance {
    use std::collections::{BTreeMap, BTreeSet};
    use std::num::NonZeroUsize;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use scheda::v1::document_operation::Operation;
    use scheda::v1::embedder_ref::EmbedderRef;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::reranker_ref::RerankerRef;
    use scheda::v1::search_item::Item;
    use scheda::v1::{
        CollectionAsync, CollectionDeclaration, CollectionKey, ColumnKind, Document,
        DocumentOperation, DocumentUpsert, DynCollectionAsync, DynVaultAsync, IndexAsync,
        IndexColumn, IndexDoc, IndexSchema, IndexValue, OpenOptions, ProjectorDescriptor,
        ProjectorSync, RawDocument, SchedaAsync, SearchHit, SearchItem, VaultAsync,
        VaultErrorCode as Code, index_value, rebuild,
    };
    use scheda::{Error, FixedReranker, Scheda};
    use serde_json::{Value, json};

    use crate::common::{Scratch, both, collection, items, opening, stream_of};
    use crate::search::on_lance::{POINT_DIMENSIONS, Points, cosine_distance, point};

    const DEADLINE: Duration = Duration::from_secs(120);

    fn column(name: &str, kind: ColumnKind) -> IndexColumn {
        IndexColumn::default().with_name(name).with_kind(kind)
    }

    /// A Projector of JSON Documents. In `notes`: `title` (fts), `kind` (a
    /// scalar index), and `phrasings`, a `vector` column: a `text list` of
    /// every text of the Document's `phrasings`, or with `lists` unset a
    /// `text` of the first of them, under the same projection. In `single`
    /// and `listed`: `phrase`, a `vector` column of the first text, a
    /// `text` in `single` and a `text list` of one in `listed`.
    struct Phrased {
        lists: bool,
    }

    impl ProjectorSync for Phrased {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("phrased@1"))
        }

        fn columns(&self, key: CollectionKey) -> Result<IndexSchema, Error> {
            let text = ColumnKind::COLUMN_KIND_TEXT;
            let list = ColumnKind::COLUMN_KIND_TEXT_LIST;
            let columns = match key.name.as_deref() {
                Some("single") => vec![column("phrase", text).with_vector(true)],
                Some("listed") => vec![column("phrase", list).with_vector(true)],
                _ => vec![
                    column("title", text).with_fts(true),
                    column("kind", text).with_scalar_index(true),
                    column("phrasings", if self.lists { list } else { text }).with_vector(true),
                ],
            };
            Ok(IndexSchema {
                columns,
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let json: Value =
                serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
            let texts: Vec<String> = json
                .get("phrasings")
                .and_then(Value::as_array)
                .map(|texts| {
                    texts
                        .iter()
                        .filter_map(|text| text.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let mut doc = IndexDoc::default();
            let mut put = |name: &str, value: index_value::Value| {
                doc.values.insert(
                    name.to_owned(),
                    IndexValue {
                        value: Some(value),
                        ..IndexValue::default()
                    },
                );
            };
            let list = |texts: Vec<String>| {
                index_value::Value::TextList(Box::new(index_value::TextList {
                    texts,
                    ..index_value::TextList::default()
                }))
            };
            match document.collection.as_deref() {
                Some("single") => {
                    if let Some(first) = texts.first() {
                        put("phrase", index_value::Value::Text(first.clone()));
                    }
                }
                Some("listed") => put("phrase", list(texts.into_iter().take(1).collect())),
                _ => {
                    for name in ["title", "kind"] {
                        if let Some(text) = json.get(name).and_then(Value::as_str) {
                            put(name, index_value::Value::Text(text.to_owned()));
                        }
                    }
                    if self.lists {
                        put("phrasings", list(texts));
                    } else if let Some(first) = texts.first() {
                        put("phrasings", index_value::Value::Text(first.clone()));
                    }
                }
            }
            Ok(doc)
        }
    }

    fn options() -> OpenOptions {
        OpenOptions::default()
            .with_index_poll_ms(10)
            .with_write_timeout_ms(60_000)
    }

    /// The Vault at `path` with the [`Phrased`] Projector and the points
    /// embedder.
    async fn open_phrased(path: &Path, lists: bool) -> DynVaultAsync {
        let mut request = opening(path, options());
        request.projector = ProjectorRef::from_sync(Phrased { lists }).into();
        request.embedder = EmbedderRef::from_sync(Points).into();
        Scheda
            .open(request)
            .await
            .unwrap()
            .to_async_inline()
            .unwrap()
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

    /// Writes the Documents `(id, JSON)` to `collection` in one batch.
    async fn put_all(vault: &DynVaultAsync, collection_name: &str, documents: &[(String, Value)]) {
        let operations: Vec<DocumentOperation> = documents
            .iter()
            .map(|(id, raw)| DocumentOperation {
                operation: Some(Operation::Upsert(Box::new(
                    DocumentUpsert::default()
                        .with_id(id.as_str())
                        .with_raw(raw.to_string().into_bytes())
                        .with_note("test"),
                ))),
                ..DocumentOperation::default()
            })
            .collect();
        collection(vault, collection_name)
            .await
            .unwrap()
            .batch(stream_of(operations))
            .await
            .unwrap();
    }

    /// The Documents of `notes`: `(id, title, kind, texts)`. `d` has a
    /// blank text at place 1, and `e` none at all.
    const NOTES: [(&str, &str, &str, &[&str]); 5] = [
        ("a", "alpha", "x", &["c1-a0", "c2-a1", "c3-a2"]),
        ("b", "bravo", "y", &["c1-b0"]),
        ("c", "charlie", "x", &["c2-c0", "c2-c1"]),
        ("d", "delta", "y", &["c4-d0", "   ", "c5-d2"]),
        ("e", "echo", "x", &[]),
    ];

    /// A Vault with `notes` of [`NOTES`], the Index built.
    async fn phrased_notes(scratch: &Scratch, lists: bool) -> DynVaultAsync {
        let path = scratch.join("vault");
        crate::common::vault(&path);
        let vault = open_phrased(&path, lists).await;
        let documents: Vec<(String, Value)> = NOTES
            .iter()
            .map(|(id, title, kind, texts)| {
                (
                    (*id).to_owned(),
                    json!({"title": title, "kind": kind, "phrasings": texts}),
                )
            })
            .collect();
        put_all(&vault, "notes", &documents).await;
        wait_indexed(&vault).await;
        vault
    }

    async fn run(target: &DynCollectionAsync, search: Value) -> Vec<SearchHit> {
        let stream = target
            .search(serde_json::from_value(search).unwrap())
            .await
            .unwrap();
        let items: Vec<SearchItem> = items(stream)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert!(matches!(
            items.first().and_then(|item| item.item.as_ref()),
            Some(Item::Summary(_))
        ));
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

    fn ids(hits: &[SearchHit]) -> Vec<String> {
        hits.iter().map(id_of).collect()
    }

    /// The column and place of the text a hit names.
    fn matched(hit: &SearchHit) -> (String, u32) {
        let matched = hit.vector_match.as_option().expect("a vector match");
        (
            matched.column.clone().unwrap_or_default(),
            matched.position.unwrap_or_default(),
        )
    }

    /// The texts of the Document `id` of [`NOTES`] that have a vector, with
    /// their places.
    fn texts_of(id: &str) -> Vec<(u32, &'static str)> {
        let (_, _, _, texts) = NOTES.iter().find(|note| note.0 == id).unwrap();
        texts
            .iter()
            .enumerate()
            .filter(|(_, text)| !text.trim().is_empty())
            .map(|(place, text)| (place as u32, *text))
            .collect()
    }

    /// The closest distance from the vectors `of` to a text of `id`, and the
    /// place of that text (the first of equal ones).
    fn closest(of: &[Vec<f32>], id: &str) -> (f32, u32) {
        let mut best = (f32::INFINITY, 0);
        for (place, text) in texts_of(id) {
            let vector = point(text, POINT_DIMENSIONS);
            for query in of {
                let distance = cosine_distance(query, &vector);
                if distance < best.0 {
                    best = (distance, place);
                }
            }
        }
        best
    }

    fn assert_close(found: f64, wanted: f32, what: &str) {
        let wanted = 1.0 / (1.0 + f64::from(wanted));
        assert!(
            (found - wanted).abs() < 1e-4,
            "{what}: vector score {found}, closest text gives {wanted}"
        );
    }

    #[test]
    fn a_document_is_found_by_each_of_its_texts_once_and_scored_by_the_closest() {
        both(|| async {
            let scratch = Scratch::new("texts-closest");
            let vault = phrased_notes(&scratch, true).await;
            let notes = collection(&vault, "notes").await.unwrap();
            for (query, place) in [("c1-a0", 0), ("c2-a1", 1), ("c3-a2", 2)] {
                let hits = run(&notes, json!({"text": query, "lexical_weight": 0})).await;
                let found = ids(&hits);
                assert_eq!(found[0], "a", "{query}: {found:?}");
                assert_eq!(matched(&hits[0]), (String::from("phrasings"), place));
                let distinct: BTreeSet<&String> = found.iter().collect();
                assert_eq!(distinct.len(), found.len(), "each Document once: {found:?}");
                // A Document without a text is no hit of the vector leg.
                assert_eq!(
                    distinct.into_iter().cloned().collect::<Vec<_>>(),
                    ["a", "b", "c", "d"]
                );
                let wanted = vec![point(query, POINT_DIMENSIONS)];
                for hit in &hits {
                    let (distance, place) = closest(&wanted, &id_of(hit));
                    assert_close(hit.vector_score.unwrap(), distance, &id_of(hit));
                    assert_eq!(matched(hit).1, place, "{query}: {}", id_of(hit));
                }
            }
            // The blank text has no vector: the text after it keeps its place.
            let hits = run(
                &notes,
                json!({"text": "c5-d2", "lexical_weight": 0, "k": 1}),
            )
            .await;
            assert_eq!(ids(&hits), ["d"]);
            assert_eq!(matched(&hits[0]), (String::from("phrasings"), 2));
            vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn k_and_offset_count_documents_not_texts() {
        both(|| async {
            let scratch = Scratch::new("texts-pages");
            let vault = phrased_notes(&scratch, true).await;
            let notes = collection(&vault, "notes").await.unwrap();
            // `c` has its two texts in the cluster of the query, `a` one.
            let search = |k: u32, offset: u32| json!({"text": "c2-q", "lexical_weight": 0, "k": k, "offset": offset});
            let whole = ids(&run(&notes, search(4, 0)).await);
            assert_eq!(whole.len(), 4, "{whole:?}");
            let mut first: Vec<&String> = whole[..2].iter().collect();
            first.sort();
            assert_eq!(first, ["a", "c"], "{whole:?}");
            assert_eq!(ids(&run(&notes, search(2, 0)).await), whole[..2]);
            assert_eq!(ids(&run(&notes, search(1, 1)).await), whole[1..2]);
            assert_eq!(ids(&run(&notes, search(2, 2)).await), whole[2..]);
            assert!(ids(&run(&notes, search(2, 4)).await).is_empty());
            vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn the_filter_narrows_the_texts_before_they_rank() {
        both(|| async {
            let scratch = Scratch::new("texts-filter");
            let vault = phrased_notes(&scratch, true).await;
            let notes = collection(&vault, "notes").await.unwrap();
            let filter = json!({"term": {"column": "kind", "value": {"text": "y"}}});
            // The query is a text of `a`, which the filter leaves out.
            let hits = run(
                &notes,
                json!({"text": "c3-a2", "lexical_weight": 0, "k": 2, "filter": filter}),
            )
            .await;
            let mut found = ids(&hits);
            found.sort();
            assert_eq!(found, ["b", "d"]);
            let nothing = json!({"term": {"column": "kind", "value": {"text": "z"}}});
            let hits = run(
                &notes,
                json!({"text": "c3-a2", "lexical_weight": 0, "filter": nothing}),
            )
            .await;
            assert!(hits.is_empty());
            vault.close(Empty::default()).await.unwrap();
        });
    }

    /// What a reranker was asked: the query, and for each Document its id
    /// and whether it had its `raw`.
    type Asked = Arc<Mutex<Vec<(String, Vec<(String, bool)>)>>>;

    #[test]
    fn the_reranker_scores_each_document_once_with_its_bytes() {
        both(|| async {
            let scratch = Scratch::new("texts-rerank");
            let vault = phrased_notes(&scratch, true).await;
            let asked = Asked::default();
            let seen = Arc::clone(&asked);
            let reranker = FixedReranker::new(
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
                    // `d` first, then the order given.
                    let scores = docs
                        .iter()
                        .enumerate()
                        .map(|(n, (id, _))| if id == "d" { 100.0 } else { -(n as f64) })
                        .collect();
                    seen.lock().unwrap().push((query.to_owned(), docs));
                    Ok(scores)
                },
            );
            vault
                .set_reranker(RerankerRef::from_sync(reranker))
                .await
                .unwrap();
            let notes = collection(&vault, "notes").await.unwrap();
            let hits = run(
                &notes,
                json!({"text": "c2-a1", "lexical_weight": 0, "k": 4, "rerank_limit": 4}),
            )
            .await;
            assert_eq!(ids(&hits)[0], "d");
            let asked = asked.lock().unwrap().clone();
            let documents: Vec<(String, bool)> = asked
                .iter()
                .flat_map(|(query, docs)| {
                    assert_eq!(query, "c2-a1");
                    docs.clone()
                })
                .collect();
            let names: BTreeSet<&String> = documents.iter().map(|(id, _)| id).collect();
            assert_eq!(documents.len(), 4, "each Document once: {documents:?}");
            assert_eq!(names.len(), 4);
            assert!(documents.iter().all(|(_, raw)| *raw), "with its bytes");
            vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn like_ranks_the_others_by_their_closest_pair_of_texts() {
        both(|| async {
            let scratch = Scratch::new("texts-like");
            let vault = phrased_notes(&scratch, true).await;
            let notes = collection(&vault, "notes").await.unwrap();
            let hits = run(
                &notes,
                json!({"like": {"collection": "notes", "id": "a"}, "lexical_weight": 0}),
            )
            .await;
            let found = ids(&hits);
            assert!(!found.contains(&String::from("a")), "not its own hit");
            assert_eq!(found.len(), 3, "{found:?}");
            let of_a: Vec<Vec<f32>> = texts_of("a")
                .into_iter()
                .map(|(_, text)| point(text, POINT_DIMENSIONS))
                .collect();
            let mut wanted: Vec<(f32, String)> = ["b", "c", "d"]
                .into_iter()
                .map(|id| (closest(&of_a, id).0, id.to_owned()))
                .collect();
            wanted.sort_by(|x, y| x.0.total_cmp(&y.0));
            assert_eq!(
                found,
                wanted.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>()
            );
            for hit in &hits {
                let (distance, place) = closest(&of_a, &id_of(hit));
                assert_close(hit.vector_score.unwrap(), distance, &id_of(hit));
                assert_eq!(matched(hit), (String::from("phrasings"), place));
            }
            vault.close(Empty::default()).await.unwrap();
        });
    }

    /// The vector score, the fused score, and the text named of a hit.
    type Scored = (Option<f64>, Option<f64>, (String, u32));

    #[test]
    fn a_list_of_one_text_ranks_and_scores_as_a_text_column_of_that_text() {
        both(|| async {
            let scratch = Scratch::new("texts-one");
            let path = scratch.join("vault");
            crate::common::vault(&path);
            let vault = open_phrased(&path, true).await;
            let documents: Vec<(String, Value)> = (0..30)
                .map(|n| {
                    (
                        format!("p{n:02}"),
                        json!({"phrasings": [format!("c{}-p{n}", n % 5), "c9-second"]}),
                    )
                })
                .collect();
            for name in ["single", "listed"] {
                let declaration: CollectionDeclaration =
                    serde_json::from_value(json!({"name": name, "schema": {"standard": {}}}))
                        .unwrap();
                vault.create_collection(declaration).await.unwrap();
                put_all(&vault, name, &documents).await;
            }
            wait_indexed(&vault).await;
            let single = collection(&vault, "single").await.unwrap();
            let listed = collection(&vault, "listed").await.unwrap();
            for query in ["c1-q", "c3-q", "c9-second"] {
                let search = json!({"text": query, "k": 30});
                let one = run(&single, search.clone()).await;
                let many = run(&listed, search).await;
                assert_eq!(ids(&one), ids(&many), "{query}");
                assert_eq!(ids(&one).len(), 30);
                let scores = |hits: &[SearchHit]| -> Vec<Scored> {
                    hits.iter()
                        .map(|hit| (hit.vector_score, hit.score, matched(hit)))
                        .collect()
                };
                assert_eq!(scores(&one), scores(&many), "{query}");
                assert!(
                    scores(&one)
                        .iter()
                        .all(|(_, _, matched)| *matched == (String::from("phrase"), 0))
                );
            }
            vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn turning_a_text_column_into_a_text_list_makes_the_index_stale_until_a_reindex() {
        both(|| async {
            let scratch = Scratch::new("texts-stale");
            let vault = phrased_notes(&scratch, false).await;
            let notes = collection(&vault, "notes").await.unwrap();
            let search = json!({"text": "c3-a2", "lexical_weight": 0, "k": 1});
            // A `text`: only the first text of each Document has a vector.
            let hits = run(&notes, json!({"text": "c3-a2", "lexical_weight": 0})).await;
            let a = hits.iter().find(|hit| id_of(hit) == "a").unwrap();
            assert_eq!(matched(a), (String::from("phrasings"), 0));
            let (distance, _) = closest(&[point("c3-a2", POINT_DIMENSIONS)], "a");
            assert!(a.vector_score.unwrap() < 1.0 / (1.0 + f64::from(distance)) - 0.01);
            vault.close(Empty::default()).await.unwrap();

            let path = scratch.join("vault");
            let vault = open_phrased(&path, true).await;
            let notes = collection(&vault, "notes").await.unwrap();
            let error = notes
                .search(serde_json::from_value(search.clone()).unwrap())
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::IndexStale), "{error}");
            let index = vault
                .index(Empty::default())
                .await
                .unwrap()
                .to_async_inline()
                .unwrap();
            index.reindex(Empty::default()).await.unwrap();
            let deadline = Instant::now() + DEADLINE;
            loop {
                let state = index.status(Empty::default()).await.unwrap();
                let rebuild = state.rebuild.as_option();
                let stage = rebuild
                    .and_then(|rebuild| rebuild.stage)
                    .and_then(|stage| stage.as_known());
                assert_ne!(stage, Some(rebuild::Stage::STAGE_FAILED), "{state:?}");
                if stage == Some(rebuild::Stage::STAGE_DONE) {
                    break;
                }
                assert!(Instant::now() < deadline, "not rebuilt: {state:?}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let hits = run(&notes, search).await;
            assert_eq!(ids(&hits), ["a"]);
            assert_eq!(matched(&hits[0]), (String::from("phrasings"), 2));
            vault.close(Empty::default()).await.unwrap();
        });
    }

    /// Enough texts for the vector index of the texts (10 000), eight per
    /// Document, 40 Documents in each of 40 clusters: a search through that
    /// index gives Documents, each once, `k` of them though the texts
    /// nearest the query belong to fewer (the 51 nearest texts lie in one
    /// cluster of 40 Documents), with at least 90 % of the exact nearest
    /// Documents by their closest text; a filter keeps it to the Documents
    /// it admits.
    #[test]
    fn a_search_through_the_index_of_the_texts_gives_k_documents_each_once() {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                const DOCUMENTS: usize = 1_600;
                const TEXTS: usize = 8;
                let scratch = Scratch::new("texts-index");
                let path = scratch.join("vault");
                crate::common::vault(&path);
                let vault = open_phrased(&path, true).await;
                // Half the Documents of each cluster: those of its first
                // twenty.
                let first_half = |n: usize| (n / 40).is_multiple_of(2);
                let texts = |n: usize| -> Vec<String> {
                    (0..TEXTS)
                        .map(|t| format!("c{}-d{n}-t{t}", n % 40))
                        .collect()
                };
                let documents: Vec<(String, Value)> = (0..DOCUMENTS)
                    .map(|n| {
                        let kind = if first_half(n) { "first" } else { "second" };
                        (
                            format!("d{n:04}"),
                            json!({"kind": kind, "phrasings": texts(n)}),
                        )
                    })
                    .collect();
                let started = Instant::now();
                put_all(&vault, "notes", &documents).await;
                wait_indexed(&vault).await;
                eprintln!(
                    "texts fixture: {} texts indexed in {:?}",
                    DOCUMENTS * TEXTS,
                    started.elapsed()
                );
                let indexed = std::fs::read_dir(path.join(".scheda/index"))
                    .unwrap()
                    .filter_map(|entry| {
                        let dir = entry.unwrap().path();
                        let name = dir.file_name()?.to_str()?.to_owned();
                        name.starts_with("g-").then_some(dir)
                    })
                    .flat_map(|generation| std::fs::read_dir(generation).unwrap())
                    .map(|entry| entry.unwrap().path())
                    .filter(|dir| dir.to_string_lossy().ends_with(".texts.phrasings.lance"))
                    .any(|dir| {
                        std::fs::read_dir(dir.join("_indices"))
                            .is_ok_and(|mut indices| indices.next().is_some())
                    });
                assert!(indexed, "the texts have their vector index");

                let vectors: BTreeMap<String, Vec<Vec<f32>>> = (0..DOCUMENTS)
                    .map(|n| {
                        (
                            format!("d{n:04}"),
                            texts(n)
                                .iter()
                                .map(|text| point(text, POINT_DIMENSIONS))
                                .collect(),
                        )
                    })
                    .collect();
                let notes = collection(&vault, "notes").await.unwrap();
                for (query, k, filter) in [
                    ("c3-q", 40, None),
                    ("c17-q", 40, None),
                    ("c29-q", 20, Some("first")),
                ] {
                    let wanted = point(query, POINT_DIMENSIONS);
                    let mut exact: Vec<(f32, &String)> = vectors
                        .iter()
                        .filter(|(id, _)| {
                            let n: usize = id[1..].parse().unwrap();
                            filter.is_none_or(|kind| (kind == "first") == first_half(n))
                        })
                        .map(|(id, texts)| {
                            let distance = texts
                                .iter()
                                .map(|vector| cosine_distance(&wanted, vector))
                                .fold(f32::INFINITY, f32::min);
                            (distance, id)
                        })
                        .collect();
                    exact.sort_by(|a, b| a.0.total_cmp(&b.0));
                    let exact: BTreeSet<&String> = exact[..k].iter().map(|(_, id)| *id).collect();
                    let mut search = json!({"text": query, "k": k, "lexical_weight": 0});
                    if let Some(kind) = filter {
                        search["filter"] =
                            json!({"term": {"column": "kind", "value": {"text": kind}}});
                    }
                    let started = Instant::now();
                    let hits = run(&notes, search).await;
                    let took = started.elapsed();
                    let found = ids(&hits);
                    let distinct: BTreeSet<&String> = found.iter().collect();
                    assert_eq!(found.len(), k, "{query}: k Documents");
                    assert_eq!(distinct.len(), k, "{query}: each once");
                    let shared = distinct.iter().filter(|id| exact.contains(*id)).count();
                    let recall = shared as f64 / k as f64;
                    eprintln!("{query} k {k} filter {filter:?}: recall {recall}, {took:?}");
                    assert!(recall >= 0.9, "{query}: recall {recall}");
                    if let Some(kind) = filter {
                        let first = kind == "first";
                        assert!(found.iter().all(|id| {
                            let n: usize = id[1..].parse().unwrap();
                            first_half(n) == first
                        }));
                    }
                }
                vault.close(Empty::default()).await.unwrap();
            });
    }
}
