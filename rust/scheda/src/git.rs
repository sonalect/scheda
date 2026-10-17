//! The git command line: running a command (with input on its standard
//! input when it takes some), and the questions `open` asks of a directory.
//! The commit plumbing is built on it in `commit`; nothing here clones,
//! pulls, pushes, or reads history.

// The commands of the next layers use what `open` does not.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the layers that write, clone, and read history are not built yet"
    )
)]

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use scheda_api::v1::VaultErrorCode;

use crate::error::{Context, Error, Fault, GitFailure, OrFault, git_fault, io as io_fault};

/// What a finished git command left behind.
pub(crate) struct GitOutput {
    stdout: Vec<u8>,
    stderr: String,
    status: ExitStatus,
}

impl GitOutput {
    /// Whether the command exited 0.
    pub(crate) fn success(&self) -> bool {
        self.status.success()
    }

    /// The failure of a command that did not exit 0, from its `stderr`.
    pub(crate) fn failure(&self) -> Fault {
        if self.stderr.trim().is_empty() {
            let message = match self.status.code() {
                Some(code) => format!("git exited with status {code}"),
                None => String::from("git exited with a failure"),
            };
            Fault::new(VaultErrorCode::Replication, message)
        } else {
            git_fault(&self.stderr)
        }
    }

    /// The exit code, if the process exited with one.
    pub(crate) fn code(&self) -> Option<i32> {
        self.status.code()
    }

    /// `stdout` as UTF-8 text.
    pub(crate) fn stdout_str(&self) -> Result<&str, Error> {
        std::str::from_utf8(&self.stdout)
            .or_fault(
                VaultErrorCode::Replication,
                "git printed text that is not UTF-8",
            )
            .map_err(Error::from)
    }

