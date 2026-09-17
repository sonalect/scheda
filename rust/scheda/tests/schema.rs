//! C2: Schema load and reject (`05`, `11`). Collection names here and in
//! `testdata/schema/` must not appear in `rust/scheda/src` (MOD-1, SCH-1).

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, Kind, OpenOptions, Vault};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-c2", n),
        }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn options(&self) -> OpenOptions {
        OpenOptions::new()
    }

    fn init(&self) -> Vault {
        let vault = Vault::open(self.vault(), self.options(), None).unwrap();
        vault.begin().unwrap();
        vault
    }

    fn reopen(&self) -> Result<Vault, scheda::Error> {
        Vault::open(self.vault(), self.options(), None)
    }

    fn write_schema(&self, file: &str, body: &str) {
        let name = peek_schema_name(body);
        let dir = self.vault().join(name);
        fs::create_dir_all(&dir).unwrap();
        let dest = if file.ends_with(".json") {
            ".collection.json"
        } else {
            ".collection.yaml"
        };
        fs::write(dir.join(dest), body).unwrap();
    }
}

const WEST: &str = include_str!("../testdata/schema/west.yaml");
const EAST: &str = include_str!("../testdata/schema/east.yaml");
const WEST_JSON: &str = include_str!("../testdata/schema/west.json");
const WEST_MISSING_KIND: &str = include_str!("../testdata/schema/west_missing_kind.yaml");
const WEST_MISSING_VERSION: &str = include_str!("../testdata/schema/west_missing_version.yaml");
const WEST_ON_WRITE: &str = include_str!("../testdata/schema/west_on_write.yaml");
const WIKI: &str = include_str!("../testdata/schema/wiki.md");
const EAST_LOG: &str = include_str!("../testdata/schema/east_log.yaml");
const EAST_LOG_OVERSIZE: &str = include_str!("../testdata/schema/east_log_oversize.yaml");

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn schema_under_git_is_read_on_open_and_check() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let vault = scratch.reopen().unwrap();
    assert!(scratch.vault().join(".git").is_dir());
    let collection = vault.collection("west_docs").unwrap();
    assert_eq!(collection.kind(), Kind::Document);
    assert_eq!(collection.version(), 1);
    vault.check().unwrap();
}

#[test]
fn missing_kind_is_rejected_on_open() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST_MISSING_KIND);
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn missing_version_is_rejected_on_open() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST_MISSING_VERSION);
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn version_2_is_rejected_on_open() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", "name: west_docs\nkind: document\nversion: 2\n");
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn executable_blob_key_is_rejected() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST_ON_WRITE);
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

const WEST_LAYOUT: &str = "\
name: west_docs
kind: document
version: 1
layout:
  - '{id}.md'
";

#[test]
fn layout_key_is_invalid_on_open_and_check() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST_LAYOUT);
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));

    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let vault = scratch.reopen().unwrap();
    scratch.write_schema("west.yaml", WEST_LAYOUT);
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));

    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "west.json",
        r#"{"name":"west_docs","kind":"document","version":1,"layout":["x.md"]}"#,
    );
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn leftover_fields_key_is_invalid_on_open() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema(
        "west.yaml",
        "name: west_docs\nkind: document\nversion: 1\nfields: []\n",
    );
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn leftover_schema_yaml_is_invalid_on_open_and_check() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    fs::write(scratch.vault().join("west_docs").join(".schema.yaml"), WEST).unwrap();
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));

    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let vault = scratch.reopen().unwrap();
    fs::write(scratch.vault().join("west_docs").join(".schema.yaml"), WEST).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn unmarked_markdown_is_not_an_extractor() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    fs::write(scratch.vault().join("README.md"), WIKI).unwrap();
    let vault = scratch.reopen().unwrap();
    vault.collection("west_docs").unwrap();
    vault.check().unwrap();
}

#[test]
fn journal_default_segment_cap_is_8_mib() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", EAST_LOG);
    let vault = scratch.reopen().unwrap();
    let collection = vault.collection("east_log").unwrap();
    assert_eq!(collection.segment_max_bytes(), Some(8 * 1024 * 1024));
}

#[test]
fn journal_cap_above_32_mib_is_rejected() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", EAST_LOG_OVERSIZE);
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn two_vaults_with_different_collection_names() {
    let west = Scratch::new();
    let east = Scratch::new();
    drop(west.init());
    drop(east.init());
    west.write_schema("west.yaml", WEST);
    east.write_schema("east.yaml", EAST);
    let west_vault = west.reopen().unwrap();
    let east_vault = east.reopen().unwrap();
    west_vault.collection("west_docs").unwrap();
    east_vault.collection("east_docs").unwrap();
    assert!(west_vault.collection("east_docs").is_err());
    assert!(east_vault.collection("west_docs").is_err());
    assert_eq!(west_vault.versions().unwrap(), &[1]);
    assert_eq!(east_vault.versions().unwrap(), &[1]);
}

#[test]
fn missing_collection_is_not_found() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.yaml", WEST);
    let vault = scratch.reopen().unwrap();
    let err = vault.collection("east_docs").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn json_collection_file_loads() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("west.json", WEST_JSON);
    let vault = scratch.reopen().unwrap();
    assert_eq!(
        vault.collection("west_docs").unwrap().kind(),
        Kind::Document
    );
}

#[test]
fn collection_dir_must_match_name() {
    let scratch = Scratch::new();
    drop(scratch.init());
    let dir = scratch.vault().join("aaaaaaaaaaaaaaaa");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".collection.yaml"), WEST).unwrap();
    let err = scratch.reopen().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn check_rereads_schema_from_disk() {
    let scratch = Scratch::new();
    let vault = scratch.init();
    scratch.write_schema("west.yaml", WEST_MISSING_VERSION);
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

fn peek_schema_name(body: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            return rest.trim().trim_matches('"').to_string();
        }
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("name")?.as_str().map(str::to_owned))
        .expect("schema fixture name")
}
