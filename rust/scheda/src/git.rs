use std::ffi::OsStr;
use std::fs;
use std::path::Path;

#[cfg(any(test, feature = "remote-git"))]
use git2::Config;
#[cfg(test)]
use git2::ConfigLevel;
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{
    AnnotatedCommit, Commit as GitCommit, Cred, CredentialType, DiffOptions, ErrorCode,
    FetchOptions, PushOptions, RemoteCallbacks, Repository, RepositoryInitOptions,
    RepositoryOpenFlags, Signature, Sort, Status, StatusOptions,
};

use crate::error::{Code, ContextAs, ContextGit, ContextIo, bail, git_code};
use crate::{Error, history};

fn git_fail(err: git2::Error, frame: &str) -> Error {
    Error::new(git_code(&err), err.message()).context(frame)
}

fn open_at(dir: &Path) -> Result<Repository, git2::Error> {
    Repository::open_ext(
        dir,
        RepositoryOpenFlags::NO_SEARCH,
        std::iter::empty::<&OsStr>(),
    )
}

fn open_work_tree(dir: &Path) -> Result<Repository, Error> {
    let repo = open_at(dir).context("[Git::open_work_tree] open repository")?;
    if repo.is_bare() {
        bail!(Error::invalid(format!(
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
        return Signature::now(name, email).context("[Git::signature] author from config");
    }
    Signature::now("Scheda", "scheda@invalid").context("[Git::signature] fallback author")
}

fn is_unborn(err: &git2::Error) -> bool {
    matches!(err.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound)
}

pub(crate) fn init(dir: &Path) -> Result<(), Error> {
    let mut opts = RepositoryInitOptions::new();
    opts.no_reinit(true);
    let _repo = Repository::init_opts(dir, &opts).context("[Git::init] create repository")?;
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
    let names = repo.remotes().context("[Git::remotes] list remotes")?;
    let mut out = Vec::new();
    for bytes in names.iter_bytes() {
        let name =
            std::str::from_utf8(bytes).context(Code::NotUtf8, "[Git::remotes] remote name")?;
        out.push(name.to_owned());
    }
    Ok(out)
}

fn url_needs_remote_git(url: &str) -> bool {
    let url = url.trim();
    let prefix = url.get(..8).unwrap_or(url);
    let lower = prefix.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ssh://")
        || url.starts_with("git@")
}

fn require_remote_git_url(url: &str) -> Result<(), Error> {
    if url_needs_remote_git(url) && !cfg!(feature = "remote-git") {
        bail!(Error::unsupported(
            "https://, http://, ssh://, and git@ remotes need the scheda crate feature remote-git",
        ));
    }
    Ok(())
}

pub(crate) fn clone(url: &str, path: &Path) -> Result<(), Error> {
    require_remote_git_url(url)?;
    let mut fetch = FetchOptions::new();
    fetch.remote_callbacks(auth_callbacks());
    RepoBuilder::new()
        .fetch_options(fetch)
        .clone(url, path)
        .context("[Git::clone] clone repository")?;
    Ok(())
}

fn auth_callbacks() -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(git_credentials);
    callbacks
}

fn git_credentials(
    url: &str,
    username_from_url: Option<&str>,
    allowed: CredentialType,
) -> Result<Cred, git2::Error> {
    if allowed.is_username() {
        return Cred::username(username_from_url.unwrap_or("git"));
    }
    if allowed.is_ssh_key() {
        let username = username_from_url.unwrap_or("git");
        if let Ok(cred) = Cred::ssh_key_from_agent(username) {
            return Ok(cred);
        }
    }
    if allowed.is_user_pass_plaintext() {
        #[cfg(feature = "remote-git")]
        {
            let cfg = Config::open_default()?;
            return Cred::credential_helper(&cfg, url, username_from_url);
        }
    }
    if allowed.is_default() {
        return Cred::default();
    }
    Err(git2::Error::from_str(&format!(
        "no git credentials available for this remote ({url})"
    )))
}

fn default_remote_name(dir: &Path) -> Result<String, Error> {
    let names = remotes(dir)?;
    if let Some(origin) = names.iter().find(|name| name.as_str() == "origin") {
        return Ok(origin.clone());
    }
    names
        .into_iter()
        .next()
        .ok_or_else(|| Error::invalid("no Remote configured"))
}

fn branch_refname(repo: &Repository) -> Result<String, Error> {
    match repo.head() {
        Ok(head) => {
            if !head.is_branch() {
                bail!(
                    Error::git_head("HEAD is not a branch")
                        .context("[Git::branch_refname] require branch")
                );
            }
            Ok(head
                .name()
                .context("[Git::branch_refname] read branch name")?
                .to_owned())
        }
        Err(err) if is_unborn(&err) => {
            let head = repo
                .find_reference("HEAD")
                .context("[Git::branch_refname] read HEAD")?;
            match head
                .symbolic_target()
                .context("[Git::branch_refname] read HEAD target")?
            {
                Some(name) => Ok(name.to_owned()),
                None => Err(Error::git_head("unborn HEAD has no branch name")
                    .context("[Git::branch_refname] read HEAD target")),
            }
        }
        Err(err) => Err(git_fail(err, "[Git::branch_refname] read HEAD")),
    }
}

fn worktree_has_local_changes(repo: &Repository) -> Result<bool, Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
    let statuses = repo
        .statuses(Some(&mut opts))
        .context("[Git::fast_forward] status")?;
    Ok(statuses.iter().any(|entry| {
        let status = entry.status();
        !status.is_empty() && status != Status::CURRENT && !status.is_ignored()
    }))
}

fn fast_forward(repo: &Repository, fetch_commit: &AnnotatedCommit<'_>) -> Result<(), Error> {
    if worktree_has_local_changes(repo)? {
        bail!(
            Error::git_uncommitted("working tree has local changes; pull needs a clean tree")
                .context("[Git::fast_forward] require clean tree")
        );
    }
    let refname = branch_refname(repo)?;
    match repo.find_reference(&refname) {
        Ok(mut local) => {
            let msg = format!("Fast-forward: setting {refname} to {}", fetch_commit.id());
            local
                .set_target(fetch_commit.id(), &msg)
                .context("[Git::fast_forward] update branch")?;
        }
        Err(err) if err.code() == ErrorCode::NotFound => {
            let msg = format!("Setting {refname} to {}", fetch_commit.id());
            repo.reference(&refname, fetch_commit.id(), true, &msg)
                .context("[Git::fast_forward] create branch")?;
        }
        Err(err) => bail!(git_fail(err, "[Git::fast_forward] find branch")),
    }
    repo.set_head(&refname)
        .context("[Git::fast_forward] set HEAD")?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .context("[Git::fast_forward] checkout HEAD")?;
    Ok(())
}

pub(crate) fn pull(dir: &Path) -> Result<(), Error> {
    let name = default_remote_name(dir)?;
    let repo = open_work_tree(dir)?;
    {
        let mut remote = repo.find_remote(&name).context("[Git::pull] find remote")?;
        require_remote_git_url(remote.url().context("[Git::pull] read remote URL")?)?;
        let mut fetch = FetchOptions::new();
        fetch.remote_callbacks(auth_callbacks());
        remote
            .fetch(&[] as &[&str], Some(&mut fetch), None)
            .context("[Git::pull] fetch")?;
    }
    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .context("[Git::pull] read FETCH_HEAD")?;
    let fetch_commit = repo
        .reference_to_annotated_commit(&fetch_head)
        .context("[Git::pull] annotate FETCH_HEAD")?;
    let (analysis, _) = repo
        .merge_analysis(&[&fetch_commit])
        .context("[Git::pull] merge analysis")?;
    if analysis.is_up_to_date() {
        return Ok(());
    }
    if analysis.is_fast_forward() || analysis.is_unborn() {
        return fast_forward(&repo, &fetch_commit);
    }
    Err(Error::not_fast_forward(
        "Remote is not a fast-forward of HEAD",
    ))
}

pub(crate) fn push(dir: &Path) -> Result<(), Error> {
    let name = default_remote_name(dir)?;
    let repo = open_work_tree(dir)?;
    let refname = branch_refname(&repo)?;
    let spec = format!("{refname}:{refname}");
    let mut remote = repo.find_remote(&name).context("[Git::push] find remote")?;
    require_remote_git_url(remote.url().context("[Git::push] read remote URL")?)?;
    let mut push = PushOptions::new();
    push.remote_callbacks(auth_callbacks());
    remote
        .push(&[spec.as_str()], Some(&mut push))
        .context("[Git::push] push")?;
    Ok(())
}

pub(crate) fn commit(dir: &Path, message: &str, paths: &[String]) -> Result<(), Error> {
    if message.trim().is_empty() {
        bail!(Error::invalid("commit message is empty"));
    }
    if paths.is_empty() {
        bail!(Error::invalid("nothing to commit"));
    }
    let repo = open_work_tree(dir)?;
    let mut index = repo.index().context("[Git::commit] open index")?;
    let mut staged = 0usize;
    for rel in paths {
        let abs = dir.join(rel);
        if abs.is_file() {
            index
                .add_path(Path::new(rel))
                .with_context(|| format!("[Git::commit] stage {rel}"))?;
            staged += 1;
        } else {
            match index.remove_path(Path::new(rel)) {
                Ok(()) => staged += 1,
                Err(err) if err.code() == ErrorCode::NotFound => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("[Git::commit] remove {rel}"));
                }
            }
        }
    }
    if staged == 0 {
        bail!(Error::invalid("nothing to commit"));
    }
    index.write().context("[Git::commit] write index")?;
    let tree_id = index.write_tree().context("[Git::commit] write tree")?;
    let tree = repo.find_tree(tree_id).context("[Git::commit] read tree")?;
    let sig = signature(&repo)?;
    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit().context("[Git::commit] peel HEAD")?),
        Err(err) if is_unborn(&err) => None,
        Err(err) => bail!(git_fail(err, "[Git::commit] read HEAD")),
    };
    if parent
        .as_ref()
        .is_some_and(|parent| parent.tree_id() == tree_id)
    {
        return Ok(());
    }
    let parents: Vec<&GitCommit<'_>> = parent.as_ref().into_iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .context("[Git::commit] create commit")?;
    Ok(())
}

