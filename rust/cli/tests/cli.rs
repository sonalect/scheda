//! The `scheda` command line as a process: one rpc per subcommand, the
//! Protobuf JSON of the reply on stdout, the `ErrorReport` on stderr. The
//! library is the reference: the same call gives the same JSON and code.
//! Collection names here must not appear in `rust/cli/src`.

mod common;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use protocontract::{BoxIter, Failure};
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::rebuild::Stage;
use scheda::v1::reranker_ref::RerankerRef;
use scheda::v1::{
    DocumentId, DocumentQuery, DocumentSearch, EmbedderErrorCode, IndexState, OpenMode,
    SchedaAsync, VaultErrorCode, VaultSync,
};
use scheda::{Error, FixedEmbedder, FixedReranker, Scheda, error_report, parse_command};
use serde::Serialize;
use serde_json::{Value, json};

use common::{
    Run, Scratch, Setup, collection, command, finish, library, names, on, opening, run, run_on,
    run_on_with, source, whole_file_as,
};

/// A collection the Index keeps lexically only, so a process without an
/// embedder indexes it.
const NOTES: &str = "tc_notes";
const PLAIN: &str = r#"{"standard":{},"indexScope":"INDEX_SCOPE_NO_VECTORS"}"#;

/// A new Vault at the scratch path with the collection [`NOTES`].
fn new_vault(scratch: &Scratch) -> PathBuf {
    let path = scratch.vault();
    let made = run_on(
        &path,
        &[
            "--mode",
            "create-new",
            "create-collection",
            NOTES,
            "--schema",
            PLAIN,
        ],
    );
    assert_eq!(made.json()["name"], NOTES);
    path
}

/// The state token a write returns for a Vault with no isolated collection:
/// the commit `HEAD` names.
fn head(path: &Path) -> String {
    common_git(path, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// The reply of creating the Document `id` in `collection` with `text` for
/// its bytes: a `DocumentWriteResult`.
fn put(path: &Path, collection: &str, id: &str, text: &str) -> Value {
    run_on_with(
        path,
        &[
            "collection",
            collection,
            "create",
            "--id",
            id,
            "--note",
            "put",
        ],
        &Setup {
            stdin: Some(text.as_bytes()),
            ..Setup::default()
        },
    )
    .json()
}

/// Standard Base64 of `bytes`: the Protobuf JSON of a `bytes` field.
fn b64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, byte)| n | (u32::from(*byte) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Every file of the Vault except git's and the Index's, with its bytes:
/// what a refused command must leave as it was.
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(base).unwrap().display().to_string();
            if rel == ".git" || rel == ".scheda" {
                continue;
            }
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut out);
    }
    out
}

fn commits(path: &Path) -> String {
    let out = common_git(path, &["rev-parse", "HEAD"]);
    out.trim().to_owned()
}

