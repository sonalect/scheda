//! A7: agent acceptance gates (`10` §4, `12` A7). Collection names here
//! must not appear in `rust/cli/src` (SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process::{self, Command};
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{OpenOptions, Schema, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scheda-a7-{}-{}-{n}",
            process::id(),
            format!("{:?}", std::thread::current().id()).replace(['(', ')'], "")
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), OpenOptions::new(), None).unwrap()
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

fn vault_arg(scratch: &Scratch) -> String {
    scratch.vault().display().to_string()
}

fn notes_decl() -> Schema {
    serde_json::from_value(json!({
        "version": 1,
        "document": {},
        "fields": [
            {"name": "title", "required": true, "string": {}},
            {"name": "tag", "string": {}},
            {"name": "rank", "integer": {}, "default": 0}
        ]
    }))
    .unwrap()
}

fn log_decl() -> Schema {
    serde_json::from_value(json!({
        "version": 1,
        "journal": {},
        "fields": [{"name": "title", "string": {}}]
    }))
    .unwrap()
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
        "{}\n{}\n{}\n{}",
        include_str!("../src/main.rs"),
        include_str!("../src/args.rs"),
        include_str!("../src/http.rs"),
        include_str!("../src/model2vec.rs")
    );
    for name in ["accept_notes", "accept_log"] {
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

    let notes_yaml = fs::read_to_string(scratch.vault().join("accept_notes/.schema.yaml")).unwrap();
    assert!(notes_yaml.contains("name: accept_notes"));
    assert!(notes_yaml.contains("kind: document"));
    assert!(notes_yaml.contains("title"));
    assert!(notes_yaml.contains("tag"));
    assert!(notes_yaml.contains("rank"));
    let log_yaml = fs::read_to_string(scratch.vault().join("accept_log/.schema.yaml")).unwrap();
    assert!(log_yaml.contains("name: accept_log"));
    assert!(log_yaml.contains("kind: journal"));

    let info = stdout_json(&run(&["--vault", &path, "info"]));
    let notes = collection(&info, "accept_notes");
    let notes_schema = &notes["schema"];
    assert!(notes_schema.get("document").is_some());
    assert_eq!(notes_schema["version"], 1);
    assert_eq!(notes_schema["fields"][0]["name"], "title");
    assert_eq!(notes_schema["fields"][0]["required"], true);
    assert!(notes_schema["fields"][0].get("string").is_some());
    assert_eq!(notes_schema["fields"][1]["name"], "tag");
    assert_eq!(notes_schema["fields"][2]["name"], "rank");
    assert_eq!(notes_schema["fields"][2]["default"].as_f64(), Some(0.0));
    assert!(notes_schema.get("isolated").is_none());

    let journal = &collection(&info, "accept_log")["schema"];
    assert!(journal.get("journal").is_some());
    assert_eq!(journal["fields"][0]["name"], "title");

    let lib = serde_json::to_value(scratch.reopen().info().unwrap()).unwrap();
    assert_eq!(lib["collections"], info["collections"]);
}

#[test]
fn tool_writes_cannot_bypass_schema() {
    let scratch = Scratch::new();
    let path = open_with_notes(&scratch);

    let extra = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "add",
        "--id",
        "rogue",
        "--record",
        &json!({"fields":{"title":"x","unknown":"y"}}).to_string(),
    ]);
    assert_eq!(error_json(&extra)["code"], "CODE_INVALID");
    assert!(!scratch.vault().join("accept_notes/rogue.md").exists());
    assert!(!scratch.vault().join("rogue.md").exists());

    let add = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "add",
        "--id",
        "keep",
        "--record",
        &json!({"fields":{"title":"Keep"}}).to_string(),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);
    assert!(scratch.vault().join("accept_notes/keep.md").is_file());

    let bad_replace = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "replace",
        "--id",
        "keep",
        "--record",
        &json!({"fields":{"title":"Keep","unknown":"y"}}).to_string(),
    ]);
    assert_eq!(error_json(&bad_replace)["code"], "CODE_INVALID");
    let kept = fs::read_to_string(scratch.vault().join("accept_notes/keep.md")).unwrap();
    assert!(!kept.contains("unknown"));
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
        "--record",
        &json!({"fields":{"title":"Alpha"}}).to_string(),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);

    let lib = scratch
        .reopen()
        .collection("accept_notes")
        .unwrap()
        .get("alpha")
        .unwrap();
    let lib_json = serde_json::to_value(&lib).unwrap();
    let cli = run(&[
        "--vault",
        &path,
        "collection",
        "accept_notes",
        "get",
        "--id",
        "alpha",
    ]);
    assert_eq!(stdout_json(&cli), lib_json);

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
}
