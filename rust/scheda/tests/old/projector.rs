//! Projector wired into the lexical Index and `find`.
//! FTS / vector slots from Projector marks, not Schema field flags.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{
    ColumnKind, Error, Filter, FindQuery, IndexColumn, IndexDoc, IndexSchema, OpenOptions,
    Projector, Vault,
};
use serde_json::json;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-f12", n),
        }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const NOTES: &str = "\
version: 1
name: proj_notes
schema:
  document: {}
";

fn rec(fields: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn filter(value: serde_json::Value) -> FindQuery {
    serde_json::from_value::<Filter>(value).unwrap().into()
}

fn ids(result: &scheda::FindResult) -> Vec<&str> {
    result
        .records
        .iter()
        .filter_map(|record| record.id.as_deref())
        .collect()
}

fn write_notes(scratch: &Scratch) {
    let dir = scratch.vault().join("proj_notes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".collection.yaml"), NOTES).unwrap();
    common::commit_file(&scratch.vault(), &dir.join(".collection.yaml"));
}

fn title_projector() -> Arc<dyn Projector> {
    common::json_text_projector(&["title"])
}

struct FailingProjector;

impl Projector for FailingProjector {
    fn schema(&self, _collection: &str) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: vec![
                IndexColumn::default()
                    .with_name("title")
                    .with_kind(ColumnKind::Text)
                    .with_find(true),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, _collection: &str, id: &str, _raw: &[u8]) -> Result<IndexDoc, Error> {
        if id == "omit" {
            return Err(Error::invalid("project failed"));
        }
        if id == "empty" {
            return Ok(IndexDoc::default());
        }
        Ok(IndexDoc {
            values: [(
                String::from("title"),
                scheda::IndexValue::Text(String::from("kept")),
            )]
            .into(),
        })
    }
}

struct TypedProjector;

impl Projector for TypedProjector {
    fn schema(&self, _collection: &str) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: vec![
                IndexColumn::default()
                    .with_name("title")
                    .with_kind(ColumnKind::Text)
                    .with_find(true),
                IndexColumn::default()
                    .with_name("count")
                    .with_kind(ColumnKind::Integer)
                    .with_find(true),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, _collection: &str, _id: &str, raw: &[u8]) -> Result<IndexDoc, Error> {
        let Ok(text) = std::str::from_utf8(raw) else {
            return Ok(IndexDoc::default());
        };
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str(text) else {
            return Ok(IndexDoc::default());
        };
        let mut values = std::collections::BTreeMap::new();
        if let Some(serde_json::Value::String(title)) = map.get("title") {
            values.insert(
                String::from("title"),
                scheda::IndexValue::Text(title.clone()),
            );
        }
        if let Some(n) = map.get("count").and_then(serde_json::Value::as_i64) {
            values.insert(String::from("count"), scheda::IndexValue::Integer(n));
        }
        Ok(IndexDoc { values })
    }
}

fn open_with(scratch: &Scratch, projector: Option<Arc<dyn Projector>>) -> Vault {
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    write_notes(scratch);
    Vault::open_with(scratch.vault(), OpenOptions::new(), None, projector).unwrap()
}

#[test]
fn term_on_projected_column_hits_one_and_id_still_hits() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, Some(title_projector()));
    let collection = vault.collection("proj_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "alpha"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"title": "beta"})), None)
        .unwrap();
    common::indexed(&vault);
    let hit = collection
        .find(
            &filter(json!({"term":{"field":"title","value":"alpha"}})),
            0,
            10,
        )
        .unwrap();
    assert_eq!(ids(&hit), ["a"]);
    let by_id = collection
        .find(&filter(json!({"term":{"field":"id","value":"b"}})), 0, 10)
        .unwrap();
    assert_eq!(ids(&by_id), ["b"]);
    let err = collection
        .find(
            &filter(json!({"term":{"field":"unknown","value":"x"}})),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(scheda::Code::Invalid));
}

#[test]
fn no_projector_identity_only() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, None);
    let collection = vault.collection("proj_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "alpha"})), None)
        .unwrap();
    common::indexed(&vault);
    let all = collection.find(&Filter::all().into(), 0, 10).unwrap();
    assert_eq!(ids(&all), ["a"]);
    let by_id = collection
        .find(&filter(json!({"term":{"field":"id","value":"a"}})), 0, 10)
        .unwrap();
    assert_eq!(ids(&by_id), ["a"]);
    let err = collection
        .find(
            &filter(json!({"term":{"field":"title","value":"alpha"}})),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(scheda::Code::Invalid));
}

