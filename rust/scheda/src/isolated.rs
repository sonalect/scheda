//! Unencrypted isolated tree: `{vault}/.isolated/` nested git.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::error::{ContextIo, bail};
use crate::git;
use crate::schema::{self, CollectionDecl};

/// Vault-relative directory that holds isolated Record files.
pub(crate) const DIR: &str = ".isolated";

const PREFIX: &str = ".isolated/";
const NESTED_GITIGNORE: &str = ".scheda/\n";

/// Absolute path of `{vault}/.isolated/`.
#[must_use]
pub(crate) fn root(vault: &Path) -> PathBuf {
    vault.join(DIR)
}

/// Record directory for a collection (`{vault}/{name}/` or under `.isolated/`).
pub(crate) fn records_dir(vault: &Path, decl: &CollectionDecl) -> Result<PathBuf, Error> {
    if decl.isolated {
        schema::join_inside(
            vault,
            &format!("{DIR}/{}", schema::collection_dir(&decl.name)),
        )
    } else {
        schema::join_in_collection(vault, &decl.name, "")
    }
}

/// Vault-relative POSIX path of a Record file name inside `decl`.
#[must_use]
pub(crate) fn vault_rel(decl: &CollectionDecl, file: &str) -> String {
    if decl.isolated {
        format!("{DIR}/{}/{file}", schema::collection_dir(&decl.name))
    } else {
        format!("{}/{file}", schema::collection_dir(&decl.name))
    }
}

/// Nested-git relative path, if `rel` is under `.isolated/`.
#[must_use]
pub(crate) fn nested_rel(rel: &str) -> Option<&str> {
    rel.strip_prefix(PREFIX)
}

/// Git work tree and path inside it for `history` of a vault-relative Record path.
pub(crate) fn git_addr(vault: &Path, rel: &str) -> (PathBuf, String) {
    match nested_rel(rel) {
        Some(nested) => (root(vault), nested.to_owned()),
        None => (vault.to_path_buf(), rel.to_owned()),
    }
}

/// Create `{vault}/.isolated/` as a git working tree with `.scheda/` gitignored.
///
/// Returns the vault-relative `.isolated/.gitignore` path when that file was
/// created or repaired, so the caller can record it for `commit`.
pub(crate) fn ensure(vault: &Path) -> Result<Option<String>, Error> {
    let root = root(vault);
    crate::store::reject_symlinks(vault, &root)?;
    fs::create_dir_all(&root).context("[Isolated] create .isolated")?;
    crate::store::restrict_dir(&root)?;
    if !git::is_work_tree(&root) {
        git::init(&root)?;
    }
    let noted = ensure_nested_gitignore(&root)?;
    Ok(noted.then_some(format!("{DIR}/.gitignore")))
}

/// Commit recorded paths: isolated paths in the nested git, the rest in the
/// main repository. Empty on both sides is `invalid`. Nested git is created
/// when isolated paths are present. Nested is committed first; if the main
/// commit then fails, nested `HEAD` is reset to the OID captured before that
/// nested commit. Both repositories use the same author and committer.
pub(crate) fn commit_recorded(
    vault: &Path,
    message: &str,
    paths: &[String],
    identity: &git::CommitIdentity,
) -> Result<(), Error> {
    let (main, nested) = split_paths(paths);
    if main.is_empty() && nested.is_empty() {
        bail!(Error::invalid("nothing to commit"));
    }
    commit_split(vault, &main, &nested, |dir, paths| {
        git::commit(dir, message, paths, identity).map(|()| true)
    })?;
    Ok(())
}

/// [`commit_recorded`] that takes "nothing to commit" as an answer: no
/// path, or paths that leave both repositories as at `HEAD`, commit
/// nothing and return `false`. `true` when either repository got a commit.
pub(crate) fn commit_recorded_if_changed(
    vault: &Path,
    message: &str,
    paths: &[String],
    identity: &git::CommitIdentity,
) -> Result<bool, Error> {
    let (main, nested) = split_paths(paths);
    commit_split(vault, &main, &nested, |dir, paths| {
        git::commit_if_changed(dir, message, paths, identity)
    })
}

