//! `Scheda.open`: what each `OpenMode` does with each kind of path, the
//! generation check, and what a refusal leaves on disk.

use buffa_types::google::protobuf::Empty;
use scheda::v1::{OpenMode, OpenOptions, VaultAsync, VaultErrorCode as Code};

use crate::common::{Scratch, both, command, git, open, snapshot, vault, work_tree, write};

fn options(mode: OpenMode) -> OpenOptions {
    OpenOptions::default().with_mode(mode)
}

/// Opens at `path` in `mode` and asserts the refusal `code` and that the
/// path is as it was.
async fn refused(path: &std::path::Path, mode: OpenMode, code: Code) {
    let before = snapshot(path);
    let existed = path.exists();
    let error = open(path, options(mode)).await.err().unwrap();
    assert!(error.is(code), "{mode:?}: {error}");
    assert_eq!(path.exists(), existed, "{mode:?}");
    assert_eq!(snapshot(path), before, "{mode:?}");
}

#[test]
fn a_missing_path_is_not_found_in_open_existing_and_stays_missing() {
    both(|| async {
        let scratch = Scratch::new("missing");
        let path = scratch.join("nowhere");
        refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotFound).await;
        assert!(!path.exists());
    });
}

#[test]
fn an_empty_folder_is_not_a_vault_in_open_existing_and_stays_empty() {
    both(|| async {
        let scratch = Scratch::new("empty");
        let path = scratch.join("empty");
        std::fs::create_dir(&path).unwrap();
        refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault).await;
        assert!(snapshot(&path).is_empty());
    });
}

#[test]
fn a_git_work_tree_without_the_marker_is_refused_by_open_existing_and_create_new() {
    both(|| async {
        let scratch = Scratch::new("tree");
        let path = scratch.join("tree");
        work_tree(
            &path,
            &[(
                "notes/.collection.yaml",
                "name: notes\nschema:\n  standard: {}\n",
            )],
        );
        refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault).await;
        refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists).await;
        assert!(!path.join(".scheda.yaml").exists());
        assert!(!path.join(".scheda").exists());
    });
}

#[test]
fn another_folder_is_refused_by_every_mode() {
    both(|| async {
        let scratch = Scratch::new("other");
        let path = scratch.join("other");
        write(&path, "readme.md", "not a Vault");
        refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault).await;
        refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists).await;
        refused(
            &path,
            OpenMode::OPEN_MODE_OPEN_OR_CREATE,
            Code::InvalidArgument,
        )
        .await;
        let error = open(&path, OpenOptions::default()).await.err().unwrap();
        assert!(error.is(Code::InvalidArgument), "unset is open or create");
        assert_eq!(snapshot(&path).len(), 1);
    });
}

#[test]
fn a_file_is_not_a_vault() {
    both(|| async {
        let scratch = Scratch::new("file");
        let path = scratch.join("file");
        std::fs::write(&path, b"x").unwrap();
        refused(&path, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault).await;
        refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists).await;
        refused(
            &path,
            OpenMode::OPEN_MODE_OPEN_OR_CREATE,
            Code::InvalidArgument,
        )
        .await;
    });
}

#[test]
fn a_folder_inside_a_work_tree_is_another_folder() {
    both(|| async {
        let scratch = Scratch::new("inner");
        let tree = scratch.join("tree");
        vault(&tree);
        let inner = tree.join("notes");
        refused(&inner, OpenMode::OPEN_MODE_OPEN_EXISTING, Code::NotAVault).await;
    });
}

#[test]
fn create_new_refuses_a_vault_and_the_other_modes_open_it() {
    both(|| async {
        let scratch = Scratch::new("vault");
        let path = scratch.join("vault");
        vault(&path);
        refused(&path, OpenMode::OPEN_MODE_CREATE_NEW, Code::AlreadyExists).await;
        for mode in [
            OpenMode::OPEN_MODE_OPEN_EXISTING,
            OpenMode::OPEN_MODE_OPEN_OR_CREATE,
            OpenMode::OPEN_MODE_UNSPECIFIED,
        ] {
            let vault = open(&path, options(mode)).await.unwrap();
            vault.close(Empty::default()).await.unwrap();
        }
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        vault.close(Empty::default()).await.unwrap();
        assert!(path.join(".scheda/writer").is_file());
    });
}

