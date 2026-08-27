//! C8: journal `id` map and OPS-1 / OPS-3 size (IDX-5, LIB-16).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use git2::Repository;
use scheda::{Code, Embedder, OpenOptions, RecordAdd, SearchQuery, Vault};
use serde_json::json;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c8-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn write_schema(&self, file: &str, body: &str) {
        let name = peek_schema_name(body);
        let dir = self.vault().join(name);
        fs::create_dir_all(&dir).unwrap();
        let dest = if file.ends_with(".json") {
            ".schema.json"
        } else {
            ".schema.yaml"
        };
        fs::write(dir.join(dest), body).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const LOG: &str = include_str!("../testdata/journal/log.yaml");

// Unix CI loads 10⁴ journal / 10³ document. Windows is one tenth so the
// same get / extra add / reindex / search checks finish under Bazel
// `large` (900s) on NTFS. Bulk insert uses one transaction; a git commit
// per Record is not this check.
const OPS_JOURNAL: u64 = if cfg!(windows) { 1_000 } else { 10_000 };
const OPS_DOCS: u64 = if cfg!(windows) { 100 } else { 1_000 };

const OPS_LOG: &str = "\
name: ops_log
kind: journal
version: 1
segment_max_lines: 100
fields:
  - name: msg
    string: {}
";

const OPS_NOTES: &str = "\
name: north_notes
kind: document
version: 1
fields:
  - name: title
    required: true
    string: {}
  - name: tag
    nullable: true
    string: {}
  - name: count
    integer: {}
";

struct OpsEmbedder;

impl Embedder for OpsEmbedder {
    fn model_id(&self) -> &str {
        "ops-stub"
    }

    fn dimensions(&self) -> usize {
        8
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, scheda::Error> {
        let mut vector = vec![0.0; 8];
        for (i, byte) in text.bytes().enumerate() {
            vector[i % 8] += f32::from(byte) / 255.0;
        }
        Ok(vector)
    }
}

fn rec_log(msg: &str) -> RecordAdd {
    serde_json::from_value(json!({
        "fields": {"msg": msg},
    }))
    .unwrap()
}

fn rec_note(title: &str) -> RecordAdd {
    serde_json::from_value(json!({
        "fields": {"title": title},
    }))
    .unwrap()
}

fn open_log(scratch: &Scratch) -> Vault {
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema("log.yaml", LOG);
    Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap()
}

#[test]
fn find_one_does_not_read_unrelated_segments() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("early", rec_log("x")).unwrap();
    collection.add("mid", rec_log("x")).unwrap();
    collection.add("late", rec_log("x")).unwrap();
    let log_dir = scratch.vault().join("south_log");
    assert!(log_dir.join("00000001.jsonl").is_file());
    fs::write(log_dir.join("ffffffff.jsonl"), vec![0u8; 2 * 1024 * 1024]).unwrap();
    let got = collection.get("early").unwrap();
    assert_eq!(got.id.as_deref(), Some("early"));
    let err = collection.get("missing").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn writer_updates_map_human_edit_needs_reindex() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec_log("x")).unwrap();
    let ids = vault
        .index_dir()
        .join("journal")
        .join("south_log")
        .join("ids");
    assert!(ids.is_file());
    let log = scratch.vault().join("south_log").join("00000001.jsonl");
    let mut text = fs::read_to_string(&log).unwrap();
    text.push_str(r#"{"id":"human","msg":"edit"}"#);
    text.push('\n');
    fs::write(&log, text).unwrap();
    let err = collection.get("human").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    vault.reindex().unwrap();
    let got = collection.get("human").unwrap();
    assert_eq!(got.id.as_deref(), Some("human"));
}

#[test]
fn missing_map_falls_back_to_scan() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("kept", rec_log("x")).unwrap();
    let journal_index = vault.index_dir().join("journal");
    fs::remove_dir_all(&journal_index).unwrap();
    drop(vault);
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
    let got = vault.collection("south_log").unwrap().get("kept").unwrap();
    assert_eq!(got.id.as_deref(), Some("kept"));
}

