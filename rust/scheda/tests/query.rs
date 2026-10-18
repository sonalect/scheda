//! The queries of the contract: `Collection.Query` and `Collection.Count`,
//! `Vault.QueryDocuments` and `Vault.CountDocuments`. A query selects the
//! exact set of Documents a Filter matches, in a stable order, as a lazy
//! stream read from one snapshot of the Index.
//!
//! What counts blob reads (a `Count` opens no file, a stream opens a file
//! as it reaches the Document) and what stops a write half way is only
//! reachable from inside the library: those are unit tests in
//! `src/queries.rs`. A build without `find` answers all four calls with
//! `UNIMPLEMENTED`.

mod common;

#[cfg(not(feature = "find"))]
mod thin {
    use buffa_types::google::protobuf::Empty;
    use scheda::v1::{
        CollectionDeclaration, CollectionKey, DocumentQuery, Feature, SchedaSync, VaultErrorCode,
        VaultQuery,
    };
    use scheda::{Error, Scheda};

    use crate::common::{Scratch, open, snapshot, vault as built_vault};

    fn query() -> DocumentQuery {
        serde_json::from_value(serde_json::json!({"filter": {"all": {}}})).unwrap()
    }

    fn vault_query() -> VaultQuery {
        serde_json::from_value(serde_json::json!({"filter": {"all": {}}})).unwrap()
    }

    /// `UNIMPLEMENTED`, and the message names the feature.
    fn names_find(error: &Error) -> bool {
        error.is(VaultErrorCode::Unimplemented) && error.to_string().contains("`find`")
    }

    #[test]
    fn without_find_the_four_calls_are_unimplemented_naming_the_feature() {
        let scratch = Scratch::new("query-thin");
        let path = scratch.join("vault");
        built_vault(&path);
        let vault = open(&path, Default::default()).unwrap();
        let notes = vault
            .get_collection(CollectionKey::default().with_name("notes"))
            .unwrap()
            .to_sync()
            .unwrap();
        let before = snapshot(&path);

        assert!(names_find(&notes.query(query()).err().unwrap()));
        assert!(names_find(&notes.count(query()).unwrap_err()));
        assert!(names_find(
            &vault.query_documents(vault_query()).err().unwrap()
        ));
        assert!(names_find(
            &vault.count_documents(vault_query()).unwrap_err()
        ));
        assert!(names_find(&vault.index(Empty::default()).err().unwrap()));
        assert_eq!(snapshot(&path), before, "a refused query writes nothing");

        // A collection declared meanwhile is refused the same way.
        let declaration: CollectionDeclaration = serde_json::from_value(
            serde_json::json!({"name": "later", "schema": {"standard": {}}}),
        )
        .unwrap();
        vault.create_collection(declaration).unwrap();
    }

    #[test]
    fn without_find_scheda_info_lists_no_find() {
        let info = Scheda.info(Empty::default()).unwrap();
        let features: Vec<Feature> = info
            .features
            .iter()
            .filter_map(|feature| feature.as_known())
            .collect();
        assert!(!features.contains(&Feature::FEATURE_FIND));
    }
}

#[cfg(feature = "find")]
mod find {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use protocontract::BoxIter;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::{
        CollectionDeclaration, CollectionKey, CollectionSync, ColumnKind, Document,
        DocumentCreation, DocumentDeletion, DocumentQuery, DocumentUpsert, Feature, IndexColumn,
        IndexDoc, IndexSchema, IndexSync, IndexValue, OpenOptions, ProjectorDescriptor,
        ProjectorSync, RawDocument, SchedaSync, VaultErrorCode as Code, VaultOpening, VaultQuery,
        VaultSync, index_value,
    };
    use scheda::{Error, Scheda};
    use serde_json::{Value, json};

    use crate::common::{Scratch, command, git, opening, snapshot, write};

    const DEADLINE: Duration = Duration::from_secs(60);

    fn column(name: &str, kind: ColumnKind, find: bool, fts: bool) -> IndexColumn {
        IndexColumn::default()
            .with_name(name)
            .with_kind(kind)
            .with_find(find)
            .with_fts(fts)
    }

    /// The columns of `collection`. `ints`, `nums`, and `texts` have one
    /// column `n` of the kind their name says; the others have a title, a
    /// tag, a body that is searched but cannot be filtered on, a count, a
    /// list of tags, and a flag. `notes` gets one more column once `extra`
    /// is set.
    fn columns_of(collection: &str, extra: bool) -> Vec<IndexColumn> {
        match collection {
            "ints" => vec![column("n", ColumnKind::COLUMN_KIND_INTEGER, true, false)],
            "nums" => vec![column("n", ColumnKind::COLUMN_KIND_NUMBER, true, false)],
            "texts" => vec![column("n", ColumnKind::COLUMN_KIND_TEXT, true, false)],
            _ => {
                let mut columns = vec![
                    column("title", ColumnKind::COLUMN_KIND_TEXT, true, true),
                    column("tag", ColumnKind::COLUMN_KIND_TEXT, true, false),
                    column("body", ColumnKind::COLUMN_KIND_TEXT, false, true),
                    column("count", ColumnKind::COLUMN_KIND_INTEGER, true, false),
                    column("tags", ColumnKind::COLUMN_KIND_TEXT_LIST, true, false),
                    column("flag", ColumnKind::COLUMN_KIND_BOOLEAN, true, false),
                ];
                if extra && collection == "notes" {
                    columns.push(column("extra", ColumnKind::COLUMN_KIND_TEXT, true, false));
                }
                columns
            }
        }
    }

    /// A Projector of JSON Documents: each member of the object whose name
    /// is a column is the value of that column.
    struct Json {
        projection: &'static str,
        extra: Arc<AtomicBool>,
    }

    impl Json {
        fn new(projection: &'static str) -> Json {
            Json {
                projection,
                extra: Arc::default(),
            }
        }
    }

