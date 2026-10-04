//! F12: Projector wired into the lexical Index and `find`.
//! F13: FTS / vector slots from Projector marks, not Schema field flags.

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
