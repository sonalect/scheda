//! Opaque document files, Index wipe, YAML projector, encrypt/isolated.
//! Collection names here must not appear in `rust/scheda/src`.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{
    Code, ColumnKind, Error, Filter, FindQuery, FindResult, IndexColumn, IndexDoc, IndexSchema,
    Journal, OpenOptions, Projector, Schema, SearchHits, SearchQuery, Unit, Vault,
};
use serde_json::json;

#[cfg(feature = "encrypt")]
use scheda::Encryption;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-f19", n),
        }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const NOTE_YAML: &str = "title: yaml-only-token\nbody: ignored\n";
const NOTE_MD: &str = "# heading\nmd-only-token\n";
const NOTE_TOML: &str = "name = \"toml-only\"\n";
const NOTE_TXT: &str = "plain txt-only-token trailer\n";
const NOTE_TXT_REPLACED: &str = "plain f19-replaced-txt-token trailer\n";
const JOURNAL_PAYLOAD: &[u8] = b"journal-line-token";

const MIXED_FILES: [(&str, &str); 4] = [
    ("note.yaml", NOTE_YAML),
    ("note.md", NOTE_MD),
    ("note.toml", NOTE_TOML),
    ("note.txt", NOTE_TXT),
];

#[cfg(feature = "encrypt")]
const PASS: &str = "p19-test-passphrase-never-on-disk";
#[cfg(feature = "encrypt")]
const PLAIN: &[u8] = b"fixture-plaintext-body-token";

fn document_decl() -> Schema {
    Schema {
        kind: Unit::default().into(),
        ..Schema::default()
    }
}

fn journal_decl() -> Schema {
    Schema {
        kind: Journal::default().into(),
        ..Schema::default()
    }
}

fn open_with(scratch: &Scratch, projector: Option<Arc<dyn Projector>>) -> Vault {
    Vault::open_with(scratch.vault(), OpenOptions::new(), None, projector).unwrap()
}

fn filter(value: serde_json::Value) -> FindQuery {
    serde_json::from_value::<Filter>(value).unwrap().into()
}

fn ids(result: &FindResult) -> Vec<&str> {
    result
        .records
        .iter()
        .filter_map(|record| record.id.as_deref())
        .collect()
}

fn sorted_ids(result: &FindResult) -> Vec<&str> {
    let mut out = ids(result);
    out.sort_unstable();
    out
}

fn collection_ids(collection: &scheda::Collection) -> Vec<String> {
    let mut names = collection.ids().unwrap();
    names.sort();
    names
}

fn document_ids(collection: &scheda::Collection) -> Vec<String> {
    let mut names: Vec<String> = collection
        .documents()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    names.sort();
    names
}

fn journal_line_ids(collection: &scheda::Collection) -> Vec<String> {
    collection
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect()
}

fn seed_mixed_notes(vault: &Vault) {
    vault
        .create_collection("mixed_notes", document_decl(), None)
        .unwrap();
    let collection = vault.collection("mixed_notes").unwrap();
    for (id, body) in MIXED_FILES {
        collection.add(id, body.as_bytes(), None).unwrap();
    }
}

fn seed_mixed_log(vault: &Vault) {
    vault
        .create_collection("mixed_log", journal_decl(), None)
        .unwrap();
    vault
        .collection("mixed_log")
        .unwrap()
        .add("j1", JOURNAL_PAYLOAD, None)
        .unwrap();
}

fn expected_note_ids() -> Vec<String> {
    let mut names: Vec<String> = MIXED_FILES.iter().map(|(id, _)| (*id).to_owned()).collect();
    names.sort();
    names
}

fn index_fail_closed(code: Option<Code>) -> bool {
    matches!(
        code,
        Some(Code::IndexMissing | Code::IndexLexical | Code::IndexVersion)
    )
}

