//! `journal` segments. Collection names here and
//! in `testdata/journal/` must not appear in `rust/scheda/src`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use scheda::{Code, Filter, FindQuery, OpenOptions, Repeat, Vault};
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
            root: common::scratch_root("scheda-c4", n),
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

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), self.options(), None).unwrap()
    }

    fn write_declaration(&self, file: &str, body: &str) {
        let name = peek_schema_name(body);
        let dir = self.vault().join(name);
        fs::create_dir_all(&dir).unwrap();
        let dest = if file.ends_with(".json") {
            ".collection.json"
        } else {
            ".collection.yaml"
        };
        fs::write(dir.join(dest), body).unwrap();
        common::commit_file(&self.vault(), &dir.join(dest));
    }

    fn log_dir(&self) -> PathBuf {
        self.vault().join("south_log")
    }

    fn lines_dir(&self) -> PathBuf {
        self.vault().join("south_lines")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const LOG: &str = include_str!("../testdata/journal/log.yaml");
const LINES: &str = include_str!("../testdata/journal/lines.yaml");
const ALPHA_JSONL: &str = include_str!("../testdata/journal/alpha.jsonl");

fn rec(msg: &str) -> Vec<u8> {
    rec_fields(json!({ "msg": msg }))
}

fn rec_rep(msg: &str) -> Vec<u8> {
    rec(msg)
}

fn rec_fields(fields: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn open_log(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    scratch.write_declaration("log.yaml", LOG);
    scratch.reopen()
}

fn open_lines(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    scratch.write_declaration("lines.yaml", LINES);
    scratch.reopen()
}

fn jsonl_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.ends_with(".jsonl").then_some(name)
        })
        .collect();
    names.sort();
    names
}

fn seq_jsonl(vault: &Vault, n: u32) -> String {
    format!("{n:08x}-{}.jsonl", vault.writer())
}

#[test]
fn insert_appends_envelope_line() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("p", rec("m"), None).unwrap();
    collection.add("q", rec("m"), None).unwrap();
    let text = fs::read_to_string(scratch.log_dir().join(seq_jsonl(&vault, 1))).unwrap();
    let mut lines = text.lines();
    let a = lines.next().unwrap();
    let b = lines.next().unwrap();
    assert!(text.ends_with('\n'));
    assert_eq!(a, r#"p: {"msg":"m"}"#);
    assert_eq!(b, r#"q: {"msg":"m"}"#);
    assert_eq!(a.replace("p", "q"), b);
}

#[test]
fn replace_one_is_unsupported() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let err = vault
        .collection("south_log")
        .unwrap()
        .replace("a", rec_rep("x"), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn duplicate_id_across_segments_is_conflict() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("one"), None).unwrap();
    collection.add("b", rec("two"), None).unwrap();
    collection.add("c", rec("three"), None).unwrap();
    assert!(jsonl_names(&scratch.log_dir()).len() >= 2);
    let err = collection.add("a", rec("again"), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::DuplicateId));
}

#[test]
fn size_cap_closes_segment_without_rewriting_it() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    collection.add("b", rec("x"), None).unwrap();
    let first = scratch.log_dir().join(seq_jsonl(&vault, 1));
    let before = fs::read(&first).unwrap();
    collection.add("c", rec("x"), None).unwrap();
    assert_eq!(fs::read(&first).unwrap(), before);
    assert!(before.len() as u64 <= 32);
    assert!(scratch.log_dir().join(seq_jsonl(&vault, 2)).is_file());
    common::indexed(&vault);
    let ids: Vec<_> = collection
        .find(&Filter::all().into(), 0, 1000)
        .unwrap()
        .records
        .into_iter()
        .map(|record| record.id.unwrap())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
}

