//! A4/A5/A6: thin CLI (`08` AGT-2–5, ISO-3, `12`, `14`). Collection names here
//! must not appear in `rust/cli/src` (SCH-1).

mod common;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use scheda::{
    Declaration, Embedder, Encryption, Filter, OpenOptions, Record, Reranker, SearchQuery, Vault,
};
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
            root: common::scratch_root("scheda-a4", n),
        }
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
            ".collection.json"
        } else {
            ".collection.yaml"
        };
        fs::write(dir.join(dest), body).unwrap();
    }

    fn payload_file(&self, name: &str, bytes: impl AsRef<[u8]>) -> String {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        path.display().to_string()
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
    run_cmd(args, &[], &[])
}

fn run_without_env(args: &[&str], unset: &str) -> Run {
    run_cmd(args, &[unset], &[])
}

fn run_with_env(args: &[&str], env: &[(&str, &str)]) -> Run {
    run_cmd(args, &[], env)
}

fn run_cmd(args: &[&str], unset: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(scheda_bin());
    cmd.args(args);
    for name in unset {
        cmd.env_remove(name);
    }
    for (name, value) in env {
        cmd.env(name, value);
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

fn rec(fields: Value) -> Vec<u8> {
    serde_json::to_vec(&fields).unwrap()
}

fn notes_schema() -> &'static str {
    "name: agent_notes\nkind: document\nversion: 1\n"
}

fn notes_decl() -> Declaration {
    serde_json::from_value(json!({
        "version": 1,
        "document": {},
    }))
    .unwrap()
}

fn log_decl() -> Declaration {
    serde_json::from_value(json!({
        "version": 1,
        "journal": {},
    }))
    .unwrap()
}

fn fixture_with_note() -> Scratch {
    let scratch = Scratch::new();
    let vault = scratch.init();
    vault
        .create_collection("agent_notes", notes_decl(), None)
        .unwrap();
    vault
        .create_collection("agent_log", log_decl(), None)
        .unwrap();
    vault
        .collection("agent_notes")
        .unwrap()
        .add(
            "alpha",
            rec(json!({"title": "Alpha token", "tag": "blue"})),
            None,
        )
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
    let help_run = run(&["--help"]);
    assert_eq!(help_run.status, 0, "stderr={}", help_run.stderr);
    let help = format!("{}{}", help_run.stdout, help_run.stderr);
    for name in [
        "open",
        "info",
        "create-collection",
        "collection",
        "find",
        "search",
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
        "author-name",
        "author-email",
        "encryption-passphrase-env-var",
        "encryption-identity-file",
        "push-isolated",
        "pull-isolated",
        "sync-isolated",
    ] {
        assert!(help.contains(name), "missing {name} in help:\n{help}");
    }
    for gone in [
        "catalog",
        "find-one",
        "insert-one",
        "replace-one",
        "delete-one",
        "delete_one",
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
        "begin",
        "discard",
    ] {
        assert!(!help.contains(gone), "legacy {gone} still in help:\n{help}");
    }
    assert!(!help.contains("agent_notes"));
    assert!(!help.contains("west_docs"));
    let collection = run(&["collection", "--help"]);
    assert_eq!(collection.status, 0, "stderr={}", collection.stderr);
    let collection_help = format!("{}{}", collection.stdout, collection.stderr);
    for name in ["documents", "ids", "schema", "add", "get"] {
        assert!(
            collection_help.contains(name),
            "missing {name} in collection help:\n{collection_help}"
        );
    }
}

#[test]
fn commit_subcommand_is_gone() {
    let gone_txn = run(&["commit", "-m", "no"]);
    assert_ne!(gone_txn.status, 0, "stdout={}", gone_txn.stdout);
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

    let logged = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "add",
        "--id",
        "alpha",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Alpha token"}))),
    ]);
    assert_eq!(logged.status, 0, "stderr={}", logged.stderr);

    let search = run(&["--vault", &path, "search", "--query", &query]);
    let hits = stdout_json(&search);
    assert_eq!(
        hits["hits"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "CLI has no default Projector until F15, so search has no FTS slots: {hits}"
    );

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
    assert!(record.get("path").is_none());

    let add = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "add",
        "--id",
        "e1",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"logged"}))),
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);
    assert!(add.stdout.trim().is_empty());
}

