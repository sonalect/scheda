//! The queries of the contract: `Collection.Query` and `Collection.Count`,
//! `Vault.QueryDocuments` and `Vault.CountDocuments`, over the Index on
//! Lance with a Projector of JSON Documents; and `not_before`. A build
//! without the Index answers all four `UNIMPLEMENTED`, and so does
//! `Vault.Index`.

use buffa_types::google::protobuf::Empty;
use scheda::Error;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, DocumentQuery, VaultAsync, VaultErrorCode as Code,
    VaultQuery,
};

use crate::common::{Scratch, both, collection, vault as built_vault};

fn query(json: serde_json::Value) -> DocumentQuery {
    serde_json::from_value(json).unwrap()
}

fn vault_query(json: serde_json::Value) -> VaultQuery {
    serde_json::from_value(json).unwrap()
}

#[cfg(not(feature = "index"))]
#[test]
fn without_the_index_the_four_calls_and_the_index_object_are_unimplemented() {
    use crate::common::{open, snapshot};

    both(|| async {
        let scratch = Scratch::new("query-unimplemented");
        let path = scratch.join("vault");
        built_vault(&path);
        let vault = open(&path, Default::default()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let before = snapshot(&path);
        let unimplemented = |error: Error| assert!(error.is(Code::Unimplemented), "{error}");
        unimplemented(
            notes
                .query(query(serde_json::json!({})))
                .await
                .err()
                .unwrap(),
        );
        unimplemented(notes.count(query(serde_json::json!({}))).await.unwrap_err());
        unimplemented(
            vault
                .query_documents(vault_query(serde_json::json!({})))
                .await
                .err()
                .unwrap(),
        );
        unimplemented(
            vault
                .count_documents(vault_query(serde_json::json!({})))
                .await
                .unwrap_err(),
        );
        unimplemented(vault.index(Empty::default()).await.err().unwrap());
        assert_eq!(snapshot(&path), before, "a refused query writes nothing");
        let declaration: CollectionDeclaration = serde_json::from_value(
            serde_json::json!({"name": "later", "schema": {"standard": {}}}),
        )
        .unwrap();
        vault.create_collection(declaration).await.unwrap();
    });
}

#[cfg(feature = "index")]
mod on_lance {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::time::Duration;

    use scheda::v1::document_operation::Operation;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::query_item::Item;
    use scheda::v1::{
        CollectionKey, ColumnKind, DocumentOperation, DocumentUpsert, DynCollectionAsync,
        DynVaultAsync, IndexAsync, IndexColumn, IndexDoc, IndexSchema, IndexValue, OpenOptions,
        ProjectorDescriptor, ProjectorSync, QueryItem, RawDocument, index_value,
    };
    use scheda::{Scheda, v1::SchedaAsync};

    use super::*;
    use crate::common::{git, items, opening, stream_of};

    /// A Projector of JSON Documents: `title` (text, scalar, fts), `n`
    /// (integer, scalar), `score` (number), `done` (boolean), `tags` (text
    /// list). The collection `odd` declares `n` as a text.
    struct Json {
        projection: &'static str,
    }

    fn column(name: &str, kind: ColumnKind) -> IndexColumn {
        IndexColumn::default().with_name(name).with_kind(kind)
    }

    impl ProjectorSync for Json {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id(self.projection))
        }

        fn columns(&self, key: CollectionKey) -> Result<IndexSchema, Error> {
            let n = if key.name.as_deref() == Some("odd") {
                ColumnKind::COLUMN_KIND_TEXT
            } else {
                ColumnKind::COLUMN_KIND_INTEGER
            };
            Ok(IndexSchema {
                columns: vec![
                    column("title", ColumnKind::COLUMN_KIND_TEXT)
                        .with_scalar_index(true)
                        .with_fts(true),
                    column("n", n).with_scalar_index(true),
                    column("score", ColumnKind::COLUMN_KIND_NUMBER),
                    column("done", ColumnKind::COLUMN_KIND_BOOLEAN),
                    column("tags", ColumnKind::COLUMN_KIND_TEXT_LIST),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let json: serde_json::Value =
                serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
            let mut values = std::collections::HashMap::new();
            let Some(object) = json.as_object() else {
                return Ok(IndexDoc::default());
            };
            for (key, value) in object {
                let value = match value {
                    serde_json::Value::String(text) => index_value::Value::Text(text.clone()),
                    serde_json::Value::Bool(flag) => index_value::Value::Boolean(*flag),
                    serde_json::Value::Number(number) if number.is_i64() => {
                        index_value::Value::Integer(number.as_i64().unwrap())
                    }
                    serde_json::Value::Number(number) => {
                        index_value::Value::Number(number.as_f64().unwrap())
                    }
                    serde_json::Value::Array(texts) => {
                        index_value::Value::TextList(Box::new(index_value::TextList {
                            texts: texts
                                .iter()
                                .filter_map(|text| text.as_str().map(str::to_owned))
                                .collect(),
                            ..index_value::TextList::default()
                        }))
                    }
                    _ => continue,
                };
                values.insert(
                    key.clone(),
                    IndexValue {
                        value: Some(value),
                        ..IndexValue::default()
                    },
                );
            }
            Ok(IndexDoc {
                values: values.into_iter().collect(),
                ..IndexDoc::default()
            })
        }
    }

    async fn open_json(
        path: &Path,
        options: OpenOptions,
        projection: &'static str,
    ) -> DynVaultAsync {
        let mut request = opening(path, options);
        request.projector = ProjectorRef::from_sync(Json { projection }).into();
        Scheda
            .open(request)
            .await
            .unwrap()
            .to_async_inline()
            .unwrap()
    }

    fn options() -> OpenOptions {
        OpenOptions::default().with_index_poll_ms(10)
    }

    /// Waits until the Index reflects the stored state now.
    async fn wait_indexed(vault: &DynVaultAsync) {
        let index = vault
            .index(Empty::default())
            .await
            .unwrap()
            .to_async_inline()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let head = vault.info(Empty::default()).await.unwrap().version;
            let state = index.status(Empty::default()).await.unwrap();
            if state.version.is_some() && state.version == head {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "not indexed: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn upsert(id: &str, json: serde_json::Value) -> DocumentUpsert {
        DocumentUpsert::default()
            .with_id(id)
            .with_raw(json.to_string().into_bytes())
            .with_note("test")
    }

    /// The Vault with `notes` of six Documents, indexed.
    async fn fixture(scratch: &Scratch) -> (std::path::PathBuf, DynVaultAsync, DynCollectionAsync) {
        let path = scratch.join("vault");
        built_vault(&path);
        let vault = open_json(&path, options(), "json@1").await;
        let notes = collection(&vault, "notes").await.unwrap();
        let documents = [
            (
                "a.md",
                serde_json::json!({"title": "alpha", "n": 1, "score": 0.5, "done": true, "tags": ["x", "y"]}),
            ),
            (
                "b.md",
                serde_json::json!({"title": "beta", "n": 2, "done": false, "tags": ["y"]}),
            ),
            (
                "c.md",
                serde_json::json!({"title": "gamma", "n": 3, "score": 2.5, "tags": []}),
            ),
            (
                "d.md",
                serde_json::json!({"title": "delta", "n": 4, "score": 1.0}),
            ),
            ("e.md", serde_json::json!({"title": "alps", "n": 5})),
            ("f.md", serde_json::json!({"n": 6, "done": true})),
        ];
        let operations: Vec<DocumentOperation> = documents
            .into_iter()
            .map(|(id, json)| DocumentOperation {
                operation: Some(Operation::Upsert(Box::new(upsert(id, json)))),
                ..DocumentOperation::default()
            })
            .collect();
        notes.batch(stream_of(operations)).await.unwrap();
        wait_indexed(&vault).await;
        (path, vault, notes)
    }

    /// The ids of a query's Documents, in order, and its total.
    async fn ids(notes: &DynCollectionAsync, json: serde_json::Value) -> (Vec<String>, u64) {
        let stream = notes.query(query(json)).await.unwrap();
        let mut total = None;
        let mut ids = Vec::new();
        for item in items(stream).await {
            match item.unwrap().item {
                Some(Item::Summary(summary)) => total = summary.total,
                Some(Item::Document(document)) => ids.push(document.id.unwrap()),
                None => {}
            }
        }
        (ids, total.unwrap_or_default())
    }

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn each_kind_of_leaf_selects_the_exact_set_and_none_of_counts_a_missing_value_as_false() {
        both(|| async {
            let scratch = Scratch::new("query-leaves");
            let (_path, _vault, notes) = fixture(&scratch).await;
            let cases = [
                (
                    serde_json::json!({"term": {"column": "n", "value": {"integer": 3}}}),
                    set(&["c.md"]),
                ),
                (
                    serde_json::json!({"term": {"column": "id", "value": {"text": "b.md"}}}),
                    set(&["b.md"]),
                ),
                (
                    serde_json::json!({"terms": {"column": "n", "values": [{"integer": 1}, {"integer": 5}]}}),
                    set(&["a.md", "e.md"]),
                ),
                (
                    serde_json::json!({"range": {"column": "n", "lower": {"value": {"integer": 2}, "inclusive": true}, "upper": {"value": {"integer": 4}}}}),
                    set(&["b.md", "c.md"]),
                ),
                (
                    serde_json::json!({"range": {"column": "score", "lower": {"value": {"number": 1.0}}}}),
                    set(&["c.md"]),
                ),
                (
                    serde_json::json!({"range": {"column": "title", "upper": {"value": {"text": "b"}}}}),
                    set(&["a.md", "e.md"]),
                ),
                (
                    serde_json::json!({"exists": {"column": "score"}}),
                    set(&["a.md", "c.md", "d.md"]),
                ),
                (
                    serde_json::json!({"noneOf": {"filters": [{"exists": {"column": "score"}}]}}),
                    set(&["b.md", "e.md", "f.md"]),
                ),
                (
                    serde_json::json!({"term": {"column": "done", "value": {"boolean": true}}}),
                    set(&["a.md", "f.md"]),
                ),
                (
                    serde_json::json!({"noneOf": {"filters": [{"term": {"column": "done", "value": {"boolean": true}}}]}}),
                    set(&["b.md", "c.md", "d.md", "e.md"]),
                ),
                (
                    serde_json::json!({"prefix": {"column": "title", "prefix": "al"}}),
                    set(&["a.md", "e.md"]),
                ),
                (
                    serde_json::json!({"prefix": {"column": "id", "prefix": "a"}}),
                    set(&["a.md"]),
                ),
                (
                    serde_json::json!({"term": {"column": "tags", "value": {"text": "y"}}}),
                    set(&["a.md", "b.md"]),
                ),
                (
                    serde_json::json!({"terms": {"column": "tags", "values": [{"text": "x"}, {"text": "z"}]}}),
                    set(&["a.md"]),
                ),
                (
                    serde_json::json!({"exists": {"column": "tags"}}),
                    set(&["a.md", "b.md"]),
                ),
                (
                    serde_json::json!({"noneOf": {"filters": [{"term": {"column": "tags", "value": {"text": "y"}}}]}}),
                    set(&["c.md", "d.md", "e.md", "f.md"]),
                ),
                (
                    serde_json::json!({"allOf": {"filters": [{"exists": {"column": "title"}}, {"range": {"column": "n", "lower": {"value": {"integer": 3}}}}]}}),
                    set(&["d.md", "e.md"]),
                ),
                (
                    serde_json::json!({"anyOf": {"filters": [{"term": {"column": "n", "value": {"integer": 1}}}, {"term": {"column": "title", "value": {"text": "delta"}}}]}}),
                    set(&["a.md", "d.md"]),
                ),
            ];
            for (filter, want) in cases {
                let (got, total) = ids(&notes, serde_json::json!({"filter": filter})).await;
                let got: BTreeSet<String> = got.into_iter().collect();
                assert_eq!(got, want, "{filter}");
                assert_eq!(total, want.len() as u64, "{filter}");
                let count = notes
                    .count(query(serde_json::json!({"filter": filter})))
                    .await
                    .unwrap();
                assert_eq!(count.count, Some(want.len() as u64), "{filter}");
            }
        });
    }

    #[test]
    fn the_order_is_stable_and_paging_with_start_after_neither_repeats_nor_skips() {
        both(|| async {
            let scratch = Scratch::new("query-order");
            let (_path, _vault, notes) = fixture(&scratch).await;
            let (all, total) = ids(&notes, serde_json::json!({})).await;
            assert_eq!(all, ["a.md", "b.md", "c.md", "d.md", "e.md", "f.md"]);
            assert_eq!(total, 6);
            let (by_score, _) = ids(
                &notes,
                serde_json::json!({"orderBy": [{"column": "score", "descending": true}]}),
            )
            .await;
            // Largest first; a Document without a value comes last, by id.
            assert_eq!(by_score, ["c.md", "d.md", "a.md", "b.md", "e.md", "f.md"]);
            for order in [
                serde_json::json!([]),
                serde_json::json!([{"column": "score", "descending": true}]),
                serde_json::json!([{"column": "done"}, {"column": "n", "descending": true}]),
            ] {
                let (whole, _) = ids(&notes, serde_json::json!({"orderBy": order})).await;
                let mut paged = Vec::new();
                let mut after: Option<String> = None;
                loop {
                    let mut request = serde_json::json!({"orderBy": order, "limit": 2});
                    if let Some(after) = &after {
                        request["startAfter"] = serde_json::json!(after);
                    }
                    let (page, total) = ids(&notes, request).await;
                    assert_eq!(total, 6, "the total ignores the page");
                    if page.is_empty() {
                        break;
                    }
                    after = page.last().cloned();
                    paged.extend(page);
                }
                assert_eq!(paged, whole, "{order}");
                let (offset, _) =
                    ids(&notes, serde_json::json!({"orderBy": order, "offset": 4})).await;
                assert_eq!(offset, whole[4..], "{order}");
            }
        });
    }

    #[test]
    fn limit_and_offset_at_their_maximum_answer_with_the_documents_there_are() {
        both(|| async {
            let scratch = Scratch::new("query-maximum");
            let (_path, _vault, notes) = fixture(&scratch).await;
            let (whole, total) = ids(&notes, serde_json::json!({})).await;
            assert_eq!(total, 6);
            let max = u32::MAX;
            for order in [
                serde_json::json!([]),
                serde_json::json!([{"column": "score", "descending": true}]),
            ] {
                let (ordered, _) = ids(&notes, serde_json::json!({"orderBy": order})).await;
                let (all, total) =
                    ids(&notes, serde_json::json!({"orderBy": order, "limit": max})).await;
                assert_eq!((all, total), (ordered.clone(), 6), "{order}");
                let (all, _) = ids(
                    &notes,
                    serde_json::json!({"orderBy": order, "limit": max, "offset": 2}),
                )
                .await;
                assert_eq!(all, ordered[2..], "{order}");
                let (none, total) = ids(
                    &notes,
                    serde_json::json!({"orderBy": order, "limit": max, "offset": max}),
                )
                .await;
                assert_eq!((none.len(), total), (0, 6), "{order}");
                let (none, _) =
                    ids(&notes, serde_json::json!({"orderBy": order, "offset": max})).await;
                assert!(none.is_empty(), "{order}");
            }
            assert_eq!(whole.len(), 6);
        });
    }

    #[test]
    fn a_column_no_collection_declares_a_value_of_another_kind_and_a_deep_filter_are_invalid() {
        both(|| async {
            let scratch = Scratch::new("query-invalid");
            let path = scratch.join("vault");
            built_vault(&path);
            let vault = open_json(&path, options().with_filter_max_depth(2), "json@1").await;
            let notes = collection(&vault, "notes").await.unwrap();
            wait_indexed(&vault).await;
            for filter in [
                serde_json::json!({"term": {"column": "nothing", "value": {"text": "a"}}}),
                serde_json::json!({"term": {"column": "n", "value": {"text": "1"}}}),
                serde_json::json!({"range": {"column": "tags", "lower": {"value": {"text": "a"}}}}),
                serde_json::json!({"allOf": {"filters": [{"anyOf": {"filters": [{"exists": {"column": "n"}}]}}]}}),
            ] {
                let error = notes
                    .query(query(serde_json::json!({"filter": filter})))
                    .await
                    .err()
                    .unwrap();
                assert!(error.is(Code::InvalidArgument), "{filter}: {error}");
            }
            let error = notes
                .query(query(serde_json::json!({"orderBy": [{"column": "tags"}]})))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::InvalidArgument), "{error}");
            let error = notes
                .query(query(
                    serde_json::json!({"startAfter": "zzz.md", "orderBy": [{"column": "n"}]}),
                ))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::FailedPrecondition), "{error}");
        });
    }

    #[test]
    fn a_filter_of_depth_100_runs_on_a_two_megabyte_stack() {
        let scratch = Scratch::new("query-deep");
        let path = scratch.join("vault");
        built_vault(&path);
        let mut filter = serde_json::json!({"term": {"column": "n", "value": {"integer": 1}}});
        for depth in 1..100 {
            let list = if depth % 2 == 0 { "allOf" } else { "anyOf" };
            filter = serde_json::json!({list: {"filters": [filter]}});
        }
        let thread = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let vault =
                            open_json(&path, options().with_filter_max_depth(100), "json@1").await;
                        let notes = collection(&vault, "notes").await.unwrap();
                        notes
                            .upsert(upsert("a.md", serde_json::json!({"n": 1})))
                            .await
                            .unwrap();
                        wait_indexed(&vault).await;
                        let (got, total) = ids(&notes, serde_json::json!({"filter": filter})).await;
                        assert_eq!(got, ["a.md"]);
                        assert_eq!(total, 1);
                        vault.close(Empty::default()).await.unwrap();
                    });
            })
            .unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn a_vault_query_orders_by_its_keys_then_the_collection_and_pages_across_collections() {
        both(|| async {
            let scratch = Scratch::new("query-vault");
            let (_path, vault, _notes) = fixture(&scratch).await;
            let declaration: CollectionDeclaration = serde_json::from_value(
                serde_json::json!({"name": "more", "schema": {"standard": {}}}),
            )
            .unwrap();
            vault.create_collection(declaration).await.unwrap();
            let more = collection(&vault, "more").await.unwrap();
            more.upsert(upsert("a.md", serde_json::json!({"n": 2})))
                .await
                .unwrap();
            more.upsert(upsert("z.md", serde_json::json!({"n": 9})))
                .await
                .unwrap();
            wait_indexed(&vault).await;
            let keys = |items: Vec<Result<QueryItem, Error>>| -> (Vec<(String, String)>, u64) {
                let mut total = 0;
                let mut keys = Vec::new();
                for item in items {
                    match item.unwrap().item {
                        Some(Item::Summary(summary)) => total = summary.total.unwrap_or_default(),
                        Some(Item::Document(document)) => {
                            keys.push((document.collection.unwrap(), document.id.unwrap()))
                        }
                        None => {}
                    }
                }
                (keys, total)
            };
            let request = serde_json::json!({
                "filter": {"range": {"column": "n", "lower": {"value": {"integer": 2}, "inclusive": true}, "upper": {"value": {"integer": 3}, "inclusive": true}}},
                "orderBy": [{"column": "n"}],
            });
            let (got, total) =
                keys(items(vault.query_documents(vault_query(request)).await.unwrap()).await);
            assert_eq!(
                got,
                [
                    ("more".to_owned(), "a.md".to_owned()),
                    ("notes".to_owned(), "b.md".to_owned()),
                    ("notes".to_owned(), "c.md".to_owned()),
                ]
            );
            assert_eq!(total, 3);
            let (whole, total) = keys(
                items(
                    vault
                        .query_documents(vault_query(serde_json::json!({})))
                        .await
                        .unwrap(),
                )
                .await,
            );
            assert_eq!(total, 8);
            let mut paged = Vec::new();
            let mut after: Option<(String, String)> = None;
            loop {
                let mut request = serde_json::json!({"limit": 3});
                if let Some((collection, id)) = &after {
                    request["startAfter"] = serde_json::json!({"collection": collection, "id": id});
                }
                let (page, _) =
                    keys(items(vault.query_documents(vault_query(request)).await.unwrap()).await);
                if page.is_empty() {
                    break;
                }
                after = page.last().cloned();
                paged.extend(page);
            }
            assert_eq!(paged, whole);
            let count = vault
                .count_documents(vault_query(
                    serde_json::json!({"collections": {"names": ["more"]}}),
                ))
                .await
                .unwrap();
            assert_eq!(count.count, Some(2));
            // `n` is a text in `odd`: an order on it across both is invalid.
            let declaration: CollectionDeclaration = serde_json::from_value(
                serde_json::json!({"name": "odd", "schema": {"standard": {}}}),
            )
            .unwrap();
            vault.create_collection(declaration).await.unwrap();
            let error = vault
                .query_documents(vault_query(
                    serde_json::json!({"orderBy": [{"column": "n"}]}),
                ))
                .await
                .err()
                .unwrap();
            assert!(error.is(Code::InvalidArgument), "{error}");
        });
    }

    #[test]
    fn a_query_waits_for_its_write_with_not_before_and_a_reset_commit_does_not_wait() {
        both(|| async {
            let scratch = Scratch::new("query-not-before");
            let (path, _vault, notes) = fixture(&scratch).await;
            let written = notes
                .upsert(upsert("g.md", serde_json::json!({"n": 7})))
                .await
                .unwrap();
            let token = written.version.clone().unwrap();
            // No sleep: the call waits for the Index to reach the write.
            let (got, _) = ids(
                &notes,
                serde_json::json!({"notBefore": token, "filter": {"term": {"column": "n", "value": {"integer": 7}}}}),
            )
            .await;
            assert_eq!(got, ["g.md"]);
            // A commit a reset removed from the history does not wait.
            let removed = notes
                .upsert(upsert("h.md", serde_json::json!({"n": 8})))
                .await
                .unwrap()
                .version
                .unwrap();
            git(&path, &["reset", "--quiet", "--hard", "HEAD~1"]);
            let count = notes
                .count(query(serde_json::json!({"notBefore": removed})))
                .await
                .unwrap();
            assert!(count.count.is_some());
            let error = notes
                .count(query(serde_json::json!({"notBefore": "not a token"})))
                .await
                .unwrap_err();
            assert!(error.is(Code::InvalidArgument), "{error}");
        });
    }

    #[test]
    fn a_wait_past_its_deadline_is_deadline_exceeded() {
        both(|| async {
            let scratch = Scratch::new("query-deadline");
            let path = scratch.join("vault");
            built_vault(&path);
            let vault = open_json(&path, options().with_index_wait_timeout_ms(300), "json@1").await;
            let notes = collection(&vault, "notes").await.unwrap();
            wait_indexed(&vault).await;
            // Another indexer holds the lock: the Index stays where it is.
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path.join(".scheda/index.lock"))
                .unwrap();
            let lock = tokio::task::spawn_blocking(move || {
                lock.lock().unwrap();
                lock
            })
            .await
            .unwrap();
            let token = notes
                .upsert(upsert("a.md", serde_json::json!({"n": 1})))
                .await
                .unwrap()
                .version
                .unwrap();
            let error = notes
                .count(query(serde_json::json!({"notBefore": token})))
                .await
                .unwrap_err();
            assert!(error.is(Code::DeadlineExceeded), "{error}");
            // A collection the Index has not reached is empty, not invalid.
            let declaration: CollectionDeclaration = serde_json::from_value(
                serde_json::json!({"name": "later", "schema": {"standard": {}}}),
            )
            .unwrap();
            vault.create_collection(declaration).await.unwrap();
            let later = collection(&vault, "later").await.unwrap();
            let (got, total) = ids(&later, serde_json::json!({})).await;
            assert!(got.is_empty());
            assert_eq!(total, 0);
            drop(lock);
            vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn an_index_never_built_is_rebuilding_and_one_of_another_projection_is_stale() {
        both(|| async {
            let scratch = Scratch::new("query-stale");
            let path = scratch.join("vault");
            built_vault(&path);
            std::fs::create_dir_all(path.join(".scheda")).unwrap();
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path.join(".scheda/index.lock"))
                .unwrap();
            lock.lock().unwrap();
            let vault = open_json(&path, options(), "json@1").await;
            let notes = collection(&vault, "notes").await.unwrap();
            let error = notes.count(query(serde_json::json!({}))).await.unwrap_err();
            assert!(error.is(Code::IndexRebuilding), "{error}");
            drop(lock);
            wait_indexed(&vault).await;
            let other = open_json(&path, options(), "json@2").await;
            let error = collection(&other, "notes")
                .await
                .unwrap()
                .count(query(serde_json::json!({})))
                .await
                .unwrap_err();
            assert!(error.is(Code::IndexStale), "{error}");
            other.close(Empty::default()).await.unwrap();
            vault.close(Empty::default()).await.unwrap();
        });
    }

    #[test]
    fn an_isolated_collection_is_scanned_only_when_named_and_one_without_an_index_is_refused() {
        both(|| async {
            let scratch = Scratch::new("query-isolated-named");
            let (_path, vault, _notes) = fixture(&scratch).await;
            for (name, schema) in [
                (
                    "secret",
                    serde_json::json!({"standard": {}, "isolated": true}),
                ),
                (
                    "bare",
                    serde_json::json!({"standard": {}, "excludeLexicalIndex": true}),
                ),
            ] {
                let declaration: CollectionDeclaration =
                    serde_json::from_value(serde_json::json!({"name": name, "schema": schema}))
                        .unwrap();
                vault.create_collection(declaration).await.unwrap();
            }
            collection(&vault, "secret")
                .await
                .unwrap()
                .upsert(upsert(
                    "s.md",
                    serde_json::json!({"n": 7, "title": "hidden"}),
                ))
                .await
                .unwrap();
            wait_indexed(&vault).await;
            let everywhere = serde_json::json!({"orderBy": [{"column": "n"}]});
            let collections = |request: serde_json::Value| async {
                let stream = vault.query_documents(vault_query(request)).await?;
                let mut names = std::collections::BTreeSet::new();
                for item in items(stream).await {
                    if let Some(Item::Document(document)) = item?.item {
                        names.insert(document.collection.unwrap());
                    }
                }
                Ok::<_, Error>(names)
            };
            // No name: every collection that is not isolated.
            let found = collections(everywhere.clone()).await.unwrap();
            assert_eq!(found, set(&["notes"]));
            // Named: the isolated one is read.
            let named = serde_json::json!({
                "collections": {"names": ["secret"]},
                "orderBy": [{"column": "n"}]
            });
            assert_eq!(collections(named).await.unwrap(), set(&["secret"]));
            // A collection that leaves its columns out of the Index is
            // refused when named and asked for a filter.
            let filtered = serde_json::json!({
                "collections": {"names": ["bare"]},
                "filter": {"exists": {"column": "n"}}
            });
            let error = collections(filtered).await.unwrap_err();
            assert!(
                error.is(scheda::v1::VaultErrorCode::Unimplemented),
                "{error}"
            );
        });
    }
}