#[test]
fn find_one_uses_collection_and_id_not_segment_name() {
    let scratch = Scratch::new();
    // Writer `c` makes the segment name end in `-c.jsonl`. A substring
    // check for `c.jsonl` would then fail even though the file is not
    // `id.jsonl`.
    let options = OpenOptions::new().writer("c");
    drop(Vault::open(scratch.vault(), options.clone(), None).unwrap());
    scratch.write_declaration("log.yaml", LOG);
    let vault = Vault::open(scratch.vault(), options, None).unwrap();
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    collection.add("b", rec("x"), None).unwrap();
    collection.add("c", rec("x"), None).unwrap();
    let got = collection.get("c").unwrap();
    assert_eq!(got.id.as_deref(), Some("c"));
    assert_eq!(got.collection.as_deref(), Some("south_log"));
    let names = jsonl_names(&scratch.log_dir());
    assert!(
        names.iter().any(|name| name.ends_with("-c.jsonl")),
        "writer c must appear in the segment name: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "c.jsonl"),
        "get must use the segment path, not id.jsonl: {names:?}"
    );
    let err = collection.get("missing").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn get_missing_segment_file_is_io_not_found_with_path() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    let rel = format!("south_log/{}", seq_jsonl(&vault, 1));
    fs::remove_file(scratch.vault().join(&rel)).unwrap();
    let err = collection.get("a").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IoNotFound));
    assert_eq!(err.path.as_deref(), Some(rel.as_str()));
}

#[test]
fn empty_find_and_scan_keep_append_order_across_rotate() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    collection.add("b", rec("x"), None).unwrap();
    collection.add("c", rec("x"), None).unwrap();
    common::indexed(&vault);
    let found = collection.find(&Filter::all().into(), 0, 1000).unwrap();
    assert_eq!(found.total, Some(3));
    let scan_ids: Vec<_> = found
        .records
        .into_iter()
        .map(|record| record.id.unwrap())
        .collect();
    let find_ids: Vec<_> = collection
        .find(&Filter::all().into(), 0, 100)
        .unwrap()
        .records
        .into_iter()
        .map(|record| record.id.unwrap())
        .collect();
    assert_eq!(scan_ids, ["a", "b", "c"]);
    assert_eq!(find_ids, scan_ids);
}

#[test]
fn mid_segment_corruption_fails_check_and_is_not_rewritten() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    collection.add("b", rec("x"), None).unwrap();
    let path = scratch.log_dir().join(seq_jsonl(&vault, 1));
    let original = fs::read_to_string(&path).unwrap();
    let first = original.split_once('\n').unwrap().0;
    let damaged = format!("{first}\nthis is not an envelope\n");
    fs::write(&path, &damaged).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(fs::read_to_string(&path).unwrap(), damaged);
    let a = collection.get("a").unwrap();
    assert_eq!(a.id.as_deref(), Some("a"));
    let err = collection.get("b").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn tail_without_newline_is_not_a_record() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    let path = scratch.log_dir().join(seq_jsonl(&vault, 1));
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str(r#"b: {"msg":"x"}"#);
    fs::write(&path, &text).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(collection.get("a").unwrap().id.as_deref(), Some("a"));
    let err = collection.get("b").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .ends_with(r#"b: {"msg":"x"}"#)
    );
}