fn find_fail_closed(result: Result<FindResult, Error>) {
    match result {
        Ok(page) => assert!(
            page.records.is_empty(),
            "find must fail closed; got ids {:?}",
            ids(&page)
        ),
        Err(err) => assert!(
            index_fail_closed(err.known_code()),
            "unexpected find error {:?}: {err:?}",
            err.known_code()
        ),
    }
}

fn search_fail_closed(result: Result<SearchHits, Error>) {
    match result {
        Ok(hits) => assert!(
            hits.hits.is_empty(),
            "search must fail closed; got {hits:?}"
        ),
        Err(err) => assert!(
            index_fail_closed(err.known_code()),
            "unexpected search error {:?}: {err:?}",
            err.known_code()
        ),
    }
}

struct YamlTitleProjector;

impl Projector for YamlTitleProjector {
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

    fn project(&self, _collection: &str, _id: &str, raw: &[u8]) -> Result<IndexDoc, Error> {
        let Ok(text) = std::str::from_utf8(raw) else {
            return Ok(IndexDoc::default());
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(rest) = line.strip_prefix("title:") else {
                continue;
            };
            let title = rest.trim();
            if title.is_empty() {
                return Ok(IndexDoc::default());
            }
            return Ok(IndexDoc {
                values: [(
                    String::from("title"),
                    scheda::IndexValue::Text(title.to_owned()),
                )]
                .into(),
            });
        }
        Ok(IndexDoc::default())
    }
}

fn title_projector() -> Arc<dyn Projector> {
    Arc::new(TitleKeepProjector {
        inner: common::json_text_projector(&["title"]),
    })
}

/// `json_text_projector` only fills columns from JSON objects. Opaque
/// YAML/MD/TOML/TXT would otherwise be omitted from `find`. Keep the
/// `title` schema and store whole-file UTF-8 when JSON projection is empty.
struct TitleKeepProjector {
    inner: Arc<dyn Projector>,
}

impl Projector for TitleKeepProjector {
    fn schema(&self, collection: &str) -> Result<IndexSchema, Error> {
        self.inner.schema(collection)
    }

    fn project(&self, collection: &str, id: &str, raw: &[u8]) -> Result<IndexDoc, Error> {
        let doc = self.inner.project(collection, id, raw)?;
        if !doc.values.is_empty() {
            return Ok(doc);
        }
        let Ok(text) = std::str::from_utf8(raw) else {
            return Ok(IndexDoc::default());
        };
        if text.is_empty() {
            return Ok(IndexDoc::default());
        }
        Ok(IndexDoc {
            values: [(
                String::from("title"),
                scheda::IndexValue::Text(text.to_owned()),
            )]
            .into(),
        })
    }
}

#[test]
fn mixed_extensions_round_trip_and_git_diff_after_replace() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, None);
    seed_mixed_notes(&vault);
    let collection = vault.collection("mixed_notes").unwrap();
    for (id, body) in MIXED_FILES {
        assert_eq!(
            collection.get(id).unwrap().raw.as_deref(),
            Some(body.as_bytes())
        );
    }
    assert_eq!(collection_ids(&collection), expected_note_ids());

    vault.begin().unwrap();
    collection
        .replace("note.txt", NOTE_TXT_REPLACED.as_bytes(), None)
        .unwrap();
    let diff = Command::new("git")
        .arg("-C")
        .arg(scratch.vault())
        .arg("diff")
        .env("LC_ALL", "C")
        .output()
        .expect("git executable");
    assert!(
        diff.status.success(),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
    assert!(!diff.stdout.contains(&0), "git diff must not contain NUL");
    let text = String::from_utf8(diff.stdout).unwrap();
    assert!(
        text.contains("f19-replaced-txt-token"),
        "git diff must show replaced text: {text}"
    );
    vault.commit("replace txt").unwrap();
    assert_eq!(
        collection.get("note.txt").unwrap().raw.as_deref(),
        Some(NOTE_TXT_REPLACED.as_bytes())
    );
}

