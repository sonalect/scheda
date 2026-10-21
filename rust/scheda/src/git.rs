//! The git command line, kept for the network. Local git work (objects,
//! trees, commits, the index, the branch) goes through the library in
//! `repo`, in process; what stays here is running `git` for `clone`, `fetch`,
//! `pull`, and `push`, which need the person's SSH configuration, agent, and
//! credential helpers, and mapping their failure to a code.
//!
//! The command line has a signal of its own for most of what can go wrong, and
//! that signal is the one read: a push is told by `git push --porcelain`
//! (git's documented machine format: one status line per reference), a fetch
//! that finds no branch is a fetch that writes no reference. Only a refusal
//! of the credentials and a failure of the network have no signal but the
//! text on `stderr`, which is read for those two (git runs with `LC_ALL=C`);
//! every other failure is `REPLICATION`.
//!
//! A command is awaited, not waited for on a thread: a push holds no thread
//! for its seconds on the network. A call that is dropped while its command
//! runs kills the command and every process the command started
//! ([`Running`]).

use std::ffi::{OsStr, OsString};
use std::io;
#[cfg(test)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use scheda_api::v1::VaultErrorCode;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{Error, Fault, git_fault, io as io_fault, redact_remote_text};

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

    /// The failure of a command that did not exit 0: `UNAUTHENTICATED` or
    /// `UNAVAILABLE` when `stderr` tells the credentials or the network
    /// failed, `REPLICATION` otherwise.
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

    /// `stdout` as UTF-8 text.
    #[cfg(test)]
    pub(crate) fn stdout_str(&self) -> Result<&str, Error> {
        std::str::from_utf8(&self.stdout).map_err(|err| {
            Error::from(
                Fault::new(
                    VaultErrorCode::Replication,
                    "git printed text that is not UTF-8",
                )
                .with_source(err),
            )
        })
    }

    /// `stdout` as UTF-8 text without the surrounding whitespace.
    #[cfg(test)]
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
    #[cfg(test)]
    input: Option<Vec<u8>>,
}

/// How many git processes were started in each folder, for the tests that
/// assert a call starts none. A command run without a folder counts under
/// the empty path.
#[cfg(test)]
static SPAWNED: std::sync::Mutex<Vec<(PathBuf, usize)>> = std::sync::Mutex::new(Vec::new());

/// How many git processes were started in `dir` so far.
#[cfg(test)]
pub(crate) fn spawned_in(dir: &Path) -> usize {
    SPAWNED
        .lock()
        .unwrap()
        .iter()
        .find(|(seen, _)| seen == dir)
        .map_or(0, |(_, count)| *count)
}

#[cfg(test)]
fn count(dir: Option<&Path>) {
    let dir = dir.unwrap_or_else(|| Path::new("")).to_path_buf();
    let mut spawned = SPAWNED.lock().unwrap();
    match spawned.iter_mut().find(|(seen, _)| *seen == dir) {
        Some((_, count)) => *count += 1,
        None => spawned.push((dir, 1)),
    }
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
        #[cfg(test)]
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
    #[cfg(test)]
    pub(crate) fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    /// Feeds `bytes` to the standard input of the command.
    #[cfg(test)]
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
    /// was. Only a command that could not start is an error. Dropping the
    /// future kills the command and every process it started.
    pub(crate) async fn output(self) -> Result<GitOutput, Error> {
        #[cfg(test)]
        count(self.dir.as_deref());
        let out = match Running::spawn(self.command()) {
            Ok(running) => running.output().await,
            Err(err) => Err(err),
        };
        match out {
            Ok(out) => Ok(GitOutput::from(out)),
            Err(err) => Err(spawn_fault(err).into()),
        }
    }

    /// Runs the command; an exit status other than 0 is a failure.
    pub(crate) async fn succeed(self) -> Result<GitOutput, Error> {
        let out = self.output().await?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(out.failure().into())
        }
    }

    /// Runs the command on this thread and returns its output whatever the
    /// exit status was: the setup of a test.
    #[cfg(test)]
    pub(crate) fn spawn(self) -> Result<GitOutput, Error> {
        count(self.dir.as_deref());
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

    /// [`GitCmd::spawn`]; an exit status other than 0 is a failure.
    #[cfg(test)]
    pub(crate) fn run(self) -> Result<GitOutput, Error> {
        let out = self.spawn()?;
        if out.status.success() {
            Ok(out)
        } else {
            Err(out.failure().into())
        }
    }
}