#[test]
fn fixture_jsonl_is_utf8_text() {
    assert!(!ALPHA_JSONL.as_bytes().contains(&0));
    assert!(ALPHA_JSONL.contains(r#""msg":"hello""#));
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("log.yaml", LOG);
    fs::create_dir_all(scratch.log_dir()).unwrap();
    fs::write(scratch.log_dir().join("00000001.jsonl"), ALPHA_JSONL).unwrap();
    let vault = scratch.reopen();
    let got = vault.collection("south_log").unwrap().get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    assert_eq!(got.raw.as_deref(), Some(br#"{"msg":"hello"}"#.as_slice()));
    common::indexed(&vault);
    vault.check().unwrap();
}

#[test]
fn journal_accepts_opaque_payload_with_extra_keys() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let record = rec_fields(json!({"msg": "x", "body": "nope"}));
    vault
        .collection("south_log")
        .unwrap()
        .add("a", &record, None)
        .unwrap();
    let got = vault.collection("south_log").unwrap().get("a").unwrap();
    assert_eq!(got.raw.as_deref(), Some(record.as_slice()));
}

#[test]
fn journal_payload_must_not_contain_cr_or_lf() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    let err = collection.add("a", "line\nbreak", None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let err = collection.add("b", "line\rbreak", None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(jsonl_names(&scratch.log_dir()).is_empty());
}

#[test]
fn journal_non_utf8_writes_no_file() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let err = vault
        .collection("south_log")
        .unwrap()
        .add("a", [0xff, 0xfe], None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotUtf8));
    assert!(jsonl_names(&scratch.log_dir()).is_empty());
}

#[test]
fn journal_identity_is_envelope_id_not_json_key() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    let payload = rec_fields(json!({"id": "other", "msg": "hello"}));
    collection.add("alpha", &payload, None).unwrap();
    let text = fs::read_to_string(scratch.log_dir().join(seq_jsonl(&vault, 1))).unwrap();
    assert!(text.starts_with("alpha: "), "{text}");
    assert_eq!(
        collection.get("alpha").unwrap().id.as_deref(),
        Some("alpha")
    );
    assert_eq!(
        collection.get("alpha").unwrap().raw.as_deref(),
        Some(payload.as_slice())
    );
    assert_eq!(
        collection.get("other").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
}

#[test]
fn integer_field_round_trips_as_integer_not_1_0() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    collection
        .add("a", rec_fields(json!({"n": 1})), None)
        .unwrap();
    let path = scratch.lines_dir().join(seq_jsonl(&vault, 1));
    let before = fs::read(&path).unwrap();
    let text = str::from_utf8(&before).unwrap();
    assert_eq!(text, "a: {\"n\":1}\n");
    vault.reindex().unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    let got = collection.get("a").unwrap();
    assert_eq!(got.raw.as_deref(), Some(br#"{"n":1}"#.as_slice()));
}

#[test]
fn reindex_rebuilds_journal_id_map_from_envelopes() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("alpha", rec("ok"), None).unwrap();
    let ids = journal_ids_path(&scratch.vault(), "south_log");
    assert!(ids.is_file());
    fs::remove_file(&ids).unwrap();
    vault.reindex().unwrap();
    let rebuilt = journal_ids_path(&scratch.vault(), "south_log");
    let text = fs::read_to_string(&rebuilt).unwrap();
    assert!(text.contains("alpha"), "{text}");
    let got = collection.get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
}

#[test]
fn get_and_find_include_raw_match_payload() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    let payload = rec_fields(json!({"n": 1}));
    collection.add("a", &payload, None).unwrap();
    let file = fs::read_to_string(scratch.lines_dir().join(seq_jsonl(&vault, 1))).unwrap();
    assert_eq!(file, "a: {\"n\":1}\n");
    let got = collection.get("a").unwrap();
    assert_eq!(got.raw.as_deref(), Some(payload.as_slice()));
    common::indexed(&vault);
    let found = collection
        .find(&FindQuery::all().with_include_raw(true), 0, 10)
        .unwrap();
    assert_eq!(found.records.len(), 1);
    assert_eq!(found.records[0].raw.as_deref(), Some(payload.as_slice()));
    let omitted = collection.find(&Filter::all().into(), 0, 10).unwrap();
    assert!(omitted.records[0].raw.is_none());
}

#[test]
fn journal_line_round_trip_preserves_payload_and_envelope_id() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    let fields = json!({
        "n": 1,
        "neg": -42,
        "x": 0.5,
        "tags": ["a", "b"],
        "meta": {
            "z": "ascii",
            "\u{1F600}": "grin",
            "\u{FB33}": "he",
        },
    });
    let payload = rec_fields(fields.clone());
    collection.add("one", &payload, None).unwrap();
    let path = scratch.lines_dir().join(seq_jsonl(&vault, 1));
    let line = fs::read_to_string(&path).unwrap();
    assert_eq!(
        line,
        format!("one: {}\n", str::from_utf8(&payload).unwrap())
    );

    let got = collection.get("one").unwrap();
    assert_eq!(got.raw.as_deref(), Some(payload.as_slice()));

    let mut again = fields;
    if let serde_json::Value::Object(map) = &mut again {
        map.insert(String::from("id"), json!("other"));
    }
    let second = rec_fields(again);
    collection.add("two", &second, None).unwrap();
    assert_eq!(collection.get("two").unwrap().id.as_deref(), Some("two"));
    assert_eq!(
        collection.get("other").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert_eq!(
        collection.get("two").unwrap().raw.as_deref(),
        Some(second.as_slice())
    );
}

#[test]
fn non_integral_number_payload_is_opaque() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let payload = rec_fields(json!({"n": 1.5}));
    vault
        .collection("south_lines")
        .unwrap()
        .add("frac", &payload, None)
        .unwrap();
    let got = vault
        .collection("south_lines")
        .unwrap()
        .get("frac")
        .unwrap();
    assert_eq!(got.raw.as_deref(), Some(payload.as_slice()));
}

