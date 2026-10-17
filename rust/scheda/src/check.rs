//! The checks of a Vault, layer by layer, and the one call that runs them
//! all.
//!
//! A check collects instead of failing on the first problem (D15): each
//! function returns the [`Problem`]s it found, and fails only when it cannot
//! run (an I/O failure, git). The `Check` call that puts them in a
//! `CheckReport`, after it has read the marker, is built on [`run`].

// `Check` is built on `run`.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the Check call is not built yet")
)]

use std::path::Path;

use scheda_api::v1::{Problem, VaultErrorCode};

use crate::documents::check_documents;
use crate::error::{Context, Error, Fault};
use crate::fs::check_scheda_owner_only;
use crate::git;
use crate::isolated::check_perimeter;
use crate::journal::check_journals;
use crate::vault::Vault;

/// A problem with `code`, about the file at `path` (Vault-relative, POSIX)
/// or about no one file.
pub(crate) fn problem(
    code: VaultErrorCode,
    path: Option<&str>,
    message: impl Into<String>,
) -> Problem {
    let found = Problem::default().with_code(code).with_message(message);
    match path {
        Some(path) => found.with_path(path),
        None => found,
    }
}

/// The problem a failure stands for: its code, its path, its message.
pub(crate) fn problem_of(fault: &Fault) -> Problem {
    problem(fault.vault_code(), fault.path(), fault.message())
}

/// Every path git holds as unmerged is an `UNRESOLVED_CONFLICT` problem
/// with that path: a merge, a pull, or a rebase stopped on it and was not
/// finished. The answer is git's own state; the bytes of a file are not
/// looked at, so a binary file, or text that happens to look like a conflict
/// marker, is not a conflict.
pub(crate) fn check_unmerged(vault: &Path) -> Result<Vec<Problem>, Error> {
    let paths = git::unmerged_paths(vault).context("check_unmerged: ask git")?;
    Ok(paths
        .into_iter()
        .map(|path| {
            problem(
                VaultErrorCode::UnresolvedConflict,
                Some(&path),
                format!("{path} has an unfinished merge"),
            )
        })
        .collect())
}

