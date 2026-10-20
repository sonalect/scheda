//! Helpers shared by the integration tests: a scratch directory, plain
//! `git` with the user's configuration kept out, a Vault built on disk
//! the way a user would build one, without the library, the objects of the
//! opening of a Vault by the library, and the two runtimes every test runs
//! on.

use std::future::{Future, poll_fn};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use protocontract::{IterStream, Stream};
use scheda::v1::{
    CollectionKey, DynCollectionAsync, DynMirrorAsync, DynVaultAsync, OpenOptions, SchedaAsync,
    VaultAsync, VaultOpening,
};
use scheda::{Error, Scheda};
use tokio::runtime::Builder;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that is removed when it is dropped.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A new, empty scratch directory.
    pub fn new(name: &str) -> Scratch {
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!("scheda-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Scratch { root }
    }

    /// A path below the directory.
    pub fn join(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `git args` to run in `dir`, with the user's and the system's git
/// configuration left out and an identity of its own.
pub fn command(dir: &Path, args: &[&str]) -> Command {
    let absent = std::env::temp_dir().join("scheda-absent-gitconfig");
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

/// A Vault of generation 2 at `dir` with the collection `notes` (a
/// standard collection) and the collection `log` (a journal).
pub fn vault(dir: &Path) {
    work_tree(
        dir,
        &[
            (".scheda.yaml", "version: 2\n"),
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
/// indexer of the handle writes them in the background.
pub fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if rel == ".scheda/index" || rel == ".scheda/index.lock" {
                continue;
            }
            if path.is_dir() {
                out.push((format!("{rel}/"), Vec::new()));
                walk(base, &path, out);
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

/// Runs `test` on a current-thread runtime, then on a multi-thread one:
/// Scheda never blocks a thread of the runtime, so it works on both.
pub fn both<F, Fut>(test: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
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
    VaultOpening {
        path: Some(path.to_string_lossy().into_owned()),
        options: options.into(),
        ..VaultOpening::default()
    }
}
