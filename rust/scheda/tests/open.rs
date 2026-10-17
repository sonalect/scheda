//! `Vault::open`: what each `OpenMode` does with each kind of path, the
//! generation check, and what a refusal leaves on disk.

mod common;

use scheda::Vault;
use scheda::v1::{OpenMode, OpenOptions, VaultErrorCode as Code};

use common::{Scratch, git, snapshot, vault, work_tree, write};

fn options(mode: OpenMode) -> OpenOptions {
    OpenOptions::default().with_mode(mode)
}

/// Opens at `path` in `mode` and asserts the refusal `code` and that the
/// path is as it was.
fn refused(path: &std::path::Path, mode: OpenMode, code: Code) {
    let before = snapshot(path);
    let existed = path.exists();
    let error = Vault::open(path, options(mode)).unwrap_err();
    assert!(error.is(code), "{mode:?}: {error}");
    assert_eq!(path.exists(), existed, "{mode:?}");
    assert_eq!(snapshot(path), before, "{mode:?}");
}

#[test]
fn a_missing_path_is_not_found_in_open_existing_and_stays_missing() {
    let scratch = Scratch::new("missing");
    let path = scratch.join("nowhere");
    refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotFound);
    assert!(!path.exists());
}

#[test]
fn an_empty_folder_is_not_a_vault_in_open_existing_and_stays_empty() {
    let scratch = Scratch::new("empty");
    let path = scratch.join("empty");
    std::fs::create_dir(&path).unwrap();
    refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault);
    assert!(snapshot(&path).is_empty());
}

#[test]
fn a_git_work_tree_without_the_marker_is_refused_by_open_existing_and_create_new() {
    let scratch = Scratch::new("tree");
    let path = scratch.join("tree");
    work_tree(
        &path,
        &[(
            "notes/.collection.yaml",
            "name: notes\nschema:\n  standard: {}\n",
        )],
    );
    refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault);
    refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists);
    assert!(!path.join(".scheda.yaml").exists());
    assert!(!path.join(".scheda").exists());
}

#[test]
fn another_folder_is_refused_by_every_mode() {
    let scratch = Scratch::new("other");
    let path = scratch.join("other");
    write(&path, "readme.md", "not a Vault");
    refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault);
    refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists);
    refused(
        &path,
        OpenMode::OPEN_MODE_OPEN_OR_CREATE,
        Code::InvalidArgument,
    );
    let error = Vault::open(&path, OpenOptions::default()).unwrap_err();
    assert!(error.is(Code::InvalidArgument), "unset is open or create");
    assert_eq!(snapshot(&path).len(), 1);
}

#[test]
fn a_file_is_not_a_vault() {
    let scratch = Scratch::new("file");
    let path = scratch.join("file");
    std::fs::write(&path, b"x").unwrap();
    refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault);
    refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists);
    refused(
        &path,
        OpenMode::OPEN_MODE_OPEN_OR_CREATE,
        Code::InvalidArgument,
    );
}

#[test]
fn a_folder_inside_a_work_tree_is_another_folder() {
    let scratch = Scratch::new("inner");
    let tree = scratch.join("tree");
    vault(&tree);
    let inner = tree.join("notes");
    refused(&inner, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault);
}

#[test]
fn create_new_refuses_a_vault_and_the_other_modes_open_it() {
    let scratch = Scratch::new("vault");
    let path = scratch.join("vault");
    vault(&path);
    refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists);
    for mode in [
        OpenMode::OPEN_MODE_OPEN_EXISTING,
        OpenMode::OPEN_MODE_OPEN_OR_CREATE,
        OpenMode::OPEN_MODE_UNSPECIFIED,
    ] {
        let vault = Vault::open(&path, options(mode)).unwrap();
        vault.close().unwrap();
    }
    let vault = Vault::open(&path, OpenOptions::default()).unwrap();
    vault.close().unwrap();
    assert!(path.join(".scheda/writer").is_file());
}