#[test]
fn integer_above_2_pow_53_is_opaque_payload() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    collection
        .add(
            "edge",
            rec_fields(json!({"n": 9_007_199_254_740_992_u64})),
            None,
        )
        .unwrap();
    let text = fs::read_to_string(scratch.lines_dir().join(seq_jsonl(&vault, 1))).unwrap();
    assert!(text.starts_with("edge: "), "{text}");
    assert!(text.contains(r#""n":9007199254740992"#), "{text}");
    collection
        .add(
            "wide",
            rec_fields(json!({"n": 18_014_398_509_481_984_u64})),
            None,
        )
        .unwrap();
    assert_eq!(collection.get("wide").unwrap().id.as_deref(), Some("wide"));
}

#[test]
fn lines_matches_find_append_order_and_raw_across_rotate() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    collection.add("b", rec("x"), None).unwrap();
    collection.add("c", rec("x"), None).unwrap();
    assert!(jsonl_names(&scratch.log_dir()).len() >= 2);

    common::indexed(&vault);
    let find_ids: Vec<_> = collection
        .find(&Filter::all().into(), 0, 100)
        .unwrap()
        .records
        .into_iter()
        .map(|record| record.id.unwrap())
        .collect();
    assert_eq!(find_ids, ["a", "b", "c"]);

    let mut walked = Vec::new();
    let mut raw_concat = Vec::new();
    for record in collection.lines().unwrap() {
        let record = record.unwrap();
        let id = record.id.clone().unwrap();
        let got = collection.get(&id).unwrap();
        assert_eq!(record.raw, got.raw);
        raw_concat.extend_from_slice(record.raw.as_deref().unwrap());
        walked.push(id);
    }
    assert_eq!(walked, find_ids);

    let mut file_payloads = Vec::new();
    for name in jsonl_names(&scratch.log_dir()) {
        let text = fs::read_to_string(scratch.log_dir().join(name)).unwrap();
        for line in text.lines() {
            let Some((_, payload)) = line.split_once(": ") else {
                panic!("missing envelope delimiter: {line}");
            };
            file_payloads.extend_from_slice(payload.as_bytes());
        }
    }
    assert_eq!(raw_concat, file_payloads);
}

#[test]
fn lines_survives_deleted_index() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    collection.add("b", rec("x"), None).unwrap();
    collection.add("c", rec("x"), None).unwrap();
    // Collection shares the Index writer; drop it before deleting `.scheda`.
    drop(collection);
    drop(vault);

    fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
    let vault = scratch.reopen();
    let ids: Vec<_> = vault
        .collection("south_log")
        .unwrap()
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(
        vault
            .collection("south_log")
            .unwrap()
            .get("b")
            .unwrap()
            .id
            .as_deref(),
        Some("b")
    );
}

