//! The `scheda` command line as a process: one rpc per subcommand, the
//! Protobuf JSON of the reply on stdout, the `ErrorReport` on stderr. The
//! library is the reference: the same call gives the same JSON and code.
//! Collection names here must not appear in `rust/cli/src`.

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use buffa_types::google::protobuf::Empty;
use scheda::v1::{DocumentId, OpenMode, SchedaSync, VaultErrorCode};
use scheda::{Scheda, error_report};
use serde_json::{Value, json};

use common::{
    Scratch, Setup, collection, command, library, names, on, opening, run, run_on, run_on_with,
    source,
};

/// The collection most tests write to.
const NOTES: &str = "tc_notes";
const PLAIN: &str = r#"{"standard":{},"excludeVectorIndex":true}"#;

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

/// The Document `id` created in `collection` with `text` for its bytes.
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
        "collection",
        "mirror",
    ] {
        assert!(
            help.stdout
                .lines()
                .any(|line| line.trim_start().starts_with(name)),
            "--help lists {name}: {}",
            help.stdout
        );
    }
    for gone in [
        "wait-indexed",
        "index-status",
        "push-isolated",
        "find ",
        "query-documents",
        "count-documents",
        "search-documents",
        "--no-wait",
    ] {
        assert!(
            !help.stdout.contains(gone),
            "{gone} is gone: {}",
            help.stdout
        );
    }
    assert!(
        !help
            .stdout
            .lines()
            .any(|line| line.trim_start().starts_with("index")),
        "the index subcommand is gone: {}",
        help.stdout
    );
    let sub = run(&["collection", "x", "--help"]);
    assert_eq!(sub.status, 0, "stderr={}", sub.stderr);
    for name in [
        "describe", "get-many", "history", "create", "upsert", "batch",
    ] {
        assert!(sub.stdout.contains(name), "{name}: {}", sub.stdout);
    }
    for gone in ["query", "count", "search"] {
        assert!(
            !sub.stdout
                .lines()
                .any(|line| line.trim_start().starts_with(gone)),
            "{gone} is gone: {}",
            sub.stdout
        );
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
    let lib = serde_json::to_value(Scheda.info(Empty::default()).unwrap()).unwrap();
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

    let info = run_on(&path, &["--writer", "library", "info"]).json();
    assert_eq!(
        info,
        serde_json::to_value(vault.info(Empty::default()).unwrap()).unwrap()
    );

    let cli = run_on(&path, &["collection", NOTES, "get", "--id", "a"]).json();
    let lib = serde_json::to_value(
        collection(&vault, NOTES)
            .get(DocumentId::default().with_id("a"))
            .unwrap(),
    )
    .unwrap();
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
    assert_eq!(created["id"], "a");
    assert_eq!(created["raw"], b64(b"one"));
    assert_eq!(fs::read(file("a")).unwrap(), b"one");
    let first = created["revision"].as_str().unwrap().to_owned();

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
    assert_eq!(updated["raw"], b64(b"two"));
    assert_ne!(updated["revision"], json!(first));
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
    assert_eq!(kept["raw"], b64(b"two"), "the Document stored is returned");

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
    assert_eq!(upsert(b"bee")["raw"], b64(b"bee"));
    assert_eq!(upsert(b"bee2")["raw"], b64(b"bee2"));

    let history = run_on(&path, &["collection", NOTES, "history", "--id", "a"]).lines();
    let notes: Vec<&Value> = history.iter().map(|entry| &entry["note"]).collect();
    assert_eq!(notes, [&json!("second"), &json!("put")], "newest first");

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
    assert_eq!(deleted.json(), json!({}));
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
    assert_eq!(ignored.json(), json!({}));
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
    // Held open and never written: a command that reads it would hang.
    let _stdin = child.stdin.take();
    let out = child.wait_with_output().unwrap();
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
    for key in ["fields", "version", "layout"] {
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
    let out = child.wait_with_output().unwrap();
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
    let out = cmd.output().unwrap();
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
    let lib = Scheda
        .open(opening_with_mode(&empty, OpenMode::OPEN_MODE_OPEN_EXISTING))
        .err()
        .unwrap();
    assert_eq!(flag.failure()["code"], json!(error_report(&lib).code));
    let made = run_on(&empty, &["--mode", "create-new", "info"]).json();
    assert_eq!(made["path"], json!(empty.display().to_string()));
    let again = run_on(&empty, &["--mode", "create-new", "info"]);
    let lib = Scheda
        .open(opening_with_mode(&empty, OpenMode::OPEN_MODE_CREATE_NEW))
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
    assert_eq!(created.json()["raw"], b64(b"sealed text"));
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
    assert_eq!(stored.json()["raw"], b64(b"id sealed"));
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
fn valid_port_flags_reach_the_library_which_answers_unimplemented() {
    let scratch = Scratch::new("port-unimplemented");
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
        let failed = run_on(&path, &all);
        assert_eq!(
            failed.code(),
            "VAULT_ERROR_CODE_UNIMPLEMENTED",
            "{args:?}: {}",
            failed.stderr
        );
        assert_eq!(tree(&path), before);
    }
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
