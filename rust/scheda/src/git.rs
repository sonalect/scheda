use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::error::{Code, ContextAs, ContextIo, bail, git_code, redact_remote_text};
use crate::{Error, OpenOptions, history};

/// Git author and committer for every commit on a Vault handle.
#[derive(Clone, Debug)]
pub(crate) struct CommitIdentity {
    name: String,
    email: String,
}

impl CommitIdentity {
    pub(crate) fn fallback() -> Self {
        Self {
            name: String::from("Scheda"),
            email: String::from("scheda@invalid"),
        }
    }
}

/// Resolve in order: `OpenOptions`, then the repository's local git config,
/// then `Scheda <scheda@invalid>`.
pub(crate) fn identity_from_open(
    options: &OpenOptions,
    repo_dir: &Path,
) -> Result<CommitIdentity, Error> {
    if let Some(identity) = options_identity(options) {
        return Ok(identity);
    }
    config_identity(repo_dir)
}

fn options_identity(options: &OpenOptions) -> Option<CommitIdentity> {
    let author = options.author.as_option()?;
    Some(CommitIdentity {
        name: author.name.as_deref()?.to_owned(),
        email: author.email.as_deref()?.to_owned(),
    })
}

fn config_identity(repo_dir: &Path) -> Result<CommitIdentity, Error> {
    let name = config_get(Some(repo_dir), "user.name")?;
    let email = config_get(Some(repo_dir), "user.email")?;
    match (name, email) {
        (Some(name), Some(email)) if !name.is_empty() && !email.is_empty() => {
            Ok(CommitIdentity { name, email })
        }
        _ => Ok(CommitIdentity::fallback()),
    }
}

struct GitOutput {
    stdout: Vec<u8>,
    stderr: String,
    status: ExitStatus,
}

impl GitOutput {
    fn stdout_str(&self, frame: &str) -> Result<&str, Error> {
        std::str::from_utf8(&self.stdout).context(Code::NotUtf8, frame)
    }

    fn stdout_trim(&self, frame: &str) -> Result<&str, Error> {
        Ok(self.stdout_str(frame)?.trim())
    }
}

struct GitCmd {
    dir: Option<PathBuf>,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
}

fn git() -> GitCmd {
    #[cfg(test)]
    isolate_test_git_env();
    GitCmd {
        dir: None,
        args: Vec::new(),
        envs: Vec::new(),
    }
}

impl GitCmd {
    fn current_dir(mut self, dir: &Path) -> Self {
        self.dir = Some(dir.to_path_buf());
        self
    }

    fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    fn spawn(self, frame: &str) -> Result<GitOutput, Error> {
        let mut cmd = Command::new("git");
        cmd.env("LC_ALL", "C")
            .env("LANG", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Disable interactive add-ons from the user's ~/.gitconfig that can
        // hang a library (watchman fsmonitor, auto-gc, repo hooks). User
        // identity, credential helpers, and the operator's GIT_SSH_COMMAND
        // still apply: Scheda does not set or read an SSH command.
        cmd.arg("-c")
            .arg("core.fsmonitor=")
            .arg("-c")
            .arg("core.useBuiltinFSMonitor=false")
            .arg("-c")
            .arg("core.hooksPath=")
            .arg("-c")
            .arg("gc.auto=0");
        if let Some(dir) = &self.dir {
            cmd.arg("-C").arg(dir);
        }
        cmd.args(&self.args);
        for (key, value) in &self.envs {
            cmd.env(key, value);
        }
        match cmd.output() {
            Ok(out) => Ok(GitOutput {
                stdout: out.stdout,
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                status: out.status,
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(Error::io_not_found(
                "git executable was not found on PATH; install Git and retry",
            )
            .context(frame)),
            Err(err) => Err(err).context(frame),
        }
    }

    fn run(self, frame: &str) -> Result<GitOutput, Error> {
        let out = self.spawn(frame)?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(git_fail(&out, frame))
        }
    }
}

fn git_fail(out: &GitOutput, frame: &str) -> Error {
    let message = if out.stderr.trim().is_empty() {
        match out.status.code() {
            Some(code) => format!("git exited with status {code}"),
            None => String::from("git exited with a failure"),
        }
    } else {
        redact_remote_text(out.stderr.trim())
    };
    Error::new(git_code(out.status.code(), &out.stderr), message).context(frame)
}

/// Point this process at an empty git config.
///
/// Tests call this so the developer's `~/.gitconfig` cannot change
/// outcomes. Production `git()` does not call it. Creates no files:
/// git treats a missing `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` path
/// as an empty config. `HOME` is left alone; isolation does not need
/// a scratch home.
#[cfg(test)]
pub(crate) fn isolate_test_git_env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let absent = std::env::temp_dir().join("scheda-absent-gitconfig");
        // SAFETY: Once serializes callers. Other test threads wait here
        // before they read these keys, so the environment is not torn.
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", &absent);
            std::env::set_var("GIT_CONFIG_SYSTEM", &absent);
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
}

fn config_get(dir: Option<&Path>, key: &str) -> Result<Option<String>, Error> {
    let mut cmd = git().arg("config").arg("--get").arg(key);
    if let Some(dir) = dir {
        cmd = cmd.current_dir(dir);
    }
    let out = cmd.spawn("[Git] read config")?;
    if out.status.success() {
        let value = out.stdout_trim("[Git] read config")?.to_owned();
        Ok((!value.is_empty()).then_some(value))
    } else if out.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(git_fail(&out, "[Git] read config"))
    }
}