/// A git command that runs, together with every process it starts.
///
/// For a mirror reached by `file://`, git runs the mirror's side on this
/// machine (`git-receive-pack` and its hooks, `git-upload-pack`); for one
/// reached by SSH, it runs `ssh`. Killing only `git` would leave those
/// running. So on Unix the command leads a session of its own: everything
/// it starts is in one process group (but for a process that makes a
/// session of its own on purpose, as the master connection of `ssh`'s
/// `ControlPersist` does), and no process of it can read the terminal of
/// the caller, so a prompt of `ssh` fails at once instead of stopping the
/// command. On Windows the command starts suspended, joins a
/// job object, and only then runs, so everything it starts is in the job.
///
/// Dropped before the command was waited for (the call was dropped, or a
/// timeout of the caller ended it), it kills the group, or the job: nothing
/// the command started outlives the call. The group is killed while the
/// command is not yet reaped, so its number is still the command's own. A
/// command that ended by itself is left as it is: git waits for what it
/// started, and a helper it leaves running on purpose stays.
///
/// The caller's own signals do not reach the command: Ctrl-C at a terminal
/// ends the calling program, not git. A program that wants git gone on such
/// a signal catches it and drops its calls.
struct Running {
    child: tokio::process::Child,
    tree: tree::Tree,
}

impl Running {
    fn spawn(cmd: Command) -> io::Result<Running> {
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.kill_on_drop(true);
        let tree = tree::Tree::prepare(&mut cmd)?;
        let running = Running {
            child: cmd.spawn()?,
            tree,
        };
        running.tree.adopt(&running.child)?;
        Ok(running)
    }

    /// Reads `stdout` and `stderr` to their end, then waits for the
    /// command. The command is reaped only after its pipes closed, so a drop
    /// before that finds it unreaped, even when a process it started holds
    /// a pipe after it exited.
    async fn output(mut self) -> io::Result<std::process::Output> {
        let stdout = read_all(self.child.stdout.take());
        let stderr = read_all(self.child.stderr.take());
        let (stdout, stderr) = tokio::try_join!(stdout, stderr)?;
        let status = self.child.wait().await?;
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.tree.kill(&self.child);
    }
}