    /// `stdout` as UTF-8 text without the surrounding whitespace.
    pub(crate) fn stdout_trim(&self) -> Result<&str, Error> {
        Ok(self.stdout_str()?.trim())
    }
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

/// One git command line under construction.
pub(crate) struct GitCmd {
    dir: Option<PathBuf>,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    input: Option<Vec<u8>>,
}

#[cfg(test)]
thread_local! {
    static SPAWNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many git processes this thread has started, for the tests that
/// assert a call starts none.
#[cfg(test)]
pub(crate) fn spawned() -> usize {
    SPAWNED.with(std::cell::Cell::get)
}

/// A new git command. Tests run it with the user's git configuration
/// replaced by an empty one.
pub(crate) fn git() -> GitCmd {
    #[cfg(test)]
    isolate_test_git_env();
    GitCmd {
        dir: None,
        args: Vec::new(),
        envs: Vec::new(),
        input: None,
    }
}

impl GitCmd {
    /// Runs the command in `dir`.
    pub(crate) fn current_dir(mut self, dir: &Path) -> Self {
        self.dir = Some(dir.to_path_buf());
        self
    }

    /// Adds one argument.
    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Sets one environment variable of the child.
    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// Feeds `bytes` to the standard input of the command.
    pub(crate) fn input(mut self, bytes: Vec<u8>) -> Self {
        self.input = Some(bytes);
        self
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.env("LC_ALL", "C")
            .env("LANG", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Interactive add-ons from the user's `~/.gitconfig` can hang a
        // library (watchman fsmonitor, auto-gc, repository hooks). The user
        // identity, credential helpers, and the operator's GIT_SSH_COMMAND
        // still apply: Scheda does not set or read an SSH command.
        // `core.longpaths` lets Windows stage a 255-byte filename component
        // when the absolute path is longer than MAX_PATH; Unix ignores it.
        // Passing it per command needs no config write before a commit.
        cmd.arg("-c")
            .arg("core.fsmonitor=")
            .arg("-c")
            .arg("core.useBuiltinFSMonitor=false")
            .arg("-c")
            .arg("core.hooksPath=")
            .arg("-c")
            .arg("gc.auto=0")
            .arg("-c")
            .arg("maintenance.auto=false")
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

    /// Runs the command and returns its output whatever the exit status
    /// was. Only a command that could not start is an error.
    pub(crate) fn spawn(self) -> Result<GitOutput, Error> {
        #[cfg(test)]
        SPAWNED.with(|count| count.set(count.get() + 1));
        let mut cmd = self.command();
        let out = match &self.input {
            None => cmd.output(),
            Some(bytes) => output_with_input(&mut cmd, bytes),
        };
        match out {
            Ok(out) => Ok(GitOutput::from(out)),
            Err(err) => Err(spawn_fault(err).into()),
        }
    }

    /// Runs the command; an exit status other than 0 is a failure.
    pub(crate) fn run(self) -> Result<GitOutput, Error> {
        let out = self.spawn()?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(out.failure().into())
        }
    }
}

/// Runs `cmd` with `input` on its standard input. The input is written from
/// another thread while the output is read, so a command that prints before
/// it has read everything cannot block on a full pipe. A write the command
/// cuts short (it exited early) is not an error here: its exit status says
/// why.
fn output_with_input(cmd: &mut Command, input: &[u8]) -> io::Result<std::process::Output> {
    cmd.stdin(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take();
    std::thread::scope(|scope| {
        let feeder = scope.spawn(move || {
            if let Some(mut stdin) = stdin {
                let _ = stdin.write_all(input);
            }
        });
        let out = child.wait_with_output();
        let _ = feeder.join();
        out
    })
}

fn spawn_fault(err: io::Error) -> Fault {
    if err.kind() == io::ErrorKind::NotFound {
        Fault::new(
            VaultErrorCode::FailedPrecondition,
            "the git executable was not found on PATH; install Git and retry",
        )
        .with_source(err)
    } else {
        io_fault(err)
    }
}

/// Points this process at an empty git configuration, so that the
/// developer's `~/.gitconfig` cannot change the outcome of a test. Creates
/// no files: git treats a missing `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM`
/// path as an empty configuration. `HOME` is left alone.
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

/// The value of git configuration key `key` (in `dir`'s repository when
/// given), or `None` when it is unset or empty.
pub(crate) fn config_get(dir: Option<&Path>, key: &str) -> Result<Option<String>, Error> {
    let mut cmd = git().arg("config").arg("--get").arg(key);
    if let Some(dir) = dir {
        cmd = cmd.current_dir(dir);
    }
    let out = cmd.spawn().context("config_get: run git")?;
    if out.status.success() {
        let value = out
            .stdout_trim()
            .context("config_get: read value")?
            .to_owned();
        Ok((!value.is_empty()).then_some(value))
    } else if out.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(out.failure()).context("config_get: read configuration")
    }
}

/// The boolean `key` of the git configuration seen from the repository at
/// `dir`, as git itself reads it (`true`, `yes`, `on`, `1`, …), or `None`
/// when it is not set. A value git cannot read as a boolean is
/// `REPLICATION`.
pub(crate) fn config_bool(dir: &Path, key: &str) -> Result<Option<bool>, Error> {
    let out = git()
        .current_dir(dir)
        .arg("config")
        .arg("--type=bool")
        .arg("--get")
        .arg(key)
        .spawn()
        .context("config_bool: run git")?;
    if out.status.success() {
        Ok(Some(
            out.stdout_trim().context("config_bool: read value")? == "true",
        ))
    } else if out.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(out.failure()).context("config_bool: read configuration")
    }
}

/// Sets `core.longpaths` in the repository at `dir`, which lets Windows
/// stage a 255-byte filename component when the absolute path is longer
/// than MAX_PATH. Unix ignores the key.
pub(crate) fn enable_long_paths(dir: &Path) -> Result<(), Error> {
    let out = git()
        .current_dir(dir)
        .arg("config")
        .arg("--local")
        .arg("--get")
        .arg("--type=bool")
        .arg("core.longpaths")
        .spawn()
        .context("enable_long_paths: run git")?;
    if out.status.success() && out.stdout_trim()? == "true" {
        return Ok(());
    }
    if out.status.success() || out.status.code() == Some(1) {
        git()
            .current_dir(dir)
            .arg("config")
            .arg("--local")
            .arg("core.longpaths")
            .arg("true")
            .run()
            .context("enable_long_paths: set core.longpaths")?;
        return Ok(());
    }
    Err(out.failure()).context("enable_long_paths: read core.longpaths")
}

/// Succeeds when `dir` is the top level of a git work tree. A bare
/// repository is `FAILED_PRECONDITION`; a directory that is not a work tree
/// is whatever git reports (`REPLICATION`).
pub(crate) fn require_work_tree(dir: &Path) -> Result<(), Error> {
    let inside = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .run()
        .context("require_work_tree: open repository")?;
    if inside.stdout_trim()? != "true" {
        return Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            "the path is a bare git repository, not a work tree",
        )
        .into());
    }
    let top = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .run()
        .context("require_work_tree: find the top level")?;
    if !paths_equal(dir, top.stdout_trim()?) {
        return Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            "the path is not the top level of a git work tree",
        )
        .into());
    }
    Ok(())
}