#[test]
fn another_generation_is_generation_in_every_mode_and_before_any_collection_file() {
    both(|| async {
        for version in ["1", "2", "3", "5"] {
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
                refused(&path, mode, Code::Generation).await;
            }
            assert!(!path.join(".scheda").exists());
        }
    });
}

#[test]
fn an_unknown_key_in_the_marker_and_a_leftover_version_in_a_collection_file_open() {
    both(|| async {
        let scratch = Scratch::new("unknown");
        let path = scratch.join("vault");
        work_tree(
            &path,
            &[
                (".scheda.yaml", "version: 4\nsomething: later\n"),
                (
                    "notes/.collection.yaml",
                    "version: 1\nname: notes\nschema:\n  standard: {}\n",
                ),
            ],
        );
        open(&path, OpenOptions::default())
            .await
            .unwrap()
            .close(Empty::default())
            .await
            .unwrap();
    });
}

#[test]
fn a_broken_marker_is_invalid_argument_and_writes_nothing() {
    both(|| async {
        let scratch = Scratch::new("marker");
        let path = scratch.join("vault");
        work_tree(&path, &[(".scheda.yaml", "{}\n")]);
        refused(
            &path,
            OpenMode::OPEN_MODE_OPEN_EXISTING,
            Code::InvalidArgument,
        )
        .await;
    });
}

#[test]
fn an_invalid_collection_file_fails_open_and_writes_nothing() {
    both(|| async {
        let scratch = Scratch::new("collection");
        let path = scratch.join("vault");
        work_tree(
            &path,
            &[
                (".scheda.yaml", "version: 4\n"),
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
        )
        .await;
        assert!(!path.join(".scheda").exists());
    });
}

#[test]
fn a_mirror_that_is_not_the_origin_is_invalid_argument() {
    both(|| async {
        let scratch = Scratch::new("mirror");
        let path = scratch.join("vault");
        vault(&path);
        let url = "https://example.test/org/vault.git";
        git(&path, &["remote", "add", "origin", url]);
        let before = snapshot(&path);
        let other = OpenOptions::default().with_mirror("https://example.test/org/other.git");
        let error = open(&path, other).await.err().unwrap();
        assert!(error.is(Code::InvalidArgument));
        assert_eq!(snapshot(&path), before);
        open(&path, OpenOptions::default().with_mirror(url))
            .await
            .unwrap()
            .close(Empty::default())
            .await
            .unwrap();
    });
}

#[test]
fn options_that_break_a_rule_are_invalid_argument() {
    both(|| async {
        let scratch = Scratch::new("options");
        let path = scratch.join("vault");
        vault(&path);
        let error = open(&path, OpenOptions::default().with_writer("NOT valid"))
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument));
        // A mode the contract does not define is refused, not taken as the
        // default: it must not create what the caller did not ask for.
        let missing = scratch.join("missing");
        let options: OpenOptions = serde_json::from_str(r#"{"mode": 9}"#).unwrap();
        let error = open(&missing, options).await.err().unwrap();
        assert!(error.is(Code::InvalidArgument), "{error}");
        assert!(!missing.exists());
    });
}

#[cfg(not(feature = "encrypt"))]
#[test]
fn encryption_without_the_feature_is_unimplemented() {
    both(|| async {
        let scratch = Scratch::new("encrypt");
        let path = scratch.join("vault");
        vault(&path);
        let options: OpenOptions =
            serde_json::from_str(r#"{"encryption": {"passphrase": "secret"}}"#).unwrap();
        let before = snapshot(&path);
        let error = open(&path, options).await.err().unwrap();
        assert!(error.is(Code::Unimplemented));
        assert_eq!(snapshot(&path), before);
    });
}

#[test]
fn signing_is_not_built_yet_and_refuses_to_open() {
    both(|| async {
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
            let error = open(&path, options).await.err().unwrap();
            assert!(error.is(Code::Unimplemented), "{key}: {error}");
            assert_eq!(snapshot(&path), before);
        }
    });
}