#[test]
fn ops_fixture_find_and_insert_and_reindex() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap());
    scratch.write_schema("log.yaml", OPS_LOG);
    scratch.write_schema("notes.yaml", OPS_NOTES);
    let vault = Vault::open(
        scratch.vault(),
        OpenOptions::new(),
        Some(std::sync::Arc::new(OpsEmbedder)),
    )
    .unwrap();
    // One transaction: the check is Index / get / reindex / search on this
    // volume, not a git commit per Record. GIT-2: that batch is one commit.
    let commits_before = log_len(vault.path());
    vault.begin().unwrap();
    let log = vault.collection("ops_log").unwrap();
    for i in 0..OPS_JOURNAL {
        log.add(&format!("j{i:05}"), rec_log("x")).unwrap();
    }
    let notes = vault.collection("north_notes").unwrap();
    for i in 0..OPS_DOCS {
        notes
            .add(&format!("d{i:04}"), rec_note(&format!("t{i}")))
            .unwrap();
    }
    assert_eq!(log_len(vault.path()), commits_before);
    vault.commit("ops fixture").unwrap();
    assert_eq!(log_len(vault.path()), commits_before + 1);
    let first_journal = format!("j{:05}", 0);
    let mid_journal = format!("j{:05}", OPS_JOURNAL / 2);
    let last_journal = format!("j{:05}", OPS_JOURNAL - 1);
    let log_dir = scratch.vault().join("ops_log");
    let segment_files = fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".jsonl"))
        })
        .count();
    assert!(
        segment_files >= 2,
        "OPS volume must rotate so lines walks more than one segment"
    );
    let mut n = 0u64;
    let mut prev: Option<String> = None;
    for record in log.lines().unwrap() {
        let record = record.unwrap();
        let id = record.id.as_deref().unwrap();
        if let Some(previous) = prev.as_deref() {
            assert!(id > previous, "append order: {id} after {previous}");
        }
        prev = record.id.clone();
        n += 1;
    }
    assert_eq!(n, OPS_JOURNAL);
    assert_eq!(prev.as_deref(), Some(last_journal.as_str()));
    for id in [&first_journal, &mid_journal, &last_journal] {
        let got = log.get(id).unwrap();
        assert_eq!(got.id.as_deref(), Some(id.as_str()));
    }
    let first_doc = format!("d{:04}", 0);
    let last_doc = format!("d{:04}", OPS_DOCS - 1);
    notes.get(&first_doc).unwrap();
    notes.get(&last_doc).unwrap();
    let extra = format!("j{OPS_JOURNAL:05}");
    log.add(&extra, rec_log("x")).unwrap();
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(OPS_JOURNAL + OPS_DOCS + 1));
    assert_eq!(counts.skipped_invalid, Some(0));
    assert_eq!(counts.segments_seen, Some(OPS_JOURNAL / 100 + 1));
    let format = fs::read_to_string(vault.index_dir().join("format")).unwrap();
    assert_eq!(format.trim(), "3");
    let mid_doc = OPS_DOCS / 2;
    let mid_title = format!("t{mid_doc}");
    let mid_id = format!("d{mid_doc:04}");
    let hits = notes
        .search(
            &SearchQuery::default().with_text(&mid_title).with_k(10),
            None,
        )
        .unwrap();
    assert!(
        hits.hits
            .iter()
            .any(|hit| hit.id.as_deref() == Some(mid_id.as_str())),
        "hybrid search expected {mid_id} in k"
    );
}

fn log_len(dir: &Path) -> usize {
    let repo = Repository::open(dir).unwrap();
    let mut walk = repo.revwalk().unwrap();
    if walk.push_head().is_err() {
        return 0;
    }
    walk.count()
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