async fn read_all(pipe: Option<impl AsyncRead + Unpin>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

/// The session of a command on Unix.
#[cfg(unix)]
mod tree {
    use std::ffi::c_int;
    use std::io;

    use tokio::process::{Child, Command};

    // `pid_t` is `int` on every Unix the crate builds for.
    unsafe extern "C" {
        fn setsid() -> c_int;
        fn kill(pid: c_int, signal: c_int) -> c_int;
    }

    /// `SIGKILL`: 9 on every Unix.
    const SIGKILL: c_int = 9;

    /// Nothing to hold: the session is the command's own.
    pub(super) struct Tree;

    impl Tree {
        /// Makes the command lead a new session when it starts.
        pub(super) fn prepare(cmd: &mut Command) -> io::Result<Tree> {
            // SAFETY: the closure runs in the child between `fork` and
            // `exec`. It makes one async-signal-safe system call and
            // allocates nothing; `last_os_error` only reads `errno`.
            unsafe {
                cmd.pre_exec(|| {
                    if setsid() == -1 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            Ok(Tree)
        }

        /// Nothing to do once it runs: the session began before `exec`.
        pub(super) fn adopt(&self, _child: &Child) -> io::Result<()> {
            Ok(())
        }

        /// Kills every process of the group `child` leads, unless `child`
        /// was reaped: a reaped command's number may name another group.
        pub(super) fn kill(&self, child: &Child) {
            let Some(pid) = child.id().and_then(|pid| c_int::try_from(pid).ok()) else {
                return;
            };
            // SAFETY: a plain system call. `-pid` names the process group
            // whose id is `pid`: the session the command leads, which its
            // number keeps until it is reaped. A group already empty is
            // `ESRCH`, which is what this wants.
            unsafe {
                kill(-pid, SIGKILL);
            }
        }
    }
}

/// The job object of a command on Windows.
#[cfg(windows)]
mod tree {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::{
        AsRawHandle, HandleOrInvalid, HandleOrNull, OwnedHandle, RawHandle,
    };

    use tokio::process::{Child, Command};

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
    const THREAD_SUSPEND_RESUME: u32 = 0x0002;

    /// `THREADENTRY32`.
    #[repr(C)]
    struct ThreadEntry {
        size: u32,
        usage: u32,
        thread_id: u32,
        owner_process_id: u32,
        base_priority: i32,
        delta_priority: i32,
        flags: u32,
    }

    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> RawHandle;
        fn AssignProcessToJobObject(job: RawHandle, process: RawHandle) -> i32;
        fn TerminateJobObject(job: RawHandle, exit_code: u32) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> RawHandle;
        fn Thread32First(snapshot: RawHandle, entry: *mut ThreadEntry) -> i32;
        fn Thread32Next(snapshot: RawHandle, entry: *mut ThreadEntry) -> i32;
        fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> RawHandle;
        fn ResumeThread(thread: RawHandle) -> u32;
    }

    /// The job the command and everything it starts belong to.
    pub(super) struct Tree {
        job: OwnedHandle,
    }

    impl Tree {
        /// A new job, and the command set to start suspended.
        pub(super) fn prepare(cmd: &mut Command) -> io::Result<Tree> {
            // SAFETY: no security attributes and no name: a new job that
            // only this handle reaches, owned by nothing else.
            let job = unsafe {
                HandleOrNull::from_raw_handle(CreateJobObjectW(std::ptr::null(), std::ptr::null()))
            };
            let job = OwnedHandle::try_from(job).map_err(|_| io::Error::last_os_error())?;
            cmd.creation_flags(CREATE_SUSPENDED);
            Ok(Tree { job })
        }

        /// Puts the suspended command in the job, then lets it run: it
        /// starts nothing before it is in the job.
        pub(super) fn adopt(&self, child: &Child) -> io::Result<()> {
            let (Some(process), Some(id)) = (child.raw_handle(), child.id()) else {
                return Err(io::Error::other("git ended before it started"));
            };
            // SAFETY: both handles are open: the job is held by `self`,
            // the process by `child`.
            if unsafe { AssignProcessToJobObject(self.job.as_raw_handle(), process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            resume(id)
        }

        /// Ends every process of the job, unless the command was waited
        /// for already.
        pub(super) fn kill(&self, child: &Child) {
            if child.id().is_some() {
                // SAFETY: the job handle is open, held by `self`.
                unsafe {
                    TerminateJobObject(self.job.as_raw_handle(), 1);
                }
            }
        }
    }

    /// Resumes the threads of the suspended process `id`: its main thread,
    /// the only one it has.
    fn resume(id: u32) -> io::Result<()> {
        // SAFETY: a snapshot of the threads of the system, owned by nothing
        // else; no pointer.
        let snapshot = unsafe {
            HandleOrInvalid::from_raw_handle(CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0))
        };
        let snapshot = OwnedHandle::try_from(snapshot).map_err(|_| io::Error::last_os_error())?;
        let mut entry = ThreadEntry {
            size: size_of::<ThreadEntry>() as u32,
            usage: 0,
            thread_id: 0,
            owner_process_id: 0,
            base_priority: 0,
            delta_priority: 0,
            flags: 0,
        };
        let mut resumed = false;
        // SAFETY: the snapshot is open and `entry` is a `THREADENTRY32`
        // whose size field is set.
        let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
        while more {
            if entry.owner_process_id == id {
                // SAFETY: opens the thread by its id, a handle owned by
                // nothing else; no pointer.
                let thread = unsafe {
                    HandleOrNull::from_raw_handle(OpenThread(
                        THREAD_SUSPEND_RESUME,
                        0,
                        entry.thread_id,
                    ))
                };
                let thread =
                    OwnedHandle::try_from(thread).map_err(|_| io::Error::last_os_error())?;
                // SAFETY: the thread handle is open.
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                resumed = true;
            }
            // SAFETY: as for `Thread32First`.
            more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
        }
        if resumed {
            Ok(())
        } else {
            Err(io::Error::other(
                "the thread of the new git process was not found",
            ))
        }
    }
}

/// `git clone -- url path`: copies the mirror at `url` into the new folder
/// `path`. The person's SSH configuration, agent, and credential helpers
/// apply; nothing is prompted for.
pub(crate) async fn clone(url: &str, path: &Path) -> Result<(), Error> {
    git()
        .arg("clone")
        .arg("--")
        .arg(url)
        .arg(path)
        .succeed()
        .await?;
    Ok(())
}

/// `git clone --bare -- url git_dir`: copies the mirror at `url` into the
/// git folder `git_dir` with no work tree, no checkout, and no git index.
pub(crate) async fn clone_bare(url: &str, git_dir: &Path) -> Result<(), Error> {
    git()
        .arg("clone")
        .arg("--bare")
        .arg("--")
        .arg(url)
        .arg(git_dir)
        .succeed()
        .await?;
    Ok(())
}

/// The namespace of Scheda's own references for the mirror's branches: a
/// fetch writes `refs/scheda/mirror/<branch>` for each branch the mirror
/// holds, so it touches neither the Vault's branches nor `FETCH_HEAD`.
pub(crate) const MIRROR_REFS: &str = "refs/scheda/mirror/";

/// `git fetch --prune --no-tags --no-write-fetch-head --refmap= -- origin
/// +refs/heads/*:refs/scheda/mirror/*` in the work tree at `dir`: every
/// branch of the mirror lands in a reference of Scheda's own, and a branch
/// the mirror deleted loses its reference. A mirror with no branch is a
/// fetch that succeeds and writes nothing.
///
/// `--refmap=` keeps git from also updating the remote-tracking references
/// the configuration names: a lock on one of them, held by a `git push`,
/// would fail the fetch. The fetch changes no branch, no `HEAD`, and no file
/// of the work tree, so it needs no write lock; two fetches of one
/// repository must not run at once (the caller serialises them).
pub(crate) async fn fetch(dir: &Path) -> Result<(), Error> {
    git()
        .current_dir(dir)
        .arg("fetch")
        .arg("--prune")
        .arg("--no-tags")
        .arg("--no-write-fetch-head")
        .arg("--refmap=")
        .arg("--")
        .arg("origin")
        .arg(format!("+refs/heads/*:{MIRROR_REFS}*"))
        .succeed()
        .await?;
    Ok(())
}

/// `git push --porcelain -- origin refs/heads/b:refs/heads/b` in the work
/// tree at `dir`, for the branch `b`. The status of the reference in the
/// porcelain output is the truth: a mirror that holds writes the Vault
/// lacks refuses it (`MIRROR_AHEAD`), a mirror that refuses it for another
/// reason is `REPLICATION` naming the reason. A push that never reached a
/// reference is told by its `stderr` as any command is (credentials,
/// network), else `REPLICATION`.
///
/// Git also updates the remote-tracking reference `refs/remotes/origin/b`
/// after a push, and when it cannot lock that reference (another push is
/// updating it) it prints an error, ignores it, and exits 0 with the
/// reference's status unchanged; so only the porcelain status is read.
pub(crate) async fn push(dir: &Path, branch: &str) -> Result<(), Error> {
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let out = git()
        .current_dir(dir)
        .arg("push")
        .arg("--porcelain")
        .arg("--")
        .arg("origin")
        .arg(refspec)
        .output()
        .await?;
    if let Some(refused) = refusal(&String::from_utf8_lossy(&out.stdout)) {
        return Err(refused.into());
    }
    if out.success() {
        Ok(())
    } else {
        Err(out.failure().into())
    }
}

/// The failure a `git push --porcelain` reports for a reference it
/// rejected: a line `!<TAB><from>:<to><TAB><summary> (<reason>)`. `None`
/// when no reference was rejected (none was tried, or all went through).
fn refusal(porcelain: &str) -> Option<Fault> {
    let line = porcelain.lines().find(|line| line.starts_with("!\t"))?;
    let summary = line.splitn(3, '\t').nth(2).unwrap_or_default();
    let behind = summary.starts_with("[rejected]")
        && (summary.ends_with("(fetch first)") || summary.ends_with("(non-fast-forward)"));
    if behind {
        Some(Fault::new(
            VaultErrorCode::MirrorAhead,
            "the mirror holds writes the Vault has not pulled; pull, then push again",
        ))
    } else {
        Some(Fault::new(
            VaultErrorCode::Replication,
            format!(
                "the mirror refused the push: {}",
                redact_remote_text(summary)
            ),
        ))
    }
}

/// Runs `cmd` with `input` on its standard input. The input is written from
/// another thread while the output is read, so a command that prints before
/// it has read everything cannot block on a full pipe. A write the command
/// cuts short (it exited early) is not an error here: its exit status says
/// why.
#[cfg(test)]
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

    use super::{git, isolate_test_git_env, refusal, spawned_in};

    #[test]
    fn a_command_runs_with_input_and_counts_as_a_process() {
        isolate_test_git_env();
        let dir = std::env::temp_dir();
        let before = spawned_in(&dir);
        let out = git()
            .current_dir(&dir)
            .arg("hash-object")
            .arg("--stdin")
            .input(b"x".to_vec())
            .run()
            .unwrap();
        assert_eq!(
            out.stdout_trim().unwrap(),
            "c1b0730e0133447badcfd47fd144e254807b06e1"
        );
        assert_eq!(spawned_in(&dir), before + 1);
    }

    #[test]
    fn a_failed_command_is_a_replication_failure_with_its_stderr() {
        isolate_test_git_env();
        let error = git().arg("no-such-command").run().map(|_| ()).unwrap_err();
        assert!(error.is(VaultErrorCode::Replication));
    }

    #[test]
    fn a_rejected_reference_is_told_by_its_porcelain_status() {
        let ahead =
            "To ../m.git\n!\trefs/heads/main:refs/heads/main\t[rejected] (fetch first)\nDone\n";
        assert_eq!(
            refusal(ahead).unwrap().vault_code(),
            VaultErrorCode::MirrorAhead
        );
        let diverged =
            "To m\n!\trefs/heads/main:refs/heads/main\t[rejected] (non-fast-forward)\nDone\n";
        assert_eq!(
            refusal(diverged).unwrap().vault_code(),
            VaultErrorCode::MirrorAhead
        );
        let hook = "To m\n!\trefs/heads/main:refs/heads/main\t[remote rejected] (pre-receive hook declined)\nDone\n";
        let fault = refusal(hook).unwrap();
        assert_eq!(fault.vault_code(), VaultErrorCode::Replication);
        assert!(fault.message().contains("pre-receive hook declined"));
        let stale = "To m\n!\trefs/heads/main:refs/heads/main\t[rejected] (stale info)\nDone\n";
        assert_eq!(
            refusal(stale).unwrap().vault_code(),
            VaultErrorCode::Replication
        );
    }

    #[test]
    fn a_push_that_rejected_nothing_has_no_refusal() {
        assert!(
            refusal("To m\n \trefs/heads/main:refs/heads/main\t35d7096..00e8795\nDone\n").is_none()
        );
        assert!(
            refusal("To m\n*\trefs/heads/main:refs/heads/main\t[new branch]\nDone\n").is_none()
        );
        assert!(refusal("").is_none());
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
