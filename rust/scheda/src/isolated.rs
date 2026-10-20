//! The isolated tree: the Documents of an `isolated` collection live under
//! `{vault}/.isolated/`, a nested git work tree of their own that the Vault
//! repository never tracks.
//!
//! This module holds the paths, the checks, and the making of the nested
//! repository. Reading it is a snapshot of `repo::Repo::Isolated`, and
//! committing in it is `commit::WriteLock::commit_in`.

use std::fs;
use std::path::{Path, PathBuf};

use scheda_api::v1::{Problem, VaultErrorCode};

use crate::check::problem;
use crate::collection::Declaration;
use crate::commit::Change;
use crate::error::{Context, Error, io};
use crate::fs::{reject_symlinks, restrict_dir, restrict_file};
use crate::repo::{self, Repo, Repos, Snap};

/// Vault-relative directory that holds the isolated Documents.
pub(crate) const DIR: &str = ".isolated";

const PREFIX: &str = ".isolated/";

/// What the nested repository ignores: the folder Scheda keeps its own
/// state in.
const NESTED_GITIGNORE: &str = ".scheda/\n";

/// Absolute path of `{vault}/.isolated/`.
pub(crate) fn root(vault: &Path) -> PathBuf {
    vault.join(DIR)
}

/// The directory of a collection: `{vault}/{name}/`, or
/// `{vault}/.isolated/{name}/` when the collection is isolated. The name is
/// a slug the declaration already passed, so it cannot step out of the
/// Vault.
pub(crate) fn records_dir(vault: &Path, decl: &Declaration) -> PathBuf {
    if decl.isolated() {
        root(vault).join(decl.name())
    } else {
        vault.join(decl.name())
    }
}

/// Vault-relative POSIX path of the file `file` (a path below the
/// collection directory, such as a Document `id`) of `decl`.
pub(crate) fn vault_rel(decl: &Declaration, file: &str) -> String {
    if decl.isolated() {
        format!("{DIR}/{}/{file}", decl.name())
    } else {
        format!("{}/{file}", decl.name())
    }
}

/// The path inside the nested repository, if `rel` (Vault-relative) is
/// under `.isolated/`.
pub(crate) fn nested_rel(rel: &str) -> Option<&str> {
    rel.strip_prefix(PREFIX)
}

/// Makes `{vault}/.isolated/` a git work tree of its own, owner-only, when
/// it is not one yet. Nothing is made for a write that stores nothing: the
/// caller asks only when it has changes. Its `.gitignore` is
/// [`gitignore_change`], which the write puts in its commit.
///
/// A symlink where `.isolated/` should be is `INVALID_ARGUMENT`.
pub(crate) fn ensure(vault: &Path) -> Result<(), Error> {
    let nested = root(vault);
    reject_symlinks(vault, &nested).context("check .isolated")?;
    fs::create_dir_all(&nested)
        .map_err(io)
        .context("create .isolated")?;
    restrict_dir(&nested)?;
    if !repo::is_work_tree(&nested)? {
        repo::init_repository(&nested).context("make the isolated repository")?;
    }
    Ok(())
}