#[test]
fn project_error_or_empty_omits_from_find_but_add_succeeds() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, Some(Arc::new(FailingProjector)));
    let collection = vault.collection("proj_notes").unwrap();
    collection
        .add("omit", rec(json!({"title": "gone"})), None)
        .unwrap();
    collection
        .add("empty", rec(json!({"title": "blank"})), None)
        .unwrap();
    collection
        .add("kept", rec(json!({"title": "kept"})), None)
        .unwrap();
    assert_eq!(collection.get("omit").unwrap().id.as_deref(), Some("omit"));
    assert_eq!(
        collection.get("empty").unwrap().id.as_deref(),
        Some("empty")
    );
    common::indexed(&vault);
    let all = collection.find(&Filter::all().into(), 0, 10).unwrap();
    assert_eq!(ids(&all), ["kept"]);
}

#[test]
fn reindex_omits_empty_projection_but_get_still_works() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, Some(Arc::new(FailingProjector)));
    let collection = vault.collection("proj_notes").unwrap();
    collection
        .add("empty", rec(json!({"title": "blank"})), None)
        .unwrap();
    collection
        .add("kept", rec(json!({"title": "kept"})), None)
        .unwrap();
    vault.reindex().unwrap();
    common::indexed(&vault);
    let all = collection.find(&Filter::all().into(), 0, 10).unwrap();
    assert_eq!(ids(&all), ["kept"]);
    assert_eq!(
        collection.get("empty").unwrap().id.as_deref(),
        Some("empty")
    );
    assert_eq!(collection.get("kept").unwrap().id.as_deref(), Some("kept"));
}

#[test]
fn phrase_on_integer_column_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, Some(Arc::new(TypedProjector)));
    let collection = vault.collection("proj_notes").unwrap();
    collection
        .add("a", rec(json!({"title": "alpha", "count": 2})), None)
        .unwrap();
    common::indexed(&vault);
    let err = collection
        .find(
            &filter(json!({"phrase":{"field":"count","terms":["one","two"]}})),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(scheda::Code::Invalid));
    let hit = collection
        .find(&filter(json!({"term":{"field":"count","value":2}})), 0, 10)
        .unwrap();
    assert_eq!(ids(&hit), ["a"]);
}

/// One column `n`, whose kind depends on the collection: `proj_ints` is an
/// `integer`, `proj_nums` a `number`, anything else `text`.
struct KindProjector;

impl KindProjector {
    fn kind(collection: &str) -> ColumnKind {
        match collection {
            "proj_ints" => ColumnKind::Integer,
            "proj_nums" => ColumnKind::Number,
            _ => ColumnKind::Text,
        }
    }
}

impl Projector for KindProjector {
    fn schema(&self, collection: &str) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: vec![
                IndexColumn::default()
                    .with_name("n")
                    .with_kind(Self::kind(collection))
                    .with_find(true),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, collection: &str, _id: &str, raw: &[u8]) -> Result<IndexDoc, Error> {
        let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(raw) else {
            return Ok(IndexDoc::default());
        };
        let value = match (Self::kind(collection), map.get("n")) {
            (ColumnKind::Integer, Some(n)) => n.as_i64().map(scheda::IndexValue::Integer),
            (ColumnKind::Number, Some(n)) => n.as_f64().map(scheda::IndexValue::Number),
            (_, Some(n)) => Some(scheda::IndexValue::Text(n.to_string())),
            (_, None) => None,
        };
        let mut values = std::collections::BTreeMap::new();
        if let Some(value) = value {
            values.insert(String::from("n"), value);
        }
        Ok(IndexDoc { values })
    }
}