#[test]
fn lines_on_document_is_unsupported() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration(
        "notes.yaml",
        include_str!("../testdata/document/notes.yaml"),
    );
    let vault = scratch.reopen();
    let err = match vault.collection("north_notes").unwrap().lines() {
        Ok(_) => panic!("document lines must be unsupported"),
        Err(err) => err,
    };
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn lines_truncated_tail_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    let path = scratch.log_dir().join(seq_jsonl(&vault, 1));
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str(r#"b: {"msg":"x"}"#);
    fs::write(&path, &text).unwrap();
    let err = collection
        .lines()
        .unwrap()
        .find_map(Result::err)
        .expect("truncated tail");
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

fn journal_ids_path(vault: &std::path::Path, collection: &str) -> PathBuf {
    let generation = fs::read_to_string(vault.join(".scheda").join("index").join("current"))
        .unwrap()
        .trim()
        .to_owned();
    vault
        .join(".scheda")
        .join("index")
        .join(generation)
        .join("journal")
        .join(collection)
        .join("ids")
}

#[test]
fn journal_id_map_outside_collection_is_not_read() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    vault
        .collection("south_log")
        .unwrap()
        .add("kept", rec("ok"), None)
        .unwrap();
    // Tamper only once no indexer of this handle can rewrite the file.
    common::indexed(&vault);
    vault.close().unwrap();
    let marker = "UNIQUE_LEAK_MARKER";
    let outside = scratch.root.join("secret.txt");
    fs::write(&outside, marker).unwrap();
    let ids = journal_ids_path(&scratch.vault(), "south_log");
    let mut text = fs::read_to_string(&ids).unwrap();
    let abs = serde_json::json!({"id": "stolen", "path": outside.to_str().unwrap()});
    let parent = serde_json::json!({"id": "parent", "path": "../secret.txt"});
    text.push_str(&format!("{abs}\n{parent}\n"));
    fs::write(&ids, text).unwrap();
    let vault = scratch.reopen();
    let collection = vault.collection("south_log").unwrap();
    assert_eq!(collection.get("kept").unwrap().id.as_deref(), Some("kept"));
    let stolen = collection.get("stolen").unwrap_err();
    assert_eq!(stolen.known_code(), Some(Code::NotFound));
    assert!(!stolen.to_string().contains(marker));
    assert_eq!(
        collection.get("parent").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    let check = vault.check().unwrap_err();
    assert_eq!(check.known_code(), Some(Code::IndexCorrupt));
}

fn as_url(path: &Path) -> String {
    let path = path.to_str().expect("utf-8 path");
    if let Some(rest) = path.strip_prefix('/') {
        format!("file:///{rest}")
    } else {
        format!("file:///{}", path.replace('\\', "/"))
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .expect("git executable");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn commit_all(dir: &Path, message: &str) {
    git_ok(dir, &["add", "-A"]);
    git_ok(
        dir,
        &[
            "-c",
            "user.name=Scheda",
            "-c",
            "user.email=scheda@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            message,
        ],
    );
}

#[test]
fn two_clones_write_disjoint_journal_segments() {
    let scratch = Scratch::new();
    let source = scratch.root.join("source");
    drop(Vault::open(&source, OpenOptions::new(), None).unwrap());
    let dir = source.join("south_log");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".collection.yaml"), LOG).unwrap();
    commit_all(&source, "schema");
    let bare = scratch.root.join("bare");
    git_ok(
        &source,
        &[
            "clone",
            "--bare",
            "--",
            &as_url(&source),
            &bare.to_string_lossy(),
        ],
    );
    let remote = as_url(&bare);
    let alice_dir = scratch.root.join("alice");
    let bob_dir = scratch.root.join("bob");
    let alice = Vault::open(
        &alice_dir,
        OpenOptions::new().remote_repo(&remote).writer("alice"),
        None,
    )
    .unwrap();
    let bob = Vault::open(
        &bob_dir,
        OpenOptions::new().remote_repo(&remote).writer("bob"),
        None,
    )
    .unwrap();
    assert_eq!(alice.writer(), "alice");
    assert_eq!(bob.writer(), "bob");
    alice
        .collection("south_log")
        .unwrap()
        .add("from-alice", rec("a"), None)
        .unwrap();
    alice.push().unwrap();
    bob.pull().unwrap();
    bob.collection("south_log")
        .unwrap()
        .add("from-bob", rec("b"), None)
        .unwrap();
    let alice_names = jsonl_names(&alice_dir.join("south_log"));
    let bob_names = jsonl_names(&bob_dir.join("south_log"));
    assert!(alice_names.contains(&String::from("00000001-alice.jsonl")));
    assert!(bob_names.contains(&String::from("00000001-bob.jsonl")));
    assert!(!alice_names.contains(&String::from("00000001-bob.jsonl")));
    bob.push().unwrap();
    alice.pull().unwrap();
    let merged = jsonl_names(&alice_dir.join("south_log"));
    assert!(merged.contains(&String::from("00000001-alice.jsonl")));
    assert!(merged.contains(&String::from("00000001-bob.jsonl")));
    let ids: Vec<_> = alice
        .collection("south_log")
        .unwrap()
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["from-alice", "from-bob"]);
}

