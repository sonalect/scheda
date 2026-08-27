//! A4/A5/A6: thin CLI (`08` AGT-2–5, ISO-3, `12`, `14`). Collection names here
//! must not appear in `rust/cli/src` (SCH-1).

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use scheda::{
    Embedder, Filter, OpenOptions, Record, RecordAdd, Reranker, Schema, SearchQuery, Vault,
};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scheda-a4-{}-{}-{n}",
            process::id(),
            format!("{:?}", std::thread::current().id()).replace(['(', ')'], "")
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn init(&self) -> Vault {
        let vault = Vault::open(self.vault(), OpenOptions::new(), None).unwrap();
        vault.begin().unwrap();
        vault
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), OpenOptions::new(), None).unwrap()
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
    run_cmd(args, &[])
}

fn run_without_env(args: &[&str], unset: &str) -> Run {
    run_cmd(args, &[unset])
}

fn run_cmd(args: &[&str], unset: &[&str]) -> Run {
    let mut cmd = Command::new(scheda_bin());
    cmd.args(args);
    for name in unset {
        cmd.env_remove(name);
    }
    let out = cmd.output().expect("spawn scheda");
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

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn notes_schema() -> &'static str {
    "name: agent_notes\nkind: document\nschema_version: 1\nfields:\n  - name: title\n    required: true\n    string: {}\n  - name: tag\n    string: {}\n"
}

fn notes_decl() -> Schema {
    serde_json::from_value(json!({
        "version": 1,
        "document": {},
        "fields": [
            {"name": "title", "required": true, "string": {}},
            {"name": "tag", "string": {}}
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

fn fixture_with_note() -> Scratch {
    let scratch = Scratch::new();
    let vault = scratch.init();
    vault
        .create_collection("agent_notes", notes_decl())
        .unwrap();
    vault.create_collection("agent_log", log_decl()).unwrap();
    vault
        .collection("agent_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "Alpha token", "tag": "blue"})))
        .unwrap();
    vault.commit("fixture").unwrap();
    scratch
}

fn vault_arg(scratch: &Scratch) -> String {
    scratch.vault().display().to_string()
}

fn all_filter() -> String {
    serde_json::to_string(&Filter::all()).unwrap()
}

fn text_query(text: &str) -> String {
    serde_json::to_string(&SearchQuery::default().with_text(text)).unwrap()
}

#[test]
fn help_lists_lib2_tools_not_collection_names() {
    let run = run(&["--help"]);
    assert_eq!(run.status, 0, "stderr={}", run.stderr);
    let help = format!("{}{}", run.stdout, run.stderr);
    for name in [
        "open",
        "info",
        "create-collection",
        "collection",
        "find",
        "search",
        "begin",
        "commit",
        "discard",
        "check",
        "reindex",
        "push",
        "pull",
        "sync",
        "model2vec-embedder",
        "embedder-openai-url",
        "embedder-model",
        "embedder-dimensions",
        "embedder-api-key-env-var",
    ] {
        assert!(help.contains(name), "missing {name} in help:\n{help}");
    }
    for gone in [
        "catalog",
        "find-one",
        "insert-one",
        "replace-one",
        "scan",
        "walk",
        "walk-inverse",
        "push-remote",
        "pull-remote",
        "sync-remote",
        "init",
        "clone",
        "onnx-embedder",
        "reranker-onnx",
    ] {
        assert!(!help.contains(gone), "legacy {gone} still in help:\n{help}");
    }
    assert!(!help.contains("agent_notes"));
    assert!(!help.contains("west_docs"));
}

#[test]
fn scripted_find_search_get_journal_add_commit() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let filter = all_filter();
    let query = text_query("Alpha");

    let found = run(&[
        "--vault", &path, "find", "--filter", &filter, "--offset", "0", "--count", "10",
    ]);
    let page = stdout_json(&found);
    assert_eq!(page["records"][0]["id"], "alpha");
    assert_eq!(page["records"][0]["collection"], "agent_notes");
    assert_eq!(page["total"], 1);
    assert!(page["records"][0].get("raw").is_none());

    let found_raw = run(&[
        "--vault",
        &path,
        "find",
        "--filter",
        &filter,
        "--offset",
        "0",
        "--count",
        "10",
        "--include-raw",
    ]);
    let page_raw = stdout_json(&found_raw);
    assert!(page_raw["records"][0]["raw"].as_str().is_some());

    let search = run(&["--vault", &path, "search", "--query", &query]);
    let hits = stdout_json(&search);
    assert_eq!(hits["hits"][0]["id"], "alpha");
    assert_eq!(hits["hits"][0]["collection"], "agent_notes");
    assert!(hits["hits"][0].get("lexicalScore").is_some());

    let one = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "alpha",
    ]);
    let record = stdout_json(&one);
    assert_eq!(record["id"], "alpha");
    assert_eq!(record["collection"], "agent_notes");
    assert!(record["path"].as_str().unwrap().contains("alpha"));
    assert_eq!(record["valid"], true);

    let begin = run(&["--vault", &path, "begin"]);
    assert_eq!(begin.status, 0, "stderr={}", begin.stderr);

    let add = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "add",
        "--id",
        "e1",
        "--record",
        &json!({"fields":{"title":"logged"}}).to_string(),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);
    assert!(add.stdout.trim().is_empty());

    let commit = run(&["--vault", &path, "commit", "--message", "agent run"]);
    assert_eq!(commit.status, 0, "stderr={}", commit.stderr);
    assert!(commit.stdout.trim().is_empty());
}

#[test]
fn extra_field_is_invalid_and_write_nothing() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("notes.yaml", notes_schema());
    let path = vault_arg(&scratch);

    let extra = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "rogue",
        "--record",
        &json!({"fields":{"title":"x","unknown":"y"}}).to_string(),
    ]);
    assert_eq!(error_json(&extra)["code"], "CODE_INVALID");
    assert!(
        !scratch
            .vault()
            .join("agent_notes")
            .join("rogue.md")
            .exists()
    );

    let keep = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "keep",
        "--record",
        &json!({"fields":{"title":"Keep"}}).to_string(),
    ]);
    assert_eq!(keep.status, 0, "stderr={}", keep.stderr);
    let bad_replace = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "replace",
        "--id",
        "keep",
        "--record",
        &json!({"fields":{"title":"Keep","unknown":"y"}}).to_string(),
    ]);
    assert_eq!(error_json(&bad_replace)["code"], "CODE_INVALID");
}

