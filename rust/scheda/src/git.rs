use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::error::{Code, ContextAs, ContextIo, bail, git_code, io_code, redact_remote_text};
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
    /// `self` when git exited 0, otherwise the failure as an `Error`.
    fn into_result(self, frame: &str) -> Result<Self, Error> {
        if self.status.success() {
            Ok(self)
        } else {
            Err(git_fail(&self, frame))
        }
    }

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

    fn command(&self) -> Command {
        #[cfg(test)]
        SPAWNS.with(|count| count.set(count.get() + 1));
        let mut cmd = Command::new("git");
        cmd.env("LC_ALL", "C")
            .env("LANG", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // `core.longpaths` lets Windows stage a 255-byte filename component
        // when the absolute path is longer than MAX_PATH; Unix ignores it.
        // Passing it per command keeps an older tree that lacks the config
        // key working without a config write before every commit.
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
            .arg("gc.auto=0")
            .arg("-c")
            .arg("core.longpaths=true");
        if let Some(dir) = &self.dir {
            cmd.arg("-C").arg(dir);
        }
        cmd.args(&self.args);
        for (key, value) in &self.envs {
            cmd.env(key, value);
        }
        cmd
    }

    fn spawn(self, frame: &str) -> Result<GitOutput, Error> {
        match self.command().output() {
            Ok(out) => Ok(GitOutput::from(out)),
            Err(err) => Err(spawn_error(err, frame)),
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

    /// Like [`spawn`](GitCmd::spawn), with `input` on stdin. A second thread
    /// writes stdin while this one drains stdout, so neither pipe can fill
    /// and stall the other. The child exits when stdin closes.
    fn spawn_with_input(self, input: &[u8], frame: &str) -> Result<GitOutput, Error> {
        let mut cmd = self.command();
        cmd.stdin(Stdio::piped());
        let mut child = cmd.spawn().map_err(|err| spawn_error(err, frame))?;
        let stdin = child.stdin.take();
        let out = std::thread::scope(|scope| {
            if let Some(mut stdin) = stdin {
                scope.spawn(move || {
                    // A child that exits early closes the pipe; its exit
                    // status carries the failure, not this write.
                    let _ = io::Write::write_all(&mut stdin, input);
                });
            }
            child.wait_with_output()
        });
        match out {
            Ok(out) => Ok(GitOutput::from(out)),
            Err(err) => Err(err).context(frame),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Git processes started by this thread; tests read it to bound the
    /// process count of one operation.
    static SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl From<std::process::Output> for GitOutput {
    fn from(out: std::process::Output) -> Self {
        Self {
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            status: out.status,
        }
    }
}

fn spawn_error(err: io::Error, frame: &str) -> Error {
    if err.kind() == io::ErrorKind::NotFound {
        Error::io_not_found("git executable was not found on PATH; install Git and retry")
            .context(frame)
    } else {
        Error::new(io_code(&err), err.to_string()).context(frame)
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
    Error::new(git_code(&out.stderr), message).context(frame)
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
    if git_code(&out.stderr) == Code::GitUnbornBranch {
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
    } else if git_code(&out.stderr) == Code::GitNotFound
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

fn fast_forward(dir: &Path, oid: &str) -> Result<(), Error> {
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
        .arg(oid)
        .run("[Git::fast_forward] update branch")?;
    Ok(())
}

/// Fetch the current branch from the default Remote and fast-forward to it.
///
/// Only that branch is fetched, by name, so the result does not depend on
/// `branch.<name>.merge` or on which branch of the Remote sorts first. A
/// branch that has no commit yet takes the Remote's `HEAD`.
pub(crate) fn pull(dir: &Path) -> Result<(), Error> {
    let name = default_remote_name(dir)?;
    let unborn = is_unborn_head(dir)?;
    let refname = branch_refname(dir)?;
    let source = if unborn { "HEAD" } else { refname.as_str() };
    git()
        .current_dir(dir)
        .arg("fetch")
        .arg("--")
        .arg(&name)
        .arg(source)
        .run("[Git::pull] fetch")?;
    let fetched = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--verify")
        .arg("FETCH_HEAD^{commit}")
        .run("[Git::pull] read FETCH_HEAD")?;
    let oid = fetched.stdout_trim("[Git::pull] read FETCH_HEAD")?;
    if unborn {
        return fast_forward(dir, oid);
    }
    let ancestor = git()
        .current_dir(dir)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(oid)
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
        .arg(oid)
        .spawn("[Git::pull] merge analysis")?;
    if can_ff.status.success() {
        return fast_forward(dir, oid);
    }
    Err(Error::not_fast_forward(
        "Remote is not a fast-forward of HEAD",
    ))
}

pub(crate) fn push(dir: &Path) -> Result<(), Error> {
    let name = default_remote_name(dir)?;
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
/// Whether git tracks `path` (repository-relative) in its index.
pub(crate) fn is_tracked(dir: &Path, path: &str) -> Result<bool, Error> {
    const FRAME: &str = "[Git::is_tracked] ls-files";
    let out = git()
        .current_dir(dir)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg("--")
        .arg(path)
        .spawn(FRAME)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_fail(&out, FRAME)),
    }
}

pub(crate) fn index_has_prefix(dir: &Path, prefix: &str) -> Result<bool, Error> {
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

/// Commit the work-tree state of `paths` (Vault-relative) on the current branch.
///
/// A path that is a file is staged, a path that is gone is removed from the
/// index when it was tracked, and any other path is skipped. All of them go
/// through one `git update-index`, so the process count does not grow with
/// the number of paths. Nothing staged is `invalid`; a staged tree equal to
/// `HEAD`'s is a no-op.
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
    let any_file = paths.iter().any(|rel| dir.join(rel).is_file());
    if !any_file && !any_tracked(dir, paths)? {
        bail!(Error::invalid("nothing to commit"));
    }
    let mut input = Vec::new();
    for rel in paths {
        input.extend_from_slice(rel.as_bytes());
        input.push(0);
    }
    git()
        .current_dir(dir)
        .arg("update-index")
        .arg("--add")
        .arg("--remove")
        .arg("--replace")
        .arg("-z")
        .arg("--stdin")
        .spawn_with_input(&input, "[Git::commit] stage paths")?
        .into_result("[Git::commit] stage paths")?;
    let tree = git()
        .current_dir(dir)
        .arg("write-tree")
        .run("[Git::commit] write tree")?;
    let tree_id = tree.stdout_trim("[Git::commit] write tree")?;
    if head_tree_oid(dir)?.as_deref() == Some(tree_id) {
        return Ok(());
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

/// Whether the index tracks at least one of `paths`, each taken literally.
///
/// The paths go on the command line, in batches that stay under the
/// smallest argument limit of a supported platform (Windows, 32 KiB).
fn any_tracked(dir: &Path, paths: &[String]) -> Result<bool, Error> {
    const FRAME: &str = "[Git::commit] list tracked paths";
    const BATCH_BYTES: usize = 16 * 1024;
    let mut rest = paths;
    while !rest.is_empty() {
        let mut bytes = 0usize;
        let mut take = 0usize;
        for rel in rest {
            if take > 0 && bytes + rel.len() > BATCH_BYTES {
                break;
            }
            bytes += rel.len() + 1;
            take += 1;
        }
        let (batch, after) = rest.split_at(take);
        rest = after;
        let mut cmd = git()
            .current_dir(dir)
            .env("GIT_LITERAL_PATHSPECS", "1")
            .arg("ls-files")
            .arg("-z")
            .arg("--cached")
            .arg("--");
        for rel in batch {
            cmd = cmd.arg(rel);
        }
        let out = cmd.run(FRAME)?;
        // A directory pathspec lists the files below it; only a listed path
        // that is one of `batch` itself counts.
        if out
            .stdout
            .split(|b| *b == 0)
            .any(|listed| batch.iter().any(|rel| rel.as_bytes() == listed))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Tree id of `HEAD`, or `None` when the branch is unborn.
fn head_tree_oid(dir: &Path) -> Result<Option<String>, Error> {
    const FRAME: &str = "[Git] peel HEAD tree";
    let out = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD^{tree}")
        .spawn(FRAME)?;
    if out.status.success() {
        Ok(Some(out.stdout_trim(FRAME)?.to_owned()))
    } else if git_code(&out.stderr) == Code::GitUnbornBranch {
        Ok(None)
    } else {
        Err(git_fail(&out, FRAME))
    }
}

/// Current `HEAD` commit, or `None` when the branch is unborn.
pub(crate) fn peel_head_oid(dir: &Path) -> Result<Option<String>, Error> {
    let out = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .spawn("[Git] read HEAD")?;
    if out.status.success() {
        Ok(Some(out.stdout_trim("[Git] peel HEAD")?.to_owned()))
    } else if git_code(&out.stderr) == Code::GitUnbornBranch {
        Ok(None)
    } else {
        Err(git_fail(&out, "[Git] read HEAD"))
    }
}

/// Move `HEAD` (and the work tree) to `oid`. `None` returns the repo to
/// an unborn branch after a first commit that must be undone.
pub(crate) fn reset_head_to(dir: &Path, oid: Option<&str>) -> Result<(), Error> {
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
    if is_unborn_head(dir)? {
        return Ok(false);
    }
    let stored = commits_storing(dir, path, "%H", "[Git::path_has_history] walk from HEAD")?;
    Ok(!stored.is_empty())
}

/// Commits reachable from HEAD that changed `path` and whose tree stores it,
/// newest first in git's `--date-order`, as `git log` records of `format`.
///
/// `format` must start with `%H`; a record's fields are split by `\x1f`.
/// Two git processes in all: the log, then one `cat-file --batch-check` that
/// drops the commits that deleted the path.
fn commits_storing(
    dir: &Path,
    path: &str,
    format: &str,
    frame: &str,
) -> Result<Vec<String>, Error> {
    let out = git()
        .current_dir(dir)
        .arg("log")
        .arg("--date-order")
        .arg("-z")
        .arg(format!("--format={format}"))
        .arg("--")
        .arg(path)
        .run(frame)?;
    let mut records = Vec::new();
    for rec in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rec = std::str::from_utf8(rec).context(Code::NotUtf8, frame)?;
        records.push(rec.to_owned());
    }
    let specs: Vec<String> = records
        .iter()
        .map(|rec| {
            let hash = rec.split('\x1f').next().unwrap_or(rec);
            format!("{hash}:{path}")
        })
        .collect();
    let stored = objects_exist(dir, &specs)?;
    Ok(records
        .into_iter()
        .zip(stored)
        .filter_map(|(rec, stored)| stored.then_some(rec))
        .collect())
}

/// History of `path`: the commits whose tree stores it, newest first by
/// author time, `count` of them from `offset`, each with the file bytes at
/// that commit. Only the requested page is read from the object store.
pub(crate) fn history(
    dir: &Path,
    path: &str,
    offset: u32,
    count: u32,
) -> Result<Vec<history::Entry>, Error> {
    const FRAME: &str = "[Git::history] walk from HEAD";
    require_vault_rel(path)?;
    if is_unborn_head(dir)? {
        return Ok(Vec::new());
    }
    struct Hit<'a> {
        hash: &'a str,
        seconds: i64,
        author: String,
        message: &'a str,
    }
    let records = commits_storing(dir, path, "%H%x1f%at%x1f%an%x1f%ae%x1f%B", FRAME)?;
    let mut hits = Vec::with_capacity(records.len());
    for rec in &records {
        let mut fields = rec.splitn(5, '\x1f');
        let (Some(hash), Some(at), Some(name), Some(email)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let message = fields.next().unwrap_or("").trim_end_matches('\n');
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
            hash,
            seconds,
            author,
            message,
        });
    }
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.seconds));
    let page: Vec<Hit> = hits
        .into_iter()
        .skip(offset as usize)
        .take(count as usize)
        .collect();
    let specs: Vec<String> = page
        .iter()
        .map(|hit| format!("{}:{path}", hit.hash))
        .collect();
    let blobs = read_blobs(dir, &specs)?;
    let mut entries = Vec::with_capacity(page.len());
    for (hit, blob) in page.into_iter().zip(blobs) {
        let Some(raw) = blob else {
            bail!(
                Error::git_generic(format!("{path} is not a file in commit {}", hit.hash))
                    .context("[Git::history] read blob")
            );
        };
        entries.push(
            history::Entry::default()
                .with_hash(hit.hash.to_owned())
                .with_time(hit.seconds)
                .with_author(hit.author)
                .with_message(hit.message.to_owned())
                .with_raw(raw),
        );
    }
    Ok(entries)
}

/// Bytes of each object named by `specs` (a blob id, or `<commit>:<path>`).
///
/// `None` for a spec that names nothing, or names something other than a
/// blob. One `git cat-file --batch` per call; the process ends with the
/// call. A spec must not contain a newline.
pub(crate) fn read_blobs(dir: &Path, specs: &[String]) -> Result<Vec<Option<Vec<u8>>>, Error> {
    const FRAME: &str = "[Git::read_blobs] read objects";
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let input = spec_lines(specs, FRAME)?;
    let out = git()
        .current_dir(dir)
        .arg("cat-file")
        .arg("--batch")
        .spawn_with_input(&input, FRAME)?
        .into_result(FRAME)?;
    let mut rest = out.stdout.as_slice();
    let mut blobs = Vec::with_capacity(specs.len());
    for _ in specs {
        let Some(end) = rest.iter().position(|&b| b == b'\n') else {
            bail!(Error::git_generic("git cat-file output is truncated").context(FRAME));
        };
        let header = std::str::from_utf8(&rest[..end]).context(Code::NotUtf8, FRAME)?;
        rest = &rest[end + 1..];
        let mut words = header.rsplitn(3, ' ');
        let (Some(last), Some(kind)) = (words.next(), words.next()) else {
            bail!(
                Error::git_generic(format!("unexpected git cat-file header: {header}"))
                    .context(FRAME)
            );
        };
        let Ok(size) = last.parse::<usize>() else {
            // `<spec> missing` or `<spec> ambiguous`: no body follows.
            blobs.push(None);
            continue;
        };
        if rest.len() < size + 1 {
            bail!(Error::git_generic("git cat-file output is truncated").context(FRAME));
        }
        let body = &rest[..size];
        rest = &rest[size + 1..];
        blobs.push((kind == "blob").then(|| body.to_vec()));
    }
    Ok(blobs)
}

/// Whether each of `specs` (`<commit>:<path>`) names an object of any type.
/// One `git cat-file --batch-check`; a spec must not contain a newline.
fn objects_exist(dir: &Path, specs: &[String]) -> Result<Vec<bool>, Error> {
    const FRAME: &str = "[Git::objects_exist] check objects";
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let input = spec_lines(specs, FRAME)?;
    let out = git()
        .current_dir(dir)
        .arg("cat-file")
        .arg("--batch-check")
        .spawn_with_input(&input, FRAME)?
        .into_result(FRAME)?;
    let text = out.stdout_str(FRAME)?;
    let mut lines = text.lines();
    let mut found = Vec::with_capacity(specs.len());
    for _ in specs {
        let Some(line) = lines.next() else {
            bail!(Error::git_generic("git cat-file output is truncated").context(FRAME));
        };
        // `<spec> missing` or `<spec> ambiguous`; a hit is `<oid> <type> <size>`.
        found.push(!line.ends_with(" missing") && !line.ends_with(" ambiguous"));
    }
    Ok(found)
}

/// `specs` as newline-terminated `cat-file --batch` input.
fn spec_lines(specs: &[String], frame: &str) -> Result<Vec<u8>, Error> {
    let mut input = Vec::new();
    for spec in specs {
        if spec.contains('\n') {
            bail!(Error::invalid("object spec contains a newline").context(frame));
        }
        input.extend_from_slice(spec.as_bytes());
        input.push(b'\n');
    }
    Ok(input)
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

/// Reading trees and blobs at a commit, for indexing from `HEAD` diffs.
#[cfg(feature = "find")]
pub(crate) mod tree {
    use std::path::Path;

    pub(crate) use super::read_blobs;
    use super::{git, git_fail};
    use crate::Error;
    use crate::error::{Code, ContextAs, bail};

    /// One path that differs between two trees.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct TreeChange {
        /// Repository-relative POSIX path.
        pub(crate) path: String,
        /// Blob id on the old side. `None` when the path was absent there or
        /// was not a regular file (a symlink, a submodule).
        pub(crate) old: Option<String>,
        /// Blob id on the new side, with the same `None` rule.
        pub(crate) new: Option<String>,
    }

    /// Id of the empty tree in this repository's object format.
    ///
    /// Diffing from it lists every file of the other side. Git knows this
    /// tree without it being stored.
    pub(crate) fn empty_tree_oid(dir: &Path) -> Result<String, Error> {
        let out = git()
            .current_dir(dir)
            .arg("hash-object")
            .arg("-t")
            .arg("tree")
            .arg("--stdin")
            .run("[Git::empty_tree_oid] hash empty tree")?;
        Ok(out
            .stdout_trim("[Git::empty_tree_oid] read empty tree id")?
            .to_owned())
    }

    /// Whether `oid` names a commit this repository still holds.
    ///
    /// `false` after `gc` pruned it, or for an id that was never a commit
    /// here. Any other git failure is an error.
    pub(crate) fn has_commit(dir: &Path, oid: &str) -> Result<bool, Error> {
        let out = git()
            .current_dir(dir)
            .arg("cat-file")
            .arg("-e")
            .arg(format!("{oid}^{{commit}}"))
            .spawn("[Git::has_commit] look up commit")?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1 | 128) if !out.stderr.contains("not a git repository") => Ok(false),
            _ => Err(git_fail(&out, "[Git::has_commit] look up commit")),
        }
    }

    /// Paths that differ between tree-ish `from` and `to`, recursively.
    ///
    /// Renames are not detected: a moved file is a deletion of the old path
    /// plus an addition of the new one. A path whose type changes between a
    /// regular file and anything else shows `None` on the non-file side.
    /// A non-empty `under` limits the diff to those literal path prefixes.
    pub(crate) fn diff_trees(
        dir: &Path,
        from: &str,
        to: &str,
        under: &[&str],
    ) -> Result<Vec<TreeChange>, Error> {
        const FRAME: &str = "[Git::diff_trees] diff trees";
        let mut cmd = git()
            .current_dir(dir)
            .env("GIT_LITERAL_PATHSPECS", "1")
            .arg("diff-tree")
            .arg("-r")
            .arg("-z")
            .arg("--raw")
            .arg("--no-renames")
            .arg("--no-abbrev")
            .arg(from)
            .arg(to);
        if !under.is_empty() {
            cmd = cmd.arg("--");
            for path in under {
                cmd = cmd.arg(path);
            }
        }
        let out = cmd.run(FRAME)?;
        let mut fields = out.stdout.split(|&b| b == 0);
        let mut changes = Vec::new();
        while let Some(meta) = fields.next() {
            if meta.is_empty() {
                continue;
            }
            let Some(path) = fields.next() else {
                bail!(
                    Error::git_generic("git diff-tree output ends without a path").context(FRAME)
                );
            };
            let meta = std::str::from_utf8(meta).context(Code::NotUtf8, FRAME)?;
            let Ok(path) = std::str::from_utf8(path) else {
                // Not a name the library writes or lists; it cannot be a Record.
                continue;
            };
            let mut parts = meta.trim_start_matches(':').split(' ');
            let (Some(old_mode), Some(new_mode), Some(old_oid), Some(new_oid)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                bail!(
                    Error::git_generic(format!("unexpected git diff-tree line: {meta}"))
                        .context(FRAME)
                );
            };
            changes.push(TreeChange {
                path: path.to_owned(),
                old: regular_blob(old_mode, old_oid),
                new: regular_blob(new_mode, new_oid),
            });
        }
        Ok(changes)
    }

    /// Whether commit `ancestor` is `descendant` or one of its ancestors.
    /// `false` when either object is missing.
    pub(crate) fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> Result<bool, Error> {
        const FRAME: &str = "[Git::is_ancestor] merge-base";
        let out = git()
            .current_dir(dir)
            .arg("merge-base")
            .arg("--is-ancestor")
            .arg(ancestor)
            .arg(descendant)
            .spawn(FRAME)?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1 | 128) => Ok(false),
            _ => Err(git_fail(&out, FRAME)),
        }
    }

    fn regular_blob(mode: &str, oid: &str) -> Option<String> {
        matches!(mode, "100644" | "100755").then(|| oid.to_owned())
    }

    /// Names of the directories at the root of `commit`'s tree.
    pub(crate) fn root_dirs(dir: &Path, commit: &str) -> Result<Vec<String>, Error> {
        const FRAME: &str = "[Git::root_dirs] list tree";
        let out = git()
            .current_dir(dir)
            .arg("ls-tree")
            .arg("-z")
            .arg(commit)
            .run(FRAME)?;
        let mut names = Vec::new();
        for entry in out.stdout.split(|&b| b == 0) {
            let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
                continue;
            };
            let Ok(meta) = std::str::from_utf8(&entry[..tab]) else {
                continue;
            };
            let Ok(name) = std::str::from_utf8(&entry[tab + 1..]) else {
                continue;
            };
            if meta.split(' ').nth(1) == Some("tree") {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }
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
        let err = Error::new(git_code(stderr), redact_remote_text(stderr))
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

#[cfg(test)]
mod process_tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{CommitIdentity, SPAWNS, commit, git, history, init, path_has_history, pull};
    use crate::Error;
    use crate::error::Code;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A fresh repository in a directory this test created; only that
    /// directory is removed on drop.
    struct Repo {
        root: PathBuf,
    }

    impl Repo {
        fn new() -> Self {
            super::isolate_test_git_env();
            let base = std::env::var_os("TEST_TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = base.join(format!("scheda-git-unit-{}-{n}", std::process::id()));
            fs::create_dir_all(&root).unwrap();
            let repo = Self { root };
            init(&repo.root).unwrap();
            repo
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, text).unwrap();
        }

        fn commit(&self, message: &str, paths: &[&str]) -> Result<(), Error> {
            let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
            commit(&self.root, message, &paths, &CommitIdentity::fallback())
        }

        fn git(&self, args: &[&str]) -> std::process::Output {
            let mut cmd = git()
                .current_dir(&self.root)
                .arg("-c")
                .arg("user.name=T")
                .arg("-c")
                .arg("user.email=t@invalid")
                .arg("-c")
                .arg("commit.gpgsign=false");
            for arg in args {
                cmd = cmd.arg(arg);
            }
            cmd.command().output().unwrap()
        }

        fn texts(&self, path: &str) -> Vec<String> {
            history(&self.root, path, 0, 100)
                .unwrap()
                .into_iter()
                .map(|entry| String::from_utf8(entry.raw.unwrap()).unwrap())
                .collect()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn spawned(run: impl FnOnce()) -> usize {
        let before = SPAWNS.with(std::cell::Cell::get);
        run();
        SPAWNS.with(std::cell::Cell::get) - before
    }

    fn names(count: usize) -> Vec<String> {
        (0..count).map(|n| format!("dir/file-{n}.txt")).collect()
    }

    #[test]
    fn commit_spawns_a_bounded_number_of_processes() {
        let repo = Repo::new();
        let small = names(3);
        let large = names(100);
        for rel in &large {
            repo.write(rel, "one");
        }
        let few: Vec<&str> = small.iter().map(String::as_str).collect();
        let many: Vec<&str> = large.iter().map(String::as_str).collect();
        let few_count = spawned(|| repo.commit("few", &few).unwrap());
        let many_count = spawned(|| repo.commit("many", &many).unwrap());
        assert_eq!(few_count, 4, "update-index, write-tree, peel HEAD, commit");
        assert_eq!(many_count, 4);
        for rel in &large {
            fs::remove_file(repo.root.join(rel)).unwrap();
        }
        let removed = spawned(|| repo.commit("removed", &many).unwrap());
        assert!(removed <= 5, "{removed}");
        let tracked = repo.git(&["ls-files"]).stdout;
        assert!(tracked.is_empty(), "every path left the index");
    }

    #[test]
    fn commit_of_untracked_missing_paths_is_nothing_to_commit() {
        let repo = Repo::new();
        repo.write("kept.txt", "kept");
        repo.commit("first", &["kept.txt"]).unwrap();
        let err = repo
            .commit("none", &["ghost.txt", "other/ghost.txt"])
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        // A ghost next to a real file is skipped, and the real file lands.
        repo.write("real.txt", "real");
        repo.commit("mixed", &["ghost.txt", "real.txt"]).unwrap();
        assert!(path_has_history(&repo.root, "real.txt").unwrap());
        assert!(!path_has_history(&repo.root, "ghost.txt").unwrap());
    }

    #[test]
    fn commit_takes_special_characters_literally() {
        let repo = Repo::new();
        repo.write("a*b.txt", "star");
        repo.write("axb.txt", "x");
        repo.commit("star", &["a*b.txt"]).unwrap();
        let tracked = String::from_utf8(repo.git(&["ls-files"]).stdout).unwrap();
        assert_eq!(tracked.trim(), "a*b.txt");
        fs::remove_file(repo.root.join("a*b.txt")).unwrap();
        repo.commit("gone", &["a*b.txt"]).unwrap();
        assert!(repo.git(&["ls-files"]).stdout.is_empty());
    }

    #[test]
    fn history_skips_the_commit_that_deleted_the_path() {
        let repo = Repo::new();
        repo.write("a.txt", "v1");
        repo.commit("add", &["a.txt"]).unwrap();
        repo.write("a.txt", "v2");
        repo.commit("edit", &["a.txt"]).unwrap();
        fs::remove_file(repo.root.join("a.txt")).unwrap();
        repo.commit("delete", &["a.txt"]).unwrap();
        assert_eq!(repo.texts("a.txt"), ["v2", "v1"]);
        repo.write("a.txt", "v3");
        repo.commit("re-add", &["a.txt"]).unwrap();
        assert_eq!(repo.texts("a.txt"), ["v3", "v2", "v1"]);
        let page = history(&repo.root, "a.txt", 1, 1).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].raw.as_deref(), Some(b"v2".as_slice()));
        assert_eq!(page[0].message.as_deref(), Some("edit"));
        assert!(history(&repo.root, "a.txt", 3, 5).unwrap().is_empty());
        assert!(path_has_history(&repo.root, "a.txt").unwrap());
        assert!(!path_has_history(&repo.root, "never.txt").unwrap());
    }

    #[test]
    fn history_of_a_deleted_path_keeps_its_earlier_versions() {
        let repo = Repo::new();
        repo.write("gone.txt", "last");
        repo.commit("add", &["gone.txt"]).unwrap();
        fs::remove_file(repo.root.join("gone.txt")).unwrap();
        repo.commit("delete", &["gone.txt"]).unwrap();
        assert_eq!(repo.texts("gone.txt"), ["last"]);
        assert!(path_has_history(&repo.root, "gone.txt").unwrap());
    }

    #[test]
    fn history_lists_a_merge_that_resolved_the_path() {
        let repo = Repo::new();
        repo.write("a.txt", "base");
        repo.commit("base", &["a.txt"]).unwrap();
        let main = String::from_utf8(repo.git(&["symbolic-ref", "--short", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        assert!(repo.git(&["checkout", "-b", "side"]).status.success());
        repo.write("a.txt", "side");
        repo.commit("side", &["a.txt"]).unwrap();
        assert!(repo.git(&["checkout", &main]).status.success());
        repo.write("a.txt", "main");
        repo.commit("main", &["a.txt"]).unwrap();
        // The two edits conflict; the merge commit stores a third text.
        assert!(!repo.git(&["merge", "--no-edit", "side"]).status.success());
        repo.write("a.txt", "merged");
        assert!(repo.git(&["add", "a.txt"]).status.success());
        assert!(repo.git(&["commit", "--no-edit"]).status.success());
        let mut texts = repo.texts("a.txt");
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(texts.first().map(String::as_str), Some("merged"));
        texts.sort();
        assert_eq!(texts, ["base", "main", "merged", "side"]);
    }

    #[test]
    fn history_on_an_unborn_branch_is_empty() {
        let repo = Repo::new();
        assert!(history(&repo.root, "a.txt", 0, 10).unwrap().is_empty());
        assert!(!path_has_history(&repo.root, "a.txt").unwrap());
    }

    #[test]
    fn history_spawns_a_bounded_number_of_processes() {
        let repo = Repo::new();
        for n in 0..20 {
            repo.write("a.txt", &format!("v{n}"));
            repo.commit("edit", &["a.txt"]).unwrap();
        }
        let count = spawned(|| {
            assert_eq!(history(&repo.root, "a.txt", 2, 3).unwrap().len(), 3);
        });
        assert_eq!(count, 4, "peel HEAD, log, batch-check, batch");
    }
    #[test]
    fn pull_into_an_unborn_branch_takes_the_remote_head() {
        let remote = Repo::new();
        remote.write("a.txt", "one");
        remote.commit("one", &["a.txt"]).unwrap();
        // Another branch that sorts first must not be the one pulled.
        assert!(remote.git(&["branch", "aaa-other"]).status.success());
        let local = Repo::new();
        let url = remote.root.to_string_lossy().into_owned();
        assert!(
            local
                .git(&["remote", "add", "origin", &url])
                .status
                .success()
        );
        pull(&local.root).unwrap();
        assert_eq!(local.texts("a.txt"), ["one"]);
    }
}