#[test]
fn another_generation_is_generation_in_every_mode_and_before_any_collection_file() {
    for version in ["1", "3"] {
        let scratch = Scratch::new("generation");
        let path = scratch.join("old");
        work_tree(
            &path,
            &[
                (".scheda.yaml", &format!("version: {version}\n")),
                // Not a valid collection file: reading it would be INVALID_ARGUMENT.
                ("notes/.collection.yaml", "this: [is not\n"),
            ],
        );
        for mode in [
            OpenMode::OPEN_MODE_UNSPECIFIED,
            OpenMode::OPEN_MODE_OPEN_OR_CREATE,
            OpenMode::OPEN_MODE_OPEN_EXISTING,
            OpenMode::OPEN_MODE_CREATE_NEW,
        ] {
            refused(&path, mode, Code::Generation);
        }
        assert!(!path.join(".scheda").exists());
    }
}

#[test]
fn an_unknown_key_in_the_marker_and_a_leftover_version_in_a_collection_file_open() {
    let scratch = Scratch::new("unknown");
    let path = scratch.join("vault");
    work_tree(
        &path,
        &[
            (".scheda.yaml", "version: 2\nsomething: later\n"),
            (
                "notes/.collection.yaml",
                "version: 1\nname: notes\nschema:\n  standard: {}\n",
            ),
        ],
    );
    Vault::open(&path, OpenOptions::default())
        .unwrap()
        .close()
        .unwrap();
}

#[test]
fn a_broken_marker_is_invalid_argument_and_writes_nothing() {
    let scratch = Scratch::new("marker");
    let path = scratch.join("vault");
    work_tree(&path, &[(".scheda.yaml", "{}\n")]);
    refused(
        &path,
        OpenMode::OPEN_MODE_OPEN_EXISTING,
        Code::InvalidArgument,
    );
}

#[test]
fn an_invalid_collection_file_fails_open_and_writes_nothing() {
    let scratch = Scratch::new("collection");
    let path = scratch.join("vault");
    work_tree(
        &path,
        &[
            (".scheda.yaml", "version: 2\n"),
            (
                "notes/.collection.yaml",
                "name: notes\nschema:\n  document: {}\n",
            ),
        ],
    );
    refused(
        &path,
        OpenMode::OPEN_MODE_OPEN_EXISTING,
        Code::InvalidArgument,
    );
    assert!(!path.join(".scheda").exists());
}

#[test]
fn a_mirror_that_is_not_the_origin_is_invalid_argument() {
    let scratch = Scratch::new("mirror");
    let path = scratch.join("vault");
    vault(&path);
    let url = "https://example.test/org/vault.git";
    git(&path, &["remote", "add", "origin", url]);
    let before = snapshot(&path);
    let other = OpenOptions::default().with_mirror("https://example.test/org/other.git");
    let error = Vault::open(&path, other).unwrap_err();
    assert!(error.is(Code::InvalidArgument));
    assert_eq!(snapshot(&path), before);
    Vault::open(&path, OpenOptions::default().with_mirror(url))
        .unwrap()
        .close()
        .unwrap();
}

#[test]
fn options_that_break_a_rule_are_invalid_argument() {
    let scratch = Scratch::new("options");
    let path = scratch.join("vault");
    vault(&path);
    let error = Vault::open(&path, OpenOptions::default().with_writer("NOT valid")).unwrap_err();
    assert!(error.is(Code::InvalidArgument));
}

#[cfg(not(feature = "encrypt"))]
#[test]
fn encryption_without_the_feature_is_unimplemented() {
    let scratch = Scratch::new("encrypt");
    let path = scratch.join("vault");
    vault(&path);
    let options: OpenOptions =
        serde_json::from_str(r#"{"encryption": {"passphrase": "secret"}}"#).unwrap();
    let before = snapshot(&path);
    let error = Vault::open(&path, options).unwrap_err();
    assert!(error.is(Code::Unimplemented));
    assert_eq!(snapshot(&path), before);
}

#[test]
fn signing_is_not_built_yet_and_refuses_to_open() {
    let scratch = Scratch::new("signing");
    let path = scratch.join("vault");
    vault(&path);
    for key in [
        r#"{"sshKey": "/keys/id_ed25519"}"#,
        r#"{"openpgpKey": "ABCD1234"}"#,
    ] {
        let options: OpenOptions =
            serde_json::from_str(&format!(r#"{{"signing": {key}}}"#)).unwrap();
        let before = snapshot(&path);
        let error = Vault::open(&path, options).unwrap_err();
        assert!(error.is(Code::Unimplemented), "{key}: {error}");
        assert_eq!(snapshot(&path), before);
    }
}