#[test]
fn library_and_cli_record_json_and_error_code_match() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let lib = scratch
        .reopen()
        .collection("agent_notes")
        .unwrap()
        .get("alpha")
        .unwrap();
    let lib_json = serde_json::to_value(&lib).unwrap();

    let cli = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "alpha",
    ]);
    assert_eq!(stdout_json(&cli), lib_json);

    let lib_err = scratch
        .reopen()
        .collection("agent_notes")
        .unwrap()
        .get("missing")
        .unwrap_err();
    let cli_err = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "missing",
    ]);
    assert_eq!(
        error_json(&cli_err)["code"],
        serde_json::to_value(&lib_err).unwrap()["code"]
    );
}

#[test]
fn contract_codes_not_found_invalid_locked_index_unusable() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let writer = scratch.reopen();
    writer.begin().unwrap();

    let missing = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "nope",
    ]);
    assert_eq!(error_json(&missing)["code"], "CODE_NOT_FOUND");

    let locked = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "beta",
        "--record",
        &json!({"fields":{"title":"B"}}).to_string(),
    ]);
    assert_eq!(error_json(&locked)["code"], "CODE_LOCKED");
    drop(writer);

    let invalid = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "x",
        "--record",
        &json!({"fields":{"title":"t","nope":1}}).to_string(),
    ]);
    assert_eq!(error_json(&invalid)["code"], "CODE_INVALID");

    fs::write(scratch.vault().join(".scheda/index/format"), "999\n").unwrap();
    let unusable = run(&["--vault", &path, "search", "--query", &text_query("Alpha")]);
    assert_eq!(error_json(&unusable)["code"], "CODE_INDEX_VERSION");
}