#[test]
fn collection_remove_deletes_the_record() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let gone = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "remove",
        "--id",
        "alpha",
    ]);
    assert_eq!(gone.status, 0, "stderr={}", gone.stderr);
    assert!(gone.stdout.trim().is_empty());
    let missing = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "get",
        "--id",
        "alpha",
    ]);
    assert_eq!(error_json(&missing)["code"], "CODE_NOT_FOUND");
    let journal = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "add",
        "--id",
        "e1",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"logged"}))),
    ]);
    assert_eq!(journal.status, 0, "stderr={}", journal.stderr);
    let bad = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "remove",
        "--id",
        "e1",
    ]);
    assert_eq!(error_json(&bad)["code"], "CODE_UNSUPPORTED");
}

#[test]
fn mutating_command_commit_message_and_identity() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let add = run(&[
        "--vault",
        &path,
        "--author-name",
        "Ada",
        "--author-email",
        "ada@invalid",
        "collection",
        "agent_notes",
        "add",
        "--id",
        "beta",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Beta","tag":"blue"}))),
        "--message",
        "note beta",
    ]);
    assert_eq!(add.status, 0, "stderr={}", add.stderr);
    let hist = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "history",
        "--id",
        "beta",
        "--offset",
        "0",
        "--count",
        "10",
    ]);
    let page = stdout_json(&hist);
    assert!(
        page["entries"][0]["message"]
            .as_str()
            .unwrap()
            .contains("note beta"),
        "{page}"
    );
    assert_eq!(page["entries"][0]["author"], "Ada <ada@invalid>");
}

#[test]
fn extra_field_is_invalid_and_write_nothing() {
    let scratch = Scratch::new();
    drop(scratch.init());
    scratch.write_schema("notes.yaml", notes_schema());
    scratch.write_schema("log.yaml", "name: agent_log\nkind: journal\nversion: 1\n");
    let path = vault_arg(&scratch);

    let extra = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "add",
        "--id",
        "rogue",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"x","unknown":"y"}))),
    ]);
    assert_eq!(extra.status, 0, "stderr={}", extra.stderr);
    let got = scratch
        .reopen()
        .collection("agent_log")
        .unwrap()
        .get("rogue")
        .unwrap();
    assert_eq!(got.id.as_deref(), Some("rogue"));

    let keep = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "keep",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Keep"}))),
    ]);
    assert_eq!(keep.status, 0, "stderr={}", keep.stderr);
    assert!(scratch.vault().join("agent_notes").join("keep").is_file());
    let extra_replace = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "replace",
        "--id",
        "keep",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Keep","unknown":"y"}))),
    ]);
    assert_eq!(extra_replace.status, 0, "stderr={}", extra_replace.stderr);
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
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"B"}))),
    ]);
    assert_eq!(error_json(&locked)["code"], "CODE_LOCKED");
    drop(writer);

    let invalid = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "add",
        "--id",
        "x",
        "--file",
        &scratch.payload_file("payload", "line\nbreak"),
    ]);
    assert_eq!(error_json(&invalid)["code"], "CODE_INVALID");

    let index = scratch.vault().join(".scheda").join("index");
    let generation = match fs::read_to_string(index.join("current")) {
        Ok(name) => index.join(name.trim()),
        Err(_) => index,
    };
    fs::write(generation.join("format"), "999\n").unwrap();
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
    assert!(declared.get("version").is_none());
    let on_disk =
        fs::read_to_string(scratch.vault().join("agent_notes").join(".collection.yaml")).unwrap();
    assert!(
        on_disk.contains("version: 1"),
        "create_collection must write generation 1: {on_disk}"
    );
    assert!(
        !on_disk.contains("fields:"),
        "create_collection must not write fields: {on_disk}"
    );

    let info = run(&["--vault", &path, "info"]);
    let info = stdout_json(&info);
    let names: Vec<&str> = info["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["agent_notes"]);

    let add = run(&[
        "--vault",
        &path,
        "collection",
        "agent_notes",
        "add",
        "--id",
        "alpha",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"A"}))),
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
    assert!(record.get("path").is_none());
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
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"A","tag":"t"}))),
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