/// `core.longpaths` lets Windows stage a 255-byte filename component when the
/// absolute path is longer than MAX_PATH. Unix ignores the key.
fn enable_long_paths(dir: &Path) -> Result<(), Error> {
    match git()
        .current_dir(dir)
        .arg("config")
        .arg("--local")
        .arg("--get")
        .arg("--type=bool")
        .arg("core.longpaths")
        .spawn("[Git] open config")?
    {
        out if out.status.success() && out.stdout_trim("[Git] open config")? == "true" => Ok(()),
        out if out.status.success() || out.status.code() == Some(1) => git()
            .current_dir(dir)
            .arg("config")
            .arg("--local")
            .arg("core.longpaths")
            .arg("true")
            .run("[Git] set core.longpaths")
            .map(|_| ()),
        out => Err(git_fail(&out, "[Git] open config")),
    }
}

fn require_work_tree(dir: &Path) -> Result<(), Error> {
    let inside = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .run("[Git::open_work_tree] open repository")?;
    if inside.stdout_trim("[Git::open_work_tree] open repository")? != "true" {
        bail!(Error::invalid(format!(
            "path is a bare git repository, not a working tree: {}",
            dir.display()
        )));
    }
    let top = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .run("[Git::open_work_tree] open repository")?;
    let toplevel = top.stdout_trim("[Git::open_work_tree] open repository")?;
    if !paths_equal(dir, toplevel) {
        bail!(Error::git_not_found(format!(
            "path is not a git working tree: {}",
            dir.display()
        )));
    }
    Ok(())
}

