//! C4: `journal` segments (`03`, `04`, `07`, `11`). Collection names here and
//! in `testdata/journal/` must not appear in `rust/scheda/src` (MOD-1, SCH-1).

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
    scratch.write_schema("log.yaml", LOG);
    scratch.reopen()
}

fn open_lines(scratch: &Scratch) -> Vault {
    drop(scratch.init());
    scratch.write_schema("lines.yaml", LINES);
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
fn insert_appends_canonical_json_line() {
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
    assert_eq!(a, r#"{"id":"p","msg":"m"}"#);
    assert_eq!(b, r#"{"id":"q","msg":"m"}"#);
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
    assert!(before.len() as u64 <= 45);
    assert!(scratch.log_dir().join(seq_jsonl(&vault, 2)).is_file());
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
    scratch.write_schema("log.yaml", LOG);
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
    let damaged = format!("{first}\nthis is not a json object\n");
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
    text.push_str(r#"{"id":"b","msg":"x"}"#);
    fs::write(&path, &text).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(collection.get("a").unwrap().id.as_deref(), Some("a"));
    let err = collection.get("b").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .ends_with(r#"{"id":"b","msg":"x"}"#)
    );
}

#[test]
fn fixture_jsonl_is_utf8_text() {
    assert!(!ALPHA_JSONL.as_bytes().contains(&0));
    assert!(ALPHA_JSONL.contains(r#""msg":"hello""#));
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("log.yaml", LOG);
    fs::create_dir_all(scratch.log_dir()).unwrap();
    fs::write(scratch.log_dir().join("00000001.jsonl"), ALPHA_JSONL).unwrap();
    let vault = scratch.reopen();
    let got = vault.collection("south_log").unwrap().get("alpha").unwrap();
    assert_eq!(got.id.as_deref(), Some("alpha"));
    vault.check().unwrap();
}

#[test]
fn journal_rejects_markdown_body() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let record = rec_fields(json!({"msg": "x", "body": "nope"}));
    let err = vault
        .collection("south_log")
        .unwrap()
        .add("a", record, None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
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
    assert!(text.contains(r#""n":1"#), "{text}");
    assert!(!text.contains(r#""n":1.0"#), "{text}");
    vault.reindex().unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    let got = collection.get("a").unwrap();
    assert_eq!(got.raw.as_deref(), Some(before.as_slice()));
}

#[test]
fn get_and_find_include_raw_match_segment_line() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    collection
        .add("a", rec_fields(json!({"n": 1})), None)
        .unwrap();
    let line = fs::read(scratch.lines_dir().join(seq_jsonl(&vault, 1))).unwrap();
    let got = collection.get("a").unwrap();
    assert_eq!(got.raw.as_deref(), Some(line.as_slice()));
    let found = collection
        .find(&FindQuery::all().with_include_raw(true), 0, 10)
        .unwrap();
    assert_eq!(found.records.len(), 1);
    assert_eq!(found.records[0].raw.as_deref(), Some(line.as_slice()));
    let omitted = collection.find(&Filter::all().into(), 0, 10).unwrap();
    assert!(omitted.records[0].raw.is_none());
}

#[test]
fn journal_line_round_trip_fixture_is_rfc_8785() {
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
    collection
        .add("one", rec_fields(fields.clone()), None)
        .unwrap();
    let path = scratch.lines_dir().join(seq_jsonl(&vault, 1));
    let line = fs::read_to_string(&path).unwrap();
    let expected = "{\"id\":\"one\",\"meta\":{\"z\":\"ascii\",\"\u{1F600}\":\"grin\",\"\u{FB33}\":\"he\"},\"n\":1,\"neg\":-42,\"tags\":[\"a\",\"b\"],\"x\":0.5}\n";
    assert_eq!(line, expected);

    let got = collection.get("one").unwrap();
    assert_eq!(got.raw.as_deref(), Some(line.as_bytes()));

    let mut again = fields;
    if let serde_json::Value::Object(map) = &mut again {
        map.insert(String::from("id"), json!("two"));
    }
    collection.add("two", rec_fields(again), None).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let mut lines = text.lines();
    let first = lines.next().unwrap();
    let second = lines.next().unwrap();
    assert_eq!(second, first.replace("one", "two"));
}

#[test]
fn non_integral_integer_field_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let err = vault
        .collection("south_lines")
        .unwrap()
        .add("frac", rec_fields(json!({"n": 1.5})), None)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(!scratch.lines_dir().join(seq_jsonl(&vault, 1)).is_file());
}

#[test]
fn integer_above_2_pow_53_is_invalid() {
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
    assert!(text.contains(r#""n":9007199254740992"#), "{text}");
    let err = collection
        .add(
            "wide",
            rec_fields(json!({"n": 18_014_398_509_481_984_u64})),
            None,
        )
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
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

    let mut file_concat = Vec::new();
    for name in jsonl_names(&scratch.log_dir()) {
        file_concat.extend(fs::read(scratch.log_dir().join(name)).unwrap());
    }
    assert_eq!(raw_concat, file_concat);
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
    scratch.write_schema(
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
    text.push_str(r#"{"id":"b","msg":"x"}"#);
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
    fs::write(dir.join(".schema.yaml"), LOG).unwrap();
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
    scratch.write_schema("log.yaml", LOG);
    fs::create_dir_all(scratch.log_dir()).unwrap();
    fs::write(
        scratch.log_dir().join("00000001.jsonl"),
        "{\"id\":\"old\",\"msg\":\"kept\"}\n",
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
    scratch.write_schema("log.yaml", LOG);
    fs::create_dir_all(scratch.log_dir()).unwrap();
    fs::write(
        scratch.log_dir().join("00000001-bob.jsonl"),
        "{\"id\":\"bob-line\",\"msg\":\"x\"}\n",
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