/// Commit `nested` in the nested repository, then `main` in the Vault, with
/// `commit` (which returns whether it made a commit). A failed main commit
/// resets a nested commit made here.
fn commit_split(
    vault: &Path,
    main: &[String],
    nested: &[String],
    commit: impl Fn(&Path, &[String]) -> Result<bool, Error>,
) -> Result<bool, Error> {
    if nested.is_empty() {
        if main.is_empty() {
            return Ok(false);
        }
        return commit(vault, main);
    }
    ensure(vault)?;
    let nested_root = root(vault);
    let previous = git::peel_head_oid(&nested_root)?;
    let nested_changed = commit(&nested_root, nested)?;
    if main.is_empty() {
        return Ok(nested_changed);
    }
    match commit(vault, main) {
        Ok(main_changed) => Ok(nested_changed || main_changed),
        Err(err) => {
            if nested_changed {
                git::reset_head_to(&nested_root, previous.as_deref())
                    .map_err(|reset_err| reset_err.context("[Isolated] rollback nested git"))?;
            }
            Err(err)
        }
    }
}

/// Restore recorded paths in each repo that owns them.
pub(crate) fn discard_recorded(vault: &Path, paths: &[String]) -> Result<(), Error> {
    let (main, nested) = split_paths(paths);
    if !nested.is_empty() {
        let nested_root = root(vault);
        if git::is_work_tree(&nested_root) {
            git::discard_paths(&nested_root, &nested)?;
        } else {
            for rel in &nested {
                let abs = nested_root.join(rel);
                match fs::remove_file(&abs) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(err).context(format!("[Isolated] remove {rel}"));
                    }
                }
            }
        }
    }
    if !main.is_empty() {
        git::discard_paths(vault, &main)?;
    }
    Ok(())
}

/// Parent must not track `.isolated/`. A nested Remote of its own is allowed.
pub(crate) fn check_perimeter(vault: &Path) -> Result<(), Error> {
    if git::index_has_prefix(vault, DIR)? {
        bail!(Error::invalid(
            "parent git tracks .isolated/; isolated Records must stay gitignored"
        ));
    }
    Ok(())
}

/// Owner-only mode on an isolated Record file and its directories under `.isolated/`.
pub(crate) fn restrict_record(vault: &Path, path: &Path) -> Result<(), Error> {
    let root = root(vault);
    if !path.starts_with(&root) {
        return Ok(());
    }
    crate::store::restrict_dir(&root)?;
    let Ok(rest) = path.strip_prefix(&root) else {
        return Ok(());
    };
    let mut cur = root;
    for component in rest.components() {
        cur.push(component);
        if cur.is_dir() {
            crate::store::restrict_dir(&cur)?;
        } else if cur.is_file() {
            crate::store::restrict_file(&cur)?;
        }
    }
    Ok(())
}

fn split_paths(paths: &[String]) -> (Vec<String>, Vec<String>) {
    let mut main = Vec::new();
    let mut nested = Vec::new();
    for rel in paths {
        match nested_rel(rel) {
            Some(rest) => nested.push(rest.to_owned()),
            None => main.push(rel.clone()),
        }
    }
    (main, nested)
}

fn ensure_nested_gitignore(root: &Path) -> Result<bool, Error> {
    let path = root.join(".gitignore");
    match fs::read_to_string(&path) {
        Ok(text) if text.lines().any(|line| line.trim() == ".scheda/") => Ok(false),
        Ok(mut text) => {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            if !text.lines().any(|line| line.trim() == ".scheda/") {
                text.push_str(NESTED_GITIGNORE);
            }
            store_write(&path, text.as_bytes())?;
            crate::store::restrict_file(&path)?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            store_write(&path, NESTED_GITIGNORE.as_bytes())?;
            crate::store::restrict_file(&path)?;
            Ok(true)
        }
        Err(err) => Err(err).context("[Isolated] read nested .gitignore"),
    }
}

fn store_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    crate::store::atomic_write(path, bytes)
}