#[test]
fn writerless_segment_is_read_and_not_appended() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("log.yaml", LOG);
    fs::create_dir_all(scratch.log_dir()).unwrap();
    fs::write(
        scratch.log_dir().join("00000001.jsonl"),
        "old: {\"msg\":\"kept\"}\n",
    )
    .unwrap();
    let vault = scratch.reopen();
    let collection = vault.collection("south_log").unwrap();
    assert_eq!(collection.get("old").unwrap().id.as_deref(), Some("old"));
    let walked: Vec<_> = collection
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(walked, ["old"]);
    let before = fs::read(scratch.log_dir().join("00000001.jsonl")).unwrap();
    collection.add("new", rec("fresh"), None).unwrap();
    assert_eq!(
        fs::read(scratch.log_dir().join("00000001.jsonl")).unwrap(),
        before
    );
    assert!(scratch.log_dir().join(seq_jsonl(&vault, 1)).is_file());
    let ids: Vec<_> = collection
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["old", "new"]);
}

#[test]
fn rotation_caps_are_per_writer() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_declaration("log.yaml", LOG);
    fs::create_dir_all(scratch.log_dir()).unwrap();
    fs::write(
        scratch.log_dir().join("00000001-bob.jsonl"),
        "bob-line: {\"msg\":\"x\"}\n",
    )
    .unwrap();
    let vault = Vault::open(scratch.vault(), OpenOptions::new().writer("alice"), None).unwrap();
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x"), None).unwrap();
    collection.add("b", rec("x"), None).unwrap();
    let alice_first = scratch.log_dir().join("00000001-alice.jsonl");
    let before_alice = fs::read(&alice_first).unwrap();
    let before_bob = fs::read(scratch.log_dir().join("00000001-bob.jsonl")).unwrap();
    collection.add("c", rec("x"), None).unwrap();
    assert_eq!(fs::read(&alice_first).unwrap(), before_alice);
    assert_eq!(
        fs::read(scratch.log_dir().join("00000001-bob.jsonl")).unwrap(),
        before_bob
    );
    assert!(scratch.log_dir().join("00000002-alice.jsonl").is_file());
    let ids: Vec<_> = collection
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["a", "b", "bob-line", "c"]);
}