/// Whether `dir` and `toplevel` name the same directory.
pub(crate) fn paths_equal(dir: &Path, toplevel: &str) -> bool {
    match (dir.canonicalize(), Path::new(toplevel).canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Whether `dir` is the top level of a git work tree. Any failure to ask is
/// "no".
pub(crate) fn is_work_tree(dir: &Path) -> bool {
    if !dir.exists() {
        return false;
    }
    let Ok(inside) = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .spawn()
    else {
        return false;
    };
    if !inside.status.success() || inside.stdout_trim().ok() != Some("true") {
        return false;
    }
    let Ok(top) = git()
        .current_dir(dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .spawn()
    else {
        return false;
    };
    top.status.success()
        && top
            .stdout_trim()
            .is_ok_and(|toplevel| paths_equal(dir, toplevel))
}

/// Whether git's index tracks `path` (repository-relative, taken literally).
pub(crate) fn is_tracked(dir: &Path, path: &str) -> Result<bool, Error> {
    let out = git()
        .current_dir(dir)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg("--")
        .arg(path)
        .spawn()
        .context("is_tracked: run git ls-files")?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(out.failure()).context("is_tracked: list files"),
    }
}

/// The fetch URL of `origin`, read from the local configuration without
/// contacting the network. No `origin`, or an empty URL, is `None`.
pub(crate) fn origin_url(dir: &Path) -> Result<Option<String>, Error> {
    require_work_tree(dir)?;
    let out = git()
        .current_dir(dir)
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .spawn()
        .context("origin_url: run git")?;
    if out.status.success() {
        let url = out.stdout_trim().context("origin_url: read the URL")?;
        Ok((!url.is_empty()).then(|| url.to_owned()))
    } else if GitFailure::classify(&out.stderr) == GitFailure::NotFound {
        Ok(None)
    } else {
        Err(out.failure()).context("origin_url: find origin")
    }
}

/// Whether git's index tracks `prefix` (repository-relative) or anything
/// below it. The path of an entry is compared as bytes: a name that is not
/// UTF-8 is still a name.
pub(crate) fn index_has_prefix(dir: &Path, prefix: &str) -> Result<bool, Error> {
    let out = git()
        .current_dir(dir)
        .arg("ls-files")
        .arg("-z")
        .arg("--cached")
        .run()
        .context("index_has_prefix: list the index")?;
    let prefix = prefix.trim_end_matches('/');
    let nested = format!("{prefix}/");
    Ok(out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .any(|path| path == prefix.as_bytes() || path.starts_with(nested.as_bytes())))
}

/// The repository-relative paths git holds as *unmerged*: a merge, a
/// pull, or a rebase that stopped on a conflict and was not finished. Each
/// path is listed once, in git's order. Git's own state is the answer; the
/// bytes of a file say nothing about it.
pub(crate) fn unmerged_paths(dir: &Path) -> Result<Vec<String>, Error> {
    let out = git()
        .current_dir(dir)
        .arg("ls-files")
        .arg("--unmerged")
        .arg("-z")
        .run()
        .context("unmerged_paths: list the unmerged entries")?;
    let mut paths: Vec<String> = Vec::new();
    for entry in out.stdout.split(|byte| *byte == 0) {
        // `<mode> <object> <stage>\t<path>`, one entry per stage.
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let path = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
        if paths.last() != Some(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::VaultErrorCode;

    use super::{
        config_get, enable_long_paths, git, index_has_prefix, is_tracked, is_work_tree,
        isolate_test_git_env, origin_url, require_work_tree, unmerged_paths,
    };

    /// A scratch directory that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn init(&self) {
            git().arg("init").arg("--").arg(&self.0).run().unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_plain_folder_is_not_a_work_tree() {
        let scratch = Scratch::new("plain");
        assert!(!is_work_tree(&scratch.0));
        assert!(!is_work_tree(&scratch.0.join("missing")));
        assert!(require_work_tree(&scratch.0).is_err());
    }

    #[test]
    fn a_folder_inside_a_work_tree_is_not_its_top_level() {
        let scratch = Scratch::new("inner");
        scratch.init();
        std::fs::create_dir(scratch.0.join("sub")).unwrap();
        assert!(is_work_tree(&scratch.0));
        require_work_tree(&scratch.0).unwrap();
        assert!(!is_work_tree(&scratch.0.join("sub")));
        let error = require_work_tree(&scratch.0.join("sub")).unwrap_err();
        assert!(error.is(VaultErrorCode::FailedPrecondition));
    }

    #[test]
    fn origin_url_is_the_fetch_url_or_none() {
        let scratch = Scratch::new("origin");
        scratch.init();
        assert_eq!(origin_url(&scratch.0).unwrap(), None);
        git()
            .current_dir(&scratch.0)
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg("https://example.test/org/repo.git")
            .run()
            .unwrap();
        assert_eq!(
            origin_url(&scratch.0).unwrap().as_deref(),
            Some("https://example.test/org/repo.git")
        );
    }

    #[test]
    fn config_and_tracking_are_read_from_the_repository() {
        let scratch = Scratch::new("config");
        scratch.init();
        assert_eq!(config_get(Some(&scratch.0), "scheda.none").unwrap(), None);
        enable_long_paths(&scratch.0).unwrap();
        enable_long_paths(&scratch.0).unwrap();
        assert_eq!(
            config_get(Some(&scratch.0), "core.longpaths")
                .unwrap()
                .as_deref(),
            Some("true")
        );
        std::fs::write(scratch.0.join("a"), b"x").unwrap();
        assert!(!is_tracked(&scratch.0, "a").unwrap());
        git()
            .current_dir(&scratch.0)
            .arg("add")
            .arg("a")
            .run()
            .unwrap();
        assert!(is_tracked(&scratch.0, "a").unwrap());
    }

    #[test]
    fn the_index_prefix_is_a_directory_or_a_file_of_that_name() {
        let scratch = Scratch::new("prefix");
        scratch.init();
        for rel in [".isolated/a/b", ".isolatedx/c", "other"] {
            let path = scratch.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        git()
            .current_dir(&scratch.0)
            .arg("add")
            .arg("other")
            .arg(".isolatedx")
            .run()
            .unwrap();
        assert!(!index_has_prefix(&scratch.0, ".isolated").unwrap());
        assert!(index_has_prefix(&scratch.0, ".isolatedx/").unwrap());
        git()
            .current_dir(&scratch.0)
            .arg("add")
            .arg("--force")
            .arg(".isolated")
            .run()
            .unwrap();
        assert!(index_has_prefix(&scratch.0, ".isolated").unwrap());
        assert!(index_has_prefix(&scratch.0, "other").unwrap());
    }

    #[test]
    fn unmerged_paths_are_git_state_listed_once_each() {
        let scratch = Scratch::new("unmerged");
        scratch.init();
        let run = |args: &[&str]| {
            let mut cmd = git().current_dir(&scratch.0);
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
        };
        let write = |rel: &str, text: &str| std::fs::write(scratch.0.join(rel), text).unwrap();
        assert!(unmerged_paths(&scratch.0).unwrap().is_empty());
        write("a", "base\n");
        write("b", "base\n");
        assert!(run(&["add", "--all"]));
        assert!(run(&["commit", "--quiet", "--message", "base"]));
        assert!(run(&["checkout", "--quiet", "-b", "other"]));
        write("a", "theirs\n");
        write("b", "theirs\n");
        assert!(run(&["commit", "--quiet", "--all", "--message", "theirs"]));
        assert!(run(&["checkout", "--quiet", "-"]));
        write("a", "ours\n");
        write("b", "ours\n");
        assert!(run(&["commit", "--quiet", "--all", "--message", "ours"]));
        assert!(!run(&["merge", "--quiet", "--no-edit", "other"]));
        assert_eq!(unmerged_paths(&scratch.0).unwrap(), ["a", "b"]);
        // Resolving one path leaves the other.
        write("a", "resolved\n");
        assert!(run(&["add", "a"]));
        assert_eq!(unmerged_paths(&scratch.0).unwrap(), ["b"]);
    }

    #[test]
    fn isolation_does_not_set_an_ssh_command_or_create_files() {
        isolate_test_git_env();
        if let Some(value) = std::env::var_os("GIT_SSH_COMMAND") {
            assert!(!value.to_string_lossy().contains("BatchMode"));
        }
        assert!(std::env::var_os("GIT_CONFIG_NOSYSTEM").is_some());
        let global = std::env::var_os("GIT_CONFIG_GLOBAL").unwrap();
        assert!(!std::path::Path::new(&global).exists());
    }
}
