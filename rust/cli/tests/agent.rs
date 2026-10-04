//! A7: agent acceptance gates (`10` §4, `12` A7). Collection names here
//! must not appear in `rust/cli/src` (SCH-1).

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use scheda::{FindQuery, OpenOptions, Schema, SearchQuery, Vault, schema_from_json};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-a7", n),
        }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn payload_file(&self, name: &str, bytes: impl AsRef<[u8]>) -> String {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        path.display().to_string()
    }

    /// A library handle on the Vault the CLI wrote, caught up with `HEAD`:
    /// each CLI process indexes in the background and may exit first.
    fn reopen(&self) -> Vault {
        let vault = Vault::open_with(
            self.vault(),
            OpenOptions::new(),
            None,
            Some(common::cli_body_projector()),
        )
        .unwrap();
        vault.wait_indexed(Duration::from_secs(30)).unwrap();
        vault
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

fn scheda_bin() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_scheda") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_scheda").expect("CARGO_BIN_EXE_scheda"))
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(scheda_bin())
        .args(args)
        .output()
        .expect("spawn scheda");
    Run {
        status: out.status.code().unwrap_or(1),
        stdout: String::from_utf8(out.stdout).expect("stdout utf8"),
        stderr: String::from_utf8(out.stderr).expect("stderr utf8"),
    }
}

fn stdout_json(run: &Run) -> Value {
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    serde_json::from_str(run.stdout.trim()).expect("stdout json")
}

fn error_json(run: &Run) -> Value {
    assert_ne!(run.status, 0, "stdout={}", run.stdout);
    serde_json::from_str(run.stderr.trim()).expect("stderr json")
}

fn rec(fields: Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn vault_arg(scratch: &Scratch) -> String {
    scratch.vault().display().to_string()
}

fn notes_decl() -> Schema {
    serde_json::from_value(json!({
        "version": 1,
        "document": {},
    }))
    .unwrap()
}

fn log_decl() -> Schema {
    serde_json::from_value(json!({
        "version": 1,
        "journal": {},
    }))
    .unwrap()
}

fn isolated_decl() -> Schema {
    serde_json::from_value(json!({
        "document": {},
        "isolated": true,
    }))
    .unwrap()
}

fn json_contains_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(key) || map.values().any(|v| json_contains_key(v, key))
        }
        Value::Array(items) => items.iter().any(|v| json_contains_key(v, key)),
        _ => false,
    }
}

fn open_with_notes(scratch: &Scratch) -> String {
    let path = vault_arg(scratch);
    let open = run(&["--vault", &path, "open"]);
    assert_eq!(open.status, 0, "stderr={}", open.stderr);
    let put = run(&[
        "--vault",
        &path,
        "create-collection",
        "accept_notes",
        "--schema",
        &serde_json::to_string(&notes_decl()).unwrap(),
    ]);
    assert_eq!(put.status, 0, "stderr={}", put.stderr);
    path
}

fn contains_collection_name(haystack: &str, name: &str) -> bool {
    haystack.match_indices(name).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + name.len()..].chars().next();
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        before.is_none_or(|c| !ident(c)) && after.is_none_or(|c| !ident(c))
    })
}

fn collection<'a>(info: &'a Value, name: &str) -> &'a Value {
    info["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|col| col["name"] == name)
        .unwrap()
}

#[test]
fn src_does_not_name_fixture_collections() {
    let src = format!(
        "{}\n{}\n{}\n{}\n{}",
        include_str!("../src/main.rs"),
        include_str!("../src/args.rs"),
        include_str!("../src/http.rs"),
        include_str!("../src/model2vec.rs"),
        include_str!("../src/projector.rs")
    );
    for name in ["accept_notes", "accept_log", "accept_sealed"] {
        assert!(
            !contains_collection_name(&src, name),
            "CLI source names {name}"
        );
    }
}