    impl ProjectorSync for Json {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id(self.projection))
        }

        fn columns(&self, collection: CollectionKey) -> Result<IndexSchema, Error> {
            let name = collection.name.unwrap_or_default();
            Ok(IndexSchema {
                columns: columns_of(&name, self.extra.load(Ordering::SeqCst)),
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let collection = document.collection.unwrap_or_default();
            let json: Value =
                serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
            let mut doc = IndexDoc::default();
            for column in columns_of(&collection, true) {
                let name = column.name.clone().unwrap_or_default();
                let Some(value) = json.get(&name) else {
                    continue;
                };
                let value = match value {
                    Value::Null => index_value::Value::from(index_value::Null::default()),
                    Value::String(text) => index_value::Value::Text(text.clone()),
                    Value::Bool(b) => index_value::Value::Boolean(*b),
                    Value::Number(n)
                        if column.kind.as_ref().and_then(|kind| kind.as_known())
                            == Some(ColumnKind::COLUMN_KIND_TEXT) =>
                    {
                        index_value::Value::Text(n.to_string())
                    }
                    Value::Number(n) if n.is_i64() => {
                        index_value::Value::Integer(n.as_i64().unwrap_or(0))
                    }
                    Value::Number(n) => index_value::Value::Number(n.as_f64().unwrap_or(0.0)),
                    Value::Array(items) => index_value::Value::from(index_value::TextList {
                        texts: items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_owned))
                            .collect(),
                        ..Default::default()
                    }),
                    Value::Object(_) => continue,
                };
                doc.values.insert(
                    name,
                    IndexValue {
                        value: Some(value),
                        ..Default::default()
                    },
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

    fn open_with(path: &Path, projector: Json) -> Arc<dyn VaultSync> {
        let request = VaultOpening {
            projector: ProjectorRef::from_sync(projector).into(),
            ..opening(path, options())
        };
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

    /// A Vault opened with the JSON Projector, in a scratch directory.
    struct Fixture {
        _scratch: Scratch,
        path: PathBuf,
        vault: Arc<dyn VaultSync>,
        extra: Arc<AtomicBool>,
    }

    impl Fixture {
        /// A Vault with the given collections declared, the Index built.
        fn new(name: &str, collections: &[Value]) -> Fixture {
            let scratch = Scratch::new(name);
            let path = scratch.join("vault");
            let projector = Json::new("json@1");
            let extra = Arc::clone(&projector.extra);
            let vault = open_with(&path, projector);
            for value in collections {
                vault.create_collection(declaration(value.clone())).unwrap();
            }
            let path = path.canonicalize().unwrap();
            let fixture = Fixture {
                _scratch: scratch,
                path,
                vault,
                extra,
            };
            fixture.wait();
            fixture
        }

        /// Waits until the Index reflects `HEAD`.
        fn wait(&self) {
            wait_indexed(&self.vault, &self.path);
        }

        fn collection(&self, name: &str) -> Arc<dyn CollectionSync> {
            collection(&self.vault, name)
        }

        /// Writes the Document `id` of `collection` (appends a line to a
        /// journal) and waits for the Index.
        fn put(&self, collection: &str, id: &str, raw: &Value) {
            self.put_unwaited(collection, id, raw);
            self.wait();
        }

        fn put_unwaited(&self, collection: &str, id: &str, raw: &Value) {
            let bytes = serde_json::to_vec(raw).unwrap();
            let target = self.collection(collection);
            let is_journal = self.is_journal(collection);
            if is_journal {
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

        fn is_journal(&self, collection: &str) -> bool {
            let declared = self
                .vault
                .get_collection(CollectionKey::default().with_name(collection))
                .unwrap()
                .to_sync()
                .unwrap()
                .describe(Empty::default())
                .unwrap();
            declared.schema.as_option().is_some_and(|schema| {
                matches!(schema.kind, Some(scheda::v1::schema::Kind::Journal(_)))
            })
        }

        fn remove(&self, collection: &str, id: &str) {
            self.collection(collection)
                .delete(DocumentDeletion::default().with_id(id).with_note("test"))
                .unwrap();
            self.wait();
        }

        /// Commits `count` files `{prefix}{n:04}` of `collection` with git
        /// itself, as one commit, and waits for the Index.
        fn bulk(&self, collection: &str, prefix: &str, count: usize) {
            for n in 0..count {
                write(
                    &self.path,
                    &format!("{collection}/{prefix}{n:04}"),
                    &format!("{{\"title\":\"bulk {n}\"}}"),
                );
            }
            git(&self.path, &["add", "--all"]);
            git(&self.path, &["commit", "--quiet", "--message", "bulk"]);
            self.wait();
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
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A `DocumentQuery` of `json` with the filter `all` unless it says
    /// another.
    fn dq(json: Value) -> DocumentQuery {
        serde_json::from_value(json).unwrap()
    }

    fn filtered(filter: Value) -> DocumentQuery {
        dq(json!({ "filter": filter }))
    }

    fn all() -> DocumentQuery {
        filtered(json!({"all": {}}))
    }

    fn vq(json: Value) -> VaultQuery {
        serde_json::from_value(json).unwrap()
    }

    fn documents(stream: BoxIter<'static, Result<Document, Error>>) -> Vec<Document> {
        stream.map(|item| item.unwrap()).collect()
    }

    fn ids(stream: BoxIter<'static, Result<Document, Error>>) -> Vec<String> {
        documents(stream)
            .into_iter()
            .map(|document| document.id.unwrap_or_default())
            .collect()
    }

    fn keys(stream: BoxIter<'static, Result<Document, Error>>) -> Vec<(String, String)> {
        documents(stream)
            .into_iter()
            .map(|document| {
                (
                    document.collection.unwrap_or_default(),
                    document.id.unwrap_or_default(),
                )
            })
            .collect()
    }

    fn found(target: &Arc<dyn CollectionSync>, query: DocumentQuery) -> Vec<String> {
        ids(target.query(query).unwrap())
    }

    fn sorted(mut ids: Vec<String>) -> Vec<String> {
        ids.sort_unstable();
        ids
    }

    fn count(target: &Arc<dyn CollectionSync>, query: DocumentQuery) -> u64 {
        target.count(query).unwrap().count.unwrap_or(0)
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    fn notes() -> Value {
        json!({"name": "notes", "schema": {"standard": {}}})
    }

    fn pages() -> Value {
        json!({"name": "pages", "schema": {"standard": {}}})
    }

    fn log() -> Value {
        json!({"name": "log", "schema": {"journal": {"segment_max_lines": 2}}})
    }

    #[test]
    fn scheda_info_lists_find() {
        let info = Scheda.info(Empty::default()).unwrap();
        assert!(
            info.features
                .iter()
                .any(|feature| feature.as_known() == Some(Feature::FEATURE_FIND))
        );
    }

    #[test]
    fn a_query_gives_every_document_in_id_order_with_keys_only_unless_raw_is_asked_for() {
        let fx = Fixture::new("query-all", &[notes()]);
        for id in ["c", "a", "b"] {
            fx.put("notes", id, &json!({"title": id}));
        }
        let target = fx.collection("notes");
        let before = snapshot(&fx.path);
        let listed = documents(target.query(all()).unwrap());
        let order: Vec<_> = listed.iter().map(|d| d.id.clone().unwrap()).collect();
        assert_eq!(order, ["a", "b", "c"]);
        for document in &listed {
            assert_eq!(document.collection.as_deref(), Some("notes"));
            assert!(document.raw.is_none());
            assert!(document.revision.is_some());
        }
        let with_raw = documents(
            target
                .query(dq(json!({"filter": {"all": {}}, "includeRaw": true})))
                .unwrap(),
        );
        assert_eq!(with_raw[0].raw.as_deref(), Some(&br#"{"title":"a"}"#[..]));
        assert_eq!(with_raw[0].revision, listed[0].revision);
        assert_eq!(snapshot(&fx.path), before, "a query writes nothing");
    }

    #[test]
    fn limit_and_offset_hold_and_count_equals_the_stream() {
        let fx = Fixture::new("query-limit", &[notes()]);
        for n in 0..7 {
            fx.put_unwaited(
                "notes",
                &format!("d{n}"),
                &json!({"title": "t", "count": n}),
            );
        }
        fx.wait();
        let target = fx.collection("notes");
        let cases = [
            json!({"filter": {"all": {}}}),
            json!({"filter": {"all": {}}, "limit": 3}),
            json!({"filter": {"all": {}}, "offset": 2}),
            json!({"filter": {"all": {}}, "offset": 2, "limit": 3}),
            json!({"filter": {"all": {}}, "offset": 5, "limit": 10}),
            json!({"filter": {"all": {}}, "offset": 9}),
            json!({"filter": {"all": {}}, "limit": 0}),
            json!({"filter": {"all": {}}, "offset": 1, "limit": 100000}),
            json!({"filter": {"range": {"field": "count", "lower": {"included": 3}}}}),
            json!({"filter": {"range": {"field": "count", "lower": {"included": 3}}}, "offset": 1, "limit": 2}),
            json!({"filter": {"all": {}}, "startAfter": "d3"}),
            json!({"filter": {"all": {}}, "startAfter": "d3", "offset": 1, "limit": 2}),
        ];
        let expected: [&[&str]; 12] = [
            &["d0", "d1", "d2", "d3", "d4", "d5", "d6"],
            &["d0", "d1", "d2"],
            &["d2", "d3", "d4", "d5", "d6"],
            &["d2", "d3", "d4"],
            &["d5", "d6"],
            &[],
            &[],
            &["d1", "d2", "d3", "d4", "d5", "d6"],
            &["d3", "d4", "d5", "d6"],
            &["d4", "d5"],
            &["d4", "d5", "d6"],
            &["d5", "d6"],
        ];
        for (case, expected) in cases.into_iter().zip(expected) {
            let streamed = found(&target, dq(case.clone()));
            assert_eq!(streamed, expected, "{case}");
            assert_eq!(
                count(&target, dq(case.clone())),
                expected.len() as u64,
                "{case}"
            );
        }
    }

    #[test]
    fn a_stream_is_read_from_one_snapshot_and_none_is_repeated_or_skipped() {
        let fx = Fixture::new("query-snapshot", &[notes()]);
        // More than a page of the Index, so the stream asks it again.
        fx.bulk("notes", "d", 600);
        let target = fx.collection("notes");
        let mut stream = target.query(all()).unwrap();
        let first = stream.next().unwrap().unwrap();
        assert_eq!(first.id.as_deref(), Some("d0000"));

        // Written and indexed while the stream is open: at both ends and in
        // the middle, and one Document gone.
        fx.put("notes", "a-new-first", &json!({"title": "new"}));
        fx.put("notes", "d0300a", &json!({"title": "new"}));
        fx.put("notes", "zz-new-last", &json!({"title": "new"}));
        fx.remove("notes", "d0450");
        assert!(
            found(&target, all()).contains(&"a-new-first".to_owned()),
            "a query started now sees the writes"
        );

        let mut seen = vec![first.id.unwrap()];
        seen.extend(stream.map(|item| item.unwrap().id.unwrap()));
        let expected: Vec<String> = (0..600).map(|n| format!("d{n:04}")).collect();
        assert_eq!(seen, expected, "the stream holds what the call saw");
    }

    #[test]
    fn a_huge_limit_or_offset_is_not_a_reservation() {
        let fx = Fixture::new("query-huge", &[notes()]);
        for id in ["a", "b", "c"] {
            fx.put_unwaited("notes", id, &json!({"title": id}));
        }
        fx.wait();
        let target = fx.collection("notes");
        let max = u32::MAX;
        assert_eq!(
            found(&target, dq(json!({"filter": {"all": {}}, "limit": max}))),
            ["a", "b", "c"]
        );
        assert!(found(&target, dq(json!({"filter": {"all": {}}, "offset": max}))).is_empty());
        assert!(
            found(
                &target,
                dq(json!({"filter": {"all": {}}, "offset": max, "limit": max}))
            )
            .is_empty()
        );
        assert_eq!(
            found(
                &target,
                dq(json!({"filter": {"all": {}}, "offset": 2, "limit": max}))
            ),
            ["c"]
        );
        assert_eq!(
            count(
                &target,
                dq(json!({"filter": {"all": {}}, "offset": max, "limit": max}))
            ),
            0
        );
        assert_eq!(
            count(&target, dq(json!({"filter": {"all": {}}, "limit": max}))),
            3
        );
    }

    #[test]
    fn a_stream_started_before_a_reindex_reads_on_from_its_snapshot() {
        let fx = Fixture::new("query-reindex", &[notes()]);
        fx.bulk("notes", "d", 600);
        let target = fx.collection("notes");
        let mut stream = target.query(all()).unwrap();
        let first = stream.next().unwrap().unwrap();
        index(&fx.vault).reindex(Empty::default()).unwrap();
        let mut seen = vec![first.id.unwrap()];
        seen.extend(stream.map(|item| item.unwrap().id.unwrap()));
        let expected: Vec<String> = (0..600).map(|n| format!("d{n:04}")).collect();
        assert_eq!(seen, expected);
        // The rebuilt Index answers the same.
        eventually("the rebuild", || {
            target.count(all()).ok().filter(|c| c.count == Some(600))
        });
    }

    #[test]
    fn pages_by_start_after_cover_a_collection_once_also_when_documents_are_added_between() {
        let fx = Fixture::new("query-pages", &[notes()]);
        for id in ["d02", "d04", "d06", "d08", "d10", "d12", "d14", "d16"] {
            fx.put_unwaited("notes", id, &json!({"title": "t"}));
        }
        fx.wait();
        let target = fx.collection("notes");
        let page = |after: Option<&str>| {
            let mut query = json!({"filter": {"all": {}}, "limit": 3});
            if let Some(after) = after {
                query["startAfter"] = json!(after);
            }
            found(&target, dq(query))
        };
        let one = page(None);
        assert_eq!(one, ["d02", "d04", "d06"]);
        // Before the cursor and after it: the next page does not move.
        fx.put("notes", "d01", &json!({"title": "t"}));
        fx.put("notes", "d07", &json!({"title": "t"}));
        let two = page(one.last().map(String::as_str));
        assert_eq!(two, ["d07", "d08", "d10"]);
        let three = page(two.last().map(String::as_str));
        assert_eq!(three, ["d12", "d14", "d16"]);
        assert!(page(three.last().map(String::as_str)).is_empty());
        // A start_after that is no Document is a place in the order.
        assert_eq!(page(Some("d09")), ["d10", "d12", "d14"]);
        // A deep page is a condition, and a count agrees with it.
        assert_eq!(
            count(
                &target,
                dq(json!({"filter": {"all": {}}, "startAfter": "d10"}))
            ),
            3
        );
    }

    #[test]
    fn a_journal_lists_in_append_order_and_pages_after_a_line() {
        let fx = Fixture::new("query-journal", &[log()]);
        for id in ["l3", "l1", "l5", "l2", "l4", "l0", "l6"] {
            fx.put_unwaited(
                "log",
                id,
                &json!({"tag": if id < "l3" { "low" } else { "high" }}),
            );
        }
        fx.wait();
        let target = fx.collection("log");
        let order = ["l3", "l1", "l5", "l2", "l4", "l0", "l6"];
        assert_eq!(found(&target, all()), order);
        // A journal has the columns of the Projector too, and a filter keeps
        // the append order.
        assert_eq!(
            found(
                &target,
                filtered(json!({"term": {"field": "tag", "value": "low"}}))
            ),
            ["l1", "l2", "l0"]
        );
        let raw = documents(
            target
                .query(dq(
                    json!({"filter": {"all": {}}, "includeRaw": true, "limit": 5}),
                ))
                .unwrap(),
        );
        assert_eq!(raw.len(), 5);
        assert_eq!(raw[0].raw.as_deref(), Some(&br#"{"tag":"high"}"#[..]));
        assert!(raw.iter().all(|line| line.revision.is_none()));
        // Pages after a line, across segments.
        let mut after: Option<String> = None;
        let mut pages = Vec::new();
        loop {
            let mut query = json!({"filter": {"all": {}}, "limit": 2});
            if let Some(last) = &after {
                query["startAfter"] = json!(last);
            }
            let page = found(&target, dq(query));
            if page.is_empty() {
                break;
            }
            after = page.last().cloned();
            pages.push(page);
        }
        assert_eq!(
            pages,
            [["l3", "l1"], ["l5", "l2"], ["l4", "l0"]]
                .iter()
                .map(|page| strings(page))
                .chain([strings(&["l6"])])
                .collect::<Vec<_>>()
        );
        // A line appended meanwhile follows the last one.
        fx.put("log", "l7", &json!({"tag": "high"}));
        assert_eq!(
            found(
                &target,
                dq(json!({"filter": {"all": {}}, "startAfter": "l6"}))
            ),
            ["l7"]
        );
        // An id that is no line has no place in the order of a journal.
        assert!(
            found(
                &target,
                dq(json!({"filter": {"all": {}}, "startAfter": "nope"}))
            )
            .is_empty()
        );
        assert_eq!(
            count(
                &target,
                dq(json!({"filter": {"all": {}}, "startAfter": "l4"}))
            ),
            3
        );
    }

    #[test]
    fn terms_sets_exists_and_nulls_select_by_column() {
        let fx = Fixture::new("query-terms", &[notes()]);
        let docs = [
            (
                "red",
                json!({"title": "R", "tag": "red", "count": 1, "tags": ["x", "y"], "flag": true}),
            ),
            (
                "blue",
                json!({"title": "B", "tag": "blue", "count": 2, "tags": ["y"], "flag": false}),
            ),
            ("nil", json!({"title": "N", "tag": null})),
            ("omit", json!({"title": "O"})),
        ];
        for (id, raw) in &docs {
            fx.put_unwaited("notes", id, raw);
        }
        fx.wait();
        let target = fx.collection("notes");
        let q = |filter: Value| sorted(found(&target, filtered(filter)));
        assert_eq!(
            q(json!({"term": {"field": "tag", "value": "red"}})),
            ["red"]
        );
        assert_eq!(
            q(json!({"termSet": {"field": "tag", "values": ["red", "blue"]}})),
            ["blue", "red"]
        );
        assert_eq!(q(json!({"term": {"field": "tag", "value": null}})), ["nil"]);
        assert_eq!(q(json!({"exists": {"field": "tag"}})), ["blue", "red"]);
        assert_eq!(q(json!({"term": {"field": "count", "value": 2}})), ["blue"]);
        assert_eq!(
            q(json!({"term": {"field": "flag", "value": true}})),
            ["red"]
        );
        assert_eq!(
            q(json!({"term": {"field": "tags", "value": "y"}})),
            ["blue", "red"]
        );
        assert_eq!(
            q(json!({"term": {"field": "id", "value": "omit"}})),
            ["omit"]
        );
        assert_eq!(
            q(json!({"termSet": {"field": "id", "values": ["omit", "nil", "ghost"]}})),
            ["nil", "omit"]
        );
        assert_eq!(
            q(json!({"exists": {"field": "id"}})),
            ["blue", "nil", "omit", "red"]
        );
        assert_eq!(
            q(
                json!({"range": {"field": "id", "lower": {"included": "nil"}, "upper": {"excluded": "red"}}})
            ),
            ["nil", "omit"]
        );
        assert_eq!(
            q(json!({"range": {"field": "id", "upper": {"included": "blue"}}})),
            ["blue"]
        );
        assert_eq!(q(json!({"range": {"field": "id"}})).len(), 4);
        assert_eq!(q(json!({"empty": {}})), Vec::<String>::new());

        // A boolean combines: and, or, not.
        let both = json!({"boolean": {"clauses": [
            {"occur": "OCCUR_MUST", "query": {"exists": {"field": "tag"}}},
            {"occur": "OCCUR_MUST_NOT", "query": {"term": {"field": "tag", "value": "red"}}},
        ]}});
        assert_eq!(q(both), ["blue"]);
        let either = json!({"boolean": {"clauses": [
            {"occur": "OCCUR_SHOULD", "query": {"term": {"field": "tag", "value": "red"}}},
            {"occur": "OCCUR_SHOULD", "query": {"term": {"field": "tag", "value": "blue"}}},
        ]}});
        assert_eq!(q(either), ["blue", "red"]);
        let not_red = json!({"boolean": {"clauses": [
            {"occur": "OCCUR_MUST_NOT", "query": {"term": {"field": "tag", "value": "red"}}},
        ]}});
        assert_eq!(q(not_red), ["blue", "nil", "omit"]);
        let at_least_two = json!({"boolean": {"minimumShouldMatch": 2, "clauses": [
            {"occur": "OCCUR_SHOULD", "query": {"term": {"field": "tag", "value": "red"}}},
            {"occur": "OCCUR_SHOULD", "query": {"term": {"field": "count", "value": 1}}},
            {"occur": "OCCUR_SHOULD", "query": {"term": {"field": "count", "value": 2}}},
        ]}});
        assert_eq!(q(at_least_two), ["red"]);
    }

    #[test]
    fn phrase_prefix_fuzzy_regex_and_text_ranges() {
        let fx = Fixture::new("query-text", &[notes()]);
        for (id, title, tag) in [
            ("job", "part time job", "blue"),
            ("hobby", "part of the job", "red"),
            ("panda", "Giant panda", "amber"),
        ] {
            fx.put_unwaited("notes", id, &json!({"title": title, "tag": tag}));
        }
        fx.wait();
        let target = fx.collection("notes");
        let q = |filter: Value| sorted(found(&target, filtered(filter)));
        assert_eq!(
            q(json!({"phrase": {"field": "title", "terms": ["giant", "panda"]}})),
            ["panda"]
        );
        assert_eq!(
            q(json!({"phrase": {"field": "title", "terms": ["part", "job"], "slop": 2}})),
            ["hobby", "job"]
        );
        assert!(q(json!({"phrase": {"field": "title", "terms": ["part", "job"]}})).is_empty());
        assert_eq!(
            q(json!({"phrasePrefix": {"field": "title", "terms": ["part"], "prefix": "ti"}})),
            ["job"]
        );
        assert_eq!(
            q(json!({"fuzzy": {"field": "title", "term": "pnda", "distance": 1}})),
            ["panda"]
        );
        assert_eq!(
            q(json!({"fuzzy": {"field": "title", "term": "pan", "prefix": true}})),
            ["panda"]
        );
        assert_eq!(
            q(json!({"regex": {"field": "title", "pattern": "pand.*"}})),
            ["panda"]
        );
        assert_eq!(
            q(json!({"regexPhrase": {"field": "title", "patterns": ["part", "tim.*"]}})),
            ["job"]
        );
        assert_eq!(
            q(
                json!({"range": {"field": "tag", "lower": {"included": "a"}, "upper": {"excluded": "c"}}})
            ),
            ["job", "panda"]
        );
        assert_eq!(
            q(json!({"range": {"field": "tag", "lower": {"excluded": "blue"}}})),
            ["hobby"]
        );
        // The title is a column of the title, not of the tag.
        assert!(q(json!({"regex": {"field": "tag", "pattern": "pand.*"}})).is_empty());
    }

    #[test]
    fn a_range_on_an_integer_column_compares_by_value() {
        let fx = Fixture::new(
            "query-ints",
            &[json!({"name": "ints", "schema": {"standard": {}}})],
        );
        // r0..r5 = 9, 10, -3, 100, 5, a very large integer
        for (n, value) in [
            json!(9),
            json!(10),
            json!(-3),
            json!(100),
            json!(5),
            json!(9_000_000_000_000_000_000_i64),
        ]
        .into_iter()
        .enumerate()
        {
            fx.put_unwaited("ints", &format!("r{n}"), &json!({"n": value}));
        }
        fx.wait();
        let target = fx.collection("ints");
        let r = |lower: Value, upper: Value| {
            let mut range = json!({"field": "n"});
            if !lower.is_null() {
                range["lower"] = lower;
            }
            if !upper.is_null() {
                range["upper"] = upper;
            }
            sorted(found(&target, filtered(json!({"range": range}))))
        };
        let null = Value::Null;
        // Text order would put "10" and "100" before "5".
        assert_eq!(
            r(json!({"included": 5}), null.clone()),
            ["r0", "r1", "r3", "r4", "r5"]
        );
        assert_eq!(
            r(json!({"excluded": 5}), null.clone()),
            ["r0", "r1", "r3", "r5"]
        );
        assert_eq!(
            r(json!({"included": -3}), json!({"included": 10})),
            ["r0", "r1", "r2", "r4"]
        );
        assert_eq!(r(null.clone(), json!({"excluded": 5})), ["r2"]);
        assert_eq!(r(null.clone(), json!({"included": 5})), ["r2", "r4"]);
        assert_eq!(r(json!({"included": 10}), json!({"included": 10})), ["r1"]);
        assert!(r(json!({"excluded": 10}), json!({"excluded": 10})).is_empty());
        // A fractional end rounds inwards.
        assert_eq!(
            r(json!({"included": 9.5}), null.clone()),
            ["r1", "r3", "r5"]
        );
        assert_eq!(
            r(null.clone(), json!({"excluded": 9.5})),
            ["r0", "r2", "r4"]
        );
        assert_eq!(
            r(json!({"included": 9.0}), json!({"excluded": 10.0})),
            ["r0"]
        );
        // Ends beyond `i64` and an empty interval.
        assert!(r(json!({"included": 1e30}), null.clone()).is_empty());
        assert!(r(null.clone(), json!({"included": -1e30})).is_empty());
        assert_eq!(r(null.clone(), json!({"included": 1e30})).len(), 6);
        assert!(r(json!({"included": 10}), json!({"included": 9})).is_empty());
        assert_eq!(r(json!({"included": 9.0e18}), null.clone()), ["r5"]);
        // A term and a set of terms match by value.
        assert_eq!(
            sorted(found(
                &target,
                filtered(json!({"term": {"field": "n", "value": 10}}))
            )),
            ["r1"]
        );
        assert_eq!(
            sorted(found(
                &target,
                filtered(json!({"termSet": {"field": "n", "values": [9, -3]}}))
            )),
            ["r0", "r2"]
        );
        // A text or boolean end on a numeric column is the wrong type.
        for bad in [json!({"included": "5"}), json!({"included": true})] {
            let error = target
                .query(filtered(json!({"range": {"field": "n", "lower": bad}})))
                .err()
                .unwrap();
            assert!(error.is(Code::InvalidArgument), "{error}");
        }
    }

    #[test]
    fn a_range_on_a_number_column_orders_negatives_and_floats() {
        let fx = Fixture::new(
            "query-nums",
            &[json!({"name": "nums", "schema": {"standard": {}}})],
        );
        // r0..r4 = -2.5, -1, 0, 1.5, 10; r5 = -0.0 shares the key of 0.
        for (n, value) in [
            json!(-2.5),
            json!(-1.5),
            json!(0.5),
            json!(1.5),
            json!(10.5),
            json!(-0.0),
        ]
        .into_iter()
        .enumerate()
        {
            fx.put_unwaited("nums", &format!("r{n}"), &json!({"n": value}));
        }
        fx.wait();
        let target = fx.collection("nums");
        let r = |lower: Value, upper: Value| {
            let mut range = json!({"field": "n"});
            if !lower.is_null() {
                range["lower"] = lower;
            }
            if !upper.is_null() {
                range["upper"] = upper;
            }
            sorted(found(&target, filtered(json!({"range": range}))))
        };
        let null = Value::Null;
        assert_eq!(
            r(json!({"included": -1.5}), null.clone()),
            ["r1", "r2", "r3", "r4", "r5"]
        );
        assert_eq!(
            r(json!({"excluded": -1.5}), null.clone()),
            ["r2", "r3", "r4", "r5"]
        );
        assert_eq!(r(null.clone(), json!({"included": 0})), ["r0", "r1", "r5"]);
        assert_eq!(
            r(null.clone(), json!({"excluded": 0.5})),
            ["r0", "r1", "r5"]
        );
        assert_eq!(
            r(json!({"included": -2.5}), json!({"excluded": 1.5})),
            ["r0", "r1", "r2", "r5"]
        );
        assert!(r(json!({"included": 1.6}), json!({"included": 9.9})).is_empty());
        assert_eq!(r(json!({"included": 5}), null.clone()), ["r4"]);
        assert_eq!(r(null.clone(), null.clone()).len(), 6);
        for (value, expected) in [(json!(10.5), "r4"), (json!(1.5), "r3"), (json!(-2.5), "r0")] {
            assert_eq!(
                found(
                    &target,
                    filtered(json!({"term": {"field": "n", "value": value}}))
                ),
                [expected]
            );
        }
        let error = target
            .query(filtered(
                json!({"range": {"field": "n", "upper": {"included": "x"}}}),
            ))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument));
    }

    #[test]
    fn a_filter_breaks_the_rules_of_the_columns_it_names() {
        let fx = Fixture::new("query-invalid", &[notes()]);
        fx.put(
            "notes",
            "a",
            &json!({"title": "alpha beta", "count": 2, "body": "hidden"}),
        );
        let target = fx.collection("notes");
        let invalid = |filter: Value| {
            let error = match target.query(filtered(filter.clone())) {
                Ok(_) => panic!("{filter} was accepted"),
                Err(error) => error,
            };
            assert!(error.is(Code::InvalidArgument), "{filter}: {error}");
            assert!(!error.retryable());
            let error = target.count(filtered(filter.clone())).unwrap_err();
            assert!(error.is(Code::InvalidArgument), "{filter}: {error}");
        };
        // Not a column, or a column that cannot be filtered on.
        invalid(json!({"term": {"field": "unknown", "value": "x"}}));
        invalid(json!({"term": {"field": "body", "value": "hidden"}}));
        invalid(json!({"regex": {"field": "hidden", "pattern": "x"}}));
        invalid(json!({"exists": {"field": "nope", "jsonSubpaths": true}}));
        // A value of another type than the column.
        invalid(json!({"term": {"field": "title", "value": 3}}));
        invalid(json!({"term": {"field": "count", "value": "2"}}));
        invalid(json!({"term": {"field": "count", "value": 1.5}}));
        invalid(json!({"term": {"field": "flag", "value": "true"}}));
        invalid(json!({"term": {"field": "id", "value": 4}}));
        invalid(json!({"term": {"field": "id", "value": null}}));
        invalid(json!({"termSet": {"field": "count", "values": [1, "x"]}}));
        invalid(json!({"termSet": {"field": "count", "values": []}}));
        invalid(json!({"range": {"field": "title", "lower": {"included": 3}}}));
        invalid(json!({"range": {"field": "flag", "lower": {"included": true}}}));
        invalid(json!({"range": {"field": "id", "lower": {"included": 3}}}));
        // A phrase, a fuzzy term, or a pattern on a column without tokens.
        invalid(json!({"phrase": {"field": "count", "terms": ["one", "two"]}}));
        invalid(json!({"phrase": {"field": "id", "terms": ["one", "two"]}}));
        invalid(json!({"phrase": {"field": "flag", "terms": ["true", "false"]}}));
        invalid(json!({"fuzzy": {"field": "count", "term": "two"}}));
        invalid(json!({"regex": {"field": "count", "pattern": "2"}}));
        invalid(json!({"regexPhrase": {"field": "id", "patterns": ["a", "b"]}}));
        // A phrase of fewer than two tokens, a fuzzy term of two.
        invalid(json!({"phrase": {"field": "title", "terms": ["alpha", "!!"]}}));
        invalid(json!({"phrasePrefix": {"field": "title", "prefix": "al"}}));
        invalid(json!({"fuzzy": {"field": "title", "term": "alpha beta"}}));
        // A pattern that does not compile.
        invalid(json!({"regex": {"field": "title", "pattern": "(unclosed"}}));
        invalid(json!({"regexPhrase": {"field": "title", "patterns": ["alp.*", "(unclosed"]}}));
        // A leaf inside a boolean is checked too.
        invalid(json!({"boolean": {"clauses": [
            {"occur": "OCCUR_MUST", "query": {"term": {"field": "unknown", "value": "x"}}},
        ]}}));
        // The request breaks a rule of the message: checked first.
        for broken in [
            json!({"filter": {"fuzzy": {"field": "title", "term": "pnda", "distance": 3}}}),
            json!({"filter": {"phrase": {"field": "title", "terms": ["one"]}}}),
            json!({}),
            json!({"filter": {"all": {}}, "startAfter": ""}),
        ] {
            let error = target.query(dq(broken.clone())).err().unwrap();
            assert!(error.is(Code::InvalidArgument), "{broken}: {error}");
        }
        // The valid forms of the same leaves.
        assert_eq!(
            found(
                &target,
                filtered(json!({"term": {"field": "count", "value": 2.0}}))
            ),
            ["a"]
        );
        assert_eq!(
            found(
                &target,
                filtered(json!({"phrase": {"field": "title", "terms": ["alpha", "beta"]}}))
            ),
            ["a"]
        );
        // An id that breaks the rules of the collection.
        let error = target
            .query(dq(json!({"filter": {"all": {}}, "startAfter": "bad\\id"})))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
    }

    #[test]
    fn a_query_of_a_collection_means_that_collection_and_no_other() {
        let fx = Fixture::new("query-scope-collection", &[notes(), pages()]);
        fx.put("notes", "n", &json!({"tag": "red"}));
        fx.put("pages", "p", &json!({"tag": "red"}));
        let target = fx.collection("notes");
        let own = json!({"term": {"field": "tag", "collection": "notes", "value": "red"}});
        assert_eq!(found(&target, filtered(own)), ["n"]);
        let other = json!({"term": {"field": "tag", "collection": "pages", "value": "red"}});
        let error = target.query(filtered(other.clone())).err().unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let error = target.count(filtered(other)).unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        // `id` takes no collection.
        assert_eq!(
            found(
                &target,
                filtered(json!({"term": {"field": "id", "value": "n"}}))
            ),
            ["n"]
        );
        // A collection that is not declared is not found.
        let missing = fx
            .vault
            .get_collection(CollectionKey::default().with_name("ghost"));
        assert!(missing.err().unwrap().is(Code::NotFound));
    }

    #[test]
    fn a_query_of_the_vault_names_the_collection_of_every_column_leaf() {
        let fx = Fixture::new("query-scope-vault", &[notes(), pages()]);
        fx.put("notes", "n", &json!({"tag": "red"}));
        fx.put("pages", "p", &json!({"tag": "red"}));
        let run = |query: Value| fx.vault.query_documents(vq(query));
        let bare = json!({"term": {"field": "tag", "value": "red"}});
        let error = run(json!({"filter": bare.clone()})).err().unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let error = fx
            .vault
            .count_documents(vq(json!({"filter": bare})))
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        // A leaf on `id` names none; a leaf on a column names one.
        assert_eq!(
            keys(run(json!({"filter": {"term": {"field": "id", "value": "p"}}})).unwrap()),
            pairs(&[("pages", "p")])
        );
        let named = json!({"term": {"field": "tag", "collection": "notes", "value": "red"}});
        assert_eq!(
            keys(run(json!({"filter": named})).unwrap()),
            pairs(&[("notes", "n")])
        );
        // A collection that is not declared: in a leaf, in the names, in
        // `startAfter`.
        for query in [
            json!({"filter": {"term": {"field": "tag", "collection": "ghost", "value": "red"}}}),
            json!({"collections": {"names": ["ghost"]}, "filter": {"all": {}}}),
            json!({"filter": {"all": {}}, "startAfter": {"collection": "ghost", "id": "x"}}),
        ] {
            let error = run(query.clone()).err().unwrap();
            assert!(error.is(Code::NotFound), "{query}: {error}");
            let error = fx.vault.count_documents(vq(query.clone())).unwrap_err();
            assert!(error.is(Code::NotFound), "{query}: {error}");
        }
        // The column of a collection the leaf names is the column of that
        // collection: `title` is not one of `ints`.
        let fx2 = Fixture::new(
            "query-scope-columns",
            &[notes(), json!({"name": "ints", "schema": {"standard": {}}})],
        );
        let error = fx2
            .vault
            .query_documents(vq(
                json!({"filter": {"term": {"field": "title", "collection": "ints", "value": "x"}}}),
            ))
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
    }

    #[test]
    fn a_vault_query_gives_collections_in_name_order_and_pages_after_a_document_key() {
        let fx = Fixture::new("query-vault-order", &[pages(), notes(), log()]);
        for id in ["c", "a", "e"] {
            fx.put_unwaited("notes", id, &json!({"title": id}));
        }
        for id in ["d", "b"] {
            fx.put_unwaited("pages", id, &json!({"title": id}));
        }
        for id in ["z", "y"] {
            fx.put_unwaited("log", id, &json!({"title": id}));
        }
        fx.wait();
        let run = |query: Value| keys(fx.vault.query_documents(vq(query)).unwrap());
        let whole = pairs(&[
            ("log", "z"),
            ("log", "y"),
            ("notes", "a"),
            ("notes", "c"),
            ("notes", "e"),
            ("pages", "b"),
            ("pages", "d"),
        ]);
        assert_eq!(run(json!({"filter": {"all": {}}})), whole);
        // Named collections, in any order and twice.
        assert_eq!(
            run(
                json!({"collections": {"names": ["pages", "notes", "pages"]}, "filter": {"all": {}}})
            ),
            whole[2..]
        );
        // After a Document: of the middle, of a journal, and of none.
        assert_eq!(
            run(json!({"filter": {"all": {}}, "startAfter": {"collection": "notes", "id": "c"}})),
            whole[4..]
        );
        assert_eq!(
            run(json!({"filter": {"all": {}}, "startAfter": {"collection": "log", "id": "z"}})),
            whole[1..]
        );
        assert_eq!(
            run(json!({"filter": {"all": {}}, "startAfter": {"collection": "notes", "id": "zzz"}})),
            whole[5..]
        );
        assert_eq!(
            run(json!({"filter": {"all": {}}, "startAfter": {"collection": "pages", "id": "d"}})),
            Vec::<(String, String)>::new()
        );
        // Offset and limit run over the whole order.
        assert_eq!(
            run(json!({"filter": {"all": {}}, "offset": 3, "limit": 3})),
            whole[3..6]
        );
        // Pages of two cover the Vault once, also across the boundary of
        // two collections and with a Document added between pages.
        let mut got = Vec::new();
        let mut after: Option<(String, String)> = None;
        for turn in 0.. {
            let mut query = json!({"filter": {"all": {}}, "limit": 2});
            if let Some((collection, id)) = &after {
                query["startAfter"] = json!({"collection": collection, "id": id});
            }
            let page = run(query);
            if page.is_empty() {
                break;
            }
            if turn == 1 {
                fx.put("notes", "b", &json!({"title": "added"}));
                fx.put("notes", "0", &json!({"title": "added"}));
            }
            after = page.last().cloned();
            got.extend(page);
        }
        assert_eq!(
            got, whole,
            "the cursor decides: `b` and `0` sort before the last Document of the second page"
        );
    }

    #[test]
    fn a_leaf_on_another_collection_is_false_where_it_does_not_apply_and_each_kind_ranges_on_its_own()
     {
        let fx = Fixture::new(
            "query-kinds",
            &[
                json!({"name": "ints", "schema": {"standard": {}}}),
                json!({"name": "nums", "schema": {"standard": {}}}),
                json!({"name": "texts", "schema": {"standard": {}}}),
            ],
        );
        for (collection, values) in [
            ("ints", [json!(9), json!(10), json!(2)]),
            ("nums", [json!(9.5), json!(10.5), json!(2.5)]),
            ("texts", [json!(9), json!(10), json!(2)]),
        ] {
            for (n, value) in values.into_iter().enumerate() {
                fx.put_unwaited(collection, &format!("r{n}"), &json!({"n": value}));
            }
        }
        fx.wait();
        let leaf = |collection: &str, bound: Value| {
            json!({
                "occur": "OCCUR_SHOULD",
                "query": {"range": {
                    "field": "n",
                    "collection": collection,
                    "lower": {"included": bound},
                }},
            })
        };
        let query = json!({"filter": {"boolean": {"clauses": [
            leaf("ints", json!(5)), leaf("nums", json!(5)), leaf("texts", json!("5")),
        ]}}});
        // Numbers compare by value, text compares as text: "10" < "5".
        assert_eq!(
            keys(fx.vault.query_documents(vq(query)).unwrap()),
            pairs(&[
                ("ints", "r0"),
                ("ints", "r1"),
                ("nums", "r0"),
                ("nums", "r1"),
                ("texts", "r0"),
            ])
        );
        // A text end on the numbers is the wrong type, wherever it is.
        let wrong = json!({"filter": {"boolean": {"clauses": [leaf("ints", json!("5"))]}}});
        let error = fx.vault.query_documents(vq(wrong)).err().unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
    }

    #[test]
    fn a_negated_leaf_of_another_collection_does_not_exclude_here() {
        let fx = Fixture::new("query-negation", &[notes(), pages()]);
        fx.put("notes", "n", &json!({"tag": "red"}));
        fx.put("pages", "p", &json!({"tag": "red"}));
        fx.put("pages", "q", &json!({"tag": "blue"}));
        let not_red_page = json!({"filter": {"boolean": {"clauses": [
            {"occur": "OCCUR_MUST_NOT", "query": {"term": {"field": "tag", "collection": "pages", "value": "red"}}},
        ]}}});
        assert_eq!(
            keys(fx.vault.query_documents(vq(not_red_page.clone())).unwrap()),
            pairs(&[("notes", "n"), ("pages", "q")])
        );
        assert_eq!(
            fx.vault.count_documents(vq(not_red_page)).unwrap().count,
            Some(2)
        );
    }

    #[test]
    fn an_isolated_collection_is_scanned_only_when_named_and_one_without_an_index_is_refused() {
        let fx = Fixture::new(
            "query-isolated",
            &[
                notes(),
                json!({"name": "diary", "schema": {"standard": {}, "isolated": true}}),
                json!({"name": "hidden", "schema": {"standard": {}, "exclude_lexical_index": true}}),
            ],
        );
        fx.put_unwaited("notes", "n", &json!({"title": "n"}));
        fx.put_unwaited("diary", "d", &json!({"title": "d"}));
        fx.put_unwaited("hidden", "h", &json!({"title": "h"}));
        fx.wait();
        let run = |query: Value| fx.vault.query_documents(vq(query));
        assert_eq!(
            keys(run(json!({"filter": {"all": {}}})).unwrap()),
            pairs(&[("notes", "n")]),
            "no names: neither isolated nor excluded"
        );
        assert_eq!(
            keys(
                run(json!({"collections": {"names": ["notes", "diary"]}, "filter": {"all": {}}}))
                    .unwrap()
            ),
            pairs(&[("diary", "d"), ("notes", "n")])
        );
        assert_eq!(
            fx.vault
                .count_documents(vq(
                    json!({"collections": {"names": ["diary"]}, "filter": {"all": {}}})
                ))
                .unwrap()
                .count,
            Some(1)
        );
        assert_eq!(found(&fx.collection("diary"), all()), ["d"]);
        // The excluded collection is refused, not answered empty.
        let error =
            run(json!({"collections": {"names": ["hidden", "notes"]}, "filter": {"all": {}}}))
                .err()
                .unwrap();
        assert!(error.is(Code::Unimplemented), "{error}");
        assert!(
            error.to_string().contains("exclude_lexical_index"),
            "{error}"
        );
        let hidden = fx.collection("hidden");
        assert!(hidden.query(all()).err().unwrap().is(Code::Unimplemented));
        assert!(hidden.count(all()).unwrap_err().is(Code::Unimplemented));
        assert!(
            fx.vault
                .count_documents(vq(
                    json!({"collections": {"names": ["hidden"]}, "filter": {"all": {}}})
                ))
                .unwrap_err()
                .is(Code::Unimplemented)
        );
        // Its Documents are still read by key.
        assert_eq!(
            hidden
                .get(scheda::v1::DocumentId::default().with_id("h"))
                .unwrap()
                .id
                .as_deref(),
            Some("h")
        );
    }

    #[cfg(feature = "encrypt")]
    #[test]
    fn an_encrypted_collection_named_is_unimplemented_and_skipped_when_not() {
        const IDENTITY: &str =
            "AGE-SECRET-KEY-1RN2D82PZQK97437LWJVQJK4Q6EQVHLQ9A8Z0QTAXZ424HV8XZZPS5Q9860";
        let scratch = Scratch::new("query-encrypted");
        let path = scratch.join("vault");
        let options: OpenOptions = serde_json::from_value(json!({
            "encryption": {"identity": IDENTITY},
            "indexPollMs": 10,
            "writeTimeoutMs": 60000
        }))
        .unwrap();
        let request = VaultOpening {
            projector: ProjectorRef::from_sync(Json::new("json@1")).into(),
            ..opening(&path, options)
        };
        let vault = Scheda.open(request).unwrap().to_sync().unwrap();
        for value in [
            notes(),
            json!({"name": "secrets", "schema": {"standard": {}, "encrypted": true}}),
        ] {
            vault.create_collection(declaration(value)).unwrap();
        }
        let path = path.canonicalize().unwrap();
        collection(&vault, "notes")
            .upsert(
                DocumentUpsert::default()
                    .with_id("n")
                    .with_raw(b"{}".to_vec())
                    .with_note("t"),
            )
            .unwrap();
        collection(&vault, "secrets")
            .upsert(
                DocumentUpsert::default()
                    .with_id("s")
                    .with_raw(b"secret".to_vec())
                    .with_note("t"),
            )
            .unwrap();
        wait_indexed(&vault, &path);
        let all_vault = |names: Value| {
            vault.query_documents(vq(
                json!({"collections": {"names": names}, "filter": {"all": {}}}),
            ))
        };
        assert_eq!(
            keys(all_vault(json!([])).unwrap()),
            pairs(&[("notes", "n")])
        );
        let error = all_vault(json!(["secrets"])).err().unwrap();
        assert!(error.is(Code::Unimplemented), "{error}");
        let secrets = collection(&vault, "secrets");
        assert!(secrets.query(all()).err().unwrap().is(Code::Unimplemented));
        assert!(secrets.count(all()).unwrap_err().is(Code::Unimplemented));
    }

    #[test]
    fn an_index_never_built_is_index_rebuilding_and_retryable() {
        let scratch = Scratch::new("query-rebuilding");
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
        assert!(rebuilding(&notes.query(all()).err().unwrap()));
        assert!(rebuilding(&notes.count(all()).unwrap_err()));
        let query = vq(json!({"filter": {"all": {}}}));
        assert!(rebuilding(
            &vault.query_documents(query.clone()).err().unwrap()
        ));
        assert!(rebuilding(&vault.count_documents(query).unwrap_err()));
        // The Documents are read without it.
        assert_eq!(
            ids(notes
                .get_many(serde_json::from_value(json!({"listing": {}})).unwrap())
                .unwrap()),
            ["a"]
        );
    }

    #[test]
    fn a_handle_without_a_projector_reads_the_index_another_handle_built() {
        let fx = Fixture::new("query-reader", &[notes()]);
        fx.put("notes", "a", &json!({"title": "alpha"}));
        let reader = crate::common::open(&fx.path, options()).unwrap();
        let target = collection(&reader, "notes");
        assert_eq!(
            found(
                &target,
                filtered(json!({"term": {"field": "title", "value": "alpha"}}))
            ),
            ["a"]
        );
        assert_eq!(count(&target, all()), 1);
    }

    /// Polls `attempt` until it gives something, or fails the test.
    fn eventually<T>(what: &str, mut attempt: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(done) = attempt() {
                return done;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_vault_of_another_projection_is_index_stale_until_its_own_reindex() {
        let scratch = Scratch::new("query-projection");
        let path = scratch.join("vault");
        let old = open_with(&path, Json::new("json@1"));
        old.create_collection(declaration(notes())).unwrap();
        let path = path.canonicalize().unwrap();
        collection(&old, "notes")
            .upsert(
                DocumentUpsert::default()
                    .with_id("a")
                    .with_raw(b"{}".to_vec())
                    .with_note("t"),
            )
            .unwrap();
        wait_indexed(&old, &path);
        old.close(Empty::default()).unwrap();

        let new = open_with(&path, Json::new("json@2"));
        let target = collection(&new, "notes");
        let stale = |error: &Error| error.is(Code::IndexStale) && !error.retryable();
        assert!(stale(&target.query(all()).err().unwrap()));
        assert!(stale(&target.count(all()).unwrap_err()));
        let query = vq(json!({"filter": {"all": {}}}));
        assert!(stale(&new.query_documents(query.clone()).err().unwrap()));
        assert!(stale(&new.count_documents(query).unwrap_err()));

        // Its own Reindex rebuilds the Index with its projection.
        index(&new).reindex(Empty::default()).unwrap();
        eventually("the rebuild", || target.count(all()).ok());
        assert_eq!(found(&target, all()), ["a"]);

        // Now the Vault of the old projection is the stale one.
        let again = open_with(&path, Json::new("json@1"));
        let error = collection(&again, "notes").query(all()).err().unwrap();
        assert!(stale(&error), "{error}");
    }

    #[test]
    fn a_collection_whose_columns_changed_is_index_stale_for_its_readers_only() {
        let fx = Fixture::new("query-columns", &[notes(), pages()]);
        fx.put("notes", "a", &json!({"title": "alpha"}));
        fx.put("pages", "p", &json!({"title": "page"}));
        let (notes, pages) = (fx.collection("notes"), fx.collection("pages"));
        assert_eq!(count(&notes, all()), 1);

        // The Projector now declares one more column of `notes`.
        fx.extra.store(true, Ordering::SeqCst);
        fx.put("notes", "b", &json!({"title": "beta", "extra": "more"}));
        let stale = |error: &Error| error.is(Code::IndexStale) && !error.retryable();
        assert!(stale(&notes.query(all()).err().unwrap()));
        assert!(stale(&notes.count(all()).unwrap_err()));
        assert_eq!(found(&pages, all()), ["p"], "another collection answers");
        let run = |names: Value| {
            fx.vault.query_documents(vq(
                json!({"collections": {"names": names}, "filter": {"all": {}}}),
            ))
        };
        assert!(
            stale(&run(json!([])).err().unwrap()),
            "no names: includes `notes`"
        );
        assert!(stale(&run(json!(["notes", "pages"])).err().unwrap()));
        assert_eq!(
            keys(run(json!(["pages"])).unwrap()),
            pairs(&[("pages", "p")])
        );

        // A Reindex takes the new columns.
        index(&fx.vault).reindex(Empty::default()).unwrap();
        eventually("the rebuild", || notes.count(all()).ok());
        assert_eq!(
            found(
                &notes,
                filtered(json!({"term": {"field": "extra", "value": "more"}}))
            ),
            ["b"]
        );
    }

    #[test]
    fn after_close_every_query_call_and_the_next_item_of_a_held_stream_are_failed_precondition() {
        let fx = Fixture::new("query-close", &[notes()]);
        for id in ["a", "b", "c"] {
            fx.put_unwaited("notes", id, &json!({"title": id}));
        }
        fx.wait();
        let target = fx.collection("notes");
        let mut held = target.query(all()).unwrap();
        assert_eq!(held.next().unwrap().unwrap().id.as_deref(), Some("a"));
        fx.vault.close(Empty::default()).unwrap();
        let error = held.next().unwrap().unwrap_err();
        assert!(error.is(Code::FailedPrecondition), "{error}");
        assert!(held.next().is_none());
        assert!(
            target
                .query(all())
                .err()
                .unwrap()
                .is(Code::FailedPrecondition)
        );
        assert!(
            target
                .count(all())
                .unwrap_err()
                .is(Code::FailedPrecondition)
        );
    }

    #[test]
    fn queries_from_several_threads_agree() {
        let fx = Fixture::new("query-threads", &[notes()]);
        for n in 0..20 {
            fx.put_unwaited("notes", &format!("d{n:02}"), &json!({"title": "t"}));
        }
        fx.wait();
        let target = fx.collection("notes");
        std::thread::scope(|scope| {
            for _ in 0..6 {
                scope.spawn(|| {
                    for _ in 0..10 {
                        assert_eq!(found(&target, all()).len(), 20);
                        assert_eq!(count(&target, all()), 20);
                    }
                });
            }
        });
    }
}
