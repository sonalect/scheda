//! The command line as an agent uses it: what it declares is what the
//! library declares, and a write cannot get around the Schema of its
//! collection. Collection names here must not appear in `rust/cli/src`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use scheda::v1::{CollectionDeclaration, DocumentId, VaultErrorCode};
use scheda::{error_report, parse_command};
use serde_json::json;

use common::{Scratch, Setup, collection, library, names, run, run_on, run_on_with, source};

const JOURNAL: &str = r#"{"journal":{}}"#;
const SINGLETON: &str = r#"{"singleton":{"id":"main"}}"#;
const PLAIN: &str = r#"{"standard":{},"excludeVectorIndex":true}"#;

/// A new Vault at the scratch path with the collections `ag_notes`
/// (standard), `ag_log` (journal), `ag_one` (singleton), and `ag_local`
/// (standard, isolated).
fn declared(scratch: &Scratch) -> PathBuf {
    let path = scratch.vault();
    let first = run_on(
        &path,
        &[
            "--mode",
            "create-new",
            "create-collection",
            "ag_notes",
            "--schema",
            PLAIN,
        ],
    );
    assert_eq!(first.json()["name"], "ag_notes");
    for (name, schema) in [
        ("ag_log", JOURNAL),
        ("ag_one", SINGLETON),
        (
            "ag_local",
            r#"{"standard":{},"isolated":true,"excludeVectorIndex":true}"#,
        ),
    ] {
        assert_eq!(
            run_on(&path, &["create-collection", name, "--schema", schema]).json()["name"],
            name
        );
    }
    path
}

/// Every file of the Vault except git's and the Index's.
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
    walk(root, root, &mut out);
    out
}

#[test]
fn src_does_not_name_fixture_collections() {
    let src = source();
    for name in ["ag_notes", "ag_log", "ag_one", "ag_local", "ag_new"] {
        assert!(!names(&src, name), "the CLI source names {name}");
    }
}