#[test]
fn a_repository_the_library_cannot_read_is_unimplemented_in_every_mode() {
    both(|| async {
        for (name, flag) in [
            ("sha256", "--object-format=sha256"),
            ("reftable", "--ref-format=reftable"),
        ] {
            let scratch = Scratch::new(name);
            let path = scratch.join("tree");
            std::fs::create_dir_all(&path).unwrap();
            let made = command(&path, &["init", "--quiet", flag])
                .status()
                .unwrap()
                .success();
            if !made {
                // The installed git cannot make such a repository.
                continue;
            }
            write(&path, "readme.md", "a person's file");
            for mode in [
                OpenMode::OPEN_MODE_OPEN_EXISTING,
                OpenMode::OPEN_MODE_CREATE_NEW,
                OpenMode::OPEN_MODE_OPEN_OR_CREATE,
                OpenMode::OPEN_MODE_UNSPECIFIED,
            ] {
                refused(&path, mode, Code::Unimplemented).await;
            }
        }
    });
}

/// Every file and folder below `dir` with its size and modification time,
/// the git directory included: what a refusal must not touch.
fn listing(dir: &std::path::Path) -> Vec<(String, u64, std::time::SystemTime)> {
    fn walk(
        base: &std::path::Path,
        at: &std::path::Path,
        out: &mut Vec<(String, u64, std::time::SystemTime)>,
    ) {
        let mut entries: Vec<_> = std::fs::read_dir(at)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            out.push((rel, meta.len(), meta.modified().unwrap()));
            if meta.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn a_new_vault_marker_says_version_4() {
    both(|| async {
        let scratch = Scratch::new("marker-four");
        let path = scratch.join("vault");
        open(&path, OpenOptions::default())
            .await
            .unwrap()
            .close(Empty::default())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join(".scheda.yaml")).unwrap(),
            "version: 4\n"
        );
        assert_eq!(
            crate::common::command(&path, &["show", "HEAD:.scheda.yaml"])
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                .unwrap(),
            "version: 4\n"
        );
    });
}

#[test]
fn a_vault_whose_marker_says_3_is_generation_in_every_open_mode_and_nothing_changes() {
    both(|| async {
        let scratch = Scratch::new("marker-three");
        let path = scratch.join("vault");
        work_tree(
            &path,
            &[
                (".scheda.yaml", "version: 3\n"),
                (
                    "notes/.collection.yaml",
                    "name: notes\nschema:\n  standard: {}\n",
                ),
                ("notes/a.md", "one"),
            ],
        );
        let before = listing(&path);
        let bytes = snapshot(&path);
        for mode in [
            OpenMode::OPEN_MODE_OPEN_EXISTING,
            OpenMode::OPEN_MODE_OPEN_OR_CREATE,
            OpenMode::OPEN_MODE_UNSPECIFIED,
        ] {
            let error = open(&path, options(mode)).await.err().unwrap();
            assert!(error.is(Code::Generation), "{mode:?}: {error}");
            assert_eq!(listing(&path), before, "{mode:?}");
            assert_eq!(snapshot(&path), bytes, "{mode:?}");
        }
        // An attach to a Vault with a work tree that is stated.
        let attach = OpenOptions::default().with_work_tree(scheda::v1::WorkTree::WORK_TREE_FILES);
        let error = open(&path, attach).await.err().unwrap();
        assert!(error.is(Code::Generation), "attach: {error}");
        assert_eq!(listing(&path), before, "attach");
        assert_eq!(snapshot(&path), bytes, "attach");
    });
}
