use std::ffi::OsStr;
use std::path::Path;

#[cfg(test)]
use git2::Config;
use git2::{
    Commit as GitCommit, DiffOptions, ErrorCode, Repository, RepositoryInitOptions,
    RepositoryOpenFlags, Signature, Sort,
};

use crate::{Error, HistoryEntry};

fn map_err(err: git2::Error) -> Error {
    Error::git(err.message().to_owned())
}

fn open_at(dir: &Path) -> Result<Repository, git2::Error> {
    Repository::open_ext(
        dir,
        RepositoryOpenFlags::NO_SEARCH,
        std::iter::empty::<&OsStr>(),
    )
}

fn open_work_tree(dir: &Path) -> Result<Repository, Error> {
    let repo = open_at(dir).map_err(map_err)?;
    if repo.is_bare() {
        return Err(Error::invalid(format!(
            "path is a bare git repository, not a working tree: {}",
            dir.display()
        )));
    }
    Ok(repo)
}

fn signature(repo: &Repository) -> Result<Signature<'static>, Error> {
    if let Ok(sig) = repo.signature()
        && let (Ok(name), Ok(email)) = (sig.name(), sig.email())
        && !name.is_empty()
        && !email.is_empty()
    {
        return Signature::now(name, email).map_err(map_err);
    }
    Signature::now("Scheda", "scheda@invalid").map_err(map_err)
}

fn is_unborn(err: &git2::Error) -> bool {
    matches!(err.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound)
}

pub(crate) fn init(dir: &Path) -> Result<(), Error> {
    let mut opts = RepositoryInitOptions::new();
    opts.no_reinit(true);
    let _repo = Repository::init_opts(dir, &opts).map_err(map_err)?;
    Ok(())
}

pub(crate) fn is_work_tree(dir: &Path) -> bool {
    match open_at(dir) {
        Ok(repo) => !repo.is_bare(),
        Err(_) => false,
    }
}

pub(crate) fn remotes(dir: &Path) -> Result<Vec<String>, Error> {
    let repo = open_work_tree(dir)?;
    let names = repo.remotes().map_err(map_err)?;
    let mut out = Vec::new();
    for bytes in names.iter_bytes() {
        let name =
            std::str::from_utf8(bytes).map_err(|_| Error::git("remote name was not UTF-8"))?;
        out.push(name.to_owned());
    }
    Ok(out)
}

pub(crate) fn commit(dir: &Path, message: &str, paths: &[String]) -> Result<(), Error> {
    if message.trim().is_empty() {
        return Err(Error::invalid("commit message is empty"));
    }
    if paths.is_empty() {
        return Err(Error::invalid("nothing to commit"));
    }
    let repo = open_work_tree(dir)?;
    let mut index = repo.index().map_err(map_err)?;
    let mut staged = 0usize;
    for rel in paths {
        let abs = dir.join(rel);
        if !abs.is_file() {
            continue;
        }
        index.add_path(Path::new(rel)).map_err(map_err)?;
        staged += 1;
    }
    if staged == 0 {
        return Err(Error::invalid("nothing to commit"));
    }
    index.write().map_err(map_err)?;
    let tree_id = index.write_tree().map_err(map_err)?;
    let tree = repo.find_tree(tree_id).map_err(map_err)?;
    let sig = signature(&repo)?;
    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit().map_err(map_err)?),
        Err(err) if is_unborn(&err) => None,
        Err(err) => return Err(map_err(err)),
    };
    if parent
        .as_ref()
        .is_some_and(|parent| parent.tree_id() == tree_id)
    {
        return Ok(());
    }
    let parents: Vec<&GitCommit<'_>> = parent.as_ref().into_iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .map_err(map_err)?;
    Ok(())
}

pub(crate) fn history(dir: &Path, path: &str) -> Result<Vec<HistoryEntry>, Error> {
    require_vault_rel(path)?;
    let repo = open_work_tree(dir)?;
    match repo.head() {
        Ok(_) => {}
        Err(err) if is_unborn(&err) => return Ok(Vec::new()),
        Err(err) => return Err(map_err(err)),
    }
    let mut revwalk = repo.revwalk().map_err(map_err)?;
    revwalk.set_sorting(Sort::TIME).map_err(map_err)?;
    revwalk.push_head().map_err(map_err)?;
    let mut out = Vec::new();
    for oid in revwalk {
        let oid = oid.map_err(map_err)?;
        let commit = repo.find_commit(oid).map_err(map_err)?;
        if !path_changed(&repo, &commit, path)? {
            continue;
        }
        let tree = commit.tree().map_err(map_err)?;
        let Ok(entry) = tree.get_path(Path::new(path)) else {
            continue;
        };
        let blob = repo.find_blob(entry.id()).map_err(map_err)?;
        let author = commit.author();
        let name = author.name().unwrap_or("");
        let email = author.email().unwrap_or("");
        let author = if email.is_empty() {
            name.to_owned()
        } else {
            format!("{name} <{email}>")
        };
        out.push(
            HistoryEntry::default()
                .with_hash(oid.to_string())
                .with_time(commit.time().seconds())
                .with_author(author)
                .with_message(commit.message().unwrap_or("").to_owned())
                .with_raw(blob.content().to_vec()),
        );
    }
    out.sort_by_key(|entry| std::cmp::Reverse(entry.time.seconds));
    Ok(out)
}

fn require_vault_rel(path: &str) -> Result<(), Error> {
    if path.is_empty() {
        return Err(Error::invalid("path is empty"));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(Error::invalid("path must be vault-relative"));
    }
    for part in path.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." {
            return Err(Error::invalid("path must be vault-relative"));
        }
    }
    Ok(())
}

fn path_changed(repo: &Repository, commit: &GitCommit<'_>, path: &str) -> Result<bool, Error> {
    let new_tree = commit.tree().map_err(map_err)?;
    let old_tree = if commit.parent_count() == 0 {
        None
    } else {
        Some(commit.parent(0).map_err(map_err)?.tree().map_err(map_err)?)
    };
    let mut opts = DiffOptions::new();
    opts.pathspec(path);
    let diff = repo
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))
        .map_err(map_err)?;
    Ok(diff.deltas().next().is_some())
}

#[cfg(test)]
pub(crate) fn add_remote(dir: &Path, name: &str, url: &str) -> Result<(), Error> {
    let repo = open_work_tree(dir)?;
    repo.remote(name, url).map_err(map_err)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn is_ignored(dir: &Path, rel: &Path) -> Result<bool, Error> {
    let repo = open_work_tree(dir)?;
    repo.status_should_ignore(rel).map_err(map_err)
}

#[cfg(test)]
pub(crate) fn local_config_mentions_lfs(dir: &Path) -> Result<bool, Error> {
    let repo = open_work_tree(dir)?;
    let config = repo.config().map_err(map_err)?;
    for key in [
        "filter.lfs.process",
        "filter.lfs.required",
        "filter.lfs.clean",
        "filter.lfs.smudge",
    ] {
        if config_has(&config, key)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
fn config_has(config: &Config, key: &str) -> Result<bool, Error> {
    match config.get_entry(key) {
        Ok(_) => Ok(true),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(false),
        Err(err) => Err(map_err(err)),
    }
}
