//! The Vault marker `.scheda.yaml` and the two open modes.
//! Collection names here must not appear in `rust/scheda/src`.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{Code, Error, OpenOptions, Schema, Unit, Vault};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

const MARKER: &str = ".scheda.yaml";

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-marker", n),
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .expect("git executable");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn commit_all(dir: &Path, message: &str) {
    git_ok(dir, &["add", "-A"]);
    commit_staged(dir, message);
}

fn commit_staged(dir: &Path, message: &str) {
    git_ok(
        dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            message,
        ],
    );
}

/// An ordinary repository: one committed file, no Scheda files.
fn plain_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git_ok(dir, &["init", "--quiet"]);
    fs::write(dir.join("README.md"), "hello\n").unwrap();
    commit_all(dir, "first");
}

/// A git work tree with a marker file with `text` committed.
fn repo_with_marker(dir: &Path, text: &str) {
    plain_repo(dir);
    fs::write(dir.join(MARKER), text).unwrap();
    commit_all(dir, "marker");
}

fn commit_count(dir: &Path) -> usize {
    git_ok(dir, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap()
}

fn head(dir: &Path) -> String {
    git_ok(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Files changed by `HEAD`, sorted.
fn head_files(dir: &Path) -> Vec<String> {
    let text = git_ok(dir, &["show", "--name-only", "--format=", "HEAD"]);
    let mut files: Vec<String> = text.lines().map(str::to_owned).collect();
    files.sort();
    files
}

fn tracked(dir: &Path, file: &str) -> bool {
    git_ok(dir, &["ls-files", "--", file]).trim() == file
}

/// Every path under `dir` outside `.git`, with its bytes (`None` for a
/// directory).
fn snapshot(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".git" {
                continue;
            }
            if path.is_dir() {
                out.insert(rel, None);
                walk(base, &path, out);
            } else {
                out.insert(rel, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// What a refused open must leave alone.
#[derive(Debug, PartialEq, Eq)]
struct Folder {
    files: BTreeMap<String, Option<Vec<u8>>>,
    status: Option<String>,
    head: Option<String>,
}

fn folder(dir: &Path) -> Folder {
    let is_git = dir.join(".git").exists();
    Folder {
        files: snapshot(dir),
        status: is_git.then(|| git_ok(dir, &["status", "--porcelain", "--ignored"])),
        head: is_git.then(|| head(dir)),
    }
}

fn existing_only() -> OpenOptions {
    OpenOptions::new().existing_only(true)
}

fn open_default(dir: &Path) -> Vault {
    Vault::open(dir, OpenOptions::new(), None).unwrap()
}

fn open_err(dir: &Path, options: OpenOptions) -> Error {
    match Vault::open(dir, options, None) {
        Ok(_) => panic!("open of {} must fail", dir.display()),
        Err(err) => err,
    }
}

fn as_url(path: &Path) -> String {
    let path = path.to_str().expect("utf-8 path");
    if let Some(rest) = path.strip_prefix('/') {
        format!("file:///{rest}")
    } else {
        format!("file:///{}", path.replace('\\', "/"))
    }
}

fn document_schema() -> Schema {
    Schema {
        kind: Unit::default().into(),
        ..Schema::default()
    }
}

// --- existing only: refusals leave the folder unchanged -------------------

#[test]
fn existing_only_refuses_a_plain_git_repository_and_leaves_it_unchanged() {
    let scratch = Scratch::new();
    let dir = scratch.dir("repo");
    plain_repo(&dir);
    let before = folder(&dir);
    let commits = commit_count(&dir);

    let err = open_err(&dir, existing_only());

    assert_eq!(err.known_code(), Some(Code::NotAVault));
    assert_eq!(folder(&dir), before);
    assert_eq!(commit_count(&dir), commits);
    assert!(!dir.join(".scheda").exists());
    assert!(!dir.join(".gitignore").exists());
    assert!(!dir.join(MARKER).exists());
}

#[test]
fn existing_only_refuses_a_missing_path_and_creates_nothing() {
    let scratch = Scratch::new();
    let dir = scratch.dir("nowhere");

    let err = open_err(&dir, existing_only());

    assert_eq!(err.known_code(), Some(Code::NotFound));
    assert!(!dir.exists());
}

#[test]
fn existing_only_never_clones_for_a_missing_path() {
    let scratch = Scratch::new();
    let remote = scratch.dir("remote");
    drop(open_default(&remote));
    let dir = scratch.dir("copy");

    let err = open_err(&dir, existing_only().remote_repo(as_url(&remote)));

    assert_eq!(err.known_code(), Some(Code::NotFound));
    assert!(!dir.exists());
}

#[test]
fn existing_only_refuses_a_folder_that_is_not_a_git_work_tree() {
    let scratch = Scratch::new();
    let dir = scratch.dir("folder");
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub").join("a.txt"), "a").unwrap();
    let before = folder(&dir);

    let err = open_err(&dir, existing_only());

    assert_eq!(err.known_code(), Some(Code::NotAVault));
    assert_eq!(folder(&dir), before);
    assert!(!dir.join(".git").exists());
    assert!(!dir.join(".scheda").exists());
}

#[test]
fn existing_only_refuses_an_empty_folder_and_does_not_init_it() {
    let scratch = Scratch::new();
    let dir = scratch.dir("empty");
    fs::create_dir_all(&dir).unwrap();

    let err = open_err(&dir, existing_only());

    assert_eq!(err.known_code(), Some(Code::NotAVault));
    assert!(snapshot(&dir).is_empty());
    assert!(!dir.join(".git").exists());
}

#[test]
fn existing_only_refuses_a_file_path() {
    let scratch = Scratch::new();
    let file = scratch.dir("file.txt");
    fs::write(&file, "x").unwrap();

    let err = open_err(&file, existing_only());

    assert_eq!(err.known_code(), Some(Code::NotAVault));
    assert_eq!(fs::read(&file).unwrap(), b"x");
}

// --- existing only: a marked Vault opens ----------------------------------

#[test]
fn existing_only_opens_a_marked_vault_with_no_collections() {
    let scratch = Scratch::new();
    let dir = scratch.dir("vault");
    drop(open_default(&dir));
    let commits = commit_count(&dir);

    let vault = Vault::open(&dir, existing_only(), None).unwrap();

    assert!(vault.info().unwrap().collections.is_empty());
    assert_eq!(commit_count(&dir), commits);
}

#[test]
fn existing_only_opens_a_marked_vault_and_reads_its_records() {
    let scratch = Scratch::new();
    let dir = scratch.dir("vault");
    {
        let vault = open_default(&dir);
        let notes = vault
            .create_collection("north_notes", document_schema(), None)
            .unwrap();
        notes.add("alpha", b"one", None).unwrap();
    }

    let vault = Vault::open(&dir, existing_only(), None).unwrap();
    let record = vault
        .collection("north_notes")
        .unwrap()
        .get("alpha")
        .unwrap();

    assert_eq!(record.raw.as_deref(), Some(&b"one"[..]));
}

#[test]
fn existing_only_opens_a_clone_of_a_marked_vault() {
    let scratch = Scratch::new();
    let remote = scratch.dir("remote");
    drop(open_default(&remote));
    let dir = scratch.dir("copy");
    git_ok(
        &scratch.root,
        &["clone", "--quiet", as_url(&remote).as_str(), "copy"],
    );

    let vault = Vault::open(&dir, existing_only(), None).unwrap();

    assert!(vault.info().unwrap().collections.is_empty());
    assert!(tracked(&dir, MARKER));
}

// --- default mode ---------------------------------------------------------

#[test]
fn default_open_of_a_new_path_commits_the_marker_in_the_first_commit() {
    let scratch = Scratch::new();
    let dir = scratch.dir("vault");

    drop(open_default(&dir));

    assert_eq!(
        fs::read_to_string(dir.join(MARKER)).unwrap(),
        "version: 1\n"
    );
    assert_eq!(commit_count(&dir), 1);
    assert_eq!(head_files(&dir), [".gitignore", MARKER]);
    assert!(git_ok(&dir, &["status", "--porcelain"]).is_empty());
}

#[test]
fn default_open_adds_the_marker_to_an_unmarked_vault_in_one_commit() {
    let scratch = Scratch::new();
    let dir = scratch.dir("vault");
    {
        let vault = open_default(&dir);
        vault
            .create_collection("north_notes", document_schema(), None)
            .unwrap()
            .add("alpha", b"one", None)
            .unwrap();
    }
    // A Vault as an older version made it: records and `.gitignore`, no marker.
    git_ok(&dir, &["rm", "--quiet", MARKER]);
    commit_all(&dir, "drop marker");
    assert!(!dir.join(MARKER).exists());
    let commits = commit_count(&dir);

    let vault = open_default(&dir);

    assert_eq!(commit_count(&dir), commits + 1);
    assert_eq!(head_files(&dir), [MARKER]);
    assert_eq!(
        fs::read_to_string(dir.join(MARKER)).unwrap(),
        "version: 1\n"
    );
    let record = vault
        .collection("north_notes")
        .unwrap()
        .get("alpha")
        .unwrap();
    assert_eq!(record.raw.as_deref(), Some(&b"one"[..]));
    drop(vault);

    // Opening again commits nothing.
    let after = head(&dir);
    drop(open_default(&dir));
    assert_eq!(head(&dir), after);
    assert_eq!(commit_count(&dir), commits + 1);
}

#[test]
fn default_open_of_an_ordinary_repository_adds_the_marker_and_gitignore() {
    let scratch = Scratch::new();
    let dir = scratch.dir("repo");
    plain_repo(&dir);
    let commits = commit_count(&dir);

    drop(open_default(&dir));

    assert_eq!(commit_count(&dir), commits + 1);
    assert_eq!(head_files(&dir), [".gitignore", MARKER]);
    assert_eq!(
        fs::read_to_string(dir.join("README.md")).unwrap(),
        "hello\n"
    );
    assert!(git_ok(&dir, &["status", "--porcelain"]).is_empty());
}

#[test]
fn default_open_commits_a_marker_file_that_git_does_not_track_yet() {
    let scratch = Scratch::new();
    let dir = scratch.dir("vault");
    drop(open_default(&dir));
    git_ok(&dir, &["rm", "--quiet", "--cached", MARKER]);
    commit_staged(&dir, "untrack marker");
    assert!(dir.join(MARKER).exists());
    assert!(!tracked(&dir, MARKER));

    drop(open_default(&dir));

    assert!(tracked(&dir, MARKER));
    assert_eq!(head_files(&dir), [MARKER]);
}

#[test]
fn default_clone_of_an_unmarked_remote_gets_the_marker() {
    let scratch = Scratch::new();
    let remote = scratch.dir("remote");
    plain_repo(&remote);
    let remote_commits = commit_count(&remote);
    let dir = scratch.dir("copy");

    drop(Vault::open(&dir, OpenOptions::new().remote_repo(as_url(&remote)), None).unwrap());

    assert_eq!(commit_count(&dir), remote_commits + 1);
    assert_eq!(head_files(&dir), [".gitignore", MARKER]);
    assert!(tracked(&dir, MARKER));
    assert_eq!(commit_count(&remote), remote_commits);
}

#[test]
fn default_clone_of_a_marked_remote_adds_no_marker_commit() {
    let scratch = Scratch::new();
    let remote = scratch.dir("remote");
    drop(open_default(&remote));
    let remote_commits = commit_count(&remote);
    let dir = scratch.dir("copy");

    drop(Vault::open(&dir, OpenOptions::new().remote_repo(as_url(&remote)), None).unwrap());

    assert_eq!(commit_count(&dir), remote_commits);
    assert_eq!(head(&dir), head(&remote));
}

#[test]
fn default_open_of_a_marked_vault_is_not_committed_to() {
    let scratch = Scratch::new();
    let dir = scratch.dir("vault");
    drop(open_default(&dir));
    let before = head(&dir);

    drop(open_default(&dir));
    drop(open_default(&dir));

    assert_eq!(head(&dir), before);
    assert_eq!(commit_count(&dir), 1);
}

#[test]
fn default_open_of_a_folder_that_is_not_a_git_work_tree_is_invalid() {
    let scratch = Scratch::new();
    let dir = scratch.dir("folder");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "a").unwrap();
    let before = folder(&dir);

    let err = open_err(&dir, OpenOptions::new());

    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(folder(&dir), before);
}

// --- a bad marker is refused in both modes --------------------------------

const BAD_MARKERS: [(&str, &str); 4] = [
    ("wrong version", "version: 2\n"),
    ("unreadable YAML", "version: [1\n"),
    ("empty file", ""),
    ("unknown key", "version: 1\nname: x\n"),
];

#[test]
fn a_bad_marker_is_refused_in_both_modes_and_nothing_is_written() {
    for (what, text) in BAD_MARKERS {
        for options in [OpenOptions::new(), existing_only()] {
            let scratch = Scratch::new();
            let dir = scratch.dir("repo");
            repo_with_marker(&dir, text);
            let before = folder(&dir);
            let commits = commit_count(&dir);

            let err = open_err(&dir, options);

            assert_eq!(err.known_code(), Some(Code::Invalid), "{what}");
            assert_eq!(err.path.as_deref(), Some(MARKER), "{what}");
            assert_eq!(folder(&dir), before, "{what}");
            assert_eq!(commit_count(&dir), commits, "{what}");
            assert!(!dir.join(".scheda").exists(), "{what}");
        }
    }
}

#[test]
fn a_marker_that_is_not_utf8_is_refused_in_both_modes() {
    for options in [OpenOptions::new(), existing_only()] {
        let scratch = Scratch::new();
        let dir = scratch.dir("repo");
        plain_repo(&dir);
        fs::write(dir.join(MARKER), [0xff, 0xfe, 0x00]).unwrap();
        let before = folder(&dir);

        let err = open_err(&dir, options);

        assert_eq!(err.known_code(), Some(Code::NotUtf8));
        assert_eq!(err.path.as_deref(), Some(MARKER));
        assert_eq!(folder(&dir), before);
    }
}

#[test]
fn a_marker_that_is_a_directory_is_refused_in_both_modes() {
    for options in [OpenOptions::new(), existing_only()] {
        let scratch = Scratch::new();
        let dir = scratch.dir("repo");
        plain_repo(&dir);
        fs::create_dir_all(dir.join(MARKER)).unwrap();
        fs::write(dir.join(MARKER).join("x"), "x").unwrap();

        let err = open_err(&dir, options);

        assert_eq!(err.path.as_deref(), Some(MARKER));
        assert!(!dir.join(".scheda").exists());
    }
}

#[test]
fn a_clone_of_a_remote_with_a_bad_marker_is_refused() {
    let scratch = Scratch::new();
    let remote = scratch.dir("remote");
    repo_with_marker(&remote, "version: 2\n");
    let dir = scratch.dir("copy");

    let err = open_err(&dir, OpenOptions::new().remote_repo(as_url(&remote)));

    assert_eq!(err.known_code(), Some(Code::Invalid));
    assert_eq!(err.path.as_deref(), Some(MARKER));
    // Nothing was committed on top of the clone.
    assert_eq!(commit_count(&dir), commit_count(&remote));
}

// --- the async form -------------------------------------------------------

#[cfg(feature = "tokio")]
mod nonblocking {
    use super::*;
    use scheda::AsyncVault;

    async fn open_err_async(dir: &Path, options: OpenOptions) -> Error {
        match AsyncVault::open(dir, options, None).await {
            Ok(_) => panic!("open of {} must fail", dir.display()),
            Err(err) => err,
        }
    }

    #[tokio::test]
    async fn existing_only_refuses_a_plain_repository_and_leaves_it_unchanged() {
        let scratch = Scratch::new();
        let dir = scratch.dir("repo");
        plain_repo(&dir);
        let before = folder(&dir);

        let err = open_err_async(&dir, existing_only()).await;

        assert_eq!(err.known_code(), Some(Code::NotAVault));
        assert_eq!(folder(&dir), before);
    }

    #[tokio::test]
    async fn existing_only_refuses_a_missing_path_and_a_non_git_folder() {
        let scratch = Scratch::new();
        let missing = scratch.dir("nowhere");
        let err = open_err_async(&missing, existing_only()).await;
        assert_eq!(err.known_code(), Some(Code::NotFound));
        assert!(!missing.exists());

        let folder_dir = scratch.dir("folder");
        fs::create_dir_all(&folder_dir).unwrap();
        fs::write(folder_dir.join("a.txt"), "a").unwrap();
        let before = folder(&folder_dir);
        let err = open_err_async(&folder_dir, existing_only()).await;
        assert_eq!(err.known_code(), Some(Code::NotAVault));
        assert_eq!(folder(&folder_dir), before);
    }

    #[tokio::test]
    async fn existing_only_opens_a_marked_vault_with_no_collections() {
        let scratch = Scratch::new();
        let dir = scratch.dir("vault");
        AsyncVault::open(&dir, OpenOptions::new(), None)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let commits = commit_count(&dir);

        let vault = AsyncVault::open(&dir, existing_only(), None).await.unwrap();

        assert!(vault.info().await.unwrap().collections.is_empty());
        assert_eq!(commit_count(&dir), commits);
    }

    #[tokio::test]
    async fn default_open_writes_the_marker_into_the_first_commit() {
        let scratch = Scratch::new();
        let dir = scratch.dir("vault");

        AsyncVault::open(&dir, OpenOptions::new(), None)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();

        assert_eq!(commit_count(&dir), 1);
        assert_eq!(head_files(&dir), [".gitignore", MARKER]);
    }

    #[tokio::test]
    async fn default_open_adds_the_marker_to_an_ordinary_repository() {
        let scratch = Scratch::new();
        let dir = scratch.dir("repo");
        plain_repo(&dir);
        let commits = commit_count(&dir);

        AsyncVault::open(&dir, OpenOptions::new(), None)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();

        assert_eq!(commit_count(&dir), commits + 1);
        assert_eq!(head_files(&dir), [".gitignore", MARKER]);
    }

    #[tokio::test]
    async fn a_bad_marker_is_refused_in_both_modes() {
        for options in [OpenOptions::new(), existing_only()] {
            let scratch = Scratch::new();
            let dir = scratch.dir("repo");
            repo_with_marker(&dir, "version: 2\n");
            let before = folder(&dir);

            let err = open_err_async(&dir, options).await;

            assert_eq!(err.known_code(), Some(Code::Invalid));
            assert_eq!(err.path.as_deref(), Some(MARKER));
            assert_eq!(folder(&dir), before);
        }
    }
}