#[test]
fn list_collections_matches_the_declaration_files() {
    let scratch = Scratch::new("declared");
    let path = declared(&scratch);
    let listed = run_on(&path, &["list-collections"]).json();
    let collections = listed["collections"].as_array().unwrap();
    let got: Vec<&str> = collections
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert_eq!(got, ["ag_local", "ag_log", "ag_notes", "ag_one"]);

    let kinds: Vec<(&str, Vec<&str>)> = collections
        .iter()
        .map(|c| {
            let name = c["name"].as_str().unwrap();
            let mut keys: Vec<&str> = c["schema"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            (name, keys)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (
                "ag_local",
                vec!["excludeVectorIndex", "isolated", "standard"]
            ),
            ("ag_log", vec!["journal"]),
            ("ag_notes", vec!["excludeVectorIndex", "standard"]),
            ("ag_one", vec!["singleton"]),
        ]
    );
    for c in collections {
        let name = c["name"].as_str().unwrap();
        let file = fs::read_to_string(path.join(name).join(".collection.yaml")).unwrap();
        assert!(file.contains(&format!("name: {name}")), "{name}: {file}");
        assert_eq!(run_on(&path, &["collection", name, "describe"]).json(), *c);
    }
    let vault = library(&path);
    let lib = serde_json::to_value(vault.list_collections(Default::default()).unwrap()).unwrap();
    assert_eq!(listed, lib);
}

#[test]
fn writes_cannot_bypass_the_schema() {
    let scratch = Scratch::new("bypass");
    let path = declared(&scratch);
    let before = tree(&path);
    let write = |args: &[&str], stdin: &[u8]| {
        run_on_with(
            &path,
            args,
            &Setup {
                stdin: Some(stdin),
                ..Setup::default()
            },
        )
    };

    let wrong = write(
        &[
            "collection",
            "ag_one",
            "create",
            "--id",
            "other",
            "--note",
            "n",
        ],
        b"x",
    );
    assert_eq!(wrong.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    let line = write(
        &["collection", "ag_log", "create", "--id", "l", "--note", "n"],
        b"two\nlines",
    );
    assert_eq!(line.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    let update = write(
        &["collection", "ag_log", "update", "--id", "l", "--note", "n"],
        b"x",
    );
    assert_eq!(update.code(), "VAULT_ERROR_CODE_UNIMPLEMENTED");
    let delete = run_on(
        &path,
        &["collection", "ag_log", "delete", "--id", "l", "--note", "n"],
    );
    assert_eq!(delete.code(), "VAULT_ERROR_CODE_UNIMPLEMENTED");
    let unknown = write(
        &[
            "collection",
            "ag_missing",
            "create",
            "--id",
            "a",
            "--note",
            "n",
        ],
        b"x",
    );
    assert_eq!(unknown.code(), "VAULT_ERROR_CODE_NOT_FOUND");
    let bad_id = write(
        &[
            "collection",
            "ag_notes",
            "create",
            "--id",
            "../escape",
            "--note",
            "n",
        ],
        b"x",
    );
    assert_eq!(bad_id.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    let in_batch = write(
        &["batch-documents"],
        br#"{"collection":"ag_notes","operation":{"create":{"id":"ok","raw":"eA==","note":"n"}}}
{"collection":"ag_one","operation":{"create":{"id":"other","raw":"eA==","note":"n"}}}"#,
    );
    assert_eq!(in_batch.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert_eq!(
        tree(&path),
        before,
        "a refused write leaves the Vault as it was"
    );
    assert!(!scratch.path("escape").exists());

    let payload = scratch.file("payload", "from a file");
    let ok = run_on(
        &path,
        &[
            "collection",
            "ag_one",
            "create",
            "--id",
            "main",
            "--note",
            "n",
            "--file",
            &payload,
        ],
    );
    assert_eq!(ok.json()["document"]["id"], "main");
    assert!(ok.json()["version"].is_string(), "{}", ok.stdout);
    let listing = run_on(
        &path,
        &["collection", "ag_one", "get-many", "--include-raw"],
    )
    .lines();
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0]["id"], "main");
    let line = write(
        &["collection", "ag_log", "create", "--id", "l", "--note", "n"],
        b"one line",
    );
    assert_eq!(line.json()["document"]["collection"], "ag_log");
}

#[test]
fn the_cli_and_the_library_create_collections_with_the_same_json_and_code() {
    let cli_scratch = Scratch::new("create-cli");
    let lib_scratch = Scratch::new("create-lib");
    let cli_path = cli_scratch.vault();
    let lib_path = lib_scratch.vault();
    let vault = library(&lib_path);
    // The first call makes the CLI's Vault, as opening makes the library's.
    let first = run_on(
        &cli_path,
        &["create-collection", "ag_new", "--schema", PLAIN],
    );

    let declaration = |name: &str, schema: &str| -> CollectionDeclaration {
        let text = format!(r#"{{"name":"{name}","schema":{schema}}}"#);
        parse_command(text.as_bytes()).unwrap()
    };
    let made = vault
        .create_collection(declaration("ag_new", PLAIN))
        .unwrap();
    assert_eq!(first.json(), serde_json::to_value(&made).unwrap());

    for (name, schema, code) in [
        ("ag_new", PLAIN, VaultErrorCode::AlreadyExists),
        (
            "ag_journal",
            r#"{"journal":{"segmentMaxLines":0}}"#,
            VaultErrorCode::InvalidArgument,
        ),
        ("../outside", PLAIN, VaultErrorCode::InvalidArgument),
    ] {
        let cli = run_on(&cli_path, &["create-collection", name, "--schema", schema]);
        let request = format!(r#"{{"name":"{name}","schema":{schema}}}"#);
        let error = match parse_command::<CollectionDeclaration>(request.as_bytes()) {
            Ok(declaration) => vault.create_collection(declaration).unwrap_err(),
            Err(error) => error,
        };
        assert!(error.is(code), "{name}: {error}");
        let report = error_report(&error);
        assert_eq!(cli.failure()["code"], json!(report.code), "{name}");
        assert_eq!(cli.failure()["message"], json!(report.message), "{name}");
    }

    let listed = run_on(&cli_path, &["list-collections"]).json();
    assert_eq!(
        listed,
        serde_json::to_value(vault.list_collections(Default::default()).unwrap()).unwrap()
    );
    let missing = run_on(&cli_path, &["collection", "ag_new", "get", "--id", "x"]);
    let lib_missing = collection(&vault, "ag_new")
        .get(DocumentId::default().with_id("x"))
        .unwrap_err();
    assert_eq!(
        missing.failure()["code"],
        json!(error_report(&lib_missing).code)
    );
}

#[test]
fn a_command_without_a_vault_is_refused_and_scheda_info_needs_none() {
    let refused = run(&["collection", "ag_notes", "describe"]);
    assert_eq!(refused.code(), "VAULT_ERROR_CODE_INVALID_ARGUMENT");
    assert!(run(&["scheda-info"]).json()["version"].is_string());
}
