//! C4: `journal` segments (`03`, `04`, `07`, `11`). Collection names here and
//! in `testdata/journal/` must not appear in `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, Filter, FindQuery, OpenOptions, RecordAdd, RecordReplace, Vault};
use serde_json::json;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-c4-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
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

fn rec(msg: &str) -> RecordAdd {
    serde_json::from_value(json!({
        "fields": { "msg": msg },
    }))
    .unwrap()
}

fn rec_rep(msg: &str) -> RecordReplace {
    serde_json::from_value(json!({
        "fields": { "msg": msg },
    }))
    .unwrap()
}

fn rec_fields(fields: serde_json::Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
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

#[test]
fn insert_appends_canonical_json_line() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("p", rec("m")).unwrap();
    collection.add("q", rec("m")).unwrap();
    let text = fs::read_to_string(scratch.log_dir().join("00000001.jsonl")).unwrap();
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
        .replace("a", rec_rep("x"))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Unsupported));
    assert_eq!(err.known_code(), Some(Code::Unsupported));
}

#[test]
fn duplicate_id_across_segments_is_conflict() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("one")).unwrap();
    collection.add("b", rec("two")).unwrap();
    collection.add("c", rec("three")).unwrap();
    assert!(jsonl_names(&scratch.log_dir()).len() >= 2);
    let err = collection.add("a", rec("again")).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::DuplicateId));
}

#[test]
fn size_cap_closes_segment_without_rewriting_it() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x")).unwrap();
    collection.add("b", rec("x")).unwrap();
    let first = scratch.log_dir().join("00000001.jsonl");
    let before = fs::read(&first).unwrap();
    collection.add("c", rec("x")).unwrap();
    assert_eq!(fs::read(&first).unwrap(), before);
    assert!(before.len() as u64 <= 45);
    assert!(scratch.log_dir().join("00000002.jsonl").is_file());
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
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x")).unwrap();
    collection.add("b", rec("x")).unwrap();
    collection.add("c", rec("x")).unwrap();
    let got = collection.get("c").unwrap();
    assert_eq!(got.id.as_deref(), Some("c"));
    assert_eq!(got.collection.as_deref(), Some("south_log"));
    assert_eq!(got.valid, Some(true));
    assert!(
        got.path
            .as_deref()
            .is_some_and(|path| path.ends_with(".jsonl"))
    );
    assert!(!got.path.as_deref().unwrap().contains("c.jsonl"));
    let err = collection.get("missing").unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
}

#[test]
fn empty_find_and_scan_keep_append_order_across_rotate() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let collection = vault.collection("south_log").unwrap();
    collection.add("a", rec("x")).unwrap();
    collection.add("b", rec("x")).unwrap();
    collection.add("c", rec("x")).unwrap();
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
    collection.add("a", rec("x")).unwrap();
    collection.add("b", rec("x")).unwrap();
    let path = scratch.log_dir().join("00000001.jsonl");
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
    collection.add("a", rec("x")).unwrap();
    let path = scratch.log_dir().join("00000001.jsonl");
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
    assert_eq!(got.valid, Some(true));
    vault.check().unwrap();
}

#[test]
fn journal_rejects_markdown_body() {
    let scratch = Scratch::new();
    let vault = open_log(&scratch);
    let mut record = rec("x");
    record.body = Some(String::from("nope"));
    let err = vault
        .collection("south_log")
        .unwrap()
        .add("a", record)
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn integer_field_round_trips_as_integer_not_1_0() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    collection.add("a", rec_fields(json!({"n": 1}))).unwrap();
    let path = scratch.lines_dir().join("00000001.jsonl");
    let before = fs::read(&path).unwrap();
    let text = str::from_utf8(&before).unwrap();
    assert!(text.contains(r#""n":1"#), "{text}");
    assert!(!text.contains(r#""n":1.0"#), "{text}");
    vault.reindex().unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    let got = collection.get("a").unwrap();
    assert_eq!(got.raw.as_deref(), Some(before.as_slice()));
    assert_eq!(got.valid, Some(true));
}

#[test]
fn get_and_find_include_raw_match_segment_line() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    collection.add("a", rec_fields(json!({"n": 1}))).unwrap();
    let line = fs::read(scratch.lines_dir().join("00000001.jsonl")).unwrap();
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
    collection.add("one", rec_fields(fields.clone())).unwrap();
    let path = scratch.lines_dir().join("00000001.jsonl");
    let line = fs::read_to_string(&path).unwrap();
    let expected = "{\"id\":\"one\",\"meta\":{\"z\":\"ascii\",\"\u{1F600}\":\"grin\",\"\u{FB33}\":\"he\"},\"n\":1,\"neg\":-42,\"tags\":[\"a\",\"b\"],\"x\":0.5}\n";
    assert_eq!(line, expected);

    let got = collection.get("one").unwrap();
    assert_eq!(got.raw.as_deref(), Some(line.as_bytes()));

    let mut again = fields;
    if let serde_json::Value::Object(map) = &mut again {
        map.insert(String::from("id"), json!("two"));
    }
    collection.add("two", rec_fields(again)).unwrap();
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
        .add("frac", rec_fields(json!({"n": 1.5})))
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert!(!scratch.lines_dir().join("00000001.jsonl").is_file());
}

#[test]
fn integer_above_2_pow_53_is_invalid() {
    let scratch = Scratch::new();
    let vault = open_lines(&scratch);
    let collection = vault.collection("south_lines").unwrap();
    collection
        .add("edge", rec_fields(json!({"n": 9_007_199_254_740_992_u64})))
        .unwrap();
    let text = fs::read_to_string(scratch.lines_dir().join("00000001.jsonl")).unwrap();
    assert!(text.contains(r#""n":9007199254740992"#), "{text}");
    let err = collection
        .add("wide", rec_fields(json!({"n": 18_014_398_509_481_984_u64})))
        .unwrap_err();
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
