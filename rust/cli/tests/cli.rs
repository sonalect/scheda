//! A4: thin CLI (`08` AGT-2–5, `12`, `14`). Collection names here must not
//! appear in `rust/cli/src` (SCH-1).

use std::fs;
use std::path::PathBuf;
use std::process::{self, Command};
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Filter, OpenOptions, RecordAdd, Schema, SearchQuery, Vault};
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
    vault
        .create_collection("agent_log", log_decl())
        .unwrap();
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
        "onnx-embedder",
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
    ] {
        assert!(
            !help.contains(gone),
            "legacy {gone} still in help:\n{help}"
        );
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
        "--vault",
        &path,
        "find",
        "--filter",
        &filter,
        "--offset",
        "0",
        "--count",
        "10",
    ]);
    let page = stdout_json(&found);
    assert_eq!(page["records"][0]["id"], "alpha");
    assert_eq!(page["records"][0]["collection"], "agent_notes");

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
    let unusable = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
    ]);
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
        include_str!("../src/onnx.rs")
    );
    for name in ["agent_notes", "agent_log", "agent_pages"] {
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
fn onnx_embedder_dir_must_have_both_files() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let dir = scratch.root.join("ops");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tokenizer.json"), "{}").unwrap();
    let missing = run(&[
        "--vault",
        &path,
        "--onnx-embedder",
        &dir.display().to_string(),
        "info",
    ]);
    assert_eq!(error_json(&missing)["code"], "CODE_INVALID");

    fs::write(dir.join("model.onnx"), b"not-onnx").unwrap();
    fs::write(dir.join("extra.bin"), b"x").unwrap();
    let extra = run(&[
        "--vault",
        &path,
        "--onnx-embedder",
        &dir.display().to_string(),
        "info",
    ]);
    assert_eq!(error_json(&extra)["code"], "CODE_INVALID");
}

#[test]
fn reranker_onnx_only_on_search_and_missing_is_invalid() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let dir = scratch.root.join("rank");
    fs::create_dir_all(&dir).unwrap();
    let missing = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--reranker-onnx",
        &dir.display().to_string(),
    ]);
    assert_eq!(error_json(&missing)["code"], "CODE_INVALID");
}

#[test]
fn embedder_openai_rejects_partial_flags_non_http_and_onnx_together() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let url = "http://127.0.0.1:8080/v1/";
    let ftp = "ftp://127.0.0.1/v1/";
    let model = "Qwen/Qwen3-Embedding-0.6B";

    let url_only = run(&[
        "--vault",
        &path,
        "--embedder-openai-url",
        url,
        "info",
    ]);
    assert_eq!(error_json(&url_only)["code"], "CODE_INVALID");

    let model_only = run(&[
        "--vault",
        &path,
        "--embedder-model",
        model,
        "info",
    ]);
    assert_eq!(error_json(&model_only)["code"], "CODE_INVALID");

    let dim_only = run(&[
        "--vault",
        &path,
        "--embedder-dimensions",
        "1024",
        "info",
    ]);
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
    fs::write(dir.join("model.onnx"), b"not-onnx").unwrap();
    fs::write(dir.join("tokenizer.json"), "{}").unwrap();
    let both = run(&[
        "--vault",
        &path,
        "--embedder-openai-url",
        url,
        "--embedder-model",
        model,
        "--embedder-dimensions",
        "1024",
        "--onnx-embedder",
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