/// Runs every check over `vault` and returns the problems, in the order of
/// the layers: the owner-only modes of `.scheda/`, the unmerged entries, the
/// perimeter of the isolated tree, the Document files, the journal
/// segments. No problem means the Vault is sound as far as the checks go.
///
/// It fails only when a check cannot run; see the checks for when.
pub(crate) fn run(vault: &Vault) -> Result<Vec<Problem>, Error> {
    let path = vault.path();
    let mut problems = check_scheda_owner_only(path).context("check: .scheda")?;
    problems.extend(check_unmerged(path)?);
    problems.extend(check_perimeter(path)?);
    let declarations = vault.declarations_now().context("check: the collections")?;
    problems.extend(check_documents(path, &declarations, vault.encryption())?);
    problems.extend(check_journals(path, &declarations)?);
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::{OpenOptions, Problem, VaultErrorCode};

    use super::{problem, problem_of, run};
    use crate::error::Fault;
    use crate::git::{git, isolate_test_git_env};
    use crate::vault::Vault;

    /// A Vault work tree made with plain git, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-check-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            git()
                .arg("init")
                .arg("--quiet")
                .arg("--")
                .arg(&dir)
                .run()
                .unwrap();
            Scratch(dir)
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn git(&self, args: &[&str]) -> bool {
            let mut cmd = git().current_dir(&self.0);
            for arg in args {
                cmd = cmd.arg(arg);
            }
            cmd.env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.test")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.test")
                .spawn()
                .unwrap()
                .success()
        }

        fn commit(&self, message: &str) {
            assert!(self.git(&["add", "--all"]));
            assert!(self.git(&["commit", "--quiet", "--message", message]));
        }

        /// A vault with the marker, a `notes` collection, a `log` journal,
        /// and a `diary` isolated collection.
        fn vault(name: &str) -> Scratch {
            let scratch = Scratch::new(name);
            scratch.write(".scheda.yaml", b"version: 2\n");
            scratch.write(".gitignore", b".scheda/\n.isolated/\n");
            scratch.write(
                "notes/.collection.yaml",
                b"name: notes\nschema:\n  standard: {}\n",
            );
            scratch.write(
                "log/.collection.yaml",
                b"name: log\nschema:\n  journal: {}\n",
            );
            scratch.write(
                "diary/.collection.yaml",
                b"name: diary\nschema:\n  standard: {}\n  isolated: true\n",
            );
            scratch
        }

        fn open(&self) -> Vault {
            let vault = Vault::open(&self.0, OpenOptions::default()).unwrap();
            // `open` makes `.scheda/` owner-only.
            vault
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn found(problems: &[Problem]) -> Vec<(VaultErrorCode, String)> {
        let mut out: Vec<_> = problems
            .iter()
            .map(|p| {
                (
                    p.code.and_then(|code| code.as_known()).unwrap(),
                    p.path.clone().unwrap_or_default(),
                )
            })
            .collect();
        out.sort_by(|a, b| (&a.1, a.0 as i32).cmp(&(&b.1, b.0 as i32)));
        out
    }

    /// A merge of `branch` into the branch checked out, which both changed
    /// `rel`, stopped on the conflict.
    fn conflict(scratch: &Scratch, rel: &str) {
        scratch.write(rel, b"base\n");
        scratch.commit("base");
        assert!(scratch.git(&["checkout", "--quiet", "-b", "other"]));
        scratch.write(rel, b"theirs\n");
        scratch.commit("theirs");
        assert!(scratch.git(&["checkout", "--quiet", "-"]));
        scratch.write(rel, b"ours\n");
        scratch.commit("ours");
        assert!(!scratch.git(&["merge", "--quiet", "--no-edit", "other"]));
    }

    #[test]
    fn a_sound_vault_has_no_problem() {
        let scratch = Scratch::vault("sound");
        scratch.write("notes/a.md", b"text");
        scratch.write("log/00000001-w.jsonl", b"a: 1\n");
        scratch.write(".isolated/diary/b", b"secret");
        scratch.commit("fixture");
        let vault = scratch.open();
        assert!(run(&vault).unwrap().is_empty());
        vault.close().unwrap();
    }

    #[test]
    fn a_binary_file_with_marker_lines_passes_and_a_real_unmerged_path_is_reported() {
        let scratch = Scratch::vault("unmerged");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF, 0x00];
        png.extend_from_slice(b"\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\n");
        scratch.write("notes/marker.png", &png);
        scratch.write(
            "notes/text.md",
            b"<<<<<<< HEAD\nnot a conflict\n>>>>>>> x\n",
        );
        scratch.write("log/00000001-w.jsonl", b"a: <<<<<<< HEAD\n");
        scratch.commit("markers that are not conflicts");
        let vault = scratch.open();
        assert!(run(&vault).unwrap().is_empty(), "no merge, no conflict");

        // A real merge conflict in a Document file, with plain git.
        conflict(&scratch, "notes/clash.md");
        let problems = run(&vault).unwrap();
        assert_eq!(
            found(&problems),
            [(
                VaultErrorCode::UnresolvedConflict,
                "notes/clash.md".to_owned()
            )]
        );
        // Finishing the merge clears it.
        scratch.write("notes/clash.md", b"resolved\n");
        scratch.commit("resolved");
        assert!(run(&vault).unwrap().is_empty());
    }

    #[test]
    fn two_problems_in_one_vault_are_both_reported() {
        let scratch = Scratch::vault("both");
        scratch.write("log/00000001-w.jsonl", b"a: 1\nbroken\n");
        conflict(&scratch, "notes/clash.md");
        scratch.write("notes/ bad name", b"x");
        let vault = scratch.open();
        let problems = run(&vault).unwrap();
        assert_eq!(
            found(&problems),
            [
                (
                    VaultErrorCode::InvalidArgument,
                    "log/00000001-w.jsonl".to_owned()
                ),
                (
                    VaultErrorCode::InvalidArgument,
                    "notes/ bad name".to_owned()
                ),
                (
                    VaultErrorCode::UnresolvedConflict,
                    "notes/clash.md".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_tracked_isolated_path_and_a_wide_mode_are_reported() {
        let scratch = Scratch::vault("perimeter");
        scratch.write(".isolated/diary/b", b"secret");
        scratch.commit("fixture");
        let vault = scratch.open();
        assert!(run(&vault).unwrap().is_empty());
        assert!(scratch.git(&["add", "--force", ".isolated/diary/b"]));
        let problems = run(&vault).unwrap();
        assert_eq!(
            found(&problems),
            [(VaultErrorCode::InvalidArgument, ".isolated".to_owned())]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = scratch.0.join(".scheda/writer");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let problems = run(&vault).unwrap();
            assert_eq!(
                found(&problems),
                [
                    (VaultErrorCode::InvalidArgument, ".isolated".to_owned()),
                    (
                        VaultErrorCode::FailedPrecondition,
                        ".scheda/writer".to_owned()
                    ),
                ]
            );
        }
    }

    #[test]
    fn a_problem_carries_its_code_path_and_message() {
        let plain = problem(VaultErrorCode::Crypto, None, "m");
        assert_eq!(
            plain.code.and_then(|c| c.as_known()),
            Some(VaultErrorCode::Crypto)
        );
        assert_eq!(plain.path, None);
        assert_eq!(plain.message.as_deref(), Some("m"));
        let fault = Fault::new(VaultErrorCode::NotFound, "gone").with_path("a/b");
        let of = problem_of(&fault);
        assert_eq!(
            of.code.and_then(|c| c.as_known()),
            Some(VaultErrorCode::NotFound)
        );
        assert_eq!(of.path.as_deref(), Some("a/b"));
        assert_eq!(of.message.as_deref(), Some("gone"));
    }
}
