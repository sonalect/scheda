//! Helpers shared by the integration tests: a scratch directory, plain
//! `git` with the user's configuration kept out, and a Vault built on disk
//! the way a user would build one, without the library.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that is removed when it is dropped.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A new, empty scratch directory.
    pub fn new(name: &str) -> Scratch {
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!("scheda-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Scratch { root }
    }

    /// A path below the directory.
    pub fn join(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Runs `git args` in `dir`, with the user's and the system's git
/// configuration left out and an identity of its own. A failure fails the
/// test.
pub fn git(dir: &Path, args: &[&str]) {
    let absent = std::env::temp_dir().join("scheda-absent-gitconfig");
    let status = Command::new("git")
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", &absent)
        .env("GIT_CONFIG_SYSTEM", &absent)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.test")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.test")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Writes `text` to `rel` below `dir`, creating the folders.
pub fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A git work tree at `dir` that holds `files` (path, text), committed. The
/// marker is one of the files when the caller wants one.
pub fn work_tree(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "--quiet"]);
    for (rel, text) in files {
        write(dir, rel, text);
    }
    git(dir, &["add", "--all"]);
    git(
        dir,
        &["commit", "--quiet", "--allow-empty", "--message", "fixture"],
    );
}

/// A Vault of generation 2 at `dir` with the collection `notes` (a
/// standard collection) and the collection `log` (a journal).
pub fn vault(dir: &Path) {
    work_tree(
        dir,
        &[
            (".scheda.yaml", "version: 2\n"),
            (
                "notes/.collection.yaml",
                "name: notes\nschema:\n  standard: {}\n",
            ),
            (
                "log/.collection.yaml",
                "name: log\nschema:\n  journal: {}\n",
            ),
        ],
    );
}

/// Every file below `dir` with its bytes, sorted by path: what a refusal
/// must leave as it was.
pub fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.push((format!("{rel}/"), Vec::new()));
                walk(base, &path, out);
            } else {
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(dir, dir, &mut out);
    }
    out
}