#[test]
fn auto_sync_off_local_info_get_add_commit() {
    let scratch = Scratch::new();
    let path = vault_arg(&scratch);
    let open = run(&["--vault", &path, "open"]);
    assert_eq!(open.status, 0, "stderr={}", open.stderr);

    let schema = serde_json::to_string(&notes_decl()).unwrap();
    let put = run(&[
        "--vault",
        &path,
        "create-collection",
        "agent_notes",
        "--schema",
        &schema,
    ]);
    let declared = stdout_json(&put);
    assert!(declared.get("document").is_some());
    assert_eq!(declared["version"], 1);

    let info = run(&["--vault", &path, "info"]);
    let info = stdout_json(&info);
    let names: Vec<&str> = info["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["agent_notes"]);

    let begin = run(&["--vault", &path, "begin"]);
    assert_eq!(begin.status, 0, "stderr={}", begin.stderr);

    let add = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "alpha",
        "--record",
        &json!({"fields":{"title":"A"}}).to_string(),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);

    let got = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "alpha",
    ]);
    let record = stdout_json(&got);
    assert_eq!(record["id"], "alpha");
    assert_eq!(record["collection"], "agent_notes");
    assert_eq!(record["valid"], true);
    assert!(record.get("path").is_some());

    let commit = run(&["--vault", &path, "commit", "-m", "local"]);
    assert_eq!(commit.status, 0, "stderr={}", commit.stderr);
}

#[test]
fn create_collection_then_add_without_handwritten_yaml() {
    let scratch = Scratch::new();
    let path = vault_arg(&scratch);
    let open = run(&["--vault", &path, "open"]);
    assert_eq!(open.status, 0, "stderr={}", open.stderr);
    let put = run(&[
        "--vault",
        &path,
        "create-collection",
        "agent_notes",
        "--schema",
        &serde_json::to_string(&notes_decl()).unwrap(),
    ]);
    assert_eq!(put.status, 0, "stderr={}", put.stderr);

    let add = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "alpha",
        "--record",
        &json!({"fields":{"title":"A","tag":"t"}}).to_string(),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);
    let got = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "alpha",
    ]);
    assert_eq!(stdout_json(&got)["id"], "alpha");
}

#[test]
fn src_does_not_name_fixture_collections() {
    let src = format!(
        "{}\n{}\n{}",
        include_str!("../src/main.rs"),
        include_str!("../src/args.rs"),
        include_str!("../src/model2vec.rs")
    );
    for name in ["agent_notes", "agent_log", "agent_pages", "agent_sealed"] {
        assert!(
            !contains_collection_name(&src, name),
            "CLI source names {name}"
        );
    }
}

fn contains_collection_name(haystack: &str, name: &str) -> bool {
    haystack.match_indices(name).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + name.len()..].chars().next();
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        before.is_none_or(|c| !ident(c)) && after.is_none_or(|c| !ident(c))
    })
}

const TREE_SEALED: &str = "agent-sealed-tree-token";
const NESTED_SEALED: &str = "agent-sealed-nested-token";

fn isolated_schema_yaml() -> &'static str {
    "name: agent_sealed\nkind: document\nschema_version: 1\nisolated: true\nfields:\n  - name: agent_pass\n    required: true\n    string: {}\n"
}

fn sealed_document(token: &str) -> String {
    format!("---\nid: sealed\nagent_pass: {token}\n---\n")
}

fn plant_isolated_sealed(scratch: &Scratch) {
    let dir = scratch.vault().join("agent_sealed");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".schema.yaml"), isolated_schema_yaml()).unwrap();
    fs::write(dir.join("sealed.md"), sealed_document(TREE_SEALED)).unwrap();
    let nested = scratch.vault().join(".isolated").join("agent_sealed");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("sealed.md"), sealed_document(NESTED_SEALED)).unwrap();
}

fn assert_no_sealed(haystack: &str) {
    assert!(
        !haystack.contains("agent_pass"),
        "isolated field leaked: {haystack}"
    );
    assert!(
        !haystack.contains(TREE_SEALED),
        "main-tree sealed value leaked: {haystack}"
    );
    assert!(
        !haystack.contains(NESTED_SEALED),
        "nested sealed value leaked: {haystack}"
    );
}