#[test]
fn wipe_index_files_stay_find_search_fail_until_reindex() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, Some(title_projector()));
    seed_mixed_notes(&vault);
    seed_mixed_log(&vault);
    let notes = vault.collection("mixed_notes").unwrap();
    let yaml_raw = notes.get("note.yaml").unwrap().raw.clone().unwrap();
    let history = notes.history("note.yaml", 0, 10).unwrap();
    assert!(
        !history.entries.is_empty(),
        "history of note.yaml must be nonempty"
    );
    let log = vault.collection("mixed_log").unwrap();
    assert_eq!(journal_line_ids(&log), ["j1"]);
    drop(notes);
    drop(log);
    drop(vault);

    fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
    let vault = open_with(&scratch, Some(title_projector()));
    let notes = vault.collection("mixed_notes").unwrap();
    assert_eq!(
        notes.get("note.yaml").unwrap().raw.as_deref(),
        Some(yaml_raw.as_slice())
    );
    assert!(
        !notes
            .history("note.yaml", 0, 10)
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        journal_line_ids(&vault.collection("mixed_log").unwrap()),
        ["j1"]
    );
    assert_eq!(document_ids(&notes), expected_note_ids());

    find_fail_closed(notes.find(&Filter::all().into(), 0, 10));
    search_fail_closed(notes.search(&SearchQuery::default().with_text("txt-only-token"), None));

    vault.reindex().unwrap();
    let all = notes.find(&Filter::all().into(), 0, 10).unwrap();
    assert_eq!(
        sorted_ids(&all),
        ["note.md", "note.toml", "note.txt", "note.yaml"]
    );
}

#[test]
fn yaml_title_projector_hits_yaml_omits_txt() {
    let scratch = Scratch::new();
    let vault = open_with(&scratch, Some(Arc::new(YamlTitleProjector)));
    seed_mixed_notes(&vault);
    let collection = vault.collection("mixed_notes").unwrap();
    let hit = collection
        .find(
            &filter(json!({"term":{"field":"title","value":"yaml-only-token"}})),
            0,
            10,
        )
        .unwrap();
    assert_eq!(ids(&hit), ["note.yaml"]);
    let all = collection.find(&Filter::all().into(), 0, 10).unwrap();
    assert_eq!(ids(&all), ["note.yaml"]);
    assert_eq!(
        collection.get("note.txt").unwrap().raw.as_deref(),
        Some(NOTE_TXT.as_bytes())
    );
}

#[cfg(feature = "encrypt")]
#[test]
fn isolated_and_encrypted_round_trip_find_encrypted_unsupported() {
    let scratch = Scratch::new();
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new().encryption(Encryption::new().passphrase(PASS)),
        None,
    )
    .unwrap();
    vault
        .create_collection("mixed_secret", document_decl().with_isolated(true), None)
        .unwrap();
    vault
        .create_collection("mixed_sealed", document_decl().with_encrypted(true), None)
        .unwrap();
    vault
        .collection("mixed_secret")
        .unwrap()
        .add("nested.txt", PLAIN, None)
        .unwrap();
    vault
        .collection("mixed_sealed")
        .unwrap()
        .add("cipher.txt", PLAIN, None)
        .unwrap();
    assert_eq!(
        vault
            .collection("mixed_secret")
            .unwrap()
            .get("nested.txt")
            .unwrap()
            .raw
            .as_deref(),
        Some(PLAIN)
    );
    assert_eq!(
        vault
            .collection("mixed_sealed")
            .unwrap()
            .get("cipher.txt")
            .unwrap()
            .raw
            .as_deref(),
        Some(PLAIN)
    );
    assert!(
        scratch
            .vault()
            .join(".isolated")
            .join("mixed_secret")
            .join("nested.txt")
            .is_file()
    );
    let err = vault
        .collection("mixed_sealed")
        .unwrap()
        .find(&FindQuery::all(), 0, 10)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    let err = vault
        .find(&["mixed_sealed"], &FindQuery::all(), 0, 10)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}