#[test]
fn info_json_matches_schema_files() {
    let scratch = Scratch::new();
    let path = open_with_notes(&scratch);
    let log = run(&[
        "--vault",
        &path,
        "create-collection",
        "accept_log",
        "--schema",
        &serde_json::to_string(&log_decl()).unwrap(),
    ]);
    assert_eq!(log.status, 0, "stderr={}", log.stderr);

    let notes_yaml =
        fs::read_to_string(scratch.vault().join("accept_notes/.collection.yaml")).unwrap();
    assert!(notes_yaml.contains("name: accept_notes"));
    assert!(notes_yaml.contains("document:"));
    let log_yaml = fs::read_to_string(scratch.vault().join("accept_log/.collection.yaml")).unwrap();
    assert!(log_yaml.contains("name: accept_log"));
    assert!(log_yaml.contains("journal:"));

    let info = stdout_json(&run(&["--vault", &path, "info"]));
    let notes = collection(&info, "accept_notes");
    let notes_schema = &notes["schema"];
    let lib_notes = scratch
        .reopen()
        .collection("accept_notes")
        .unwrap()
        .schema()
        .unwrap();
    assert_eq!(notes_schema, &serde_json::to_value(&lib_notes).unwrap());
    assert!(notes_schema.get("document").is_some());
    assert!(notes_schema.get("version").is_none());
    assert!(notes_schema.get("fields").is_none());
    assert!(notes_schema.get("isolated").is_none());

    let journal = &collection(&info, "accept_log")["schema"];
    let lib_log = scratch
        .reopen()
        .collection("accept_log")
        .unwrap()
        .schema()
        .unwrap();
    assert_eq!(journal, &serde_json::to_value(&lib_log).unwrap());
    assert!(journal.get("journal").is_some());
    assert!(journal.get("fields").is_none());

    let lib = serde_json::to_value(scratch.reopen().info().unwrap()).unwrap();
    assert_eq!(lib["collections"], info["collections"]);
    assert!(
        !json_contains_key(&info["collections"], "fields"),
        "CLI info JSON must not list fields: {info}"
    );
    assert!(
        !json_contains_key(&lib["collections"], "fields"),
        "library info JSON must not list fields: {lib}"
    );
}

#[test]
fn info_marks_isolated_without_fields() {
    let scratch = Scratch::new();
    let path = open_with_notes(&scratch);
    let put = run(&[
        "--vault",
        &path,
        "create-collection",
        "accept_sealed",
        "--schema",
        &serde_json::to_string(&isolated_decl()).unwrap(),
    ]);
    assert_eq!(put.status, 0, "stderr={}", put.stderr);

    let info = stdout_json(&run(&["--vault", &path, "info"]));
    let lib = serde_json::to_value(scratch.reopen().info().unwrap()).unwrap();
    assert_eq!(lib["collections"], info["collections"]);

    let sealed = collection(&info, "accept_sealed");
    assert_eq!(sealed["schema"]["isolated"], true);
    assert!(sealed["schema"].get("fields").is_none());
    assert!(
        !json_contains_key(&info["collections"], "fields"),
        "isolated info JSON must not list fields: {info}"
    );
    assert!(
        !json_contains_key(&lib["collections"], "fields"),
        "library isolated info JSON must not list fields: {lib}"
    );
}

#[test]
fn tool_writes_cannot_bypass_schema() {
    let scratch = Scratch::new();
    let path = open_with_notes(&scratch);
    let log = run(&[
        "--vault",
        &path,
        "create-collection",
        "accept_log",
        "--schema",
        &serde_json::to_string(&log_decl()).unwrap(),
    ]);
    assert_eq!(log.status, 0, "stderr={}", log.stderr);

    let extra = run(&[
        "--vault",
        &path,
        "collection",
        "accept_log",
        "add",
        "--id",
        "rogue",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"x","unknown":"y"}))),
    ]);
    assert_eq!(extra.status, 0, "stderr={}", extra.stderr);
    let got = scratch
        .reopen()
        .collection("accept_log")
        .unwrap()
        .get("rogue")
        .unwrap();
    assert_eq!(got.id.as_deref(), Some("rogue"));

    let add = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "add",
        "--id",
        "keep",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Keep"}))),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);
    assert!(scratch.vault().join("accept_notes/keep").is_file());

    let ok_replace = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "replace",
        "--id",
        "keep",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Keep","unknown":"y"}))),
    ]);
    assert_eq!(ok_replace.status, 0, "stderr={}", ok_replace.stderr);
    let kept = fs::read_to_string(scratch.vault().join("accept_notes/keep")).unwrap();
    assert!(kept.contains("unknown") || kept.contains("Keep"), "{kept}");
}