#[test]
fn cli_omits_isolated_sealed_values() {
    let scratch = fixture_with_note();
    plant_isolated_sealed(&scratch);
    let path = vault_arg(&scratch);
    let reindex = run(&["--vault", &path, "reindex"]);
    assert_eq!(reindex.status, 0, "stderr={}", reindex.stderr);

    let info = stdout_json(&run(&["--vault", &path, "info"]));
    let isolated = info["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|col| col["name"] == "agent_sealed")
        .unwrap();
    assert_eq!(isolated["schema"]["isolated"], true);
    assert!(isolated["schema"].get("fields").is_none());
    assert_no_sealed(&info.to_string());

    let filter = all_filter();
    let found = stdout_json(&run(&[
        "--vault", &path, "find", "--filter", &filter, "--offset", "0", "--count", "10",
    ]));
    assert_eq!(found["records"].as_array().unwrap().len(), 1);
    assert_eq!(found["records"][0]["id"], "alpha");
    assert_eq!(found["records"][0]["collection"], "agent_notes");
    assert_no_sealed(&found.to_string());

    let search = stdout_json(&run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha token"),
    ]));
    assert_eq!(search["hits"][0]["id"], "alpha");
    assert_eq!(search["hits"][0]["collection"], "agent_notes");
    assert_no_sealed(&search.to_string());

    let miss = stdout_json(&run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query(NESTED_SEALED),
    ]));
    assert_eq!(
        miss["hits"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "sealed token must not be a search hit: {miss}"
    );

    let got = stdout_json(&run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "alpha",
    ]));
    assert_eq!(got["id"], "alpha");
    assert_no_sealed(&got.to_string());

    let scoped = stdout_json(&run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "find",
        "--filter",
        &filter,
        "--offset",
        "0",
        "--count",
        "10",
    ]));
    assert_eq!(scoped["records"].as_array().unwrap().len(), 1);
    assert_no_sealed(&scoped.to_string());

    let scoped_search = stdout_json(&run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "search",
        "--query",
        &text_query("Alpha token"),
    ]));
    assert_eq!(scoped_search["hits"][0]["id"], "alpha");
    assert_eq!(scoped_search["hits"][0]["collection"], "agent_notes");
    assert_no_sealed(&scoped_search.to_string());

    let isolated_get = run(&[
        "--vault",
        &path,
        "collection",
        "agent_sealed",
        "get",
        "--id",
        "sealed",
    ]);
    assert_eq!(error_json(&isolated_get)["code"], "CODE_UNSUPPORTED");
    assert_no_sealed(&isolated_get.stdout);
    assert_no_sealed(&isolated_get.stderr);

    let named = run(&[
        "--vault",
        &path,
        "find",
        "--collections",
        "agent_sealed",
        "--filter",
        &filter,
        "--offset",
        "0",
        "--count",
        "10",
    ]);
    assert_eq!(error_json(&named)["code"], "CODE_UNSUPPORTED");
    assert_no_sealed(&named.stdout);
    assert_no_sealed(&named.stderr);
}

#[test]
fn search_query_json_matches_library() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let query = SearchQuery::default().with_text("Alpha");
    let lib = scratch
        .reopen()
        .collection("agent_notes")
        .unwrap()
        .search(&query, None)
        .unwrap();
    let lib_json = serde_json::to_value(&lib).unwrap();
    let cli = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "search",
        "--query",
        &serde_json::to_string(&query).unwrap(),
    ]);
    assert_eq!(stdout_json(&cli), lib_json);
}

#[test]
fn vault_find_scopes_collections() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let filter = all_filter();
    let scoped = run(&[
        "--vault",
        &path,
        "find",
        "--collections",
        "agent_notes",
        "--filter",
        &filter,
        "--offset",
        "0",
        "--count",
        "10",
    ]);
    let page = stdout_json(&scoped);
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    assert_eq!(page["records"][0]["collection"], "agent_notes");
    assert_eq!(page["total"], 1);

    let missing = run(&[
        "--vault",
        &path,
        "find",
        "--collections",
        "nope",
        "--filter",
        &filter,
        "--offset",
        "0",
        "--count",
        "10",
    ]);
    assert_eq!(error_json(&missing)["code"], "CODE_NOT_FOUND");
}

#[test]
fn model2vec_embedder_dir_must_have_model_files() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let dir = scratch.root.join("ops");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tokenizer.json"), "{}").unwrap();
    let missing = run(&[
        "--vault",
        &path,
        "--model2vec-embedder",
        &dir.display().to_string(),
        "info",
    ]);
    assert_eq!(error_json(&missing)["code"], "CODE_INVALID");
}

