//! Helpers shared by the integration tests: a scratch directory, plain
//! `git` with the user's configuration kept out, a Vault built on disk
//! the way a user would build one, without the library, the objects of the
//! opening of a Vault by the library, and the two runtimes every test runs
//! on.

use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, ExitStatus};
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use protocontract::{IterStream, Stream};
use scheda::v1::{
    CollectionKey, DynCollectionAsync, DynMirrorAsync, DynVaultAsync, OpenOptions, SchedaAsync,
    VaultAsync, VaultOpening,
};
use scheda::{Error, Scheda};
use tokio::runtime::Builder;

static NEXT: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The scratch directories of the test this thread runs.
    static LATER: Later = const { Later(RefCell::new(Vec::new())) };
}

/// Directories removed when the thread that holds them ends.
struct Later(RefCell<Vec<PathBuf>>);

impl Drop for Later {
    fn drop(&mut self) {
        for root in self.0.get_mut().drain(..) {
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

/// A scratch directory, removed when the thread of the test that made it
/// ends, whether the test passed or failed.
///
/// A Vault dropped without `Close` lets its indexer finish the round it is
/// in, on the blocking pool of its runtime, and that round writes under
/// `.scheda/index/`. Removed when the test's last value goes, the directory
/// races the round: a file the round adds stops the removal, and the rest
/// of the directory stays. The thread of a test ends after the runtimes the
/// test built ([`both`]) were dropped, and dropping a runtime waits for its
/// blocking steps, so the directory goes after the last round. The test
/// harness runs each test on a thread of its own and joins it before it
/// reports it. A runtime that outlives the test (a `static` one) is not
/// waited for: a test on one closes its Vaults.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A new, empty scratch directory.
    pub fn new(name: &str) -> Scratch {
        isolate_library_git();
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!("scheda-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        LATER.with(|later| later.0.borrow_mut().push(root.clone()));
        Scratch { root }
    }

    /// A path below the directory.
    pub fn join(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

/// The path the library reports for the Vault at `path`: resolved, and on
/// Windows in the plain form (`C:\…`), not the verbatim `\\?\C:\…` that
/// resolving gives there. A scratch directory always has a plain form.
pub fn resolved(path: &Path) -> PathBuf {
    let path = path.canonicalize().unwrap();
    #[cfg(windows)]
    if let Some(plain) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\"))
        && !plain.starts_with(r"UNC\")
    {
        return PathBuf::from(plain);
    }
    path
}

/// The `file://` URL of the absolute path `path`: `file:///home/a/m.git`,
/// and on Windows `file:///C:/a/m.git`.
pub fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

/// A git configuration file that is not there: git and the library take it
/// as an empty one.
fn absent_config() -> PathBuf {
    std::env::temp_dir().join("scheda-absent-gitconfig")
}

/// Points the library's search for git configuration files (system, XDG,
/// global, `ProgramData`) at a file that is not there, once for the
/// process, before any test opens a Vault: a test must not take the
/// machine's or the user's `init.defaultBranch`, `core.autocrlf`, and the
/// rest. The git command line reads `GIT_CONFIG_*` (see [`command`]); the
/// library reads none of them. On a Windows runner it read the system
/// configuration of Git for Windows, and the Vaults it made pushed a branch
/// the test mirrors (`--initial-branch=main`) do not check out.
pub fn isolate_library_git() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let absent = absent_config();
        for level in [
            git2::ConfigLevel::System,
            git2::ConfigLevel::XDG,
            git2::ConfigLevel::Global,
            git2::ConfigLevel::ProgramData,
        ] {
            // SAFETY: the library reads its configuration only inside a
            // Vault, and every test reaches one through these helpers,
            // which wait here until the paths are set.
            unsafe { git2::opts::set_search_path(level, &absent) }.unwrap();
        }
    });
}

/// `git args` to run in `dir`, with the user's and the system's git
/// configuration left out and an identity of its own.
pub fn command(dir: &Path, args: &[&str]) -> Command {
    let absent = absent_config();
    let mut cmd = Command::new("git");
    cmd.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", &absent)
        .env("GIT_CONFIG_SYSTEM", &absent)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.test")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.test");
    cmd
}

/// Runs `git args` in `dir` as [`command`] sets it up. A failure fails the
/// test.
pub fn git(dir: &Path, args: &[&str]) {
    let status = command(dir, args).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Writes `text` to `rel` below `dir`, creating the folders.
pub fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A git work tree at `dir` that holds `files` (path, text), committed. The
/// marker is one of the files when the caller wants one.
pub fn work_tree(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "--quiet"]);
    for (rel, text) in files {
        write(dir, rel, text);
    }
    git(dir, &["add", "--all"]);
    git(
        dir,
        &["commit", "--quiet", "--allow-empty", "--message", "fixture"],
    );
}

/// A Vault of generation 3 at `dir` with the collection `notes` (a
/// standard collection) and the collection `log` (a journal).
pub fn vault(dir: &Path) {
    work_tree(
        dir,
        &[
            (".scheda.yaml", "version: 3\n"),
            (
                "notes/.collection.yaml",
                "name: notes\nschema:\n  standard: {}\n",
            ),
            (
                "log/.collection.yaml",
                "name: log\nschema:\n  journal: {}\n",
            ),
        ],
    );
}

/// Every file below `dir` with its bytes, sorted by path: what a refusal
/// must leave as it was. The Index (`.scheda/index/`) and the lock of its
/// rounds (`.scheda/index.lock`) are left out: they are derived, and the
/// indexer of the handle writes them in the background. On Windows a lock
/// is mandatory, and the content of a lock file a handle holds cannot be
/// read: there the lock files of `.scheda/` stand in by their names alone.
pub fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel_path = path.strip_prefix(base).unwrap();
            // Compared as paths: Windows writes `.scheda\index`.
            if rel_path == Path::new(".scheda/index") || rel_path == Path::new(".scheda/index.lock")
            {
                continue;
            }
            let rel = rel_path.to_string_lossy().into_owned();
            let lock_file = rel_path.parent() == Some(Path::new(".scheda"))
                && rel_path.extension() == Some(std::ffi::OsStr::new("lock"));
            if path.is_dir() {
                out.push((format!("{rel}/"), Vec::new()));
                walk(base, &path, out);
            } else if cfg!(windows) && lock_file {
                out.push((rel, Vec::new()));
            } else {
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(dir, dir, &mut out);
    }
    out
}

