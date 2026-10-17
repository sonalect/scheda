//! The git command line, kept for the network. Local git work (objects,
//! trees, commits, the index, the branch) goes through the library in
//! `repo`, in process; what stays here is running `git` for `clone`, `fetch`,
//! `pull`, and `push`, which need the person's SSH configuration, agent, and
//! credential helpers, and mapping what it prints on `stderr` to a failure.

// The standard output of a command, its input, and its environment are for
// the tests that drive git by hand; the network commands read none of them.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the tests that drive git by hand use the output, the input, and the environment"
    )
)]

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use scheda_api::v1::VaultErrorCode;

use crate::error::{Error, Fault, OrFault, git_fault, io as io_fault};

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

    /// Whether `stderr` says the mirror has no such branch: an empty mirror,
    /// or one without the branch asked for.
    pub(crate) fn missing_ref(&self) -> bool {
        self.stderr
            .to_ascii_lowercase()
            .contains("couldn't find remote ref")
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

/// `git clone -- url path`: copies the mirror at `url` into the new folder
/// `path`. The person's SSH configuration, agent, and credential helpers
/// apply; nothing is prompted for.
pub(crate) fn clone(url: &str, path: &Path) -> Result<(), Error> {
    git().arg("clone").arg("--").arg(url).arg(path).run()?;
    Ok(())
}

/// `git fetch -- origin what` in the work tree at `dir`: the commit of
/// `what` (a branch name, or `HEAD`) lands in `FETCH_HEAD`. `false` when the
/// mirror has no such branch (an empty mirror); every other failure is the
/// classified one.
pub(crate) fn fetch(dir: &Path, what: &str) -> Result<bool, Error> {
    let out = git()
        .current_dir(dir)
        .arg("fetch")
        .arg("--")
        .arg("origin")
        .arg(what)
        .spawn()?;
    if out.success() {
        Ok(true)
    } else if out.missing_ref() {
        Ok(false)
    } else {
        Err(out.failure().into())
    }
}

/// `git push -- origin refs/heads/b:refs/heads/b` in the work tree at
/// `dir`, for the branch `b`. A mirror that holds writes the Vault lacks
/// refuses it: `MIRROR_AHEAD`.
pub(crate) fn push(dir: &Path, branch: &str) -> Result<(), Error> {
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    git()
        .current_dir(dir)
        .arg("push")
        .arg("--")
        .arg("origin")
        .arg(refspec)
        .run()?;
    Ok(())
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
        // The library reads no `GIT_CONFIG_*` variable: point its search
        // paths at nothing as well.
        for level in [
            git2::ConfigLevel::System,
            git2::ConfigLevel::XDG,
            git2::ConfigLevel::Global,
            git2::ConfigLevel::ProgramData,
        ] {
            // SAFETY: Once serializes callers, and the library's search
            // paths are set before any repository or configuration is
            // opened by this process (`repo::init_library` calls this
            // first).
            let _ = unsafe { git2::opts::set_search_path(level, &absent) };
        }
    });
}

#[cfg(test)]
mod tests {
    use scheda_api::v1::VaultErrorCode;

    use super::{git, isolate_test_git_env, spawned};

    #[test]
    fn a_command_runs_with_input_and_counts_as_a_process() {
        isolate_test_git_env();
        let before = spawned();
        let out = git()
            .arg("hash-object")
            .arg("--stdin")
            .input(b"x".to_vec())
            .run()
            .unwrap();
        assert_eq!(
            out.stdout_trim().unwrap(),
            "c1b0730e0133447badcfd47fd144e254807b06e1"
        );
        assert_eq!(spawned(), before + 1);
    }

    #[test]
    fn a_failed_command_is_a_replication_failure_with_its_stderr() {
        isolate_test_git_env();
        let error = git().arg("no-such-command").run().map(|_| ()).unwrap_err();
        assert!(error.is(VaultErrorCode::Replication));
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