#[test]
fn embedder_openai_rejects_partial_flags_non_http_and_model2vec_together() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let url = "http://127.0.0.1:8080/v1/";
    let ftp = "ftp://127.0.0.1/v1/";
    let model = "Qwen/Qwen3-Embedding-0.6B";

    let url_only = run(&["--vault", &path, "--embedder-openai-url", url, "info"]);
    assert_eq!(error_json(&url_only)["code"], "CODE_INVALID");

    let model_only = run(&["--vault", &path, "--embedder-model", model, "info"]);
    assert_eq!(error_json(&model_only)["code"], "CODE_INVALID");

    let dim_only = run(&["--vault", &path, "--embedder-dimensions", "1024", "info"]);
    assert_eq!(error_json(&dim_only)["code"], "CODE_INVALID");

    let missing_dim = run(&[
        "--vault",
        &path,
        "--embedder-openai-url",
        url,
        "--embedder-model",
        model,
        "info",
    ]);
    assert_eq!(error_json(&missing_dim)["code"], "CODE_INVALID");

    let zero_dim = run(&[
        "--vault",
        &path,
        "--embedder-openai-url",
        url,
        "--embedder-model",
        model,
        "--embedder-dimensions",
        "0",
        "info",
    ]);
    assert_eq!(error_json(&zero_dim)["code"], "CODE_INVALID");

    let bad = run(&[
        "--vault",
        &path,
        "--embedder-openai-url",
        ftp,
        "--embedder-model",
        model,
        "--embedder-dimensions",
        "1024",
        "info",
    ]);
    assert_eq!(error_json(&bad)["code"], "CODE_INVALID");

    let dir = scratch.root.join("ops");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.json"), "{}").unwrap();
    fs::write(dir.join("tokenizer.json"), "{}").unwrap();
    fs::write(dir.join("model.safetensors"), b"not-a-model").unwrap();
    let both = run(&[
        "--vault",
        &path,
        "--embedder-openai-url",
        url,
        "--embedder-model",
        model,
        "--embedder-dimensions",
        "1024",
        "--model2vec-embedder",
        &dir.display().to_string(),
        "info",
    ]);
    assert_eq!(error_json(&both)["code"], "CODE_INVALID");

    let key_only = run(&[
        "--vault",
        &path,
        "--embedder-api-key-env-var",
        "SCHEDA_TEST_EMBEDDER_KEY",
        "info",
    ]);
    assert_eq!(error_json(&key_only)["code"], "CODE_INVALID");

    let missing_key = run_without_env(
        &[
            "--vault",
            &path,
            "--embedder-openai-url",
            url,
            "--embedder-model",
            model,
            "--embedder-dimensions",
            "1024",
            "--embedder-api-key-env-var",
            "SCHEDA_TEST_EMBEDDER_KEY",
            "info",
        ],
        "SCHEDA_TEST_EMBEDDER_KEY",
    );
    assert_eq!(error_json(&missing_key)["code"], "CODE_INVALID");
}

#[test]
fn reranker_cohere_only_on_search_and_rejects_partial_or_non_http() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let search_help = run(&["search", "--help"]);
    assert_eq!(search_help.status, 0, "stderr={}", search_help.stderr);
    let help = format!("{}{}", search_help.stdout, search_help.stderr);
    assert!(
        help.contains("reranker-cohere-url"),
        "missing reranker-cohere-url:\n{help}"
    );
    assert!(
        help.contains("reranker-model"),
        "missing reranker-model:\n{help}"
    );
    assert!(
        help.contains("reranker-api-key-env-var"),
        "missing reranker-api-key-env-var:\n{help}"
    );
    assert!(
        !help.contains("reranker-onnx"),
        "legacy reranker-onnx still in search help:\n{help}"
    );

    let url_only = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--reranker-cohere-url",
        "http://127.0.0.1:8080/v1/",
    ]);
    assert_eq!(error_json(&url_only)["code"], "CODE_INVALID");

    let model_only = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--reranker-model",
        "Qwen/Qwen3-Reranker-0.6B",
    ]);
    assert_eq!(error_json(&model_only)["code"], "CODE_INVALID");

    let bad = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--reranker-cohere-url",
        "ftp://127.0.0.1/v1/",
        "--reranker-model",
        "Qwen/Qwen3-Reranker-0.6B",
    ]);
    assert_eq!(error_json(&bad)["code"], "CODE_INVALID");

    let key_only = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--reranker-api-key-env-var",
        "SCHEDA_TEST_RERANKER_KEY",
    ]);
    assert_eq!(error_json(&key_only)["code"], "CODE_INVALID");

    let missing_key = run_without_env(
        &[
            "--vault",
            &path,
            "search",
            "--query",
            &text_query("Alpha"),
            "--reranker-cohere-url",
            "http://127.0.0.1:8080/v1/",
            "--reranker-model",
            "Qwen/Qwen3-Reranker-0.6B",
            "--reranker-api-key-env-var",
            "SCHEDA_TEST_RERANKER_KEY",
        ],
        "SCHEDA_TEST_RERANKER_KEY",
    );
    assert_eq!(error_json(&missing_key)["code"], "CODE_INVALID");
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