fn common_git(path: &Path, args: &[&str]) -> String {
    let absent = std::env::temp_dir().join("scheda-absent-gitconfig");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", &absent)
        .env("GIT_CONFIG_SYSTEM", &absent)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// Whether a state of the Index shows it done: it reflects the stored
/// state, and no rebuild runs.
fn settled(state: &IndexState) -> bool {
    let running = state
        .rebuild
        .as_option()
        .and_then(|rebuild| rebuild.stage.as_ref())
        .and_then(|stage| stage.as_known())
        == Some(Stage::STAGE_RUNNING);
    state.version.is_some() && state.version == state.stored_version && !running
}

/// Waits until the Index of `vault` is settled.
fn settle(vault: &Arc<dyn VaultSync>) {
    let index = vault.index(Empty::default()).unwrap();
    let index = {
        let _inside = common::runtime().enter();
        index.to_sync().unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    for state in index.watch(Empty::default()).unwrap() {
        if settled(&state.unwrap()) {
            return;
        }
        assert!(Instant::now() < deadline, "the Index did not settle");
    }
    panic!("the watch ended before the Index settled");
}

/// A stream of the library as the lines the command line prints.
fn json_lines<T: Serialize>(stream: BoxIter<'static, Result<T, Error>>) -> Vec<Value> {
    stream
        .map(|item| serde_json::to_value(item.unwrap()).unwrap())
        .collect()
}

#[test]
fn help_lists_the_subcommands_and_not_the_ones_that_are_gone() {
    let help = run(&["--help"]);
    assert_eq!(help.status, 0, "stderr={}", help.stderr);
    for name in [
        "scheda-info",
        "info",
        "check",
        "list-collections",
        "create-collection",
        "update-collection",
        "delete-collection",
        "batch-collections",
        "batch-documents",
        "query-documents",
        "count-documents",
        "search-documents",
        "collection",
        "mirror",
        "index",
    ] {
        assert!(
            help.stdout
                .lines()
                .any(|line| line.trim_start().starts_with(name)),
            "--help lists {name}: {}",
            help.stdout
        );
    }
    for gone in ["wait-indexed", "index-status", "push-isolated", "find "] {
        assert!(
            !help.stdout.contains(gone),
            "{gone} is gone: {}",
            help.stdout
        );
    }
    let sub = run(&["collection", "x", "--help"]);
    assert_eq!(sub.status, 0, "stderr={}", sub.stderr);
    for name in [
        "describe", "get-many", "history", "create", "upsert", "batch", "query", "count", "search",
    ] {
        assert!(sub.stdout.contains(name), "{name}: {}", sub.stdout);
    }
    let version = run(&["--version"]);
    assert_eq!(version.status, 0);
    assert!(version.stdout.starts_with("scheda "), "{}", version.stdout);
    assert!(help.stderr.is_empty() && version.stderr.is_empty());
}

#[test]
fn usage_errors_and_a_missing_vault_are_invalid_argument() {
    for args in [
        &["--bogus", "info"][..],
        &["no-such-subcommand"],
        &[],
        &["--vault", "x", "collection", "n", "get"],
        &["--vault", "x", "collection", "n", "create", "--id", "a"],
        &[
            "--vault",
            "x",
            "collection",
            "n",
            "get-many",
            "--ids",
            "a",
            "--limit",
            "2",
        ],
    ] {
        let failed = run(args);
        assert_eq!(
            failed.code(),
            "VAULT_ERROR_CODE_INVALID_ARGUMENT",
            "{args:?}"
        );
        assert!(failed.stdout.is_empty(), "{args:?}");
    }
    let missing = run(&["info"]);
    assert_eq!(missing.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert_eq!(missing.failure()["message"], "missing --vault");
}

#[test]
fn scheda_info_needs_no_vault() {
    let info = run(&["scheda-info"]).json();
    assert!(info["version"].is_string(), "{info}");
    let lib = serde_json::to_value(
        common::runtime()
            .block_on(Scheda.info(Empty::default()))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(info, lib);
}

#[test]
fn the_cli_and_the_library_give_the_same_json_and_code() {
    let scratch = Scratch::new("same");
    let path = new_vault(&scratch);
    for (id, text) in [("a", "alpha one"), ("b", "beta two"), ("c", "alpha three")] {
        put(&path, NOTES, id, text);
    }
    let vault = library(&path);
    settle(&vault);

    let info = run_on(&path, &["--writer", "library", "info"]).json();
    assert_eq!(
        info,
        serde_json::to_value(vault.info(Empty::default()).unwrap()).unwrap()
    );
    assert_eq!(
        info["version"],
        json!(head(&path)),
        "info prints the token of the stored state now"
    );

    let query = r#"{"filter":{"prefix":{"column":"body","prefix":"alpha"}},"includeRaw":true}"#;
    let cli = run_on(&path, &["collection", NOTES, "query", "--query", query]).lines();
    let lib = json_lines(
        collection(&vault, NOTES)
            .query(parse_command::<DocumentQuery>(query.as_bytes()).unwrap())
            .unwrap(),
    );
    assert_eq!(cli.len(), 3, "the summary and two Documents: {cli:?}");
    assert_eq!(cli[0], json!({"summary": {"total": "2"}}));
    assert_eq!(cli, lib);

    let search = r#"{"text":"alpha","includeRaw":true}"#;
    let cli = run_on(&path, &["collection", NOTES, "search", "--search", search]).lines();
    let lib = json_lines(
        collection(&vault, NOTES)
            .search(parse_command::<DocumentSearch>(search.as_bytes()).unwrap())
            .unwrap(),
    );
    assert!(
        cli[0].get("summary").is_some(),
        "the summary comes first: {cli:?}"
    );
    assert_eq!(cli.len(), 3, "the summary and two hits: {cli:?}");
    assert_eq!(cli, lib);

    let failed = run_on(&path, &["collection", NOTES, "get", "--id", "missing"]);
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_NOT_FOUND");
    let error = collection(&vault, NOTES)
        .get(DocumentId::default().with_id("missing"))
        .unwrap_err();
    assert!(error.is(VaultErrorCode::NotFound));
    let report = error_report(&error);
    assert_eq!(failed.failure()["code"], json!(report.code));
    assert_eq!(failed.failure()["message"], json!(report.message));
}

#[test]
fn writes_round_trip_with_a_note() {
    let scratch = Scratch::new("writes");
    let path = new_vault(&scratch);
    let file = |id: &str| path.join(NOTES).join(id);

    let made = run_on(&path, &["create-collection", "tc_pages", "--schema", PLAIN]).json();
    assert_eq!(
        made["name"], "tc_pages",
        "a collection is made without a declaration file"
    );

    let created = put(&path, NOTES, "a", "one");
    assert_eq!(created["document"]["id"], "a");
    assert_eq!(created["document"]["raw"], b64(b"one"));
    assert_eq!(
        created["version"],
        json!(head(&path)),
        "the reply carries the token of the commit the write made"
    );
    assert_eq!(fs::read(file("a")).unwrap(), b"one");
    let first = created["document"]["revision"].as_str().unwrap().to_owned();

    let updated = run_on_with(
        &path,
        &[
            "collection",
            NOTES,
            "update",
            "--id",
            "a",
            "--note",
            "second",
            "--revision",
            &first,
        ],
        &Setup {
            stdin: Some(b"two"),
            ..Setup::default()
        },
    )
    .json();
    assert_eq!(updated["document"]["raw"], b64(b"two"));
    assert_ne!(updated["document"]["revision"], json!(first));
    assert_eq!(updated["version"], json!(head(&path)));
    assert_ne!(updated["version"], created["version"]);
    assert_eq!(fs::read(file("a")).unwrap(), b"two");

    let stale = run_on_with(
        &path,
        &[
            "collection",
            NOTES,
            "update",
            "--id",
            "a",
            "--note",
            "third",
            "--revision",
            &first,
        ],
        &Setup {
            stdin: Some(b"three"),
            ..Setup::default()
        },
    );
    assert_eq!(stale.code(), "VAULT_ERROR_CODE_FAILED_PRECONDITION");
    assert_eq!(fs::read(file("a")).unwrap(), b"two");

    let again = run_on_with(
        &path,
        &["collection", NOTES, "create", "--id", "a", "--note", "dup"],
        &Setup {
            stdin: Some(b"dup"),
            ..Setup::default()
        },
    );
    assert_eq!(again.code(), "VAULT_ERROR_CODE_ALREADY_EXISTS");
    let kept = run_on_with(
        &path,
        &[
            "collection",
            NOTES,
            "create",
            "--id",
            "a",
            "--note",
            "dup",
            "--keep-existing",
        ],
        &Setup {
            stdin: Some(b"dup"),
            ..Setup::default()
        },
    )
    .json();
    assert_eq!(
        kept["document"]["raw"],
        b64(b"two"),
        "the Document stored is returned"
    );
    assert_eq!(
        kept["version"], updated["version"],
        "nothing was stored: the token of the state found"
    );

    let upsert = |text: &'static [u8]| {
        run_on_with(
            &path,
            &["collection", NOTES, "upsert", "--id", "b", "--note", "up"],
            &Setup {
                stdin: Some(text),
                ..Setup::default()
            },
        )
        .json()
    };
    assert_eq!(upsert(b"bee")["document"]["raw"], b64(b"bee"));
    let bee2 = upsert(b"bee2");
    assert_eq!(bee2["document"]["raw"], b64(b"bee2"));
    assert_eq!(bee2["version"], json!(head(&path)));

    let history = run_on(&path, &["collection", NOTES, "history", "--id", "a"]).lines();
    let notes: Vec<&Value> = history.iter().map(|entry| &entry["note"]).collect();
    assert_eq!(notes, [&json!("second"), &json!("put")], "newest first");
    let writes: Vec<&str> = history
        .iter()
        .map(|entry| entry["writeId"].as_str().unwrap())
        .collect();
    assert!(writes.iter().all(|id| !id.is_empty()) && writes[0] != writes[1]);

    let listing = run_on(&path, &["collection", NOTES, "get-many"]).lines();
    let ids: Vec<&Value> = listing.iter().map(|doc| &doc["id"]).collect();
    assert_eq!(ids, [&json!("a"), &json!("b")]);
    assert!(listing[0].get("raw").is_none());
    let some = run_on(
        &path,
        &[
            "collection",
            NOTES,
            "get-many",
            "--ids",
            "b",
            "--include-raw",
        ],
    )
    .lines();
    assert_eq!(some.len(), 1);
    assert_eq!(some[0]["raw"], b64(b"bee2"));
    let page = run_on(
        &path,
        &[
            "collection",
            NOTES,
            "get-many",
            "--limit",
            "1",
            "--start-after",
            "a",
        ],
    )
    .lines();
    assert_eq!(page[0]["id"], "b");

    let described = run_on(&path, &["collection", NOTES, "describe"]).json();
    assert_eq!(described["name"], NOTES);
    let got = run_on(&path, &["collection", NOTES, "get", "--id", "b"]).json();
    assert_eq!(got["raw"], b64(b"bee2"));

    let deleted = run_on(
        &path,
        &["collection", NOTES, "delete", "--id", "a", "--note", "del"],
    );
    assert_eq!(
        deleted.json(),
        json!({"version": head(&path)}),
        "a deletion returns the token alone"
    );
    assert!(!file("a").exists());
    let missing = run_on(
        &path,
        &["collection", NOTES, "delete", "--id", "a", "--note", "del"],
    );
    assert_eq!(missing.code(), "VAULT_ERROR_CODE_NOT_FOUND");
    let ignored = run_on(
        &path,
        &[
            "collection",
            NOTES,
            "delete",
            "--id",
            "a",
            "--note",
            "del",
            "--ignore-missing",
        ],
    );
    assert_eq!(
        ignored.json(),
        json!({"version": head(&path)}),
        "nothing was stored: the token of the state found"
    );
    let get = run_on(&path, &["collection", NOTES, "get", "--id", "a"]);
    assert_eq!(get.code(), "VAULT_ERROR_CODE_NOT_FOUND");

    let before = commits(&path);
    let no_note = run_on(&path, &["collection", NOTES, "delete", "--id", "b"]);
    assert_eq!(no_note.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert_eq!(commits(&path), before, "a refused write commits nothing");

    let subject = common_git(&path, &["log", "-1", "--format=%s%n%an <%ae>"]);
    assert_eq!(subject, "del\nTest <test@example.test>\n");
}

#[test]
fn collections_are_listed_updated_and_deleted() {
    let scratch = Scratch::new("collections");
    let path = new_vault(&scratch);
    let listed = run_on(&path, &["list-collections"]).json();
    assert_eq!(listed["collections"][0]["name"], NOTES);

    let updated = run_on(
        &path,
        &["update-collection", NOTES, "--schema", r#"{"standard":{}}"#],
    )
    .json();
    assert_eq!(updated["schema"], json!({"standard": {}}));
    let described = run_on(&path, &["collection", NOTES, "describe"]).json();
    assert_eq!(described, updated);

    let unknown = run_on(&path, &["update-collection", "tc_nope", "--schema", PLAIN]);
    assert_eq!(unknown.code(), "VAULT_ERROR_CODE_NOT_FOUND");

    assert_eq!(
        run_on(&path, &["delete-collection", NOTES]).json(),
        json!({})
    );
    assert_eq!(run_on(&path, &["list-collections"]).json(), json!({}));
    let again = run_on(&path, &["delete-collection", NOTES]);
    assert_eq!(again.code(), "VAULT_ERROR_CODE_NOT_FOUND");
    let ignored = run_on(&path, &["delete-collection", NOTES, "--ignore-missing"]);
    assert_eq!(ignored.json(), json!({}));
    assert_eq!(run_on(&path, &["check"]).json(), json!({}));
}

#[test]
fn schema_from_stdin_and_missing_json_are_handled() {
    let scratch = Scratch::new("stdin-json");
    let path = new_vault(&scratch);
    let made = run_on_with(
        &path,
        &["create-collection", "tc_piped"],
        &Setup {
            stdin: Some(PLAIN.as_bytes()),
            ..Setup::default()
        },
    )
    .json();
    assert_eq!(made["name"], "tc_piped");
    let before = tree(&path);
    for stdin in [&b""[..], b"  \n"] {
        let empty = run_on_with(
            &path,
            &["create-collection", "tc_empty"],
            &Setup {
                stdin: Some(stdin),
                ..Setup::default()
            },
        );
        assert_eq!(empty.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
        assert!(
            empty.failure()["message"]
                .as_str()
                .unwrap()
                .contains("--schema")
        );
    }
    let query = run_on(&path, &["query-documents"]);
    assert_eq!(query.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert_eq!(tree(&path), before);
}

#[test]
fn file_does_not_read_an_open_stdin_pipe() {
    let scratch = Scratch::new("file-stdin");
    let path = new_vault(&scratch);
    let payload = scratch.file("payload", "from the file");
    let mut cmd = command(&[]);
    cmd.args(on(&path))
        .args([
            "collection",
            NOTES,
            "create",
            "--id",
            "f",
            "--note",
            "file",
            "--file",
            &payload,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    // Held open and never written: a command that reads it would hang, and
    // fail the test once the wait is over.
    let _stdin = child.stdin.take();
    let out = finish(child);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(path.join(NOTES).join("f")).unwrap(),
        b"from the file"
    );
}

#[test]
fn an_unreadable_file_is_invalid_argument_before_the_vault_is_opened() {
    let scratch = Scratch::new("bad-file");
    let path = scratch.vault();
    let missing = scratch.path("no-such-file").display().to_string();
    let failed = run_on(
        &path,
        &[
            "--mode",
            "create-new",
            "collection",
            "x",
            "create",
            "--id",
            "a",
            "--note",
            "n",
            "--file",
            &missing,
        ],
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    let report = failed.failure();
    assert!(
        report["message"].as_str().unwrap().contains(&missing),
        "{report}"
    );
    assert!(
        report.get("path").is_none(),
        "the path field is for Vault paths: {report}"
    );
    assert!(!path.exists(), "nothing was opened or made");

    let dir = scratch.path("a-directory");
    fs::create_dir_all(&dir).unwrap();
    let vault = new_vault(&scratch);
    let before = (tree(&vault), commits(&vault));
    let failed = run_on(
        &vault,
        &[
            "collection",
            NOTES,
            "create",
            "--id",
            "a",
            "--note",
            "n",
            "--file",
            &dir.display().to_string(),
        ],
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert_eq!((tree(&vault), commits(&vault)), before);
}

#[test]
fn create_collection_rejects_an_unknown_schema_key_and_writes_nothing() {
    let scratch = Scratch::new("unknown-key");
    let path = new_vault(&scratch);
    let before = (tree(&path), commits(&path));
    for key in [
        "fields",
        "version",
        "layout",
        "excludeLexicalIndex",
        "excludeVectorIndex",
    ] {
        let schema = format!(r#"{{"standard":{{}},"{key}":1}}"#);
        let failed = run_on(
            &path,
            &["create-collection", "tc_extra", "--schema", &schema],
        );
        assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT", "{key}");
        let message = failed.failure()["message"].as_str().unwrap().to_owned();
        assert!(
            message.contains(key),
            "the message names the key: {message}"
        );
        assert_eq!(
            (tree(&path), commits(&path)),
            before,
            "{key} wrote something"
        );
    }
    let nested = run_on(
        &path,
        &[
            "create-collection",
            "tc_extra",
            "--schema",
            r#"{"journal":{"extra":1}}"#,
        ],
    );
    assert_eq!(nested.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert!(
        nested.failure()["message"]
            .as_str()
            .unwrap()
            .contains("journal.extra")
    );
}

#[test]
fn create_collection_rejects_an_undefined_index_scope_and_writes_nothing() {
    let scratch = Scratch::new("undefined-scope");
    let path = new_vault(&scratch);
    let before = (tree(&path), commits(&path));
    for value in [r#""INDEX_SCOPE_EVERYTHING""#, "9"] {
        let schema = format!(r#"{{"standard":{{}},"indexScope":{value}}}"#);
        let failed = run_on(
            &path,
            &["create-collection", "tc_scope", "--schema", &schema],
        );
        assert_eq!(
            failed.code(),
            "VAULT_ERROR_CODE_INVALID_ARGUMENT",
            "{value}"
        );
        assert_eq!((tree(&path), commits(&path)), before, "{value}");
    }
    let defined = run_on(
        &path,
        &[
            "create-collection",
            "tc_scope",
            "--schema",
            r#"{"standard":{},"indexScope":"INDEX_SCOPE_IDS"}"#,
        ],
    );
    assert_eq!(defined.json()["schema"]["indexScope"], "INDEX_SCOPE_IDS");
}

#[test]
fn a_query_document_with_an_unknown_key_is_invalid_argument() {
    let scratch = Scratch::new("strict-query");
    let path = new_vault(&scratch);
    let failed = run_on(
        &path,
        &[
            "collection",
            NOTES,
            "query",
            "--query",
            r#"{"filter":{"exists":{"column":"body"}},"bogus":1}"#,
        ],
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert!(
        failed.failure()["message"]
            .as_str()
            .unwrap()
            .contains("bogus")
    );
    // A key of the filter tree of 0.22.
    let stale = run_on(
        &path,
        &[
            "collection",
            NOTES,
            "query",
            "--query",
            r#"{"filter":{"all":{}}}"#,
        ],
    );
    assert_eq!(stale.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
}

#[test]
fn batch_collections_is_all_or_nothing_and_answers_in_order() {
    let scratch = Scratch::new("batch-collections");
    let path = new_vault(&scratch);
    let lines = |ops: &[&str]| ops.join("\n");
    let create = |name: &str| format!(r#"{{"create":{{"name":"{name}","schema":{PLAIN}}}}}"#);

    let good = lines(&[
        &create("tc_b1"),
        "",
        &create("tc_b2"),
        &format!(r#"{{"delete":{{"name":"{NOTES}"}}}}"#),
    ]);
    let ok = run_on_with(
        &path,
        &["batch-collections"],
        &Setup {
            stdin: Some(good.as_bytes()),
            ..Setup::default()
        },
    )
    .json();
    let results = ok["results"].as_array().unwrap();
    assert_eq!(results.len(), 3, "one result per operation: {ok}");
    assert_eq!(results[0]["collection"]["name"], "tc_b1");
    assert_eq!(results[1]["collection"]["name"], "tc_b2");
    assert!(results[2].get("deleted").is_some());
    let names: Vec<String> = run_on(&path, &["list-collections"]).json()["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["tc_b1", "tc_b2"]);

    let before = (tree(&path), commits(&path));
    let bad_line = lines(&[&create("tc_b3"), r#"{"create":{"name":"tc_b4","bogus":1}}"#]);
    let failed = run_on_with(
        &path,
        &["batch-collections"],
        &Setup {
            stdin: Some(bad_line.as_bytes()),
            ..Setup::default()
        },
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert!(
        failed.failure()["message"]
            .as_str()
            .unwrap()
            .contains("line 2"),
        "{}",
        failed.stderr
    );
    assert_eq!(
        (tree(&path), commits(&path)),
        before,
        "a bad line writes nothing"
    );

    let failing = lines(&[&create("tc_b5"), &create("tc_b1")]);
    let failed = run_on_with(
        &path,
        &["batch-collections"],
        &Setup {
            stdin: Some(failing.as_bytes()),
            ..Setup::default()
        },
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_ALREADY_EXISTS");
    assert_eq!(
        (tree(&path), commits(&path)),
        before,
        "a failing operation writes nothing"
    );
}

#[test]
fn batch_documents_and_collection_batch_are_all_or_nothing() {
    let scratch = Scratch::new("batch-documents");
    let path = new_vault(&scratch);
    run_on(&path, &["create-collection", "tc_pages", "--schema", PLAIN]).json();
    let doc = |c: &str, op: &str, id: &str, text: &str| {
        format!(
            r#"{{"collection":"{c}","operation":{{"{op}":{{"id":"{id}","raw":"{}","note":"n"}}}}}}"#,
            b64(text.as_bytes())
        )
    };
    let good = [
        doc(NOTES, "create", "x", "ex"),
        doc("tc_pages", "upsert", "y", "why"),
        doc(NOTES, "update", "x", "ex2"),
    ]
    .join("\n");
    let ok = run_on_with(
        &path,
        &["batch-documents"],
        &Setup {
            stdin: Some(good.as_bytes()),
            ..Setup::default()
        },
    )
    .json();
    let results = ok["results"].as_array().unwrap();
    assert_eq!(results.len(), 3, "{ok}");
    assert_eq!(results[0]["document"]["collection"], NOTES);
    assert_eq!(results[1]["document"]["collection"], "tc_pages");
    assert_eq!(results[2]["document"]["raw"], b64(b"ex2"));
    assert_eq!(
        ok["version"],
        json!(head(&path)),
        "the batch carries the token of its commit"
    );
    assert_eq!(fs::read(path.join(NOTES).join("x")).unwrap(), b"ex2");

    let before = (tree(&path), commits(&path));
    let bad = [doc(NOTES, "create", "z", "z"), "not json".to_owned()].join("\n");
    let failed = run_on_with(
        &path,
        &["batch-documents"],
        &Setup {
            stdin: Some(bad.as_bytes()),
            ..Setup::default()
        },
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert!(
        failed.failure()["message"]
            .as_str()
            .unwrap()
            .contains("line 2")
    );
    assert_eq!((tree(&path), commits(&path)), before);

    let failing = [
        doc(NOTES, "create", "z", "z"),
        doc("tc_pages", "update", "absent", "a"),
    ]
    .join("\n");
    let failed = run_on_with(
        &path,
        &["batch-documents"],
        &Setup {
            stdin: Some(failing.as_bytes()),
            ..Setup::default()
        },
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_NOT_FOUND");
    assert_eq!((tree(&path), commits(&path)), before);

    let own = |op: &str, id: &str, text: &str| {
        format!(
            r#"{{"{op}":{{"id":"{id}","raw":"{}","note":"n"}}}}"#,
            b64(text.as_bytes())
        )
    };
    let batch = [own("create", "m", "em"), own("upsert", "n", "en")].join("\n");
    let ok = run_on_with(
        &path,
        &["collection", NOTES, "batch"],
        &Setup {
            stdin: Some(batch.as_bytes()),
            ..Setup::default()
        },
    )
    .json();
    assert_eq!(ok["results"].as_array().unwrap().len(), 2);
    assert_eq!(ok["version"], json!(head(&path)));
    let before = (tree(&path), commits(&path));
    let bad = [own("create", "o", "eo"), own("update", "absent", "a")].join("\n");
    let failed = run_on_with(
        &path,
        &["collection", NOTES, "batch"],
        &Setup {
            stdin: Some(bad.as_bytes()),
            ..Setup::default()
        },
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_NOT_FOUND");
    assert_eq!((tree(&path), commits(&path)), before);
}

#[test]
fn queries_and_searches_run_across_collections() {
    let scratch = Scratch::new("vault-queries");
    let path = new_vault(&scratch);
    run_on(&path, &["create-collection", "tc_pages", "--schema", PLAIN]).json();
    put(&path, NOTES, "a", "alpha note");
    put(&path, "tc_pages", "p", "alpha page");
    put(&path, "tc_pages", "q", "other page");

    let all = "{}";
    let count = run_on(&path, &["count-documents", "--query", all]).json();
    assert_eq!(count, json!({"count": "3"}));
    let one = r#"{"collections":{"names":["tc_pages"]}}"#;
    assert_eq!(
        run_on(&path, &["count-documents", "--query", one]).json(),
        json!({"count": "2"})
    );
    // The summary first, then one Document per line.
    let docs = run_on(&path, &["query-documents", "--query", all]).lines();
    assert_eq!(docs[0], json!({"summary": {"total": "3"}}), "{docs:?}");
    assert_eq!(docs.len(), 4);
    assert!(
        docs[1..]
            .iter()
            .all(|doc| doc["document"].is_object() && doc["document"].get("raw").is_none())
    );
    let filtered = r#"{"filter":{"prefix":{"column":"body","prefix":"alpha"}}}"#;
    let docs = run_on(&path, &["query-documents", "--query", filtered]).lines();
    assert_eq!(docs[0], json!({"summary": {"total": "2"}}), "{docs:?}");

    let hits = run_on(
        &path,
        &[
            "search-documents",
            "--search",
            r#"{"search":{"text":"alpha"}}"#,
        ],
    )
    .lines();
    assert!(hits[0].get("summary").is_some(), "{hits:?}");
    let ids: Vec<&str> = hits[1..]
        .iter()
        .filter_map(|hit| hit["hit"]["document"]["id"].as_str())
        .collect();
    assert_eq!(ids.len(), 2, "{hits:?}");
    assert!(ids.contains(&"a") && ids.contains(&"p"));

    let counted = run_on(&path, &["collection", NOTES, "count", "--query", all]).json();
    assert_eq!(counted, json!({"count": "1"}));
}

#[test]
fn no_wait_queries_the_index_as_it_is_and_waiting_sees_the_write() {
    let scratch = Scratch::new("waits");
    let path = new_vault(&scratch);
    put(&path, NOTES, "alpha", "first");
    let all = "{}";
    let ids = |run: Run| -> Vec<String> {
        run.lines()
            .iter()
            .filter_map(|line| line["document"]["id"].as_str())
            .map(str::to_owned)
            .collect()
    };
    // A query after a write by an earlier process sees that write.
    assert_eq!(
        ids(run_on(
            &path,
            &["collection", NOTES, "query", "--query", all]
        )),
        ["alpha"]
    );
    put(&path, NOTES, "beta", "second");
    assert_eq!(
        ids(run_on(
            &path,
            &["collection", NOTES, "query", "--query", all]
        )),
        ["alpha", "beta"]
    );

    // Nobody may index the next commit while this lock is held.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".scheda/index.lock"))
        .unwrap();
    lock.lock().unwrap();
    put(&path, NOTES, "gamma", "third");
    // A query waits for the Index to reach the state at open, and past its
    // deadline fails instead of answering without the write.
    let late = run_on(
        &path,
        &[
            "--index-wait-timeout-ms",
            "300",
            "collection",
            NOTES,
            "query",
            "--query",
            all,
        ],
    );
    assert_eq!(late.code(), "VAULT_ERROR_CODE_DEADLINE_EXCEEDED");
    let started = Instant::now();
    let as_it_is = run_on(
        &path,
        &["--no-wait", "collection", NOTES, "query", "--query", all],
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "--no-wait does not wait"
    );
    assert_eq!(
        ids(as_it_is),
        ["alpha", "beta"],
        "the unindexed commit is not there yet"
    );
    lock.unlock().unwrap();
}

#[test]
fn index_watch_until_settled_and_reindex_end() {
    let scratch = Scratch::new("index");
    let path = new_vault(&scratch);
    put(&path, NOTES, "a", "alpha");

    let watched = run_on(&path, &["index", "watch", "--until-settled"]).lines();
    let last = watched.last().unwrap();
    assert!(last["version"].is_string(), "{watched:?}");
    assert_eq!(last["version"], last["storedVersion"], "{watched:?}");
    let status = run_on(&path, &["index", "status"]).json();
    assert!(status.is_object(), "{status}");

    let reindexed = run_on(&path, &["index", "reindex"]);
    assert_eq!(reindexed.json(), json!({}));
    let after = run_on(&path, &["index", "watch", "--until-settled"]).lines();
    assert!(after.last().unwrap()["version"].is_string());
    assert_eq!(
        run_on(&path, &["--no-wait", "index", "reindex"]).json(),
        json!({})
    );
}

#[test]
fn a_reader_that_closes_stdout_ends_the_command_quietly() {
    let scratch = Scratch::new("broken-pipe");
    let path = new_vault(&scratch);
    let ops: String = (0..2000)
        .map(|i| {
            format!(
                r#"{{"create":{{"id":"d{i:04}","raw":"{}","note":"n"}}}}"#,
                b64(b"filler filler filler")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let batch = run_on_with(
        &path,
        &["collection", NOTES, "batch"],
        &Setup {
            stdin: Some(ops.as_bytes()),
            ..Setup::default()
        },
    );
    assert_eq!(batch.status, 0, "stderr={}", batch.stderr);

    let mut cmd = command(&[]);
    cmd.args(on(&path))
        .args(["collection", NOTES, "get-many", "--include-raw"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    assert!(first.starts_with("{\"collection\""), "{first}");
    drop(stdout);
    let out = finish(child);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn another_stdout_failure_is_storage() {
    let scratch = Scratch::new("full");
    let path = new_vault(&scratch);
    let mut cmd = command(&[]);
    cmd.args(on(&path))
        .arg("info")
        .stdin(Stdio::null())
        .stdout(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .stderr(Stdio::piped());
    let out = finish(cmd.spawn().unwrap());
    assert_eq!(out.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(report["code"], "VAULT_ERROR_CODE_STORAGE", "{report}");
}

#[test]
fn open_flags_reach_the_library() {
    let scratch = Scratch::new("flags");
    let path = new_vault(&scratch);
    let info = run_on(
        &path,
        &[
            "--writer",
            "flagged",
            "--write-timeout-ms",
            "5000",
            "--auto-sync",
            "info",
        ],
    )
    .json();
    assert_eq!(info["writer"], "flagged");

    let one_author = run(&[
        "--vault",
        &path.display().to_string(),
        "--author-name",
        "Only",
        "info",
    ]);
    assert_eq!(one_author.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");

    let empty = scratch.path("empty-folder");
    fs::create_dir_all(&empty).unwrap();
    let flag = run_on(&empty, &["--mode", "open-existing", "info"]);
    let lib = common::runtime()
        .block_on(Scheda.open(opening_with_mode(&empty, OpenMode::OPEN_MODE_OPEN_EXISTING)))
        .err()
        .unwrap();
    assert_eq!(flag.failure()["code"], json!(error_report(&lib).code));
    let made = run_on(&empty, &["--mode", "create-new", "info"]).json();
    assert_eq!(made["path"], json!(empty.display().to_string()));
    let again = run_on(&empty, &["--mode", "create-new", "info"]);
    let lib = common::runtime()
        .block_on(Scheda.open(opening_with_mode(&empty, OpenMode::OPEN_MODE_CREATE_NEW)))
        .err()
        .unwrap();
    assert_eq!(again.failure()["code"], json!(error_report(&lib).code));
}

fn opening_with_mode(path: &Path, mode: OpenMode) -> scheda::v1::VaultOpening {
    let mut request = opening(path, "library");
    request.options = scheda::v1::OpenOptions::default().with_mode(mode).into();
    request
}

#[test]
fn the_mirror_is_set_pushed_and_removed() {
    let scratch = Scratch::new("mirror");
    let path = new_vault(&scratch);
    let bare = scratch.path("bare.git");
    let init = std::process::Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(&bare)
        .env("GIT_CONFIG_GLOBAL", "/nonexistent")
        .env("GIT_CONFIG_SYSTEM", "/nonexistent")
        .status()
        .unwrap();
    assert!(init.success());
    let url = format!("file://{}", bare.display());

    assert_eq!(
        run_on(&path, &["mirror", "info"])
            .json()
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(""),
        ""
    );
    assert_eq!(run_on(&path, &["mirror", "set", &url]).json(), json!({}));
    assert_eq!(run_on(&path, &["mirror", "info"]).json()["url"], json!(url));
    let other = format!("{url}-other");
    let replace = run_on(&path, &["mirror", "set", &other]);
    assert_eq!(replace.code(), "VAULT_ERROR_CODE_ALREADY_EXISTS");
    assert_eq!(
        run_on(&path, &["mirror", "set", &other, "--replace-existing"]).json(),
        json!({})
    );
    assert_eq!(
        run_on(&path, &["mirror", "set", &url, "--replace-existing"]).json(),
        json!({})
    );
    assert_eq!(run_on(&path, &["mirror", "push"]).json(), json!({}));
    let refs = common_git(&bare, &["for-each-ref"]);
    assert!(
        !refs.trim().is_empty(),
        "the mirror holds the pushed commits"
    );
    assert_eq!(run_on(&path, &["mirror", "pull"]).json(), json!({}));
    assert_eq!(run_on(&path, &["mirror", "sync"]).json(), json!({}));
    assert_eq!(run_on(&path, &["mirror", "remove"]).json(), json!({}));
    assert_eq!(
        run_on(&path, &["mirror", "info"])
            .json()
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(""),
        ""
    );
    let bad = run_on(&path, &["mirror", "set", "not a url"]);
    assert_eq!(bad.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
}

const PASSPHRASE: &str = "cli-passphrase-never-on-disk";
const IDENTITY: &str = "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

fn tree_contains(root: &Path, needle: &str) -> bool {
    fn walk(path: &Path, needle: &str) -> bool {
        if path.is_file() {
            return fs::read(path).is_ok_and(|bytes| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            });
        }
        fs::read_dir(path)
            .map(|entries| entries.flatten().any(|entry| walk(&entry.path(), needle)))
            .unwrap_or(false)
    }
    walk(root, needle)
}

#[test]
fn encryption_flags_round_trip_and_the_secret_stays_off_the_output() {
    let scratch = Scratch::new("encryption");
    let path = new_vault(&scratch);
    let sealed = r#"{"standard":{},"encrypted":true}"#;
    let env = [("TC_PASSPHRASE", PASSPHRASE)];
    let with_pass = Setup {
        env: &env,
        stdin: Some(b"sealed text"),
        ..Setup::default()
    };
    let flags = ["--encryption-passphrase-env-var", "TC_PASSPHRASE"];
    let args = |rest: &[&str]| -> Vec<String> {
        flags.iter().chain(rest).map(|s| (*s).to_owned()).collect()
    };
    let run_pass = |rest: &[&str], setup: &Setup| {
        let all = args(rest);
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        run_on_with(&path, &refs, setup)
    };
    let made = run_pass(
        &["create-collection", "tc_sealed", "--schema", sealed],
        &with_pass,
    );
    assert_eq!(made.json()["name"], "tc_sealed");
    let created = run_pass(
        &[
            "collection",
            "tc_sealed",
            "create",
            "--id",
            "k",
            "--note",
            "n",
        ],
        &with_pass,
    );
    assert_eq!(created.json()["document"]["raw"], b64(b"sealed text"));
    let got = run_pass(
        &["collection", "tc_sealed", "get", "--id", "k"],
        &Setup {
            env: &env,
            ..Setup::default()
        },
    );
    assert_eq!(got.json()["raw"], b64(b"sealed text"));
    let blob = format!("{}{}", got.stdout, got.stderr);
    assert!(!blob.contains(PASSPHRASE), "{blob}");
    assert!(!tree_contains(&path, PASSPHRASE));
    assert!(
        !tree_contains(&path, "sealed text"),
        "the Document is sealed on disk"
    );

    let unset = run_pass(
        &["collection", "tc_sealed", "get", "--id", "k"],
        &Setup {
            unset: &["TC_PASSPHRASE"],
            ..Setup::default()
        },
    );
    assert_eq!(unset.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    let empty_env = [("TC_PASSPHRASE", "")];
    let empty = run_pass(
        &["collection", "tc_sealed", "get", "--id", "k"],
        &Setup {
            env: &empty_env,
            ..Setup::default()
        },
    );
    assert_eq!(empty.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    let none = run_on(&path, &["collection", "tc_sealed", "get", "--id", "k"]);
    assert_eq!(none.code(), "VAULT_ERROR_CODE_CRYPTO");

    // The identity file is the other secret; a malformed one fails before
    // the Vault is opened.
    let fresh = scratch.path("identity-vault");
    let bad = scratch.file("bad-identity", "not-an-age-key\n");
    let failed = run_on(
        &fresh,
        &[
            "--mode",
            "create-new",
            "--encryption-identity-file",
            &bad,
            "info",
        ],
    );
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert!(!fresh.exists());
    let unreadable = scratch.path("no-identity").display().to_string();
    let failed = run_on(&fresh, &["--encryption-identity-file", &unreadable, "info"]);
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");

    let identity = scratch.file(
        "identity.txt",
        format!("# created: 2026-01-01T00:00:00Z\n# public key: age1cli\n{IDENTITY}\n"),
    );
    let via_id = ["--encryption-identity-file", identity.as_str()];
    let run_id = |rest: &[&str], setup: &Setup| {
        let all: Vec<&str> = via_id.iter().chain(rest).copied().collect();
        run_on_with(&fresh, &all, setup)
    };
    let sealed_by_id = run_id(
        &[
            "--mode",
            "create-new",
            "create-collection",
            "tc_sealed",
            "--schema",
            sealed,
        ],
        &Setup::default(),
    );
    assert_eq!(sealed_by_id.json()["name"], "tc_sealed");
    let stored = run_id(
        &[
            "collection",
            "tc_sealed",
            "create",
            "--id",
            "k",
            "--note",
            "n",
        ],
        &Setup {
            stdin: Some(b"id sealed"),
            ..Setup::default()
        },
    );
    assert_eq!(stored.json()["document"]["raw"], b64(b"id sealed"));
    let back = run_id(
        &["collection", "tc_sealed", "get", "--id", "k"],
        &Setup::default(),
    );
    assert_eq!(back.json()["raw"], b64(b"id sealed"));
    assert!(!format!("{}{}", back.stdout, back.stderr).contains(IDENTITY));
    assert!(!tree_contains(&fresh, IDENTITY));
}

#[test]
fn no_flag_takes_the_secret_itself() {
    let help = run(&["--help"]).stdout;
    for flag in [
        "--encryption-passphrase ",
        "--encryption-passphrase=",
        "--encryption-identity ",
        "--encryption-secret",
    ] {
        assert!(!help.contains(flag), "{flag}");
    }
    let failed = run(&[
        "--encryption-passphrase",
        PASSPHRASE,
        "--vault",
        "/nonexistent-vault",
        "info",
    ]);
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert!(!failed.stderr.contains(PASSPHRASE), "{}", failed.stderr);
}

#[test]
fn port_flags_that_do_not_fit_together_are_invalid_argument() {
    let scratch = Scratch::new("port-flags");
    let path = new_vault(&scratch);
    let model_dir = scratch.path("model");
    fs::create_dir_all(&model_dir).unwrap();
    let model = model_dir.display().to_string();
    let lone = scratch.path("lone-model");
    fs::create_dir_all(&lone).unwrap();
    fs::write(lone.join("config.json"), "{}").unwrap();
    let lone = lone.display().to_string();
    let missing = scratch.path("no-model").display().to_string();
    let key = "--embedder-api-key-env-var";
    for args in [
        vec![
            "--embedder-openai-url",
            "http://127.0.0.1:1/v1",
            "--embedder-model",
            "m",
        ],
        vec![
            "--embedder-openai-url",
            "http://127.0.0.1:1/v1",
            "--embedder-dimensions",
            "4",
        ],
        vec!["--embedder-model", "m"],
        vec!["--embedder-dimensions", "4"],
        vec![key, "TC_KEY"],
        vec![
            "--embedder-openai-url",
            "ftp://host/v1",
            "--embedder-model",
            "m",
            "--embedder-dimensions",
            "4",
        ],
        vec![
            "--embedder-openai-url",
            "http://127.0.0.1:1/v1",
            "--embedder-model",
            "m",
            "--embedder-dimensions",
            "4",
            key,
            "TC_UNSET_KEY",
        ],
        vec![
            "--model2vec-embedder",
            &model,
            "--embedder-openai-url",
            "http://127.0.0.1:1/v1",
            "--embedder-model",
            "m",
            "--embedder-dimensions",
            "4",
        ],
        vec!["--model2vec-embedder", &model, "--embedder-model", "m"],
        vec!["--model2vec-embedder", &model],
        vec!["--model2vec-embedder", &lone],
        vec!["--model2vec-embedder", &missing],
        vec!["--reranker-cohere-url", "http://127.0.0.1:1/v1"],
        vec!["--reranker-model", "m"],
        vec!["--reranker-api-key-env-var", "TC_KEY"],
        vec![
            "--reranker-cohere-url",
            "ftp://host/v1",
            "--reranker-model",
            "m",
        ],
        vec![
            "--reranker-cohere-url",
            "http://127.0.0.1:1/v1",
            "--reranker-model",
            "m",
            "--reranker-api-key-env-var",
            "TC_UNSET_KEY",
        ],
    ] {
        let mut all = args.clone();
        all.push("info");
        let before = tree(&path);
        let failed = run_on_with(
            &path,
            &all,
            &Setup {
                unset: &["TC_KEY", "TC_UNSET_KEY"],
                ..Setup::default()
            },
        );
        assert_eq!(
            failed.code(),
            "VAULT_ERROR_CODE_INVALID_ARGUMENT",
            "{args:?}: {}",
            failed.stderr
        );
        assert_eq!(tree(&path), before);
    }
}

#[test]
fn valid_port_flags_reach_the_library_which_accepts_them() {
    let scratch = Scratch::new("port-accepted");
    let path = new_vault(&scratch);
    let url = "http://127.0.0.1:1/v1";
    for args in [
        vec![
            "--embedder-openai-url",
            url,
            "--embedder-model",
            "m",
            "--embedder-dimensions",
            "4",
        ],
        vec!["--reranker-cohere-url", url, "--reranker-model", "m"],
    ] {
        let mut all = args.clone();
        all.push("info");
        let before = tree(&path);
        // The ports call no model to open: the URL need not answer.
        let info = run_on(&path, &all).json();
        assert!(info["path"].is_string(), "{args:?}: {info}");
        assert_eq!(tree(&path), before);
    }
}

#[test]
fn a_search_that_reranks_without_a_reranker_is_invalid_argument() {
    let scratch = Scratch::new("no-reranker");
    let path = new_vault(&scratch);
    put(&path, NOTES, "a", "alpha");
    let search = r#"{"text":"alpha","rerankLimit":3}"#;
    let failed = run_on(&path, &["collection", NOTES, "search", "--search", search]);
    assert_eq!(failed.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    let vault = library(&path);
    settle(&vault);
    let error = collection(&vault, NOTES)
        .search(parse_command::<DocumentSearch>(search.as_bytes()).unwrap())
        .map(|stream| stream.collect::<Result<Vec<_>, _>>())
        .and_then(|items| items)
        .unwrap_err();
    assert!(error.is(VaultErrorCode::InvalidArgument));
}

#[test]
fn an_index_of_another_projection_is_stale_and_one_reindex_mends_it() {
    let scratch = Scratch::new("stale-projection");
    let path = scratch.vault();
    // A Vault made and indexed by the Projector the command line had before.
    {
        let mut request = opening(&path, "library");
        request.projector = whole_file_as("scheda-cli/whole-file@1").into();
        request.options = scheda::v1::OpenOptions::default()
            .with_mode(OpenMode::OPEN_MODE_CREATE_NEW)
            .with_writer("library")
            .with_index_poll_ms(10)
            .into();
        let vault = common::open(request);
        let declaration = format!(r#"{{"name":"{NOTES}","schema":{PLAIN}}}"#);
        vault
            .create_collection(parse_command(declaration.as_bytes()).unwrap())
            .unwrap();
        let creation = format!(r#"{{"id":"a","raw":"{}","note":"put"}}"#, b64(b"alpha"));
        let written = collection(&vault, NOTES)
            .create(parse_command(creation.as_bytes()).unwrap())
            .unwrap()
            .version;
        // `settle` alone could hold at the state before the write: wait for
        // the Index to reach the state the write made.
        let index = vault.index(Empty::default()).unwrap();
        let index = {
            let _inside = common::runtime().enter();
            index.to_sync().unwrap()
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut reached = false;
        for state in index.watch(Empty::default()).unwrap() {
            reached = state.unwrap().version == written;
            if reached || Instant::now() >= deadline {
                break;
            }
        }
        assert!(reached, "the Index did not reach the write");
        vault.close(Empty::default()).unwrap();
    }

    let query = r#"{"filter":{"prefix":{"column":"body","prefix":"alpha"}}}"#;
    let stale = run_on(&path, &["collection", NOTES, "query", "--query", query]);
    let report = stale.failure();
    assert_eq!(report["code"], "VAULT_ERROR_CODE_INDEX_STALE");
    let message = report["message"].as_str().unwrap_or_default();
    assert!(message.contains("scheda index reindex"), "{message}");

    assert_eq!(run_on(&path, &["index", "reindex"]).json(), json!({}));
    let mended = run_on(&path, &["collection", NOTES, "query", "--query", query]).lines();
    assert_eq!(mended[0], json!({"summary": {"total": "1"}}));
}

#[test]
fn src_does_not_name_fixture_collections() {
    let src = source();
    for name in [
        NOTES,
        "tc_pages",
        "tc_sealed",
        "tc_b1",
        "tc_extra",
        "tc_vec",
        "tc_piped",
    ] {
        assert!(!names(&src, name), "the CLI source names {name}");
    }
}

// The in-test HTTP twin: an OpenAI-style `/embeddings` and a Cohere-style
// `/rerank` on 127.0.0.1, and the same two ports written on the library.

const HTTP_MODEL: &str = "tc-stub-embedder";
const RERANK_MODEL: &str = "tc-stub-reranker";

fn embed_vector(text: &str) -> [f32; 4] {
    if text.contains("car") || text.contains("automobile") {
        [1.0, 0.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0, 0.0]
    }
}

struct HttpStub {
    base: String,
    /// What the service was asked: `embed TEXT` and `rerank N`.
    seen: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl HttpStub {
    fn start() -> HttpStub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (seen_thread, shutdown_thread) = (Arc::clone(&seen), Arc::clone(&shutdown));
        let handle = thread::spawn(move || {
            while !shutdown_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => handle_conn(stream, &seen_thread),
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        HttpStub {
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
    let Ok(Some((path, body))) = read_request(&mut reader) else {
        return;
    };
    let stream = reader.get_mut();
    if path.contains("embeddings") {
        let inputs: Vec<String> = match body.get("input") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|i| i.as_str().unwrap_or("").to_owned())
                .collect(),
            Some(Value::String(text)) => vec![text.clone()],
            _ => vec![String::new()],
        };
        let mut seen = seen.lock().unwrap();
        let data: Vec<Value> = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                seen.push(format!("embed {input}"));
                json!({"index": index, "embedding": embed_vector(input)})
            })
            .collect();
        drop(seen);
        write_json(stream, &json!({"data": data}));
    } else if path.contains("rerank") {
        let documents = body
            .get("documents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        seen.lock()
            .unwrap()
            .push(format!("rerank {}", documents.len()));
        let results: Vec<Value> = documents
            .iter()
            .enumerate()
            .map(|(index, doc)| {
                let text = doc.as_str().unwrap_or("");
                let score = if text.contains("car") || text.contains("automobile") {
                    0.9
                } else {
                    0.1
                };
                json!({"index": index, "relevance_score": score})
            })
            .collect();
        write_json(stream, &json!({"results": results}));
    }
    let _ = stream.shutdown(Shutdown::Write);
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
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut buf = vec![0; content_length];
    reader.read_exact(&mut buf)?;
    Ok(Some((
        path,
        serde_json::from_slice(&buf).unwrap_or(Value::Null),
    )))
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

/// POSTs `body` to the stub at `url` and parses the JSON answer.
fn post_json(url: &str, body: &Value) -> Result<Value, Error> {
    let unavailable = |message: String| -> Error {
        Failure::new(EmbedderErrorCode::EMBEDDER_ERROR_CODE_UNAVAILABLE, message).into()
    };
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let addr: SocketAddr = hostport
        .parse()
        .map_err(|err| unavailable(format!("{err}")))?;
    let payload = body.to_string();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| unavailable(e.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| unavailable(e.to_string()))?;
    let request = format!(
        "POST /{path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| unavailable(e.to_string()))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| unavailable(e.to_string()))?;
    let text = String::from_utf8(buf).map_err(|e| unavailable(e.to_string()))?;
    let (_, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| unavailable("no body".to_owned()))?;
    serde_json::from_str(body.trim()).map_err(|e| unavailable(e.to_string()))
}

/// The embedder of the stub written on the library's adapter.
fn twin_embedder(base: &str) -> EmbedderRef {
    let url = format!("{}/embeddings", base.trim_end_matches('/'));
    EmbedderRef::from_sync(FixedEmbedder::new(
        HTTP_MODEL,
        4,
        NonZeroUsize::new(32).unwrap(),
        move |texts: Vec<String>| {
            let value = post_json(&url, &json!({"model": HTTP_MODEL, "input": texts}))?;
            let data = value["data"].as_array().cloned().unwrap_or_default();
            Ok(data
                .iter()
                .map(|row| {
                    row["embedding"]
                        .as_array()
                        .map(|v| v.iter().map(|n| n.as_f64().unwrap_or(0.0) as f32).collect())
                        .unwrap_or_default()
                })
                .collect())
        },
    ))
}

/// The reranker of the stub written on the library's adapter.
fn twin_reranker(base: &str) -> RerankerRef {
    let url = format!("{}/rerank", base.trim_end_matches('/'));
    RerankerRef::from_sync(FixedReranker::new(
        NonZeroUsize::new(32).unwrap(),
        move |query: &str, documents: Vec<scheda::v1::Document>| {
            let texts: Vec<String> = documents
                .iter()
                .map(|d| String::from_utf8_lossy(d.raw.as_deref().unwrap_or_default()).into_owned())
                .collect();
            let value = post_json(
                &url,
                &json!({"model": RERANK_MODEL, "query": query, "documents": texts}),
            )?;
            let mut scores = vec![0.0; texts.len()];
            for hit in value["results"].as_array().cloned().unwrap_or_default() {
                let index = hit["index"].as_u64().unwrap_or(0) as usize;
                scores[index] = hit["relevance_score"].as_f64().unwrap_or(0.0);
            }
            Ok(scores)
        },
    ))
}

#[test]
fn the_http_ports_write_vectors_and_search_like_the_library_with_the_same_ports() {
    let stub = HttpStub::start();
    let scratch = Scratch::new("http-twin");
    let path = scratch.vault();
    let embed = [
        "--embedder-openai-url",
        &stub.base,
        "--embedder-model",
        HTTP_MODEL,
        "--embedder-dimensions",
        "4",
    ];
    let rerank = [
        "--reranker-cohere-url",
        &stub.base,
        "--reranker-model",
        RERANK_MODEL,
    ];
    let with = |rest: &[&str]| -> Run {
        let all: Vec<&str> = embed
            .iter()
            .chain(rerank.iter())
            .chain(rest)
            .copied()
            .collect();
        run_on(&path, &all)
    };

    let made = with(&[
        "--mode",
        "create-new",
        "create-collection",
        "tc_vec",
        "--schema",
        r#"{"standard":{}}"#,
    ]);
    assert_eq!(made.json()["name"], "tc_vec");
    for (id, text) in [
        ("decoy", "banana"),
        ("vehicle", "car"),
        ("split", "a car too"),
        ("auto", "automobile"),
    ] {
        let created = run_on_with(
            &path,
            &[
                embed.as_slice(),
                &["collection", "tc_vec", "create", "--id", id, "--note", "n"],
            ]
            .concat(),
            &Setup {
                stdin: Some(text.as_bytes()),
                ..Setup::default()
            },
        );
        assert_eq!(created.json()["document"]["id"], id);
    }

    let vector_only = r#"{"text":"automobile","k":5,"lexicalWeight":0}"#;
    let cli = with(&["collection", "tc_vec", "search", "--search", vector_only]).lines();
    {
        let seen = stub.seen.lock().unwrap();
        assert!(
            seen.iter().any(|s| s.starts_with("embed ")),
            "the embedder was called: {seen:?}"
        );
    }
    let ids: Vec<&str> = cli
        .iter()
        .filter_map(|item| item["hit"]["document"]["id"].as_str())
        .collect();
    assert!(ids.contains(&"auto") && ids.contains(&"vehicle"), "{cli:?}");
    assert_ne!(ids.first(), Some(&"decoy"));

    let mut request = opening(&path, "library");
    request.embedder = twin_embedder(&stub.base).into();
    request.reranker = twin_reranker(&stub.base).into();
    let vault = common::open(request);
    settle(&vault);
    let lib = json_lines(
        collection(&vault, "tc_vec")
            .search(parse_command::<DocumentSearch>(vector_only.as_bytes()).unwrap())
            .unwrap(),
    );
    assert_eq!(cli, lib, "the same hits and scores");

    let reranked = r#"{"text":"automobile","k":5,"lexicalWeight":0,"rerankLimit":5}"#;
    let cli = with(&["collection", "tc_vec", "search", "--search", reranked]).lines();
    assert!(
        stub.seen
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.starts_with("rerank "))
    );
    let lib = json_lines(
        collection(&vault, "tc_vec")
            .search(parse_command::<DocumentSearch>(reranked.as_bytes()).unwrap())
            .unwrap(),
    );
    assert!(
        cli.iter()
            .any(|item| item["hit"]["rerankScore"].is_number()),
        "{cli:?}"
    );
    assert_eq!(cli, lib, "the same reranked hits and scores");
}