const CLI_PASS: &str = "p16-cli-passphrase-never-on-disk";
const CLI_IDENTITY: &str =
    "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

fn encrypted_decl() -> Declaration {
    serde_json::from_value(json!({
        "version": 1,
        "encrypted": true,
        "document": {},
    }))
    .unwrap()
}

fn tree_contains_text(root: &Path, needle: &str) -> bool {
    fn walk(path: &Path, needle: &str) -> bool {
        if path.is_file() {
            return fs::read_to_string(path).is_ok_and(|text| text.contains(needle));
        }
        let Ok(entries) = fs::read_dir(path) else {
            return false;
        };
        entries.flatten().any(|entry| walk(&entry.path(), needle))
    }
    walk(root, needle)
}

#[test]
fn create_collection_rejects_layout_json_key() {
    let scratch = Scratch::new();
    let path = vault_arg(&scratch);
    assert_eq!(run(&["--vault", &path, "open"]).status, 0);
    let schema = json!({
        "document": {},
        "layout": ["c/{id}.md"],
    })
    .to_string();
    let err = run(&[
        "--vault",
        &path,
        "create-collection",
        "agent_notes",
        "--schema",
        &schema,
    ]);
    assert_eq!(error_json(&err)["code"], "CODE_INVALID");
    assert!(
        !scratch
            .vault()
            .join("agent_notes")
            .join(".collection.yaml")
            .is_file()
    );
}

#[test]
fn collection_documents_and_ids() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let ids = run(&["--vault", &path, "collection", "agent_notes", "ids"]);
    assert_eq!(stdout_json(&ids), json!(["alpha"]));
    let docs = run(&["--vault", &path, "collection", "agent_notes", "documents"]);
    let docs = stdout_json(&docs);
    assert_eq!(docs.as_array().map(Vec::len), Some(1));
    assert_eq!(docs[0]["id"], "alpha");

    let journal_ids = run(&["--vault", &path, "collection", "agent_log", "ids"]);
    assert_eq!(error_json(&journal_ids)["code"], "CODE_UNSUPPORTED");
    let journal_docs = run(&["--vault", &path, "collection", "agent_log", "documents"]);
    assert_eq!(error_json(&journal_docs)["code"], "CODE_UNSUPPORTED");
}

#[test]
fn encryption_secret_is_not_an_argv_flag() {
    let out = run(&[
        "--encryption-passphrase",
        CLI_PASS,
        "--vault",
        "/tmp/scheda-missing-vault",
        "info",
    ]);
    assert_ne!(out.status, 0);
    let err = error_json(&out);
    assert_eq!(err["code"], "CODE_INVALID");
}