/// The change that gives the nested repository its `.gitignore` (`.scheda/`)
/// when the commit of `snap`, a snapshot of it, lacks the entry. The write
/// that needs the nested repository puts that change in its commit.
pub(crate) fn gitignore_change(snap: &Snap) -> Result<Vec<Change>, Error> {
    let current = snap
        .read(&format!("{DIR}/.gitignore"))
        .map_err(|fault| fault.with_path(format!("{DIR}/.gitignore")))?
        .map(|blob| blob.bytes)
        .unwrap_or_default();
    let has = current
        .split(|byte| *byte == b'\n')
        .any(|line| line.trim_ascii() == b".scheda/");
    if has {
        return Ok(Vec::new());
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    text.extend_from_slice(NESTED_GITIGNORE.as_bytes());
    Ok(vec![Change::put(format!("{DIR}/.gitignore"), text)])
}

/// Owner-only mode on the file or directory `path` and on every directory
/// between `.isolated/` and it, `.isolated/` itself included. A path outside
/// `.isolated/` is left alone.
pub(crate) fn restrict_record(vault: &Path, path: &Path) -> Result<(), Error> {
    let root = root(vault);
    let Ok(rest) = path.strip_prefix(&root) else {
        return Ok(());
    };
    restrict_dir(&root).context("restrict_record: restrict .isolated")?;
    let mut cur = root;
    for component in rest.components() {
        cur.push(component);
        if cur.is_dir() {
            restrict_dir(&cur)?;
        } else if cur.is_file() {
            restrict_file(&cur)?;
        }
    }
    Ok(())
}

/// The perimeter: the Vault repository must not track `.isolated/`. A
/// tracked path there is an `INVALID_ARGUMENT` problem, because isolated
/// Documents must stay out of the repository that has a mirror. The nested
/// repository may have a remote of its own.
pub(crate) fn check_perimeter(repos: &Repos) -> Result<Vec<Problem>, Error> {
    let Some(lease) = repos.lease(Repo::Main)? else {
        return Ok(Vec::new());
    };
    let tracked = repo::index_has_prefix(&lease, DIR).context("check_perimeter: read the index")?;
    let mut problems = Vec::new();
    if tracked {
        problems.push(problem(
            VaultErrorCode::InvalidArgument,
            Some(DIR),
            "the Vault repository tracks .isolated/; isolated Documents must stay gitignored",
        ));
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::VaultErrorCode;

    use super::{check_perimeter, nested_rel, records_dir, root, vault_rel};
    use crate::collection::{Declaration, Declarations};
    use crate::git::{git, isolate_test_git_env};
    use crate::repo::{Repos, fixture};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-isolated-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    fn declared(scratch: &Scratch, name: &str, schema: &str) -> Declaration {
        std::fs::create_dir_all(scratch.0.join(name)).unwrap();
        std::fs::write(
            scratch.0.join(name).join(".collection.yaml"),
            format!("name: {name}\nschema:\n{schema}"),
        )
        .unwrap();
        fixture::commit_all(&scratch.0);
        let declarations: Declarations =
            crate::collection::load(&fixture::snap(&scratch.0)).unwrap();
        declarations.get(name).unwrap().clone()
    }

    #[test]
    fn paths_follow_the_isolated_mark() {
        let scratch = Scratch::new("paths");
        let plain = declared(&scratch, "notes", "  standard: {}\n");
        let hidden = declared(&scratch, "diary", "  standard: {}\n  isolated: true\n");
        let vault = scratch.0.as_path();
        assert_eq!(records_dir(vault, &plain), vault.join("notes"));
        assert_eq!(records_dir(vault, &hidden), vault.join(".isolated/diary"));
        assert_eq!(vault_rel(&plain, "a/b.md"), "notes/a/b.md");
        assert_eq!(vault_rel(&hidden, "a/b.md"), ".isolated/diary/a/b.md");
        assert_eq!(nested_rel(".isolated/diary/a"), Some("diary/a"));
        assert_eq!(nested_rel("notes/a"), None);
        assert_eq!(nested_rel(".isolated"), None);
        assert_eq!(root(vault), vault.join(".isolated"));
    }

    #[cfg(unix)]
    #[test]
    fn a_record_and_its_directories_become_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        use super::restrict_record;

        let scratch = Scratch::new("restrict");
        let vault = scratch.0.as_path();
        let file = vault.join(".isolated/diary/2026/a.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        restrict_record(vault, &file).unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&vault.join(".isolated")), 0o700);
        assert_eq!(mode(&vault.join(".isolated/diary")), 0o700);
        assert_eq!(mode(&vault.join(".isolated/diary/2026")), 0o700);
        assert_eq!(mode(&file), 0o600);
        let outside = vault.join("notes/a.md");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, b"x").unwrap();
        let before = mode(&outside);
        restrict_record(vault, &outside).unwrap();
        assert_eq!(mode(&outside), before);
    }

    #[test]
    fn a_tracked_isolated_path_breaks_the_perimeter() {
        let scratch = Scratch::new("perimeter");
        let vault = scratch.0.as_path();
        git().arg("init").arg("--").arg(vault).run().unwrap();
        std::fs::write(vault.join(".gitignore"), ".isolated/\n").unwrap();
        std::fs::create_dir_all(vault.join(".isolated/diary")).unwrap();
        std::fs::write(vault.join(".isolated/diary/a"), b"x").unwrap();
        std::fs::write(vault.join("kept"), b"x").unwrap();
        git()
            .current_dir(vault)
            .arg("add")
            .arg("--all")
            .run()
            .unwrap();
        assert!(check_perimeter(&Repos::new(vault)).unwrap().is_empty());
        git()
            .current_dir(vault)
            .arg("add")
            .arg("--force")
            .arg(".isolated/diary/a")
            .run()
            .unwrap();
        let problems = check_perimeter(&Repos::new(vault)).unwrap();
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].code.and_then(|code| code.as_known()),
            Some(VaultErrorCode::InvalidArgument)
        );
        assert_eq!(problems[0].path.as_deref(), Some(".isolated"));
    }
}