const HTTP_MODEL: &str = "scheda-a5-stub";
const RERANK_MODEL: &str = "scheda-a5-rerank";

fn join_leaf(base: &str, leaf: &str) -> String {
    format!("{}/{leaf}", base.trim().trim_end_matches('/'))
}

fn embed_vector(text: &str) -> [f32; 4] {
    if text == "car" || text == "automobile" {
        [1.0, 0.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0, 0.0]
    }
}

fn vector_query(text: &str) -> String {
    serde_json::to_string(
        &SearchQuery::default()
            .with_text(text)
            .with_k(5)
            .with_lexical_weight(0.0),
    )
    .unwrap()
}

fn run_embed(path: &str, url: &str, rest: &[&str]) -> Run {
    let mut args = Vec::with_capacity(8 + rest.len());
    args.extend([
        "--vault",
        path,
        "--embedder-openai-url",
        url,
        "--embedder-model",
        HTTP_MODEL,
        "--embedder-dimensions",
        "4",
    ]);
    args.extend(rest);
    run(&args)
}

fn hit_ids(hits: &Value) -> Vec<&str> {
    hits["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hit| hit["id"].as_str())
        .collect()
}

fn set_dead_origin(vault: &Path) {
    let config = vault.join(".git/config");
    let mut body = fs::read_to_string(&config).unwrap();
    if !body.contains("[remote \"origin\"]") {
        body.push_str(
            "\n[remote \"origin\"]\n\turl = http://127.0.0.1:1/dead.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        );
        fs::write(config, body).unwrap();
    }
}

struct HttpStub {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl HttpStub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let seen_thread = Arc::clone(&seen);
        let shutdown_thread = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            while !shutdown_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        handle_conn(stream, &seen_thread);
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base: format!("http://{addr}/v1/"),
            seen,
            shutdown,
            handle: Some(handle),
        }
    }
}

impl Drop for HttpStub {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_conn(stream: TcpStream, seen: &Mutex<Vec<String>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(stream);
    if let Ok(Some((path, body))) = read_request(&mut reader) {
        let stream = reader.get_mut();
        if path.contains("embeddings") {
            let input = body.get("input").and_then(Value::as_str).unwrap_or("");
            seen.lock().unwrap().push(input.to_owned());
            write_json(
                stream,
                &json!({ "data": [{ "embedding": embed_vector(input) }] }),
            );
        } else if path.contains("rerank") {
            let results = body
                .get("documents")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(index, doc)| {
                    let text = match doc {
                        Value::String(text) => text.as_str(),
                        other => other.as_str().unwrap_or(""),
                    };
                    let score = if text.contains("car") || text.contains("automobile") {
                        0.9
                    } else {
                        0.1
                    };
                    json!({ "index": index, "relevance_score": score })
                })
                .collect::<Vec<_>>();
            write_json(stream, &json!({ "results": results }));
        }
        let _ = stream.shutdown(Shutdown::Write);
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> io::Result<Option<(String, Value)>> {
    let mut first = String::new();
    if reader.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut buf = vec![0; content_length];
    reader.read_exact(&mut buf)?;
    let body = serde_json::from_slice(&buf).unwrap_or(Value::Null);
    Ok(Some((path, body)))
}

fn write_json(stream: &mut TcpStream, value: &Value) {
    let payload = value.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.flush();
}

struct ParsedUrl {
    hostport: String,
    path: String,
}

fn parse_http_url(url: &str) -> Result<ParsedUrl, scheda::Error> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| scheda::Error::invalid("test HTTP URL must be http"))?;
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    Ok(ParsedUrl {
        hostport: hostport.to_owned(),
        path: format!("/{path}"),
    })
}

