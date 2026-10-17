//! What the write tests look at in a Vault after a write: git's own view of
//! the history and the work tree, and the lock file.

use std::path::Path;

use super::common::command;

/// The standard output of `git args` in `dir`, without the final newline.
pub fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = command(dir, args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
}

/// How many commits `HEAD` holds.
pub fn commits(dir: &Path) -> usize {
    git_out(dir, &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

/// What a stored write leaves: `git status --porcelain` is clean (but for
/// the paths in `except`, a person's own files) and `commit.lock` is empty.
pub fn assert_settled(dir: &Path, except: &[&str]) {
    let status = git_out(dir, &["status", "--porcelain"]);
    let lines: Vec<&str> = status
        .lines()
        .filter(|line| !except.iter().any(|path| line[3..] == **path))
        .collect();
    assert!(lines.is_empty(), "git status is not clean: {lines:?}");
    let lock = std::fs::read(dir.join(".scheda/commit.lock")).unwrap();
    assert!(lock.is_empty(), "commit.lock holds {lock:?}");
}