#[test]
fn encryption_passphrase_env_round_trips_and_stays_off_output() {
    let scratch = Scratch::new();
    let secret = Encryption::new().passphrase(CLI_PASS);
    let vault = Vault::open(scratch.vault(), OpenOptions::new().encryption(secret), None).unwrap();
    vault
        .create_collection("agent_sealed", encrypted_decl(), None)
        .unwrap();
    vault
        .collection("agent_sealed")
        .unwrap()
        .add("alpha", rec(json!({"title": "sealed"})), None)
        .unwrap();
    drop(vault);

    let path = vault_arg(&scratch);
    let missing = run_without_env(
        &[
            "--vault",
            &path,
            "--encryption-passphrase-env-var",
            "SCHEDA_P16_PASS",
            "collection",
            "agent_sealed",
            "get",
            "--id",
            "alpha",
        ],
        "SCHEDA_P16_PASS",
    );
    assert_eq!(error_json(&missing)["code"], "CODE_INVALID");

    let got = run_with_env(
        &[
            "--vault",
            &path,
            "--encryption-passphrase-env-var",
            "SCHEDA_P16_PASS",
            "collection",
            "agent_sealed",
            "get",
            "--id",
            "alpha",
        ],
        &[("SCHEDA_P16_PASS", CLI_PASS)],
    );
    assert_eq!(stdout_json(&got)["id"], "alpha");
    let blob = format!("{}{}", got.stdout, got.stderr);
    assert!(!blob.contains(CLI_PASS), "{blob}");
    assert!(!tree_contains_text(&scratch.vault(), CLI_PASS));

    let ids = run(&["--vault", &path, "collection", "agent_sealed", "ids"]);
    assert_eq!(stdout_json(&ids), json!(["alpha"]));
    let docs = run(&["--vault", &path, "collection", "agent_sealed", "documents"]);
    assert_eq!(error_json(&docs)["code"], "CODE_CRYPTO");
}

#[test]
fn encryption_identity_file_round_trips_and_malformed_is_invalid_before_open() {
    let scratch = Scratch::new();
    let dest = scratch.vault();
    assert!(!dest.exists() || !dest.join(".git").exists());
    let bad = scratch.root.join("bad-identity");
    fs::write(&bad, "not-an-age-key\n").unwrap();
    let bad_run = run(&[
        "--vault",
        &dest.display().to_string(),
        "--encryption-identity-file",
        &bad.display().to_string(),
        "open",
    ]);
    assert_eq!(error_json(&bad_run)["code"], "CODE_INVALID");
    assert!(!dest.join(".git").exists());

    let secret = Encryption::new().identity(CLI_IDENTITY);
    let vault = Vault::open(dest, OpenOptions::new().encryption(secret), None).unwrap();
    vault
        .create_collection("agent_sealed", encrypted_decl(), None)
        .unwrap();
    vault
        .collection("agent_sealed")
        .unwrap()
        .add("alpha", rec(json!({"title": "sealed"})), None)
        .unwrap();
    drop(vault);

    let file = scratch.root.join("identity.txt");
    fs::write(
        &file,
        format!("# created: 2026-01-01T00:00:00Z\n# public key: age1cli\n{CLI_IDENTITY}\n"),
    )
    .unwrap();
    let path = vault_arg(&scratch);
    let got = run(&[
        "--vault",
        &path,
        "--encryption-identity-file",
        &file.display().to_string(),
        "collection",
        "agent_sealed",
        "get",
        "--id",
        "alpha",
    ]);
    assert_eq!(stdout_json(&got)["id"], "alpha");
    let blob = format!("{}{}", got.stdout, got.stderr);
    assert!(!blob.contains(CLI_IDENTITY), "{blob}");
}

const TREE_SEALED: &str = "agent-sealed-tree-token";
const NESTED_SEALED: &str = "agent-sealed-nested-token";

fn isolated_schema_yaml() -> &'static str {
    "name: agent_sealed\nkind: document\nversion: 1\nisolated: true\n"
}

fn sealed_document(token: &str) -> String {
    format!("---\nid: sealed\nagent_pass: {token}\n---\n")
}