fn post_json(url: &str, body: &Value) -> Result<Value, scheda::Error> {
    let parsed = parse_http_url(url)?;
    let addr: SocketAddr = parsed
        .hostport
        .parse()
        .map_err(|err| scheda::Error::invalid(format!("bad test HTTP URL: {err}")))?;
    let payload = body.to_string();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|err| scheda::Error::io_other(err.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| scheda::Error::io_other(err.to_string()))?;
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        parsed.path,
        parsed.hostport,
        payload.len(),
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| scheda::Error::io_other(err.to_string()))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|err| scheda::Error::io_other(err.to_string()))?;
    let text = String::from_utf8(buf).map_err(|err| scheda::Error::invalid(err.to_string()))?;
    let (_, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| scheda::Error::invalid("test HTTP response has no body"))?;
    serde_json::from_str(body.trim()).map_err(|err| scheda::Error::invalid(err.to_string()))
}

struct HttpEmbedder {
    url: String,
    model: String,
    model_id: String,
}

impl HttpEmbedder {
    fn new(base: &str) -> Self {
        let url = join_leaf(base, "embeddings");
        Self {
            model_id: format!("{url} {HTTP_MODEL}"),
            url,
            model: HTTP_MODEL.to_owned(),
        }
    }
}

impl Embedder for HttpEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        4
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, scheda::Error> {
        let value = post_json(&self.url, &json!({ "model": self.model, "input": text }))?;
        let embedding = value
            .get("data")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("embedding"))
            .and_then(Value::as_array)
            .ok_or_else(|| scheda::Error::invalid("embeddings response has no embedding"))?;
        embedding
            .iter()
            .map(|item| {
                item.as_f64()
                    .map(|n| n as f32)
                    .ok_or_else(|| scheda::Error::invalid("embeddings response is not float32"))
            })
            .collect()
    }
}

struct HttpReranker {
    url: String,
    model: String,
}

impl HttpReranker {
    fn new(base: &str) -> Self {
        Self {
            url: join_leaf(base, "rerank"),
            model: RERANK_MODEL.to_owned(),
        }
    }
}

impl Reranker for HttpReranker {
    fn score(&self, query: &str, record: &Record) -> Result<f64, scheda::Error> {
        let scores = self.score_many(query, std::slice::from_ref(record))?;
        scores
            .into_iter()
            .next()
            .ok_or_else(|| scheda::Error::invalid("rerank response has no relevance_score"))
    }

