//! No public name of the contract carries a git term.
//!
//! A message, a field, an enum, an enum value, a service, or an rpc is
//! named for what a caller sees (a revision, a version, a note), never for
//! the storage underneath: a name with the word `commit`, `head`, or `git`
//! (also as `git_...` or `GIT_...`) would tie every consumer to git. This
//! test reads the protos, drops the comments and the text of the string
//! literals (messages of rules may say what they like), splits every name
//! into its words, and fails on a forbidden word. A commit id is
//! `commit_id` or `sha`, and has the word `commit` or `sha` in it.

use std::fs;
use std::path::{Path, PathBuf};

/// The words no name may hold.
const FORBIDDEN: &[&str] = &["commit", "head", "git", "sha", "oid"];

/// The source without comments and without the content of strings.
fn code_of(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            '"' => {
                // The string is dropped; an escaped quote does not end it.
                let mut escaped = false;
                for c in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        break;
                    }
                }
                out.push_str("\"\"");
            }
            c => out.push(c),
        }
    }
    out
}

/// The words of a name: `GitHead`, `git_head`, and `GIT_HEAD` hold `git`
/// and `head`.
fn words_of(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut word = String::new();
        let mut previous: Option<char> = None;
        for c in part.chars() {
            let boundary =
                previous.is_some_and(|p| p.is_ascii_lowercase() && c.is_ascii_uppercase());
            if boundary && !word.is_empty() {
                words.push(std::mem::take(&mut word).to_ascii_lowercase());
            }
            word.push(c);
            previous = Some(c);
        }
        if !word.is_empty() {
            words.push(word.to_ascii_lowercase());
        }
    }
    words
}

/// The names in `src` with a forbidden word.
fn offenders(src: &str) -> Vec<String> {
    let code = code_of(src);
    let mut found = Vec::new();
    for name in code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if name.is_empty() {
            continue;
        }
        if words_of(name)
            .iter()
            .any(|word| FORBIDDEN.contains(&word.as_str()))
        {
            found.push(name.to_owned());
        }
    }
    found
}

fn protos_in(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "proto") {
            files.push(path);
        }
    }
}

/// The folders that may hold `scheda/v1/*.proto`: the checkout under
/// Cargo, the working directory, and the runfiles of Bazel.
fn api_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest).join("../../api"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.clone());
        candidates.push(cwd.join("api"));
    }
    for var in ["RUNFILES_DIR", "TEST_SRCDIR"] {
        let Ok(base) = std::env::var(var) else {
            continue;
        };
        let base = PathBuf::from(base);
        candidates.push(base.join("_main/api"));
        candidates.push(base.join("scheda/api"));
        if let Ok(workspace) = std::env::var("TEST_WORKSPACE")
            && !workspace.is_empty()
        {
            candidates.push(base.join(workspace).join("api"));
        }
    }
    let mut roots: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|candidate| candidate.join("scheda/v1/marker.proto").is_file())
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

fn all_protos() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in api_roots() {
        protos_in(&root.join("scheda/v1"), &mut files);
    }
    files.sort();
    files.dedup();
    files
}

#[test]
fn no_name_of_the_contract_carries_a_git_term() {
    let files = all_protos();
    assert!(
        !files.is_empty(),
        "no api/scheda/v1/*.proto found (CARGO_MANIFEST_DIR={:?}, RUNFILES_DIR={:?}, TEST_SRCDIR={:?}, cwd={:?})",
        std::env::var_os("CARGO_MANIFEST_DIR"),
        std::env::var_os("RUNFILES_DIR"),
        std::env::var_os("TEST_SRCDIR"),
        std::env::current_dir().ok(),
    );
    let mut errors = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path).unwrap();
        for name in offenders(&text) {
            errors.push(format!("{}: `{name}` is a git term", path.display()));
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn the_scan_finds_a_git_term_in_a_name_and_not_in_a_comment_or_a_string() {
    for source in [
        "message Commit {}",
        "message Entry { string commit_id = 1; }",
        "message Entry { string HEAD = 1; }",
        "message Entry { string head = 1; }",
        "message Entry { string git_url = 1; }",
        "message Entry { string gitRef = 1; }",
        "enum Kind { KIND_GIT_TAG = 0; }",
        "service GitMirror { rpc PushCommit(A) returns (B); }",
        "message Entry { string sha = 1; }",
    ] {
        assert!(!offenders(source).is_empty(), "{source}");
    }
    for source in [
        "// the commit at HEAD\nmessage Entry {}",
        "/* git commit */ message Entry {}",
        "message Entry { string url = 1 [(r) = {message: \"credentials come from git\"}]; }",
        "message Header { string ahead = 1; string github = 2; string revision = 3; }",
    ] {
        assert!(
            offenders(source).is_empty(),
            "{source}: {:?}",
            offenders(source)
        );
    }
}