/// Restore Scheda-recorded paths to HEAD. New files (not in HEAD) are deleted.
/// Does not `git reset --hard` the whole tree.
pub(crate) fn discard_paths(dir: &Path, paths: &[String]) -> Result<(), Error> {
    let repo = open_work_tree(dir)?;
    let head_tree = match repo.head() {
        Ok(head) => Some(
            head.peel_to_tree()
                .context("[Git::discard] peel HEAD tree")?,
        ),
        Err(err) if is_unborn(&err) => None,
        Err(err) => bail!(git_fail(err, "[Git::discard] read HEAD")),
    };
    for rel in paths {
        let in_head = head_tree
            .as_ref()
            .is_some_and(|tree| tree.get_path(Path::new(rel)).is_ok());
        if in_head {
            let mut opts = CheckoutBuilder::new();
            opts.force().path(rel.as_str());
            repo.checkout_head(Some(&mut opts))
                .with_context(|| format!("[Git::discard] restore {rel}"))?;
        } else {
            let abs = dir.join(rel);
            match fs::remove_file(&abs) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(err).context(format!("[Git::discard] remove {rel}"));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn history(
    dir: &Path,
    path: &str,
    offset: u32,
    count: u32,
) -> Result<Vec<history::Entry>, Error> {
    require_vault_rel(path)?;
    let repo = open_work_tree(dir)?;
    match repo.head() {
        Ok(_) => {}
        Err(err) if is_unborn(&err) => return Ok(Vec::new()),
        Err(err) => bail!(git_fail(err, "[Git::history] read HEAD")),
    }
    let mut revwalk = repo.revwalk().context("[Git::history] start revwalk")?;
    revwalk
        .set_sorting(Sort::TIME)
        .context("[Git::history] sort revwalk")?;
    revwalk
        .push_head()
        .context("[Git::history] walk from HEAD")?;
    struct Hit {
        oid: git2::Oid,
        seconds: i64,
        author: String,
        message: String,
    }
    let mut hits = Vec::new();
    for oid in revwalk {
        let oid = oid.context("[Git::history] next commit")?;
        let commit = repo
            .find_commit(oid)
            .context("[Git::history] read commit")?;
        if !path_changed(&repo, &commit, path)? {
            continue;
        }
        let tree = commit.tree().context("[Git::history] read tree")?;
        if tree.get_path(Path::new(path)).is_err() {
            continue;
        }
        let author = commit.author();
        let name = author.name().unwrap_or("");
        let email = author.email().unwrap_or("");
        let author = if email.is_empty() {
            name.to_owned()
        } else {
            format!("{name} <{email}>")
        };
        hits.push(Hit {
            oid,
            seconds: commit.time().seconds(),
            author,
            message: commit.message().unwrap_or("").to_owned(),
        });
    }
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.seconds));
    let mut out = Vec::new();
    for hit in hits
        .into_iter()
        .skip(offset as usize)
        .take(count as usize)
    {
        let commit = repo
            .find_commit(hit.oid)
            .context("[Git::history] read commit")?;
        let tree = commit.tree().context("[Git::history] read tree")?;
        let entry = tree
            .get_path(Path::new(path))
            .context("[Git::history] read path")?;
        let blob = repo
            .find_blob(entry.id())
            .context("[Git::history] read blob")?;
        out.push(
            history::Entry::default()
                .with_hash(hit.oid.to_string())
                .with_time(hit.seconds)
                .with_author(hit.author)
                .with_message(hit.message)
                .with_raw(blob.content().to_vec()),
        );
    }
    Ok(out)
}

fn require_vault_rel(path: &str) -> Result<(), Error> {
    if path.is_empty() {
        bail!(Error::invalid("path is empty"));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        bail!(Error::invalid("path must be vault-relative"));
    }
    for part in path.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." {
            bail!(Error::invalid("path must be vault-relative"));
        }
    }
    Ok(())
}