fn open_kinds(scratch: &Scratch, names: &[&str]) -> Vault {
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    for name in names {
        let dir = scratch.vault().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!("version: 1\nname: {name}\nschema:\n  document: {{}}\n");
        std::fs::write(dir.join(".collection.yaml"), body).unwrap();
        common::commit_file(&scratch.vault(), &dir.join(".collection.yaml"));
    }
    Vault::open_with(
        scratch.vault(),
        OpenOptions::new(),
        None,
        Some(Arc::new(KindProjector)),
    )
    .unwrap()
}

fn add_numbers(vault: &Vault, collection: &str, values: &[serde_json::Value]) {
    let collection = vault.collection(collection).unwrap();
    for (i, n) in values.iter().enumerate() {
        collection
            .add(&format!("r{i}"), rec(json!({"n": n})), None)
            .unwrap();
    }
}

/// Sorted ids of `collection` records matching one `range` on `n`.
fn range_ids(
    vault: &Vault,
    collection: &str,
    lower: serde_json::Value,
    upper: serde_json::Value,
) -> Vec<String> {
    let mut range = json!({"field": "n"});
    if !lower.is_null() {
        range["lower"] = lower;
    }
    if !upper.is_null() {
        range["upper"] = upper;
    }
    let found = vault
        .collection(collection)
        .unwrap()
        .find(&filter(json!({ "range": range })), 0, 100)
        .unwrap();
    let mut out: Vec<String> = ids(&found).into_iter().map(str::to_owned).collect();
    out.sort_unstable();
    out
}

#[test]
fn range_on_integer_column_compares_by_value() {
    let scratch = Scratch::new();
    let vault = open_kinds(&scratch, &["proj_ints"]);
    // r0..r5 = 9, 10, -3, 100, 5, i64::MAX
    add_numbers(
        &vault,
        "proj_ints",
        &[
            json!(9),
            json!(10),
            json!(-3),
            json!(100),
            json!(5),
            json!(i64::MAX),
        ],
    );
    common::indexed(&vault);
    let r = |lower, upper| range_ids(&vault, "proj_ints", lower, upper);
    let null = serde_json::Value::Null;
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
    assert_eq!(
        r(json!({"excluded": 10}), json!({"excluded": 10})),
        Vec::<String>::new()
    );
    // A fractional bound rounds inwards.
    assert_eq!(
        r(json!({"included": 9.5}), null.clone()),
        ["r1", "r3", "r5"]
    );
    assert_eq!(
        r(json!({"excluded": 9.5}), null.clone()),
        ["r1", "r3", "r5"]
    );
    assert_eq!(
        r(null.clone(), json!({"included": 9.5})),
        ["r0", "r2", "r4"]
    );
    assert_eq!(
        r(null.clone(), json!({"excluded": 9.5})),
        ["r0", "r2", "r4"]
    );
    assert_eq!(
        r(json!({"included": 9.0}), json!({"excluded": 10.0})),
        ["r0"]
    );
    // Bounds beyond `i64` and an empty interval match nothing or all.
    assert_eq!(
        r(json!({"included": 1e30}), null.clone()),
        Vec::<String>::new()
    );
    assert_eq!(
        r(null.clone(), json!({"included": -1e30})),
        Vec::<String>::new()
    );
    assert_eq!(r(null.clone(), json!({"included": 1e30})).len(), 6);
    assert_eq!(
        r(json!({"included": 10}), json!({"included": 9})),
        Vec::<String>::new()
    );
    // The largest `i64` is exact.
    assert_eq!(r(json!({"included": 9.0e18}), null.clone()), ["r5"]);
    // Term and term_set still match.
    let collection = vault.collection("proj_ints").unwrap();
    let hit = collection
        .find(&filter(json!({"term": {"field": "n", "value": 10}})), 0, 10)
        .unwrap();
    assert_eq!(ids(&hit), ["r1"]);
    let set = collection
        .find(
            &filter(json!({"termSet": {"field": "n", "values": [9, -3]}})),
            0,
            10,
        )
        .unwrap();
    let mut set_ids = ids(&set);
    set_ids.sort_unstable();
    assert_eq!(set_ids, ["r0", "r2"]);
    // A text or boolean bound on a numeric column is invalid.
    for bad in [json!({"included": "5"}), json!({"included": true})] {
        let err = collection
            .find(
                &filter(json!({"range": {"field": "n", "lower": bad}})),
                0,
                10,
            )
            .unwrap_err();
        assert_eq!(err.known_code(), Some(scheda::Code::Invalid));
    }
}