fn plant_isolated_sealed(scratch: &Scratch) {
    let dir = scratch.vault().join("agent_sealed");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".collection.yaml"), isolated_schema_yaml()).unwrap();
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
    assert_eq!(isolated["declaration"]["isolated"], true);
    assert!(isolated["declaration"].get("fields").is_none());
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
    assert_eq!(
        search["hits"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "document collections do not index title for search: {search}"
    );
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
    assert_eq!(
        scoped_search["hits"].as_array().map(Vec::len).unwrap_or(0),
        0
    );
    assert_no_sealed(&scoped_search.to_string());

    let isolated_get = stdout_json(&run(&[
        "--vault",
        &path,
        "collection",
        "agent_sealed",
        "get",
        "--id",
        "sealed.md",
    ]));
    assert_eq!(isolated_get["id"], "sealed.md");
    let isolated_record: scheda::Record = serde_json::from_value(isolated_get.clone()).unwrap();
    let isolated_text = String::from_utf8_lossy(isolated_record.raw.as_deref().unwrap_or(&[]));
    assert!(isolated_text.contains(NESTED_SEALED));
    assert!(!isolated_text.contains(TREE_SEALED));

    let named = stdout_json(&run(&[
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
    ]));
    assert_eq!(named["records"].as_array().unwrap().len(), 1);
    assert_eq!(named["records"][0]["id"], "sealed.md");
    assert_eq!(named["records"][0]["collection"], "agent_sealed");

    let isolated_find = stdout_json(&run(&[
        "--vault",
        &path,
        "collection",
        "agent_sealed",
        "find",
        "--filter",
        &filter,
        "--offset",
        "0",
        "--count",
        "10",
    ]));
    assert_eq!(isolated_find["records"].as_array().unwrap().len(), 1);
    assert_eq!(isolated_find["records"][0]["id"], "sealed.md");

    let ignore = Command::new("git")
        .args(["-C", &path, "check-ignore", "-q", ".scheda"])
        .status()
        .expect("git check-ignore");
    assert!(ignore.success(), ".scheda must be gitignored");
    let ignore_index = Command::new("git")
        .args(["-C", &path, "check-ignore", "-q", ".scheda/index"])
        .status()
        .expect("git check-ignore index");
    assert!(ignore_index.success(), ".scheda/index must be gitignored");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let scheda = scratch.vault().join(".scheda");
        let mode = fs::metadata(&scheda).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let index = scheda.join("index");
        let mode = fs::metadata(&index).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
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

#[test]
fn rerank_limit_flag_on_search_and_rejects_zero_or_below_k() {
    let scratch = fixture_with_note();
    let path = vault_arg(&scratch);
    let search_help = run(&["search", "--help"]);
    assert_eq!(search_help.status, 0, "stderr={}", search_help.stderr);
    let help = format!("{}{}", search_help.stdout, search_help.stderr);
    assert!(
        help.contains("rerank-limit"),
        "missing rerank-limit:\n{help}"
    );

    let zero = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--rerank-limit",
        "0",
    ]);
    assert_eq!(error_json(&zero)["code"], "CODE_INVALID");

    let json_zero = serde_json::to_string(
        &SearchQuery::default()
            .with_text("Alpha")
            .with_rerank_limit(0),
    )
    .unwrap();
    let from_json = run(&["--vault", &path, "search", "--query", &json_zero]);
    assert_eq!(error_json(&from_json)["code"], "CODE_INVALID");

    let logged = run(&[
        "--vault",
        &path,
        "collection",
        "agent_log",
        "add",
        "--id",
        "alpha",
        "--file",
        &scratch.payload_file("payload", rec(json!({"title":"Alpha token"}))),
    ]);
    assert_eq!(logged.status, 0, "stderr={}", logged.stderr);

    let ignored = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--rerank-limit",
        "2",
    ]);
    assert_eq!(ignored.status, 0, "stderr={}", ignored.stderr);
    let hits = stdout_json(&ignored);
    assert_eq!(
        hits["hits"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "CLI has no default Projector, so search has no FTS slots: {hits}"
    );

    let below_k = run(&[
        "--vault",
        &path,
        "search",
        "--query",
        &text_query("Alpha"),
        "--reranker-cohere-url",
        "http://127.0.0.1:1/",
        "--reranker-model",
        "scheda-test-rerank",
        "--rerank-limit",
        "2",
    ]);
    let err = error_json(&below_k);
    assert_eq!(err["code"], "CODE_INVALID");
    let message = err["message"].as_str().unwrap_or("");
    assert!(
        message.contains('2') && message.contains("10"),
        "below-k message should name both numbers: {message}"
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
        .map(|arr| arr.iter().filter_map(|hit| hit["id"].as_str()).collect())
        .unwrap_or_default()
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
            let inputs: Vec<String> = match body.get("input") {
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| item.as_str().unwrap_or("").to_owned())
                    .collect(),
                Some(Value::String(text)) => vec![text.clone()],
                _ => vec![String::new()],
            };
            let mut seen = seen.lock().unwrap();
            let data: Vec<Value> = inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    seen.push(input.clone());
                    json!({ "index": index, "embedding": embed_vector(input) })
                })
                .collect();
            drop(seen);
            write_json(stream, &json!({ "data": data }));
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

    fn dimensions(&self) -> NonZeroUsize {
        NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
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

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let value = post_json(&self.url, &json!({ "model": self.model, "input": texts }))?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| scheda::Error::invalid("embeddings response has no data"))?;
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        for (i, row) in data.iter().enumerate() {
            let index = row
                .get("index")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(i);
            let embedding = row
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| scheda::Error::invalid("embeddings response has no embedding"))?;
            let vector = embedding
                .iter()
                .map(|item| {
                    item.as_f64()
                        .map(|n| n as f32)
                        .ok_or_else(|| scheda::Error::invalid("embeddings response is not float32"))
                })
                .collect::<Result<Vec<f32>, _>>()?;
            let Some(slot) = slots.get_mut(index) else {
                return Err(scheda::Error::invalid(
                    "embeddings response index is out of range",
                ));
            };
            *slot = Some(vector);
        }
        slots
            .into_iter()
            .map(|vector| {
                vector
                    .ok_or_else(|| scheda::Error::invalid("embeddings response is missing a text"))
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
    fn many_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(32).unwrap()
    }

    fn score(&self, query: &str, record: &Record) -> Result<f64, scheda::Error> {
        let scores = self.score_many(query, &[record])?;
        scores
            .into_iter()
            .next()
            .ok_or_else(|| scheda::Error::invalid("rerank response has no relevance_score"))
    }

    fn score_many(&self, query: &str, records: &[&Record]) -> Result<Vec<f64>, scheda::Error> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let documents = records
            .iter()
            .map(|record| match record.raw.as_deref() {
                Some(raw) => String::from_utf8(raw.to_vec())
                    .map_err(|err| scheda::Error::invalid(err.to_string())),
                None => Ok(String::new()),
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
    let schema = serde_json::to_string(&log_decl()).unwrap();
    let decoy = scratch.payload_file("decoy", rec(json!({"title":"banana"})));
    let vehicle = scratch.payload_file("vehicle", rec(json!({"title":"car"})));
    let split = scratch.payload_file("split", rec(json!({"title":"car"})));
    let replaced = scratch.payload_file("auto", rec(json!({"title":"automobile"})));
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
                "--file",
                record,
            ],
        );
        assert_eq!(add.status, 0, "stderr={}", add.stderr);
    }

    {
        let seen = stub.seen.lock().unwrap();
        assert!(
            seen.is_empty(),
            "CLI has no default Projector, so the embedder must not run: {seen:?}"
        );
    }

    let search = run_embed(&path, url, &["search", "--query", &query]);
    let hits = stdout_json(&search);
    let ids = hit_ids(&hits);
    assert!(
        ids.is_empty(),
        "CLI has no default Projector, so search has no vector slots: {ids:?}"
    );

    let added = run_embed(
        &path,
        url,
        &[
            "collection",
            "http_notes",
            "add",
            "--id",
            "auto",
            "--file",
            &replaced,
        ],
    );
    assert_eq!(added.status, 0, "stderr={}", added.stderr);
    let reindex = run_embed(&path, url, &["reindex"]);
    assert_eq!(reindex.status, 0, "stderr={}", reindex.stderr);
    let again = run_embed(&path, url, &["search", "--query", &query]);
    assert_eq!(again.status, 0, "stderr={}", again.stderr);
    let again_hits = stdout_json(&again);
    assert!(
        hit_ids(&again_hits).is_empty(),
        "reindex without a Projector still has no vector slots: {again_hits}"
    );

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
