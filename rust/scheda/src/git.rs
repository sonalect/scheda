use std::ffi::OsStr;
use std::path::Path;

#[cfg(test)]
use git2::{Config, ErrorCode, StatusOptions};
use git2::{Repository, RepositoryInitOptions, RepositoryOpenFlags};

use crate::Error;

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

#[cfg(test)]
pub(crate) fn add_remote(dir: &Path, name: &str, url: &str) -> Result<(), Error> {
    let repo = open_work_tree(dir)?;
    repo.remote(name, url).map_err(map_err)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn worktree_is_clean(dir: &Path) -> Result<bool, Error> {
    let repo = open_work_tree(dir)?;
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_unmodified(false);
    let statuses = repo.statuses(Some(&mut opts)).map_err(map_err)?;
    Ok(statuses.is_empty())
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