#[test]
fn overridden_writer_must_be_a_token() {
    let scratch = Scratch::new();
    let err = Vault::open(scratch.vault(), OpenOptions::new().writer(""), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let err = Vault::open(scratch.vault(), OpenOptions::new().writer("Alice"), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let err =
        Vault::open(scratch.vault(), OpenOptions::new().writer("00000001"), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let err = Vault::open(
        scratch.vault(),
        OpenOptions::new().writer("abcdef01-node"),
        None,
    )
    .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn foreign_thread_cannot_join_open_transaction() {
    let scratch = Scratch::new();
    let vault = Arc::new(open_log(&scratch));
    vault.begin().unwrap();
    vault
        .collection("south_log")
        .unwrap()
        .add("a", rec("in-txn"), None)
        .unwrap();
    let other = Arc::clone(&vault);
    let err = thread::spawn(move || {
        other
            .collection("south_log")
            .unwrap()
            .add("b", rec("intruder"), None)
    })
    .join()
    .expect("thread")
    .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Locked));
    assert_eq!(err.known_repeat(), Some(Repeat::Allowed));
    let committer = Arc::clone(&vault);
    let err = thread::spawn(move || committer.commit("nope"))
        .join()
        .expect("thread")
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Locked));
    vault.commit("owner").unwrap();
    let collection = vault.collection("south_log").unwrap();
    assert_eq!(collection.get("a").unwrap().id.as_deref(), Some("a"));
    assert_eq!(
        collection.get("b").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
}

#[test]
fn payload_with_marker_text_keeps_other_lines_readable() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    let quoted = rec_fields(json!({"n": 2, "note": "quoting a diff: >>>>>>> theirs <<<<<<< ours"}));
    collection
        .add("a", rec_fields(json!({"n": 1})), None)
        .unwrap();
    collection.add("b", &quoted, None).unwrap();
    collection
        .add("c", rec_fields(json!({"n": 3})), None)
        .unwrap();
    let segment = scratch.lines_dir().join(seq_jsonl(&vault, 1));
    let text = fs::read_to_string(&segment).unwrap();
    assert!(text.contains(">>>>>>> theirs"), "{text}");
    for id in ["a", "b", "c"] {
        assert_eq!(collection.get(id).unwrap().id.as_deref(), Some(id));
    }
    assert_eq!(
        collection.get("b").unwrap().raw.as_deref(),
        Some(quoted.as_slice())
    );
    let ids: Vec<String> = collection
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
    common::indexed(&vault);
    let found = collection.find(&FindQuery::all(), 0, 10).unwrap();
    assert_eq!(found.total, Some(3));
    drop(vault);
    let vault = scratch.reopen();
    common::indexed(&vault);
    let collection = vault.collection("south_lines").unwrap();
    assert_eq!(
        collection.find(&FindQuery::all(), 0, 10).unwrap().total,
        Some(3)
    );
    assert_eq!(collection.get("a").unwrap().id.as_deref(), Some("a"));
    vault.check().unwrap();
}

#[test]
fn conflict_marker_line_in_segment_fails_check() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    collection
        .add("a", rec_fields(json!({"n": 1})), None)
        .unwrap();
    let segment = scratch.lines_dir().join(seq_jsonl(&vault, 1));
    let mut text = fs::read_to_string(&segment).unwrap();
    text.push_str("<<<<<<< HEAD\nb: {\"n\":2}\n=======\nb: {\"n\":3}\n>>>>>>> theirs\n");
    fs::write(&segment, text).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::ConflictMarkers));
    assert!(err.to_string().contains("south_lines/"), "{err}");
    let lines: Vec<_> = collection.lines().unwrap().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[1].as_ref().unwrap_err().known_code(),
        Some(Code::ConflictMarkers)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_segment_is_not_followed_on_append_and_check_names_it() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let outside = scratch.root.join("outside.jsonl");
    fs::write(&outside, "z: {\"n\":9}\n").unwrap();
    let link = scratch.lines_dir().join(seq_jsonl(&vault, 1));
    symlink(&outside, &link).unwrap();
    let collection = vault.collection("south_lines").unwrap();
    let err = collection
        .add("a", rec_fields(json!({"n": 1})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(
        err.path.as_deref(),
        Some(format!("south_lines/{}", seq_jsonl(&vault, 1)).as_str())
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "z: {\"n\":9}\n");
    assert_eq!(collection.lines().unwrap().count(), 0);
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(
        err.path.as_deref(),
        Some(format!("south_lines/{}", seq_jsonl(&vault, 1)).as_str())
    );
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