fn paths_equal(dir: &Path, toplevel: &str) -> bool {
    match (dir.canonicalize(), Path::new(toplevel).canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn is_unborn_head(dir: &Path) -> Result<bool, Error> {
    let out = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .spawn("[Git] read HEAD")?;
    if out.status.success() {
        return Ok(false);
    }
    if git_code(out.status.code(), &out.stderr) == Code::GitUnbornBranch {
        return Ok(true);
    }
    Err(git_fail(&out, "[Git] read HEAD"))
}

pub(crate) fn init(dir: &Path) -> Result<(), Error> {
    if is_work_tree(dir) {
        bail!(
            Error::git_exists(format!(
                "git repository already exists at {}",
                dir.display()
            ))
            .context("[Git::init] create repository")
        );
    }
    git()
        .arg("init")
        .arg("--")
        .arg(dir)
        .run("[Git::init] create repository")?;
    enable_long_paths(dir)
}

pub(crate) fn is_work_tree(dir: &Path) -> bool {
    if !dir.exists() {
        return false;
    }
    let Ok(inside) = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .spawn("[Git::is_work_tree] open repository")
    else {
        return false;
    };
    if !inside.status.success() || inside.stdout_trim("").ok() != Some("true") {
        return false;
    }
    let Ok(top) = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .spawn("[Git::is_work_tree] open repository")
    else {
        return false;
    };
    top.status.success()
        && top
            .stdout_trim("")
            .is_ok_and(|toplevel| paths_equal(dir, toplevel))
}

pub(crate) fn remotes(dir: &Path) -> Result<Vec<String>, Error> {
    require_work_tree(dir)?;
    let out = git()
        .current_dir(dir)
        .arg("remote")
        .run("[Git::remotes] list remotes")?;
    let text = out.stdout_str("[Git::remotes] remote name")?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Fetch URL of `origin`, if that remote exists. Local config; not a fetch.
///
/// Missing `origin`, or an empty URL, is `None`.
pub(crate) fn origin_url(dir: &Path) -> Result<Option<String>, Error> {
    require_work_tree(dir)?;
    let out = git()
        .current_dir(dir)
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .spawn("[Git::origin_url] find origin")?;
    if out.status.success() {
        let url = out.stdout_trim("[Git::origin_url] read origin URL")?;
        Ok((!url.is_empty()).then(|| url.to_owned()))
    } else if git_code(out.status.code(), &out.stderr) == Code::GitNotFound
        || out.stderr.to_ascii_lowercase().contains("no such remote")
    {
        Ok(None)
    } else {
        Err(git_fail(&out, "[Git::origin_url] find origin"))
    }
}

pub(crate) fn clone(url: &str, path: &Path) -> Result<(), Error> {
    git()
        .arg("clone")
        .arg("--")
        .arg(url)
        .arg(path)
        .run("[Git::clone] clone repository")?;
    enable_long_paths(path)
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

fn branch_refname(dir: &Path) -> Result<String, Error> {
    let out = git()
        .current_dir(dir)
        .arg("symbolic-ref")
        .arg("HEAD")
        .spawn("[Git::branch_refname] read HEAD")?;
    if out.status.success() {
        let name = out.stdout_trim("[Git::branch_refname] read branch name")?;
        if name.is_empty() {
            return Err(Error::git_head("unborn HEAD has no branch name")
                .context("[Git::branch_refname] read HEAD target"));
        }
        return Ok(name.to_owned());
    }
    let lower = out.stderr.to_ascii_lowercase();
    if lower.contains("not a symbolic ref") || lower.contains("is not a symbolic ref") {
        return Err(
            Error::git_head("HEAD is not a branch").context("[Git::branch_refname] require branch")
        );
    }
    Err(git_fail(&out, "[Git::branch_refname] read HEAD"))
}

fn worktree_has_local_changes(dir: &Path) -> Result<bool, Error> {
    let out = git()
        .current_dir(dir)
        .arg("status")
        .arg("--porcelain=v1")
        .arg("--untracked-files=normal")
        .arg("--ignore-submodules=all")
        .run("[Git::fast_forward] status")?;
    Ok(out
        .stdout_str("[Git::fast_forward] status")?
        .lines()
        .any(|line| !line.trim().is_empty()))
}

fn fast_forward(dir: &Path) -> Result<(), Error> {
    if worktree_has_local_changes(dir)? {
        bail!(
            Error::git_uncommitted("working tree has local changes; pull needs a clean tree")
                .context("[Git::fast_forward] require clean tree")
        );
    }
    git()
        .current_dir(dir)
        .arg("merge")
        .arg("--ff-only")
        .arg("FETCH_HEAD")
        .run("[Git::fast_forward] update branch")?;
    Ok(())
}

pub(crate) fn pull(dir: &Path) -> Result<(), Error> {
    let name = default_remote_name(dir)?;
    require_work_tree(dir)?;
    git()
        .current_dir(dir)
        .arg("fetch")
        .arg("--")
        .arg(&name)
        .run("[Git::pull] fetch")?;
    git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--verify")
        .arg("FETCH_HEAD")
        .run("[Git::pull] read FETCH_HEAD")?;
    if is_unborn_head(dir)? {
        return fast_forward(dir);
    }
    let ancestor = git()
        .current_dir(dir)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg("FETCH_HEAD")
        .arg("HEAD")
        .spawn("[Git::pull] merge analysis")?;
    if ancestor.status.success() {
        return Ok(());
    }
    let can_ff = git()
        .current_dir(dir)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg("HEAD")
        .arg("FETCH_HEAD")
        .spawn("[Git::pull] merge analysis")?;
    if can_ff.status.success() {
        return fast_forward(dir);
    }
    Err(Error::not_fast_forward(
        "Remote is not a fast-forward of HEAD",
    ))
}

pub(crate) fn push(dir: &Path) -> Result<(), Error> {
    let name = default_remote_name(dir)?;
    require_work_tree(dir)?;
    let refname = branch_refname(dir)?;
    let spec = format!("{refname}:{refname}");
    git()
        .current_dir(dir)
        .arg("push")
        .arg("--")
        .arg(&name)
        .arg(&spec)
        .run("[Git::push] push")?;
    Ok(())
}

/// True when the git index tracks `prefix` or any path under it.
pub(crate) fn index_has_prefix(dir: &Path, prefix: &str) -> Result<bool, Error> {
    require_work_tree(dir)?;
    let out = git()
        .current_dir(dir)
        .arg("ls-files")
        .arg("-z")
        .arg("--cached")
        .run("[Git::index_has_prefix] open index")?;
    let prefix = prefix.trim_end_matches('/');
    let nested = format!("{prefix}/");
    for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path =
            std::str::from_utf8(path).context(Code::NotUtf8, "[Git::index_has_prefix] path")?;
        if path == prefix || path.starts_with(&nested) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn commit(
    dir: &Path,
    message: &str,
    paths: &[String],
    identity: &CommitIdentity,
) -> Result<(), Error> {
    if message.trim().is_empty() {
        bail!(Error::invalid("commit message is empty"));
    }
    if paths.is_empty() {
        bail!(Error::invalid("nothing to commit"));
    }
    require_work_tree(dir)?;
    enable_long_paths(dir)?;
    let mut staged = 0usize;
    for rel in paths {
        let abs = dir.join(rel);
        if abs.is_file() {
            git()
                .current_dir(dir)
                .arg("add")
                .arg("--")
                .arg(rel)
                .run(&format!("[Git::commit] stage {rel}"))?;
            staged += 1;
        } else {
            let tracked = git()
                .current_dir(dir)
                .arg("ls-files")
                .arg("--error-unmatch")
                .arg("--")
                .arg(rel)
                .spawn(&format!("[Git::commit] remove {rel}"))?;
            if tracked.status.success() {
                git()
                    .current_dir(dir)
                    .arg("rm")
                    .arg("--cached")
                    .arg("--")
                    .arg(rel)
                    .run(&format!("[Git::commit] remove {rel}"))?;
                staged += 1;
            } else if git_code(tracked.status.code(), &tracked.stderr) == Code::GitNotFound
                || tracked
                    .stderr
                    .to_ascii_lowercase()
                    .contains("did not match")
            {
            } else {
                return Err(git_fail(&tracked, &format!("[Git::commit] remove {rel}")));
            }
        }
    }
    if staged == 0 {
        bail!(Error::invalid("nothing to commit"));
    }
    let tree = git()
        .current_dir(dir)
        .arg("write-tree")
        .run("[Git::commit] write tree")?;
    let tree_id = tree.stdout_trim("[Git::commit] write tree")?;
    if !is_unborn_head(dir)? {
        let head_tree = git()
            .current_dir(dir)
            .arg("rev-parse")
            .arg("HEAD^{tree}")
            .run("[Git::commit] peel HEAD")?;
        if head_tree.stdout_trim("[Git::commit] peel HEAD")? == tree_id {
            return Ok(());
        }
    }
    git()
        .current_dir(dir)
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("commit")
        .arg("--no-edit")
        .arg("-m")
        .arg(message)
        .env("GIT_AUTHOR_NAME", &identity.name)
        .env("GIT_AUTHOR_EMAIL", &identity.email)
        .env("GIT_COMMITTER_NAME", &identity.name)
        .env("GIT_COMMITTER_EMAIL", &identity.email)
        .run("[Git::commit] create commit")?;
    Ok(())
}

/// Current `HEAD` commit, or `None` when the branch is unborn.
pub(crate) fn peel_head_oid(dir: &Path) -> Result<Option<String>, Error> {
    require_work_tree(dir)?;
    let out = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .spawn("[Git] read HEAD")?;
    if out.status.success() {
        Ok(Some(out.stdout_trim("[Git] peel HEAD")?.to_owned()))
    } else if git_code(out.status.code(), &out.stderr) == Code::GitUnbornBranch {
        Ok(None)
    } else {
        Err(git_fail(&out, "[Git] read HEAD"))
    }
}

/// Move `HEAD` (and the work tree) to `oid`. `None` returns the repo to
/// an unborn branch after a first commit that must be undone.
pub(crate) fn reset_head_to(dir: &Path, oid: Option<&str>) -> Result<(), Error> {
    require_work_tree(dir)?;
    enable_long_paths(dir)?;
    match oid {
        Some(oid) => {
            git()
                .current_dir(dir)
                .arg("reset")
                .arg("--hard")
                .arg(oid)
                .run("[Git] reset HEAD")?;
        }
        None => {
            if is_unborn_head(dir)? {
                return Ok(());
            }
            let name = branch_refname(dir)?;
            git()
                .current_dir(dir)
                .arg("update-ref")
                .arg("-d")
                .arg(&name)
                .run("[Git] delete branch to unborn")?;
        }
    }
    Ok(())
}

/// Restore Scheda-recorded paths to HEAD. New files (not in HEAD) are deleted.
/// Does not `git reset --hard` the whole tree.
pub(crate) fn discard_paths(dir: &Path, paths: &[String]) -> Result<(), Error> {
    require_work_tree(dir)?;
    enable_long_paths(dir)?;
    let unborn = is_unborn_head(dir)?;
    for rel in paths {
        let in_head = if unborn {
            false
        } else {
            git()
                .current_dir(dir)
                .arg("cat-file")
                .arg("-e")
                .arg(format!("HEAD:{rel}"))
                .spawn("[Git::discard] peel HEAD tree")?
                .status
                .success()
        };
        if in_head {
            git()
                .current_dir(dir)
                .arg("checkout")
                .arg("-f")
                .arg("HEAD")
                .arg("--")
                .arg(rel)
                .run(&format!("[Git::discard] restore {rel}"))?;
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

/// True when any commit reachable from HEAD stored `path` in its tree.
pub(crate) fn path_has_history(dir: &Path, path: &str) -> Result<bool, Error> {
    require_vault_rel(path)?;
    require_work_tree(dir)?;
    if is_unborn_head(dir)? {
        return Ok(false);
    }
    let out = git()
        .current_dir(dir)
        .arg("log")
        .arg("--format=%H")
        .arg("--")
        .arg(path)
        .run("[Git::path_has_history] walk from HEAD")?;
    for hash in out
        .stdout_str("[Git::path_has_history] next commit")?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let probe = git()
            .current_dir(dir)
            .arg("cat-file")
            .arg("-e")
            .arg(format!("{hash}:{path}"))
            .spawn("[Git::path_has_history] read tree")?;
        if probe.status.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn history(
    dir: &Path,
    path: &str,
    offset: u32,
    count: u32,
) -> Result<Vec<history::Entry>, Error> {
    require_vault_rel(path)?;
    require_work_tree(dir)?;
    if is_unborn_head(dir)? {
        return Ok(Vec::new());
    }
    let out = git()
        .current_dir(dir)
        .arg("log")
        .arg("--date-order")
        .arg("-z")
        .arg("--format=%H%x1f%at%x1f%an%x1f%ae%x1f%B")
        .arg("--")
        .arg(path)
        .run("[Git::history] walk from HEAD")?;
    struct Hit {
        hash: String,
        seconds: i64,
        author: String,
        message: String,
    }
    let mut hits = Vec::new();
    for rec in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rec = std::str::from_utf8(rec).context(Code::NotUtf8, "[Git::history] next commit")?;
        let mut fields = rec.splitn(5, '\x1f');
        let Some(hash) = fields.next() else {
            continue;
        };
        let Some(at) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(email) = fields.next() else {
            continue;
        };
        let message = fields.next().unwrap_or("").trim_end_matches('\n');
        let in_tree = git()
            .current_dir(dir)
            .arg("cat-file")
            .arg("-e")
            .arg(format!("{hash}:{path}"))
            .spawn("[Git::history] read tree")?;
        if !in_tree.status.success() {
            continue;
        }
        let seconds = at.parse::<i64>().map_err(|_| {
            Error::git_generic(format!("commit time is not an integer: {at}"))
                .context("[Git::history] read commit")
        })?;
        let author = if email.is_empty() {
            name.to_owned()
        } else {
            format!("{name} <{email}>")
        };
        hits.push(Hit {
            hash: hash.to_owned(),
            seconds,
            author,
            message: message.to_owned(),
        });
    }
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.seconds));
    let mut out = Vec::new();
    for hit in hits.into_iter().skip(offset as usize).take(count as usize) {
        let blob = git()
            .current_dir(dir)
            .arg("cat-file")
            .arg("blob")
            .arg(format!("{}:{path}", hit.hash))
            .run("[Git::history] read blob")?;
        out.push(
            history::Entry::default()
                .with_hash(hit.hash)
                .with_time(hit.seconds)
                .with_author(hit.author)
                .with_message(hit.message)
                .with_raw(blob.stdout),
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

#[cfg(test)]
pub(crate) fn add_remote(dir: &Path, name: &str, url: &str) -> Result<(), Error> {
    require_work_tree(dir)?;
    git()
        .current_dir(dir)
        .arg("remote")
        .arg("add")
        .arg(name)
        .arg(url)
        .run("[Git::add_remote] add remote")?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn is_ignored(dir: &Path, rel: &Path) -> Result<bool, Error> {
    require_work_tree(dir)?;
    let out = git()
        .current_dir(dir)
        .arg("check-ignore")
        .arg("-q")
        .arg("--")
        .arg(rel)
        .spawn("[Git::is_ignored] check ignore")?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_fail(&out, "[Git::is_ignored] check ignore")),
    }
}

#[cfg(test)]
pub(crate) fn local_config_mentions_lfs(dir: &Path) -> Result<bool, Error> {
    require_work_tree(dir)?;
    for key in [
        "filter.lfs.process",
        "filter.lfs.required",
        "filter.lfs.clean",
        "filter.lfs.smudge",
    ] {
        let out = git()
            .current_dir(dir)
            .arg("config")
            .arg("--local")
            .arg("--get")
            .arg(key)
            .spawn("[Git::local_config_mentions_lfs] read config entry")?;
        if out.status.success() {
            return Ok(true);
        }
        if out.status.code() != Some(1) {
            return Err(git_fail(
                &out,
                "[Git::local_config_mentions_lfs] read config entry",
            ));
        }
    }
    Ok(false)
}

#[cfg(test)]
mod transport_tests {
    use crate::Error;
    use crate::error::{git_code, redact_remote_text};

    #[test]
    fn git_fail_redacts_userinfo_in_message() {
        let stderr =
            "could not find repository from 'https://user:token@example.test/org/repo.git'";
        let err = Error::new(git_code(Some(128), stderr), redact_remote_text(stderr))
            .context("[Git::clone] clone repository");
        let msg = err.message.as_deref().unwrap_or("");
        assert!(!msg.contains("token"), "{msg}");
        assert!(!msg.contains("user:"), "{msg}");
        assert!(msg.contains("https://example.test"), "{msg}");
    }

    #[test]
    fn isolate_does_not_set_an_ssh_command() {
        super::isolate_test_git_env();
        match std::env::var_os("GIT_SSH_COMMAND") {
            None => {}
            Some(val) => {
                assert!(
                    !val.to_string_lossy().contains("BatchMode"),
                    "Scheda must not set GIT_SSH_COMMAND: {val:?}"
                );
            }
        }
        assert!(std::env::var_os("GIT_CONFIG_NOSYSTEM").is_some());
        let global = std::env::var_os("GIT_CONFIG_GLOBAL").expect("GIT_CONFIG_GLOBAL");
        assert!(
            !std::path::Path::new(&global).exists(),
            "isolation must not create {global:?}"
        );
    }
}
