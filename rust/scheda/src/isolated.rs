//! Unencrypted isolated tree: `{vault}/.isolated/` nested git, no Remote.

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

/// Isolated Index directory: `{vault}/.isolated/.scheda/index`.
#[must_use]
pub(crate) fn index_dir(vault: &Path) -> PathBuf {
    root(vault).join(".scheda").join("index")
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

/// Index directory that holds the journal `id` map for `decl`.
#[must_use]
pub(crate) fn ids_index_dir(vault: &Path, main_index: &Path, isolated: bool) -> PathBuf {
    if isolated {
        index_dir(vault)
    } else {
        main_index.to_path_buf()
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
    fs::create_dir_all(&root).context("[Isolated] create .isolated")?;
    restrict_dir(&root)?;
    if !git::is_work_tree(&root) {
        git::init(&root)?;
    }
    let noted = ensure_nested_gitignore(&root)?;
    Ok(noted.then_some(format!("{DIR}/.gitignore")))
}

/// Commit recorded paths: isolated paths in the nested git, the rest in the
/// main repository. Empty on both sides is `invalid`. Nested git is created
/// when isolated paths are present. The two commits are not one atomic unit.
pub(crate) fn commit_recorded(vault: &Path, message: &str, paths: &[String]) -> Result<(), Error> {
    let (main, nested) = split_paths(paths);
    if main.is_empty() && nested.is_empty() {
        bail!(Error::invalid("nothing to commit"));
    }
    if !nested.is_empty() {
        ensure(vault)?;
        git::commit(&root(vault), message, &nested)?;
    }
    if !main.is_empty() {
        git::commit(vault, message, &main)?;
    }
    Ok(())
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

/// Parent must not track `.isolated/`; nested git must have no Remote.
pub(crate) fn check_perimeter(vault: &Path) -> Result<(), Error> {
    if git::index_has_prefix(vault, DIR)? {
        bail!(Error::invalid(
            "parent git tracks .isolated/; isolated Records must stay gitignored"
        ));
    }
    let nested = root(vault);
    if !nested.exists() {
        return Ok(());
    }
    if !git::is_work_tree(&nested) {
        return Ok(());
    }
    if !git::remotes(&nested)?.is_empty() {
        bail!(Error::invalid("isolated nested git must not have a Remote"));
    }
    Ok(())
}

/// Owner-only mode on an isolated Record file and its directories under `.isolated/`.
pub(crate) fn restrict_record(vault: &Path, path: &Path) -> Result<(), Error> {
    let root = root(vault);
    if !path.starts_with(&root) {
        return Ok(());
    }
    restrict_dir(&root)?;
    let Ok(rest) = path.strip_prefix(&root) else {
        return Ok(());
    };
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
            restrict_file(&path)?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            store_write(&path, NESTED_GITIGNORE.as_bytes())?;
            restrict_file(&path)?;
            Ok(true)
        }
        Err(err) => Err(err).context("[Isolated] read nested .gitignore"),
    }
}

fn store_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    crate::store::atomic_write(path, bytes)
}

fn restrict_file(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o600)
}

fn restrict_dir(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o700)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, Permissions::from_mode(mode))
            .context("[Isolated] set owner-only mode")?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}