/// Waits until a write of this process waits for the write lock of the Vault
/// at `path`, which the test holds (`held` files of `.scheda/commit.lock`
/// are open in the test already). A write that waits has the lock file open
/// and tries the lock again and again, so the open file is its mark.
///
/// Only Linux lists the open files of a process (`/proc/self/fd`). Elsewhere
/// no state shows a waiting write, and the wait is a margin of 300 ms: a
/// slow runner can make a test that follows it check less, never pass for a
/// wrong reason, as long as the test says so where it relies on it.
pub async fn write_waits_for_the_lock(path: &Path, held: usize) {
    let lock = path.join(".scheda/commit.lock");
    #[cfg(target_os = "linux")]
    {
        let lock = std::fs::canonicalize(&lock).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let open = std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
                .filter(|target| *target == lock)
                .count();
            if open > held {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no write waits for {}",
                lock.display()
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (lock, held);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// How long a test waits for a child process it started to end.
pub const CHILD_DEADLINE: Duration = Duration::from_secs(300);

/// A child process of a test: killed and reaped when dropped, so a test
/// that fails or panics before the child ends never leaves it running.
pub struct Child(std::process::Child);

impl Child {
    /// Starts `command`.
    pub fn spawn(command: &mut Command) -> Child {
        Child(command.spawn().unwrap())
    }

    /// Waits for the child to end, without holding a thread of the
    /// runtime. Past [`CHILD_DEADLINE`] the test fails, and the child is
    /// killed: a child that hangs fails the test instead of hanging it.
    pub async fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + CHILD_DEADLINE;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the child process did not end within {CHILD_DEADLINE:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Kills the child, unless it has ended, and reaps it.
    pub fn kill(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Runs `test` on a current-thread runtime, then on a multi-thread one:
/// Scheda never blocks a thread of the runtime, so it works on both.
pub fn both<F, Fut>(test: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    isolate_library_git();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test());
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test());
}

/// Opens the Vault at `path` with `options` and no port.
pub async fn open(path: &Path, options: OpenOptions) -> Result<DynVaultAsync, Error> {
    Scheda.open(opening(path, options)).await?.to_async_inline()
}

/// The collection `name` of `vault`.
pub async fn collection(vault: &DynVaultAsync, name: &str) -> Result<DynCollectionAsync, Error> {
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .await?
        .to_async_inline()
}

/// The mirror of `vault`.
pub async fn mirror(vault: &DynVaultAsync) -> Result<DynMirrorAsync, Error> {
    vault
        .mirror(buffa_types::google::protobuf::Empty::default())
        .await?
        .to_async_inline()
}

/// The next item of `stream`.
pub async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
}

/// Every item of `stream`, up to its first failure.
pub async fn all<T>(stream: impl Stream<Item = Result<T, Error>>) -> Result<Vec<T>, Error> {
    let mut stream = Box::pin(stream);
    let mut items = Vec::new();
    while let Some(item) = next(&mut stream).await {
        items.push(item?);
    }
    Ok(items)
}

/// Every item of `stream`, failures included.
#[cfg(feature = "index")]
pub async fn items<T>(stream: impl Stream<Item = T>) -> Vec<T> {
    let mut stream = Box::pin(stream);
    let mut items = Vec::new();
    while let Some(item) = next(&mut stream).await {
        items.push(item);
    }
    items
}

/// The requests of a batch, as the stream a batch call takes.
pub fn stream_of<T: Send + 'static>(
    items: Vec<T>,
) -> IterStream<std::vec::IntoIter<Result<T, Error>>> {
    IterStream::new(
        items
            .into_iter()
            .map(Ok)
            .collect::<Vec<Result<T, Error>>>()
            .into_iter(),
    )
}

/// The request that opens the Vault at `path` with `options` and no port;
/// a test sets the ports it wants on it.
pub fn opening(path: &Path, options: OpenOptions) -> VaultOpening {
    isolate_library_git();
    VaultOpening {
        path: Some(path.to_string_lossy().into_owned()),
        options: options.into(),
        ..VaultOpening::default()
    }
}