#[test]
fn library_and_cli_json_and_code_match() {
    let scratch = Scratch::new();
    let path = open_with_notes(&scratch);
    let add = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "add",
        "--id",
        "alpha",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Alpha"}))),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);

    let lib = scratch
        .reopen()
        .collection("accept_notes")
        .unwrap()
        .get("alpha")
        .unwrap();
    let cli = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "get",
        "--id",
        "alpha",
    ]);
    assert_eq!(cli.status, 0, "stderr={}", cli.stderr);
    assert_eq!(cli.stdout.as_bytes(), lib.raw.as_deref().unwrap_or(&[]));

    let lib_err = scratch
        .reopen()
        .collection("accept_notes")
        .unwrap()
        .get("missing")
        .unwrap_err();
    let cli_err = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "get",
        "--id",
        "missing",
    ]);
    assert_eq!(
        error_json(&cli_err)["code"],
        serde_json::to_value(&lib_err).unwrap()["code"]
    );

    let lib_info = serde_json::to_value(scratch.reopen().info().unwrap()).unwrap();
    let cli_info = stdout_json(&run(&["--vault", &path, "info"]));
    assert_eq!(cli_info["collections"], lib_info["collections"]);
    assert!(
        !json_contains_key(&cli_info["collections"], "fields"),
        "CLI info JSON must not list fields: {cli_info}"
    );
    assert!(
        !json_contains_key(&lib_info["collections"], "fields"),
        "library info JSON must not list fields: {lib_info}"
    );

    let lib_find = scratch
        .reopen()
        .find(&[], &FindQuery::all(), 0, 10)
        .unwrap();
    let cli_find = run(&[
        "--vault",
        &path,
        "find",
        "--filter",
        "{\"all\":{}}",
        "--offset",
        "0",
        "--count",
        "10",
    ]);
    assert_eq!(
        stdout_json(&cli_find),
        serde_json::to_value(&lib_find).unwrap()
    );

    let query: SearchQuery = serde_json::from_value(json!({"text": "Alpha"})).unwrap();
    let lib_search = scratch.reopen().search(&[], &query, None).unwrap();
    let cli_search = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        "{\"text\":\"Alpha\"}",
    ]);
    assert_eq!(
        stdout_json(&cli_search),
        serde_json::to_value(&lib_search).unwrap()
    );

    let lib_invalid = scratch
        .reopen()
        .collection("accept_notes")
        .unwrap()
        .get("bad$id")
        .unwrap_err();
    let cli_invalid = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "get",
        "--id",
        "bad$id",
    ]);
    assert_eq!(error_json(&cli_invalid)["code"], "CODE_INVALID");
    assert_eq!(
        error_json(&cli_invalid)["code"],
        serde_json::to_value(&lib_invalid).unwrap()["code"]
    );
}

#[test]
fn library_and_cli_create_collection_json_and_code_match() {
    let lib_scratch = Scratch::new();
    let cli_scratch = Scratch::new();
    let schema = serde_json::to_string(&notes_decl()).unwrap();
    let lib_json = {
        let vault = lib_scratch.reopen();
        let collection = vault
            .create_collection("accept_notes", notes_decl(), None)
            .unwrap();
        serde_json::to_value(collection.schema().unwrap()).unwrap()
    };
    let path = vault_arg(&cli_scratch);
    let open = run(&["--vault", &path, "open"]);
    assert_eq!(open.status, 0, "stderr={}", open.stderr);
    let cli = run(&[
        "--vault",
        &path,
        "create-collection",
        "accept_notes",
        "--schema",
        &schema,
    ]);
    assert_eq!(stdout_json(&cli), lib_json);

    let leftover = r#"{"document":{},"fields":[{"name":"title","string":{}}]}"#;
    let lib_err = schema_from_json(leftover).unwrap_err();
    let cli_err = run(&[
        "--vault",
        &path,
        "create-collection",
        "accept_log",
        "--schema",
        leftover,
    ]);
    assert_eq!(error_json(&cli_err)["code"], "CODE_INVALID");
    assert_eq!(
        error_json(&cli_err)["code"],
        serde_json::to_value(&lib_err).unwrap()["code"]
    );
    assert!(
        !cli_scratch
            .vault()
            .join("accept_log")
            .join(".collection.yaml")
            .is_file()
    );
}