fn path_changed(repo: &Repository, commit: &GitCommit<'_>, path: &str) -> Result<bool, Error> {
    let new_tree = commit.tree().context("[Git::history] read tree")?;
    let old_tree = if commit.parent_count() == 0 {
        None
    } else {
        Some(
            commit
                .parent(0)
                .context("[Git::history] read parent")?
                .tree()
                .context("[Git::history] read parent tree")?,
        )
    };
    let mut opts = DiffOptions::new();
    opts.pathspec(path);
    let diff = repo
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))
        .context("[Git::history] diff path")?;
    Ok(diff.deltas().next().is_some())
}

#[cfg(test)]
pub(crate) fn add_remote(dir: &Path, name: &str, url: &str) -> Result<(), Error> {
    let repo = open_work_tree(dir)?;
    repo.remote(name, url)
        .context("[Git::add_remote] add remote")?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn is_ignored(dir: &Path, rel: &Path) -> Result<bool, Error> {
    let repo = open_work_tree(dir)?;
    repo.status_should_ignore(rel)
        .context("[Git::is_ignored] check ignore")
}

#[cfg(test)]
pub(crate) fn local_config_mentions_lfs(dir: &Path) -> Result<bool, Error> {
    let repo = open_work_tree(dir)?;
    let config = repo
        .config()
        .context("[Git::local_config_mentions_lfs] read config")?;
    for key in [
        "filter.lfs.process",
        "filter.lfs.required",
        "filter.lfs.clean",
        "filter.lfs.smudge",
    ] {
        if config_has_local(&config, key)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
fn config_has_local(config: &Config, key: &str) -> Result<bool, Error> {
    match config.get_entry(key) {
        Ok(entry) => Ok(entry.level() == ConfigLevel::Local),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(false),
        Err(err) => Err(git_fail(
            err,
            "[Git::local_config_mentions_lfs] read config entry",
        )),
    }
}

#[cfg(test)]
mod transport_tests {
    use super::url_needs_remote_git;

    #[test]
    fn file_and_path_urls_do_not_need_remote_git() {
        assert!(!url_needs_remote_git("file:///tmp/repo"));
        assert!(!url_needs_remote_git("/tmp/repo"));
        assert!(!url_needs_remote_git(""));
    }

    #[test]
    fn https_ssh_and_git_at_urls_need_remote_git() {
        assert!(url_needs_remote_git("https://github.com/a/b.git"));
        assert!(url_needs_remote_git("http://example.test/a.git"));
        assert!(url_needs_remote_git("ssh://git@host/repo.git"));
        assert!(url_needs_remote_git("git@host:repo.git"));
    }

    #[test]
    #[cfg(not(feature = "remote-git"))]
    fn https_clone_is_unsupported_without_the_feature() {
        use super::require_remote_git_url;
        use crate::Code;
        let err = require_remote_git_url("https://example.invalid/x.git").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Unsupported));
        assert_ne!(err.to_string(), "unsupported");
    }

    #[test]
    #[cfg(feature = "remote-git")]
    fn libgit2_https_and_ssh_are_linked() {
        let v = git2::Version::get();
        assert!(v.https(), "libgit2 HTTPS missing: {v:?}");
        assert!(v.ssh(), "libgit2 SSH missing: {v:?}");
    }
}