#[test]
fn range_on_number_column_orders_negatives_and_floats() {
    let scratch = Scratch::new();
    let vault = open_kinds(&scratch, &["proj_nums"]);
    // r0..r4 = -2.5, -1, 0, 1.5, 10; r5 = -0.0 shares the key of 0.
    add_numbers(
        &vault,
        "proj_nums",
        &[
            json!(-2.5),
            json!(-1),
            json!(0),
            json!(1.5),
            json!(10),
            json!(-0.0),
        ],
    );
    common::indexed(&vault);
    let r = |lower, upper| range_ids(&vault, "proj_nums", lower, upper);
    let null = serde_json::Value::Null;
    assert_eq!(
        r(json!({"included": -1}), null.clone()),
        ["r1", "r2", "r3", "r4", "r5"]
    );
    assert_eq!(
        r(json!({"excluded": -1}), null.clone()),
        ["r2", "r3", "r4", "r5"]
    );
    assert_eq!(
        r(null.clone(), json!({"included": 0})),
        ["r0", "r1", "r2", "r5"]
    );
    assert_eq!(r(null.clone(), json!({"excluded": 0})), ["r0", "r1"]);
    assert_eq!(
        r(json!({"included": -2.5}), json!({"excluded": 1.5})),
        ["r0", "r1", "r2", "r5"]
    );
    assert_eq!(
        r(json!({"excluded": -2.5}), json!({"included": 1.5})),
        ["r1", "r2", "r3", "r5"]
    );
    assert_eq!(
        r(json!({"included": 1.6}), json!({"included": 9.9})),
        Vec::<String>::new()
    );
    assert_eq!(r(json!({"included": 5}), null.clone()), ["r4"]);
    assert_eq!(r(null.clone(), null.clone()).len(), 6);
    // Term: a JSON number 10 matches the stored 10.0, and 1.5 matches 1.5.
    let collection = vault.collection("proj_nums").unwrap();
    for (value, expected) in [(json!(10), "r4"), (json!(1.5), "r3"), (json!(-2.5), "r0")] {
        let hit = collection
            .find(
                &filter(json!({"term": {"field": "n", "value": value}})),
                0,
                10,
            )
            .unwrap();
        assert_eq!(ids(&hit), [expected]);
    }
    let err = collection
        .find(
            &filter(json!({"range": {"field": "n", "upper": {"included": "x"}}})),
            0,
            10,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(scheda::Code::Invalid));
}

#[test]
fn same_column_name_with_different_kinds_ranges_per_collection() {
    let scratch = Scratch::new();
    let vault = open_kinds(&scratch, &["proj_ints", "proj_nums", "proj_texts"]);
    add_numbers(&vault, "proj_ints", &[json!(9), json!(10), json!(2)]);
    add_numbers(&vault, "proj_nums", &[json!(9.5), json!(10), json!(2.5)]);
    add_numbers(&vault, "proj_texts", &[json!(9), json!(10), json!(2)]);
    common::indexed(&vault);
    let leaf = |collection: &str| {
        json!({
            "occur": "OCCUR_SHOULD",
            "query": {"range": {
                "field": "n",
                "collection": collection,
                "lower": {"included": 5},
            }},
        })
    };
    let query = filter(json!({"boolean": {"clauses": [
        leaf("proj_ints"), leaf("proj_nums"), leaf("proj_texts"),
    ]}}));
    let found = vault.find(&[], &query, 0, 100).unwrap();
    let mut got: Vec<(String, String)> = found
        .records
        .iter()
        .map(|record| {
            (
                record.collection.clone().unwrap(),
                record.id.clone().unwrap(),
            )
        })
        .collect();
    got.sort_unstable();
    let pair = |c: &str, i: &str| (c.to_owned(), i.to_owned());
    // Numbers compare by value; text compares as text ("10" < "5", "2" < "5").
    assert_eq!(
        got,
        [
            pair("proj_ints", "r0"),
            pair("proj_ints", "r1"),
            pair("proj_nums", "r0"),
            pair("proj_nums", "r1"),
            pair("proj_texts", "r0"),
        ]
    );
}
