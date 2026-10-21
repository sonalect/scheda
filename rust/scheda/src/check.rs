//! The checks of a Vault, layer by layer, and the one call that runs them
//! all.
//!
//! A check collects instead of failing on the first problem: each
//! function returns the [`Problem`]s it found, and fails only when it cannot
//! run (an I/O failure, git). The `Check` call that puts them in a
//! `CheckReport` is built on [`run`], which reads the marker first and the
//! collection files one by one, so one file that does not load is a problem
//! of the report and not a failure of the call.

use scheda_api::v1::{Problem, VaultErrorCode};

use crate::collection::{self, Declarations};
use crate::documents::check_documents;
use crate::error::{Context, Error, Fault};
use crate::fs::check_scheda_owner_only;
use crate::idmap::check_journal_id_maps;
use crate::isolated::{self, check_perimeter};
use crate::journal::check_journals;
use crate::marker;
use crate::repo::{self, Repo, Repos};
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
pub(crate) fn check_unmerged(repos: &Repos) -> Result<Vec<Problem>, Error> {
    let Some(lease) = repos.lease(Repo::Main)? else {
        return Ok(Vec::new());
    };
    let paths = repo::unmerged_paths(&lease).context("check_unmerged: ask git")?;
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

/// Every file of the collections of the Vault that a person changed and did
/// not commit is a `FAILED_PRECONDITION` problem with its path: a file
/// modified, deleted, staged, or new inside a collection directory differs
/// from what the Vault stores, which is the commit `HEAD` names. The
/// Vault's reads do not see such a file and its writes do not commit it.
pub(crate) fn check_changed(
    repos: &Repos,
    declarations: &Declarations,
) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    for repo in [Repo::Main, Repo::Isolated] {
        let dirs: Vec<String> = declarations
            .iter()
            .filter(|decl| decl.repo() == repo)
            .map(|decl| decl.name().to_owned())
            .collect();
        let Some(lease) = repos.lease(repo)? else {
            continue;
        };
        for path in repo::changed_paths(&lease, &dirs).context("check_changed: ask git")? {
            let path = match repo {
                Repo::Main => path,
                Repo::Isolated => format!("{}/{path}", isolated::DIR),
            };
            let message = format!("{path} differs from what the Vault stores");
            problems.push(problem(
                VaultErrorCode::FailedPrecondition,
                Some(&path),
                message,
            ));
        }
    }
    Ok(problems)
}

/// The problem a marker failure stands for, when it is one the marker's
/// content causes (another generation, a broken file); any other failure is
/// the call's own.
fn marker_problem(error: Error) -> Result<Problem, Error> {
    if (error.is(VaultErrorCode::Generation) || error.is(VaultErrorCode::InvalidArgument))
        && let Some(fault) = error.downcast_ref::<Fault>()
    {
        return Ok(problem_of(fault));
    }
    Err(error)
}

