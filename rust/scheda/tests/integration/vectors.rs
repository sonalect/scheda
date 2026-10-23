//! The vectors of the Index as a caller reaches them: `Collection.GetVectors`
//! reads back the stored vectors of a column, a search takes a vector as
//! its subject, and an `exact` search, or one bounded by
//! `min_vector_score`, ranks by a full scan in a repeatable order.

#[cfg(not(feature = "index"))]
#[test]
fn without_the_index_get_vectors_is_unimplemented() {
    use scheda::v1::{CollectionAsync, VaultErrorCode, VectorSelection};

    use crate::common::{Scratch, both, collection, open, vault};

    both(|| async {
        let scratch = Scratch::new("vectors-thin");
        let path = scratch.join("vault");
        vault(&path);
        let vault = open(&path, Default::default()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let error = notes
            .get_vectors(VectorSelection {
                ids: vec![String::from("a")],
                ..VectorSelection::default().with_column("topic")
            })
            .await
            .err()
            .unwrap();
        assert!(error.is(VaultErrorCode::Unimplemented), "{error}");
    });
}

#[cfg(feature = "index")]
mod on_lance {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use git2::{ObjectType, Oid};
    use scheda::v1::document_operation::Operation;
    use scheda::v1::embedder_ref::EmbedderRef;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::rebuild;
    use scheda::v1::search_item::Item;
    use scheda::v1::search_summary::VectorLeg;
    use scheda::v1::vector_item::Item as VectorArm;
    use scheda::v1::{
        CollectionAsync, CollectionDeclaration, CollectionKey, ColumnKind, DocumentOperation,
        DocumentUpsert, DynCollectionAsync, DynVaultAsync, IndexAsync, IndexColumn, IndexDoc,
        IndexSchema, IndexState, IndexValue, OpenOptions, ProjectorDescriptor, ProjectorSync,
        RawDocument, SchedaAsync, SearchHit, SearchItem, VaultAsync, VaultErrorCode as Code,
        VectorItem, VectorSelection, index_value,
    };
    use scheda::{Error, Scheda};
    use serde_json::{Value, json};

    use crate::common::{Scratch, collection, items, opening, stream_of};
    use crate::search::on_lance::{POINT_DIMENSIONS, Points, point};

    const DEADLINE: Duration = Duration::from_secs(300);

    fn column(name: &str, kind: ColumnKind) -> IndexColumn {
        IndexColumn::default().with_name(name).with_kind(kind)
    }

    /// A Projector of JSON Documents: `kind` (a scalar index), `topic` (a
    /// `vector` column of kind `text`), and `phrasings` (a `vector` column
    /// of kind `text list`).
    struct Topics;

    impl ProjectorSync for Topics {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("topics@1"))
        }

        fn columns(&self, _key: CollectionKey) -> Result<IndexSchema, Error> {
            let text = ColumnKind::COLUMN_KIND_TEXT;
            Ok(IndexSchema {
                columns: vec![
                    column("kind", text).with_scalar_index(true),
                    column("topic", text).with_vector(true),
                    column("phrasings", ColumnKind::COLUMN_KIND_TEXT_LIST).with_vector(true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let json: Value =
                serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
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
            for name in ["kind", "topic"] {
                if let Some(text) = json.get(name).and_then(Value::as_str) {
                    put(name, index_value::Value::Text(text.to_owned()));
                }
            }
            if let Some(texts) = json.get("phrasings").and_then(Value::as_array) {
                put(
                    "phrasings",
                    index_value::Value::TextList(Box::new(index_value::TextList {
                        texts: texts
                            .iter()
                            .filter_map(|text| text.as_str().map(str::to_owned))
                            .collect(),
                        ..index_value::TextList::default()
                    })),
                );
            }
            Ok(doc)
        }
    }

    fn options() -> OpenOptions {
        OpenOptions::default()
            .with_index_poll_ms(10)
            .with_write_timeout_ms(60_000)
    }

    /// The Vault at `path` with the [`Topics`] Projector, and the points
    /// embedder unless `embedder` is unset.
    async fn open_topics(path: &Path, embedder: bool) -> DynVaultAsync {
        let mut request = opening(path, options());
        request.projector = ProjectorRef::from_sync(Topics).into();
        if embedder {
            request.embedder = EmbedderRef::from_sync(Points).into();
        }
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

    /// Asks for a rebuild of the Index of `vault` and waits until it is done
    /// at the stored state now.
    async fn reindex(vault: &DynVaultAsync) {
        let index = vault
            .index(Empty::default())
            .await
            .unwrap()
            .to_async_inline()
            .unwrap();
        index.reindex(Empty::default()).await.unwrap();
        let head = vault.info(Empty::default()).await.unwrap().version;
        let deadline = Instant::now() + DEADLINE;
        loop {
            let state: IndexState = index.status(Empty::default()).await.unwrap();
            let rebuild = state.rebuild.as_option();
            let done = rebuild
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known())
                == Some(rebuild::Stage::STAGE_DONE);
            let asked = rebuild
                .and_then(|rebuild| rebuild.cause)
                .and_then(|cause| cause.as_known())
                == Some(rebuild::Cause::CAUSE_REINDEX);
            if done && asked && state.version == head {
                return;
            }
            assert!(Instant::now() < deadline, "not rebuilt: {state:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Writes the Documents `(id, JSON)` to `notes` in one batch.
    async fn put_all(notes: &DynCollectionAsync, documents: &[(String, Value)]) {
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
        notes.batch(stream_of(operations)).await.unwrap();
    }

    /// The git blob id of `text`: the digest a stored vector carries.
    fn digest(text: &str) -> String {
        Oid::hash_object(ObjectType::Blob, text.as_bytes())
            .unwrap()
            .to_string()
    }

    fn selection(column: &str, ids: &[&str]) -> VectorSelection {
        VectorSelection {
            ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            ..VectorSelection::default().with_column(column)
        }
    }

    /// One item of `GetVectors`, short: `(id, position, digest, values)`,
    /// or `(id, None…)` for an absent one.
    type Got = (String, Option<u32>, Option<String>, Vec<f32>);

    fn got(item: VectorItem) -> Got {
        match item.item {
            Some(VectorArm::Vector(vector)) => (
                vector.id.unwrap_or_default(),
                vector.position,
                vector.digest,
                vector.values,
            ),
            Some(VectorArm::Absent(id)) => (id, None, None, Vec::new()),
            None => panic!("an item without its arm"),
        }
    }

    async fn vectors_of(
        notes: &DynCollectionAsync,
        selection: VectorSelection,
    ) -> Result<Vec<Got>, Error> {
        let stream = notes.get_vectors(selection).await?;
        items(stream)
            .await
            .into_iter()
            .map(|item| item.map(got))
            .collect()
    }

    fn vector(id: &str, position: u32, text: &str) -> Got {
        (
            id.to_owned(),
            Some(position),
            Some(digest(text)),
            point(text, POINT_DIMENSIONS),
        )
    }

    fn absent(id: &str) -> Got {
        (id.to_owned(), None, None, Vec::new())
    }

    /// A Vault with `notes` holding `a`, `b`, `c`, and the collection
    /// `plain`, which keeps no vectors; the Index built.
    async fn small(scratch: &Scratch) -> DynVaultAsync {
        let path = scratch.join("vault");
        crate::common::vault(&path);
        let vault = open_topics(&path, true).await;
        let plain: CollectionDeclaration = serde_json::from_value(json!({
            "name": "plain",
            "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_NO_VECTORS"}
        }))
        .unwrap();
        vault.create_collection(plain).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        put_all(
            &notes,
            &[
                (
                    String::from("a"),
                    json!({"topic": "c1-a", "phrasings": ["c2-a0", "   ", "c3-a2"]}),
                ),
                (String::from("b"), json!({"topic": "c1-b", "phrasings": []})),
                (String::from("c"), json!({"kind": "no text"})),
            ],
        )
        .await;
        wait_indexed(&vault).await;
        vault
    }

    fn multi_thread() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn stored_vectors_come_back_in_the_order_of_the_ids_with_the_digest_of_their_text() {
        multi_thread().block_on(async {
            let scratch = Scratch::new("vectors-read");
            let vault = small(&scratch).await;
            let notes = collection(&vault, "notes").await.unwrap();

            assert_eq!(
                vectors_of(&notes, selection("topic", &["b", "zz", "a", "c", "b"]))
                    .await
                    .unwrap(),
                [
                    vector("b", 0, "c1-b"),
                    absent("zz"),
                    vector("a", 0, "c1-a"),
                    absent("c"),
                    vector("b", 0, "c1-b"),
                ]
            );
            // A blank text has no vector, and no item; a Document whose
            // texts have none is absent.
            assert_eq!(
                vectors_of(&notes, selection("phrasings", &["c", "a", "b"]))
                    .await
                    .unwrap(),
                [
                    absent("c"),
                    vector("a", 0, "c2-a0"),
                    vector("a", 2, "c3-a2"),
                    absent("b"),
                ]
            );
            // Many portions keep the order of the ids.
            let many: Vec<String> = (0..300)
                .map(|n| ["a", "b", "x"][n % 3].to_owned())
                .collect();
            let ids: Vec<&str> = many.iter().map(String::as_str).collect();
            let read = vectors_of(&notes, selection("topic", &ids)).await.unwrap();
            assert_eq!(read.len(), 300);
            for (n, item) in read.iter().enumerate() {
                assert_eq!(item.0, many[n], "item {n}");
            }

            for (bad, code) in [
                (selection("kind", &["a"]), Code::InvalidArgument),
                (selection("nothing", &["a"]), Code::InvalidArgument),
                (selection("topic", &[]), Code::InvalidArgument),
                (
                    VectorSelection {
                        ids: vec![String::from("a")],
                        ..VectorSelection::default()
                    },
                    Code::InvalidArgument,
                ),
            ] {
                let error = vectors_of(&notes, bad).await.err().unwrap();
                assert!(error.is(code), "{error}");
            }
            let plain = collection(&vault, "plain").await.unwrap();
            let error = vectors_of(&plain, selection("topic", &["a"]))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::Unimplemented), "{error}");
            vault.close(Empty::default()).await.unwrap();
        });
    }

    /// The mean of `vectors`.
    fn mean(vectors: &[Vec<f32>]) -> Vec<f32> {
        let mut sum = vec![0.0_f32; vectors[0].len()];
        for vector in vectors {
            for (total, value) in sum.iter_mut().zip(vector) {
                *total += value;
            }
        }
        sum.iter()
            .map(|total| total / vectors.len() as f32)
            .collect()
    }

    async fn search(notes: &DynCollectionAsync, search: Value) -> Result<Vec<SearchItem>, Error> {
        let stream = notes
            .search(serde_json::from_value(search).unwrap())
            .await?;
        Ok(items(stream)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect())
    }

    fn leg(items: &[SearchItem]) -> VectorLeg {
        match items.first().and_then(|item| item.item.as_ref()) {
            Some(Item::Summary(summary)) => summary
                .vector_leg
                .as_ref()
                .and_then(|leg| leg.as_known())
                .unwrap(),
            other => panic!("the first item is not the summary: {other:?}"),
        }
    }

    fn hits(items: &[SearchItem]) -> Vec<SearchHit> {
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

    /// The ids and vector scores of the hits.
    fn ranked(items: &[SearchItem]) -> Vec<(String, f64)> {
        hits(items)
            .iter()
            .map(|hit| (id_of(hit), hit.vector_score.unwrap_or_default()))
            .collect()
    }

    /// Cosine distance in `f64`, of vectors of any length.
    fn distance(a: &[f32], b: &[f32]) -> f64 {
        let dot: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| f64::from(*x) * f64::from(*y))
            .sum();
        let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        1.0 - dot / (norm(a) * norm(b))
    }

    /// Lance computes in `f32`: distances this close are one rank.
    const SAME: f64 = 1e-5;

    /// Checks that `found` (id, vector score) is `wanted`, the brute force
    /// (distance, id) ordered by distance and then id, from its start: the
    /// same distance at each rank, the score of that distance, and of ids
    /// whose vectors are equal, `group` of each, the smaller first.
    fn assert_brute_force(
        found: &[(String, f64)],
        wanted: &[(f64, String)],
        group: impl Fn(&str) -> u64,
    ) {
        assert!(found.len() <= wanted.len());
        let distances: BTreeMap<&str, f64> =
            wanted.iter().map(|(d, id)| (id.as_str(), *d)).collect();
        for (rank, (id, score)) in found.iter().enumerate() {
            let d = distances[id.as_str()];
            assert!(
                (d - wanted[rank].0).abs() < SAME,
                "rank {rank}: {id} at {d}, brute force {} at {}",
                wanted[rank].1,
                wanted[rank].0
            );
            assert!(
                (score - 1.0 / (1.0 + d)).abs() < SAME,
                "rank {rank}: {id} scored {score} for distance {d}"
            );
        }
        // Equal vectors: every one with a smaller id comes before.
        let at: BTreeMap<&str, usize> = found
            .iter()
            .enumerate()
            .map(|(rank, (id, _))| (id.as_str(), rank))
            .collect();
        for (rank, (id, _)) in found.iter().enumerate() {
            for (_, other) in wanted
                .iter()
                .filter(|(_, other)| other.as_str() < id.as_str() && group(other) == group(id))
            {
                assert!(
                    at.get(other.as_str()).is_some_and(|before| *before < rank),
                    "{other} ties with {id} and has the smaller id: it comes first ({found:?})"
                );
            }
        }
    }

    #[test]
    fn a_search_by_a_vector_ranks_as_the_brute_force_and_needs_no_embedder() {
        multi_thread().block_on(async {
            let scratch = Scratch::new("vectors-subject");
            let path = scratch.join("vault");
            crate::common::vault(&path);
            let vault = open_topics(&path, true).await;
            let notes = collection(&vault, "notes").await.unwrap();
            let text = |n: usize| format!("c{}-t{n}", n % 7);
            let documents: Vec<(String, Value)> = (0..400)
                .map(|n| {
                    (
                        format!("p{n:03}"),
                        json!({"topic": text(n), "phrasings": [text(n), format!("c{}-u{n}", n % 5)]}),
                    )
                })
                .collect();
            put_all(&notes, &documents).await;
            wait_indexed(&vault).await;

            // The mean of three stored vectors, read back.
            let three = vectors_of(&notes, selection("topic", &["p003", "p010", "p017"]))
                .await
                .unwrap();
            let query = mean(&three.into_iter().map(|item| item.3).collect::<Vec<_>>());
            let mut wanted: Vec<(f64, String)> = (0..400)
                .map(|n| {
                    (
                        distance(&query, &point(&text(n), POINT_DIMENSIONS)),
                        format!("p{n:03}"),
                    )
                })
                .collect();
            wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let group = |id: &str| id[1..].parse::<u64>().unwrap() * 1000;
            for exact in [false, true] {
                let found = search(
                    &notes,
                    json!({"vector": {"column": "topic", "values": query}, "k": 20, "exact": exact}),
                )
                .await
                .unwrap();
                assert_eq!(leg(&found), VectorLeg::VECTOR_LEG_RAN);
                let found = ranked(&found);
                assert_eq!(found.len(), 20);
                assert_brute_force(&found, &wanted, group);
            }

            // A `text list` column: each Document by its closest text.
            let mut by_list: Vec<(f64, String)> = (0..400)
                .map(|n| {
                    let closest = [text(n), format!("c{}-u{n}", n % 5)]
                        .iter()
                        .map(|t| distance(&query, &point(t, POINT_DIMENSIONS)))
                        .fold(f64::INFINITY, f64::min);
                    (closest, format!("p{n:03}"))
                })
                .collect();
            by_list.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let found = search(
                &notes,
                json!({"vector": {"column": "phrasings", "values": query}, "k": 15, "exact": true}),
            )
            .await
            .unwrap();
            assert_brute_force(&ranked(&found), &by_list, group);

            for bad in [
                json!({"vector": {"column": "topic", "values": [0.5, 0.5]}}),
                json!({"vector": {"column": "topic", "values": vec![0.0; POINT_DIMENSIONS]}}),
                json!({"vector": {"column": "kind", "values": query}}),
                json!({"vector": {"column": "topic", "values": query}, "vector_weight": 0}),
                json!({"vector": {"column": "topic", "values": query}, "rerank_limit": 20}),
                json!({"vector": {"column": "topic", "values": []}}),
            ] {
                let error = search(&notes, bad.clone()).await.err().unwrap();
                assert!(error.is(Code::InvalidArgument), "{bad}: {error}");
            }

            // A Vault without an embedder reads the Index all the same, and
            // a vector needs no model.
            vault.close(Empty::default()).await.unwrap();
            let vault = open_topics(&path, false).await;
            let notes = collection(&vault, "notes").await.unwrap();
            let found = search(
                &notes,
                json!({"vector": {"column": "topic", "values": query}, "k": 20, "exact": true}),
            )
            .await
            .unwrap();
            assert_eq!(leg(&found), VectorLeg::VECTOR_LEG_RAN);
            assert_brute_force(&ranked(&found), &wanted, group);
            let found = search(&notes, json!({"text": "c3-t3", "k": 5})).await.unwrap();
            assert_eq!(leg(&found), VectorLeg::VECTOR_LEG_NO_EMBEDDER);
            vault.close(Empty::default()).await.unwrap();
        });
    }

    /// Just past the vector index's threshold (10 000 rows), so the table
    /// has the index an exact search must not use; in groups of four
    /// Documents with the same text, so equal distances abound.
    const ROWS: usize = 10_400;
    const GROUP: usize = 4;

    fn grouped(n: usize) -> String {
        let group = n / GROUP;
        format!("c{}-g{group}", group % 40)
    }

    #[test]
    fn an_exact_search_is_the_brute_force_ties_by_id_and_stays_so_after_writes_and_a_rebuild() {
        multi_thread().block_on(async {
            let scratch = Scratch::new("vectors-exact");
            let path = scratch.join("vault");
            crate::common::vault(&path);
            let vault = open_topics(&path, true).await;
            let notes = collection(&vault, "notes").await.unwrap();
            let documents: Vec<(String, Value)> = (0..ROWS)
                .map(|n| {
                    (
                        format!("p{n:05}"),
                        json!({"kind": "first", "topic": grouped(n)}),
                    )
                })
                .collect();
            let started = Instant::now();
            put_all(&notes, &documents).await;
            wait_indexed(&vault).await;
            eprintln!(
                "exact fixture: {ROWS} rows indexed in {:?}",
                started.elapsed()
            );

            let query = point("c7-q", POINT_DIMENSIONS);
            let mut wanted: Vec<(f64, String)> = (0..ROWS)
                .map(|n| {
                    (
                        distance(&query, &point(&grouped(n), POINT_DIMENSIONS)),
                        format!("p{n:05}"),
                    )
                })
                .collect();
            wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let group = |id: &str| (id[1..].parse::<usize>().unwrap() / GROUP) as u64;
            // A page that ends inside a group: the leg's last row ties with
            // rows it did not take.
            let ask = json!({
                "vector": {"column": "topic", "values": query}, "k": 53, "exact": true
            });
            let first = ranked(&search(&notes, ask.clone()).await.unwrap());
            assert_eq!(first.len(), 53);
            assert_brute_force(&first, &wanted, group);

            // Rewrite the first two Documents of each group near the query,
            // so Lance stores them after the two others.
            let near: BTreeSet<usize> = wanted[..400]
                .iter()
                .map(|(_, id)| id[1..].parse::<usize>().unwrap())
                .filter(|n| n % GROUP < 2)
                .collect();
            let mut rewrites: Vec<(String, Value)> = near
                .iter()
                .map(|n| {
                    (
                        format!("p{n:05}"),
                        json!({"kind": "second", "topic": grouped(*n)}),
                    )
                })
                .collect();
            // And more, far from it, to 1 000 writes.
            let far: Vec<usize> = (0..ROWS)
                .rev()
                .filter(|n| !near.contains(n))
                .take(1_000 - rewrites.len())
                .collect();
            rewrites.extend(far.into_iter().map(|n| {
                (
                    format!("p{n:05}"),
                    json!({"kind": "second", "topic": grouped(n)}),
                )
            }));
            put_all(&notes, &rewrites).await;
            wait_indexed(&vault).await;
            assert_eq!(
                ranked(&search(&notes, ask.clone()).await.unwrap()),
                first,
                "the same hits after the writes"
            );
            reindex(&vault).await;
            assert_eq!(
                ranked(&search(&notes, ask.clone()).await.unwrap()),
                first,
                "the same hits after a rebuild"
            );

            // Every Document at least this close, page by page: exactly the
            // brute force's, between two groups so no rank is in doubt.
            let at = |rank: usize| 1.0 / (1.0 + wanted[rank].0);
            let bound = (at(GROUP * 9 - 1) + at(GROUP * 9)) / 2.0;
            let above: Vec<(f64, String)> = wanted
                .iter()
                .filter(|(d, _)| 1.0 / (1.0 + d) >= bound)
                .cloned()
                .collect();
            assert_eq!(above.len(), GROUP * 9);
            let mut paged = Vec::new();
            for offset in (0..).step_by(10) {
                let page = ranked(
                    &search(
                        &notes,
                        json!({
                            "vector": {"column": "topic", "values": query},
                            "k": 10, "offset": offset, "exact": true, "min_vector_score": bound
                        }),
                    )
                    .await
                    .unwrap(),
                );
                let last = page.len() < 10;
                paged.extend(page);
                if last {
                    break;
                }
            }
            assert_eq!(paged.len(), above.len(), "{paged:?}");
            assert_brute_force(&paged, &above, group);
            // Through the vector index a bound is refused: Lance would
            // compare it with the index's own distances.
            let error = search(
                &notes,
                json!({"text": "c7-q", "lexical_weight": 0, "min_vector_score": bound}),
            )
            .await
            .err()
            .unwrap();
            assert!(error.is(Code::InvalidArgument), "{error}");
            vault.close(Empty::default()).await.unwrap();
        });
    }
}