    fn score_many(&self, query: &str, records: &[Record]) -> Result<Vec<f64>, scheda::Error> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let documents = records
            .iter()
            .map(|record| {
                serde_json::to_string(&json!({
                    "fields": record.fields,
                    "body": record.body,
                }))
                .map_err(|err| scheda::Error::invalid(err.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let value = post_json(
            &self.url,
            &json!({
                "model": self.model,
                "query": query,
                "documents": documents,
            }),
        )?;
        let results = value
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| scheda::Error::invalid("rerank response has no results"))?;
        let mut scores = vec![None; records.len()];
        for hit in results {
            let index = hit
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| scheda::Error::invalid("rerank response is missing index"))?
                as usize;
            let score = hit
                .get("relevance_score")
                .and_then(Value::as_f64)
                .ok_or_else(|| scheda::Error::invalid("rerank response has no relevance_score"))?;
            let Some(slot) = scores.get_mut(index) else {
                return Err(scheda::Error::invalid(
                    "rerank response index is out of range",
                ));
            };
            *slot = Some(score);
        }
        scores
            .into_iter()
            .map(|score| {
                score.ok_or_else(|| scheda::Error::invalid("rerank response is missing a document"))
            })
            .collect()
    }
}

#[test]
fn http_embedder_writes_vectors_and_scores_match() {
    let stub = HttpStub::start();
    let scratch = Scratch::new();
    let path = vault_arg(&scratch);
    let url = stub.base.as_str();
    let schema = serde_json::to_string(&notes_decl()).unwrap();
    let decoy = json!({"fields":{"title":"banana"}}).to_string();
    let vehicle = json!({"fields":{"title":"car"}}).to_string();
    let split = json!({"fields":{"title":"zzzz"},"body":"car"}).to_string();
    let replaced = json!({"fields":{"title":"automobile"}}).to_string();
    let query = vector_query("automobile");

    let open = run(&["--vault", &path, "open"]);
    assert_eq!(open.status, 0, "stderr={}", open.stderr);
    let created = run(&[
        "--vault",
        &path,
        "create-collection",
        "http_notes",
        "--schema",
        &schema,
    ]);
    assert_eq!(created.status, 0, "stderr={}", created.stderr);

    let begin = run(&["--vault", &path, "begin"]);
    assert_eq!(begin.status, 0, "stderr={}", begin.stderr);
    for (id, record) in [
        ("decoy", decoy.as_str()),
        ("vehicle", vehicle.as_str()),
        ("split", split.as_str()),
    ] {
        let add = run_embed(
            &path,
            url,
            &[
                "collection",
                "http_notes",
                "add",
                "--id",
                id,
                "--record",
                record,
            ],
        );
        assert_eq!(add.status, 0, "stderr={}", add.stderr);
    }
    let commit = run(&["--vault", &path, "commit", "-m", "a5"]);
    assert_eq!(commit.status, 0, "stderr={}", commit.stderr);

    {
        let seen = stub.seen.lock().unwrap();
        assert!(
            seen.iter().filter(|text| text.as_str() == "car").count() >= 2,
            "per-field embed missed title/body car: {seen:?}"
        );
        assert!(seen.iter().any(|text| text == "zzzz"), "{seen:?}");
        assert!(seen.iter().any(|text| text == "banana"), "{seen:?}");
    }

    let search = run_embed(&path, url, &["search", "--query", &query]);
    let hits = stdout_json(&search);
    let ids = hit_ids(&hits);
    assert!(ids.contains(&"vehicle"), "{ids:?}");
    assert!(ids.contains(&"split"), "{ids:?}");
    for id in ["vehicle", "split"] {
        let hit = hits["hits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["id"] == id)
            .unwrap();
        assert!(hit.get("vectorScore").is_some(), "{hit}");
        assert!(hit.get("lexicalScore").is_none(), "{hit}");
    }

    let replace = run_embed(
        &path,
        url,
        &[
            "collection",
            "http_notes",
            "replace",
            "--id",
            "vehicle",
            "--record",
            &replaced,
        ],
    );
    assert_eq!(replace.status, 0, "stderr={}", replace.stderr);
    let reindex = run_embed(&path, url, &["reindex"]);
    assert_eq!(reindex.status, 0, "stderr={}", reindex.stderr);
    let again = run_embed(&path, url, &["search", "--query", &query]);
    assert_eq!(again.status, 0, "stderr={}", again.stderr);
    let again_hits = stdout_json(&again);
    let ids = hit_ids(&again_hits);
    assert!(ids.contains(&"vehicle"), "{ids:?}");

    let query: SearchQuery = serde_json::from_str(&query).unwrap();
    let embedder = Arc::new(HttpEmbedder::new(url));
    let vault = Vault::open(scratch.vault(), OpenOptions::new(), Some(embedder)).unwrap();
    let lib = vault.search(&[], &query, None).unwrap();
    let lib_json = serde_json::to_value(&lib).unwrap();
    let cli = run_embed(
        &path,
        url,
        &["search", "--query", &vector_query("automobile")],
    );
    assert_eq!(stdout_json(&cli), lib_json);

    let reranker = HttpReranker::new(url);
    let lib = vault.search(&[], &query, Some(&reranker)).unwrap();
    let lib_json = serde_json::to_value(&lib).unwrap();
    assert!(
        lib.hits.iter().all(|hit| hit.rerank_score.is_some()),
        "{lib_json}"
    );
    let cli = run_embed(
        &path,
        url,
        &[
            "search",
            "--query",
            &vector_query("automobile"),
            "--reranker-cohere-url",
            url,
            "--reranker-model",
            RERANK_MODEL,
        ],
    );
    assert_eq!(stdout_json(&cli), lib_json);

    set_dead_origin(&scratch.vault());
    let info = run_embed(&path, url, &["info"]);
    assert_eq!(info.status, 0, "stderr={}", info.stderr);
    let got = run_embed(
        &path,
        url,
        &["collection", "http_notes", "get", "--id", "vehicle"],
    );
    assert_eq!(got.status, 0, "stderr={}", got.stderr);
}