/// Runs every check over `vault` and returns the problems, in the order of
/// the layers: the marker of the commit `HEAD` names, the owner-only modes
/// of `.scheda/`, the unmerged entries, the perimeter of the isolated tree,
/// the collection files, the Document files, the journal segments, and the
/// id maps of the journals (the last four over the commits `HEAD` names),
/// and the files a person changed and did not commit. No problem means the
/// Vault is sound as far as the checks go.
///
/// A marker of another generation or one that does not parse
/// (`GENERATION`, `INVALID_ARGUMENT`), and a commit with no marker
/// (`NOT_A_VAULT`), are one problem with the path `.scheda.yaml`, and no
/// other check runs: nothing else in the commit can be read as this build
/// reads it. A collection file that does not load is a problem with its
/// path, and the other checks run over the collections that do.
///
/// It fails only when a check cannot run; see the checks for when.
pub(crate) fn run(vault: &Vault) -> Result<Vec<Problem>, Error> {
    let path = vault.path();
    let repos = vault.repos();
    let views = repos.views()?;
    match marker::read(views.of(Repo::Main)) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Ok(vec![problem(
                VaultErrorCode::NotAVault,
                Some(marker::FILE),
                "the commit HEAD names holds no Vault marker",
            )]);
        }
        Err(error) => return Ok(vec![marker_problem(error)?]),
    }
    let mut problems = check_scheda_owner_only(path).context("check: .scheda")?;
    // A Vault without a work tree has no index and no files to compare.
    let files = !vault.has_no_files();
    if files {
        problems.extend(check_unmerged(repos)?);
        problems.extend(check_perimeter(repos)?);
    }
    let (declarations, faults) =
        collection::load_each(views.of(Repo::Main)).context("check: the collections")?;
    problems.extend(faults.iter().map(problem_of));
    problems.extend(check_documents(&views, &declarations, vault.encryption())?);
    problems.extend(check_journals(&views, &declarations)?);
    problems.extend(check_journal_id_maps(
        &vault.index(),
        &views,
        &declarations,
    )?);
    if files {
        problems.extend(check_changed(repos, &declarations)?);
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use scheda_api::v1::{OpenOptions, Problem, VaultErrorCode};

    use super::{problem, problem_of};
    use crate::error::{Error, Fault};
    use crate::git::{git, isolate_test_git_env};
    use crate::testing::{open, step};
    use crate::vault::Vault;

    /// The problems `check::run` finds in `vault`.
    async fn run(vault: &Arc<Vault>) -> Result<Vec<Problem>, Error> {
        step(vault, |_cx, vault| super::run(vault)).await
    }

    /// A Vault work tree made with plain git, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-check-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
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
            scratch.write(".scheda.yaml", b"version: 3\n");
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

        async fn open(&self) -> Arc<Vault> {
            // `open` makes `.scheda/` owner-only.
            open(&self.0, OpenOptions::default()).await.unwrap()
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

    #[tokio::test]
    async fn a_sound_vault_has_no_problem() {
        let scratch = Scratch::vault("sound");
        scratch.write("notes/a.md", b"text");
        scratch.write("log/00000001-w.jsonl", b"a: 1\n");
        scratch.write(".isolated/diary/b", b"secret");
        scratch.commit("fixture");
        let vault = scratch.open().await;
        assert!(run(&vault).await.unwrap().is_empty());
        vault.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_binary_file_with_marker_lines_passes_and_a_real_unmerged_path_is_reported() {
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
        let vault = scratch.open().await;
        assert!(
            run(&vault).await.unwrap().is_empty(),
            "no merge, no conflict"
        );

        // A real merge conflict in a Document file, with plain git.
        conflict(&scratch, "notes/clash.md");
        let problems = run(&vault).await.unwrap();
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
        assert!(run(&vault).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn two_problems_in_one_vault_are_both_reported() {
        let scratch = Scratch::vault("both");
        scratch.write("log/00000001-w.jsonl", b"a: 1\nbroken\n");
        scratch.write("notes/ bad name", b"x");
        conflict(&scratch, "notes/clash.md");
        let vault = scratch.open().await;
        let problems = run(&vault).await.unwrap();
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

    #[tokio::test]
    async fn a_tracked_isolated_path_and_a_wide_mode_are_reported() {
        let scratch = Scratch::vault("perimeter");
        scratch.write(".isolated/diary/b", b"secret");
        scratch.commit("fixture");
        let vault = scratch.open().await;
        assert!(run(&vault).await.unwrap().is_empty());
        assert!(scratch.git(&["add", "--force", ".isolated/diary/b"]));
        let problems = run(&vault).await.unwrap();
        assert_eq!(
            found(&problems),
            [(VaultErrorCode::InvalidArgument, ".isolated".to_owned())]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = scratch.0.join(".scheda/writer");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let problems = run(&vault).await.unwrap();
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

    #[tokio::test]
    async fn files_a_person_changed_and_did_not_commit_are_listed_and_a_write_does_not_clear_them()
    {
        use scheda_api::v1::{DocumentCreation, DocumentUpsert};
        let scratch = Scratch::vault("changed");
        for rel in ["notes/a.md", "notes/b.md", "notes/c.md"] {
            scratch.write(rel, b"one");
        }
        scratch.write("draft.txt", b"outside the collections");
        scratch.commit("fixture");
        let vault = scratch.open().await;
        crate::testing::create(
            &vault,
            "diary",
            DocumentCreation::default()
                .with_id("d")
                .with_raw(b"secret".to_vec())
                .with_note("test"),
        )
        .await
        .unwrap();
        assert!(run(&vault).await.unwrap().is_empty());
        scratch.write("notes/a.md", b"modified");
        std::fs::remove_file(scratch.0.join("notes/b.md")).unwrap();
        scratch.write("notes/c.md", b"staged");
        assert!(scratch.git(&["add", "notes/c.md"]));
        scratch.write("notes/new.md", b"new");
        scratch.write("draft.txt", b"outside, edited");
        scratch.write(".isolated/diary/d", b"edited secret");
        let expected = [
            (
                VaultErrorCode::FailedPrecondition,
                ".isolated/diary/d".to_owned(),
            ),
            (VaultErrorCode::FailedPrecondition, "notes/a.md".to_owned()),
            (VaultErrorCode::FailedPrecondition, "notes/b.md".to_owned()),
            (VaultErrorCode::FailedPrecondition, "notes/c.md".to_owned()),
            (
                VaultErrorCode::FailedPrecondition,
                "notes/new.md".to_owned(),
            ),
        ];
        assert_eq!(found(&run(&vault).await.unwrap()), expected);
        // A write of the same Document lands in HEAD; the person's file still
        // differs from what the Vault stores.
        crate::testing::upsert(
            &vault,
            "notes",
            DocumentUpsert::default()
                .with_id("a.md")
                .with_raw(b"by the API".to_vec())
                .with_note("test"),
        )
        .await
        .unwrap();
        assert_eq!(found(&run(&vault).await.unwrap()), expected);
        // Once the person commits what they have, nothing differs.
        assert!(scratch.git(&["add", "--all"]));
        assert!(scratch.git(&["commit", "--quiet", "--message", "by hand"]));
        let nested = scratch.0.join(".isolated");
        let mut cmd = git().current_dir(&nested);
        for arg in ["commit", "--quiet", "--all", "--message", "by hand"] {
            cmd = cmd.arg(arg);
        }
        assert!(
            cmd.env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.test")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.test")
                .spawn()
                .unwrap()
                .success()
        );
        assert!(run(&vault).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_collection_file_that_does_not_load_is_a_problem_and_the_other_checks_go_on() {
        let scratch = Scratch::vault("bad-collection");
        scratch.write("notes/a.md", b"text");
        scratch.commit("fixture");
        let vault = scratch.open().await;
        assert!(run(&vault).await.unwrap().is_empty());
        // A person commits a collection file that names no kind, and a
        // change in a sound collection that stays uncommitted.
        scratch.write("bad/.collection.yaml", b"name: bad\n");
        scratch.commit("a broken collection");
        scratch.write("notes/a.md", b"modified");
        let problems = run(&vault).await.unwrap();
        assert_eq!(
            found(&problems),
            [
                (
                    VaultErrorCode::InvalidArgument,
                    "bad/.collection.yaml".to_owned()
                ),
                (VaultErrorCode::FailedPrecondition, "notes/a.md".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_marker_of_another_generation_is_the_one_problem() {
        let scratch = Scratch::vault("generation");
        scratch.write("notes/a.md", b"text");
        scratch.commit("fixture");
        let vault = scratch.open().await;
        assert!(run(&vault).await.unwrap().is_empty());
        scratch.write(".scheda.yaml", b"version: 4\n");
        scratch.write("bad/.collection.yaml", b"name: bad\n");
        scratch.commit("another generation");
        scratch.write("notes/a.md", b"modified");
        let problems = run(&vault).await.unwrap();
        assert_eq!(
            found(&problems),
            [(VaultErrorCode::Generation, ".scheda.yaml".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_commit_without_the_marker_is_the_one_problem() {
        let scratch = Scratch::vault("no-marker");
        scratch.commit("fixture");
        let vault = scratch.open().await;
        std::fs::remove_file(scratch.0.join(".scheda.yaml")).unwrap();
        scratch.commit("no marker");
        let problems = run(&vault).await.unwrap();
        assert_eq!(
            found(&problems),
            [(VaultErrorCode::NotAVault, ".scheda.yaml".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_problem_carries_its_code_path_and_message() {
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
