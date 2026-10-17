//! The write path: one short lock, a commit first, the files after.
//!
//! Every write of a Vault goes through [`Committer::lock`] and
//! [`WriteLock::commit`]. A writer takes its turn among the writes of its
//! handle, then `flock` on `{vault}/.scheda/commit.lock`, which it waits for
//! while another handle, of this process or another, holds it. Holding the
//! lock it builds the commit from git objects with a temporary index and
//! moves `HEAD` by compare and swap; only then are the files written into
//! the work tree, and git's own index brought up to the commit for those
//! paths. A write never leaves a change in the work tree that is not
//! committed.
//!
//! The lock file is also the note of the write in flight. Before it moves
//! `HEAD` the writer writes the Vault-relative paths of its write into the
//! file (each one ended by a NUL byte) and flushes them; once the files are
//! written it empties the file. Whoever takes the lock and finds paths
//! there knows a writer died midway, and brings exactly those paths of the
//! work tree back to what `HEAD` holds. If the commit had landed, the files
//! become its files; if not, they were never touched. No other path is ever
//! touched: a person may have edited it.

// The Document writes and the mirror of the next layers use the rest.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the layers that write Documents and the mirror are not built yet"
    )
)]

use std::fs::{self, File, OpenOptions as FileOptions, TryLockError as FileLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::thread::sleep;
use std::time::{Duration, Instant};

use scheda_api::v1::{OpenOptions, VaultErrorCode};

use crate::error::{Context, Error, Fault, io};
use crate::fs::{Flock, atomic_write, reject_symlinks, restrict_dir};
use crate::git::{self, git};

/// How long a write waits for the lock when `write_timeout_ms` is unset.
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(60_000);

/// The first and the longest pause between two tries of the lock.
const FIRST_POLL: Duration = Duration::from_millis(1);
const LAST_POLL: Duration = Duration::from_millis(25);

/// The author and committer name used when neither the options nor the git
/// configuration name one.
const FALLBACK_NAME: &str = "Scheda";
const FALLBACK_EMAIL: &str = "scheda@invalid";

/// The mode of a file a write puts into the tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileMode {
    /// A plain file, `100644`.
    Regular,
    /// An executable file, `100755`.
    Executable,
}

impl FileMode {
    fn git(self) -> &'static str {
        match self {
            FileMode::Regular => "100644",
            FileMode::Executable => "100755",
        }
    }
}

/// What a write does to one path.
#[derive(Clone, Debug)]
pub(crate) enum Change {
    /// The path holds `bytes` afterwards.
    Put {
        /// Vault-relative POSIX path.
        path: String,
        /// The new bytes of the file.
        bytes: Vec<u8>,
        /// Its mode.
        mode: FileMode,
    },
    /// The path is gone afterwards.
    Remove {
        /// Vault-relative POSIX path.
        path: String,
    },
}

impl Change {
    /// A plain file with `bytes`.
    pub(crate) fn put(path: impl Into<String>, bytes: Vec<u8>) -> Change {
        Change::Put {
            path: path.into(),
            bytes,
            mode: FileMode::Regular,
        }
    }

    /// The path the change is about.
    pub(crate) fn path(&self) -> &str {
        match self {
            Change::Put { path, .. } | Change::Remove { path } => path,
        }
    }
}

/// The author and committer of the commits of a handle.
#[derive(Clone, Debug)]
struct Identity {
    name: String,
    email: String,
}

impl Identity {
    /// In order: `OpenOptions.author`, the git configuration of the
    /// repository at `root` (`user.name` and `user.email`), then
    /// `Scheda <scheda@invalid>`.
    fn resolve(options: &OpenOptions, root: &Path) -> Result<Identity, Error> {
        if let Some(author) = options.author.as_option()
            && let (Some(name), Some(email)) = (author.name.as_deref(), author.email.as_deref())
        {
            return Ok(Identity {
                name: name.to_owned(),
                email: email.to_owned(),
            });
        }
        let name = git::config_get(Some(root), "user.name")?;
        let email = git::config_get(Some(root), "user.email")?;
        Ok(match (name, email) {
            (Some(name), Some(email)) => Identity { name, email },
            _ => Identity {
                name: FALLBACK_NAME.to_owned(),
                email: FALLBACK_EMAIL.to_owned(),
            },
        })
    }
}

/// The write path of one handle.
#[derive(Debug)]
pub(crate) struct Committer {
    root: PathBuf,
    timeout: Duration,
    identity: Identity,
    /// The turn among the writes of this handle. It guards no data: the lock
    /// file guards the state of the Vault.
    turn: Mutex<()>,
}

impl Committer {
    /// The write path of the Vault at `root`, with the author and the
    /// timeout `options` give.
    pub(crate) fn new(root: &Path, options: &OpenOptions) -> Result<Committer, Error> {
        let timeout = options
            .write_timeout_ms
            .map_or(DEFAULT_TIMEOUT, |ms| Duration::from_millis(u64::from(ms)));
        Ok(Committer {
            root: root.to_path_buf(),
            timeout,
            identity: Identity::resolve(options, root).context("resolve the author")?,
            turn: Mutex::new(()),
        })
    }

    /// Takes the write lock: the turn among the writes of this handle, then
    /// `flock` on `commit.lock`, polled without blocking until the timeout
    /// runs out. A list of paths left in the file by a writer that died is
    /// recovered before the lock is returned.
    ///
    /// # Errors
    ///
    /// `ABORTED` (retryable) when the lock is not free within the timeout;
    /// nothing is written then. `STORAGE` or `REPLICATION` when the lock
    /// file or git fails.
    pub(crate) fn lock(&self) -> Result<WriteLock<'_>, Error> {
        let deadline = Instant::now() + self.timeout;
        let turn = self.take_turn(deadline)?;
        let file = open_lock_file(&self.root)?;
        take_flock(&file, deadline)?;
        let flock = Flock::held(file);
        recover(&self.root, flock.file()).context("recover an unfinished write")?;
        Ok(WriteLock {
            committer: self,
            _turn: turn,
            flock,
        })
    }

    /// Recovers the unfinished write a dead writer left, if one did. It
    /// takes the lock for that, with the timeout, and only when the lock
    /// file holds paths.
    ///
    /// # Errors
    ///
    /// As [`Committer::lock`].
    pub(crate) fn recover_pending(&self) -> Result<(), Error> {
        if pending(&self.root)? {
            drop(self.lock()?);
        }
        Ok(())
    }

    fn take_turn(&self, deadline: Instant) -> Result<MutexGuard<'_, ()>, Error> {
        let mut pause = FIRST_POLL;
        loop {
            match self.turn.try_lock() {
                Ok(guard) => return Ok(guard),
                // The mutex guards no data, so a holder that panicked left
                // nothing broken behind.
                Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {}
            }
            wait(&mut pause, deadline)?;
        }
    }
}

/// Sleeps one poll interval, or is `ABORTED` when `deadline` has passed.
fn wait(pause: &mut Duration, deadline: Instant) -> Result<(), Error> {
    if Instant::now() >= deadline {
        return Err(Fault::new(
            VaultErrorCode::Aborted,
            "another write holds the write lock; try again",
        )
        .into());
    }
    sleep(*pause);
    *pause = (*pause * 2).min(LAST_POLL);
    Ok(())
}

/// `{vault}/.scheda/commit.lock`.
fn lock_path(root: &Path) -> PathBuf {
    root.join(".scheda").join("commit.lock")
}

fn open_lock_file(root: &Path) -> Result<File, Error> {
    let path = lock_path(root);
    if let Some(dir) = path.parent()
        && !dir.is_dir()
    {
        fs::create_dir_all(dir)
            .map_err(io)
            .context("create .scheda")?;
        restrict_dir(dir)?;
    }
    let mut options = FileOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&path)
        .map_err(io)
        .context("open the lock file")
}

fn take_flock(file: &File, deadline: Instant) -> Result<(), Error> {
    let mut pause = FIRST_POLL;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(FileLockError::WouldBlock) => {}
            Err(FileLockError::Error(err)) => {
                return Err(io(err)).context("take the write lock");
            }
        }
        wait(&mut pause, deadline)?;
    }
}

/// Whether the lock file holds a list of paths: a write died midway, or one
/// is in flight. Reads the file without the lock.
pub(crate) fn pending(root: &Path) -> Result<bool, Error> {
    match fs::metadata(lock_path(root)) {
        Ok(meta) => Ok(meta.len() > 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(io(err)).context("look at the lock file"),
    }
}

/// The commit `HEAD` names, or `None` on an unborn branch.
pub(crate) fn read_head(root: &Path) -> Result<Option<String>, Error> {
    let out = git()
        .current_dir(root)
        .arg("rev-parse")
        .arg("--verify")
        .arg("-q")
        .arg("HEAD")
        .spawn()
        .context("read HEAD")?;
    match out.code() {
        Some(0) => Ok(Some(out.stdout_trim().context("read HEAD")?.to_owned())),
        Some(1) => Ok(None),
        _ => Err(out.failure()).context("read HEAD"),
    }
}

/// The write lock, held. Dropping it lets the lock go; it leaves the list
/// of paths alone, so a write that ended in the middle is recovered by the
/// next holder.
#[derive(Debug)]
pub(crate) struct WriteLock<'a> {
    committer: &'a Committer,
    _turn: MutexGuard<'a, ()>,
    flock: Flock,
}

impl WriteLock<'_> {
    /// The Vault the lock is for.
    pub(crate) fn root(&self) -> &Path {
        &self.committer.root
    }

    /// The commit `HEAD` names now, or `None` on an unborn branch.
    pub(crate) fn head(&self) -> Result<Option<String>, Error> {
        read_head(self.root())
    }

    /// Stores `changes` as one commit with `message`, then writes the files.
    ///
    /// The paths are checked first (a path that steps out of the Vault, or a
    /// symlink on a path, is `INVALID_ARGUMENT`). The new blobs are hashed
    /// by one `git hash-object`, the tree is built with one temporary index,
    /// and `HEAD` moves by `git update-ref HEAD <new> <old>`; a `HEAD` that
    /// moved under the write (a `git` command run by hand) makes the call
    /// `ABORTED` with nothing written to the work tree. A set of changes
    /// that leaves the tree as `HEAD` holds it makes no commit; its files
    /// are still written, so the work tree ends as the changes say.
    ///
    /// Returns the commit `HEAD` names afterwards.
    ///
    /// # Errors
    ///
    /// The codes above, and `STORAGE` or `REPLICATION` when a file or git
    /// fails.
    pub(crate) fn commit(
        &self,
        message: &str,
        changes: &[Change],
    ) -> Result<Option<String>, Error> {
        let root = self.root();
        let old = read_head(root)?;
        if changes.is_empty() {
            return Ok(old);
        }
        for change in changes {
            check_relative(change.path())?;
            reject_symlinks(root, &root.join(change.path())).context("check the path")?;
        }
        let new = build_commit(
            root,
            old.as_deref(),
            message,
            changes,
            &self.committer.identity,
        )?;
        write_list(self.flock.file(), changes)?;
        #[cfg(test)]
        hooks::after_list()?;
        if let Some(new) = &new {
            #[cfg(test)]
            hooks::before_swap(root);
            if let Err(err) = update_head(root, new, old.as_deref()) {
                // Nothing was written to the work tree: the note goes.
                empty_list(self.flock.file())?;
                return Err(err);
            }
            #[cfg(test)]
            hooks::after_swap()?;
        }
        write_files(root, changes)?;
        // The commit is stored and the files are written. A git index that
        // cannot be brought up now (a person's `git add` holds its lock)
        // keeps the note, and the next holder of the lock does it.
        if update_index(root, changes).is_ok() {
            empty_list(self.flock.file())?;
        }
        Ok(new.or(old))
    }
}

/// A Vault-relative POSIX path a write may name: no empty part, no `.` or
/// `..`, nothing that names git's own directory, no NUL, newline, or
/// backslash.
fn check_relative(path: &str) -> Result<(), Fault> {
    let bad = path.is_empty()
        || path.contains(['\0', '\n', '\\'])
        || path.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
        });
    if bad {
        return Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            "the path is not a path inside the Vault",
        ));
    }
    Ok(())
}

/// The temporary files of one commit; removed when it is dropped.
struct TmpFiles(Vec<PathBuf>);

impl Drop for TmpFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// Builds the commit object of `changes` on top of `old`, and returns its
/// id, or `None` when the tree does not change. Four to six git processes,
/// whatever the size of the write: `hash-object`, `read-tree`,
/// `update-index`, `write-tree`, `rev-parse` of the old tree, `commit-tree`.
fn build_commit(
    root: &Path,
    old: Option<&str>,
    message: &str,
    changes: &[Change],
    identity: &Identity,
) -> Result<Option<String>, Error> {
    let tmp = root.join(".scheda").join("tmp");
    fs::create_dir_all(&tmp)
        .map_err(io)
        .context("create .scheda/tmp")?;
    restrict_dir(&tmp)?;
    let mut files = TmpFiles(Vec::new());
    let hashes = hash_blobs(root, &tmp, changes, &mut files)?;
    let index = tmp.join("commit-index");
    let _ = fs::remove_file(&index);
    files.0.push(index.clone());
    if let Some(old) = old {
        git()
            .current_dir(root)
            .env("GIT_INDEX_FILE", &index)
            .arg("read-tree")
            .arg(old)
            .run()
            .context("read the tree of HEAD")?;
    }
    let zero = "0".repeat(hashes.first().map_or(40, String::len));
    let mut entries = Vec::new();
    let mut hashed = hashes.iter();
    for change in changes {
        match change {
            Change::Put { path, mode, .. } => {
                let hash = hashed.next().ok_or_else(|| {
                    Fault::new(
                        VaultErrorCode::Replication,
                        "git hashed fewer files than asked",
                    )
                })?;
                entries.extend_from_slice(format!("{} {hash}\t{path}\0", mode.git()).as_bytes());
            }
            Change::Remove { path } => {
                entries.extend_from_slice(format!("0 {zero}\t{path}\0").as_bytes());
            }
        }
    }
    git()
        .current_dir(root)
        .env("GIT_INDEX_FILE", &index)
        .arg("update-index")
        .arg("-z")
        .arg("--index-info")
        .input(entries)
        .run()
        .context("build the tree")?;
    let tree = git()
        .current_dir(root)
        .env("GIT_INDEX_FILE", &index)
        .arg("write-tree")
        .run()
        .context("write the tree")?
        .stdout_trim()?
        .to_owned();
    if let Some(old) = old {
        let old_tree = git()
            .current_dir(root)
            .arg("rev-parse")
            .arg("--verify")
            .arg(format!("{old}^{{tree}}"))
            .run()
            .context("read the tree of HEAD")?;
        if old_tree.stdout_trim()? == tree {
            return Ok(None);
        }
    }
    let mut commit = git()
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", &identity.name)
        .env("GIT_AUTHOR_EMAIL", &identity.email)
        .env("GIT_COMMITTER_NAME", &identity.name)
        .env("GIT_COMMITTER_EMAIL", &identity.email)
        .arg("commit-tree")
        .arg(&tree);
    if let Some(old) = old {
        commit = commit.arg("-p").arg(old);
    }
    let mut text = message.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let id = commit
        .input(text.into_bytes())
        .run()
        .context("write the commit")?
        .stdout_trim()?
        .to_owned();
    Ok(Some(id))
}

/// Writes the new bytes of the `Put` changes below `tmp` and hashes them
/// into the object store with one `git hash-object -w --stdin-paths`. The
/// ids come back in the order of the changes.
fn hash_blobs(
    root: &Path,
    tmp: &Path,
    changes: &[Change],
    files: &mut TmpFiles,
) -> Result<Vec<String>, Error> {
    let mut list = Vec::new();
    let mut count = 0usize;
    for change in changes {
        let Change::Put { bytes, .. } = change else {
            continue;
        };
        let file = tmp.join(format!("blob-{count}"));
        files.0.push(file.clone());
        fs::write(&file, bytes)
            .map_err(io)
            .context("write a blob to hash")?;
        list.extend_from_slice(file.as_os_str().as_encoded_bytes());
        list.push(b'\n');
        count += 1;
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    // `--no-filters`: the bytes are stored as they are, whatever the
    // attributes of the work tree say.
    let out = git()
        .current_dir(root)
        .arg("hash-object")
        .arg("-w")
        .arg("--no-filters")
        .arg("--stdin-paths")
        .input(list)
        .run()
        .context("hash the new files")?;
    let hashes: Vec<String> = out.stdout_str()?.lines().map(str::to_owned).collect();
    if hashes.len() != count {
        return Err(Fault::new(
            VaultErrorCode::Replication,
            "git hashed fewer files than asked",
        )
        .into());
    }
    Ok(hashes)
}

/// `git update-ref HEAD <new> <old>`: moves `HEAD` only if it still names
/// `old` (the zero id for an unborn branch). A `HEAD` that moved is
/// `ABORTED`.
fn update_head(root: &Path, new: &str, old: Option<&str>) -> Result<(), Error> {
    let zero = "0".repeat(new.len());
    let out = git()
        .current_dir(root)
        .arg("update-ref")
        .arg("HEAD")
        .arg(new)
        .arg(old.unwrap_or(&zero))
        .spawn()
        .context("move HEAD")?;
    if out.success() {
        return Ok(());
    }
    if read_head(root)?.as_deref() != old {
        return Err(Fault::new(
            VaultErrorCode::Aborted,
            "HEAD moved while the write was being stored; try again",
        )
        .into());
    }
    Err(out.failure()).context("move HEAD")
}

/// Writes the paths of `changes` into the lock file, each ended by a NUL
/// byte, and flushes them to disk.
fn write_list(file: &File, changes: &[Change]) -> Result<(), Error> {
    let mut bytes = Vec::new();
    for change in changes {
        bytes.extend_from_slice(change.path().as_bytes());
        bytes.push(0);
    }
    let mut handle = file;
    handle
        .set_len(0)
        .and_then(|()| handle.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| handle.write_all(&bytes))
        .and_then(|()| handle.sync_all())
        .map_err(io)
        .context("note the paths of the write")
}

/// Empties the lock file and flushes it.
fn empty_list(file: &File) -> Result<(), Error> {
    file.set_len(0)
        .and_then(|()| file.sync_all())
        .map_err(io)
        .context("clear the note of the write")
}

/// Writes the files of `changes` into the work tree: the removals first,
/// with the directories they empty, then each new file with
/// `fs::atomic_write`. A symlink on a path is refused.
fn write_files(root: &Path, changes: &[Change]) -> Result<(), Error> {
    for change in changes {
        if let Change::Remove { path } = change {
            remove_path(root, path)?;
        }
    }
    for change in changes {
        let Change::Put { path, bytes, mode } = change else {
            continue;
        };
        let target = root.join(path);
        reject_symlinks(root, &target).context("write a file")?;
        atomic_write(&target, bytes).context("write a file")?;
        set_mode(&target, *mode)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: FileMode) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let bits = match mode {
        FileMode::Regular => 0o644,
        FileMode::Executable => 0o755,
    };
    fs::set_permissions(path, fs::Permissions::from_mode(bits))
        .map_err(io)
        .context("set the mode of a file")
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: FileMode) -> Result<(), Error> {
    Ok(())
}

/// Removes the file at `rel` below `root` and the directories it leaves
/// empty, up to but not including `root`. A file that is not there is
/// nothing to do; a directory of that name is left alone.
fn remove_path(root: &Path, rel: &str) -> Result<(), Error> {
    let target = root.join(rel);
    reject_symlinks(root, &target).context("remove a file")?;
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => fs::remove_file(&target)
            .map_err(io)
            .context("remove a file")?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(io(err)).context("remove a file"),
    }
    prune_empty_parents(root, &target);
    Ok(())
}

/// Removes the empty directories above `path`, nearest first, and stops at
/// the first that is not empty or at `root`.
fn prune_empty_parents(root: &Path, path: &Path) {
    let mut dir = path.parent();
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) || fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
}

/// Brings git's own index up to the commit for the paths of `changes`:
/// `git update-index --add --remove`, which takes each path from the work
/// tree, so `git status` is clean for them.
fn update_index(root: &Path, changes: &[Change]) -> Result<(), Error> {
    git()
        .current_dir(root)
        .arg("update-index")
        .arg("--add")
        .arg("--remove")
        .arg("-z")
        .arg("--stdin")
        .input(paths_input(changes.iter().map(Change::path)))
        .run()
        .context("update the git index")?;
    Ok(())
}

fn paths_input<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
    }
    bytes
}

/// Recovers the write a dead writer left, if the lock file holds paths: the
/// paths go back to what `HEAD` holds, and the file is emptied. Called with
/// the lock held.
fn recover(root: &Path, lock: &File) -> Result<(), Error> {
    let (len, paths) = read_list(lock)?;
    if !paths.is_empty() {
        restore(root, &paths)?;
    }
    if len > 0 {
        empty_list(lock)?;
    }
    Ok(())
}

/// The size of the lock file, and the complete, safe paths in it. An entry the writer did not
/// finish (no NUL after it) belongs to a write that had not reached `HEAD`
/// and had touched no file; a path that is not inside the Vault is not ours
/// to touch. Both are passed over.
fn read_list(lock: &File) -> Result<(usize, Vec<String>), Error> {
    let mut handle = lock;
    let mut bytes = Vec::new();
    handle
        .seek(SeekFrom::Start(0))
        .and_then(|_| handle.read_to_end(&mut bytes))
        .map_err(io)
        .context("read the note of the write")?;
    let end = bytes.iter().rposition(|b| *b == 0).map_or(0, |at| at + 1);
    let paths = bytes[..end]
        .split(|b| *b == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| check_relative(path).is_ok())
        .map(str::to_owned)
        .collect();
    Ok((bytes.len(), paths))
}

/// Brings `paths` of the work tree and of git's index to what `HEAD` holds:
/// a path in `HEAD` is restored from it, a path not in `HEAD` is removed
/// from both. No other path is touched.
fn restore(root: &Path, paths: &[String]) -> Result<(), Error> {
    let (present, absent) = split_by_head(root, paths)?;
    if !present.is_empty() {
        git()
            .current_dir(root)
            .env("GIT_LITERAL_PATHSPECS", "1")
            .arg("checkout")
            .arg("HEAD")
            .arg("--pathspec-from-file=-")
            .arg("--pathspec-file-nul")
            .input(paths_input(present.iter().map(String::as_str)))
            .run()
            .context("restore files from HEAD")?;
    }
    if !absent.is_empty() {
        for rel in &absent {
            let target = root.join(rel);
            if reject_symlinks(root, &target).is_err() {
                continue;
            }
            match fs::symlink_metadata(&target) {
                Ok(meta) if !meta.is_dir() => {
                    fs::remove_file(&target)
                        .map_err(io)
                        .context("remove a file that HEAD does not hold")?;
                    prune_empty_parents(root, &target);
                }
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(io(err)).context("look at a file of an unfinished write"),
            }
        }
        git()
            .current_dir(root)
            .arg("update-index")
            .arg("--force-remove")
            .arg("-z")
            .arg("--stdin")
            .input(paths_input(absent.iter().map(String::as_str)))
            .run()
            .context("drop files that HEAD does not hold from the git index")?;
    }
    Ok(())
}

/// Splits `paths` into those `HEAD` holds as a file and the rest, in their
/// order. One `git cat-file --batch-check`.
fn split_by_head(root: &Path, paths: &[String]) -> Result<(Vec<String>, Vec<String>), Error> {
    if read_head(root)?.is_none() {
        return Ok((Vec::new(), paths.to_vec()));
    }
    let mut input = String::new();
    for path in paths {
        input.push_str("HEAD:");
        input.push_str(path);
        input.push('\n');
    }
    let out = git()
        .current_dir(root)
        .arg("cat-file")
        .arg("--batch-check")
        .input(input.into_bytes())
        .run()
        .context("ask what HEAD holds")?;
    let lines: Vec<&str> = out.stdout_str()?.lines().collect();
    if lines.len() != paths.len() {
        return Err(Fault::new(
            VaultErrorCode::Replication,
            "git answered for fewer paths than asked",
        )
        .into());
    }
    let (mut present, mut absent) = (Vec::new(), Vec::new());
    for (path, line) in paths.iter().zip(lines) {
        if line.split(' ').nth(1) == Some("blob") {
            present.push(path.clone());
        } else {
            absent.push(path.clone());
        }
    }
    Ok((present, absent))
}

/// Test hooks that stop a write where a kill would, and run a plain `git`
/// command in the middle of one. They exist in test builds only.
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::{Cell, RefCell};
    use std::path::Path;

    use scheda_api::v1::VaultErrorCode;

    use crate::error::{Error, Fault};

    type Callback = Box<dyn FnOnce(&Path)>;

    thread_local! {
        static STOP_AFTER_LIST: Cell<bool> = const { Cell::new(false) };
        static STOP_AFTER_SWAP: Cell<bool> = const { Cell::new(false) };
        static BEFORE_SWAP: RefCell<Option<Callback>> = const { RefCell::new(None) };
    }

    /// The next write of this thread stops right after it wrote the list of
    /// its paths, before it moves `HEAD`.
    pub(crate) fn stop_after_list() {
        STOP_AFTER_LIST.with(|stop| stop.set(true));
    }

    /// The next write of this thread stops right after it moved `HEAD`,
    /// before it writes the files.
    pub(crate) fn stop_after_swap() {
        STOP_AFTER_SWAP.with(|stop| stop.set(true));
    }

    /// The next write of this thread runs `callback` with the Vault path
    /// between the read of `HEAD` and the swap.
    pub(crate) fn run_before_swap(callback: impl FnOnce(&Path) + 'static) {
        BEFORE_SWAP.with(|cell| *cell.borrow_mut() = Some(Box::new(callback)));
    }

    fn stopped() -> Error {
        Fault::new(
            VaultErrorCode::Storage,
            "the write was stopped by a test hook",
        )
        .into()
    }

    pub(super) fn after_list() -> Result<(), Error> {
        if STOP_AFTER_LIST.with(Cell::take) {
            return Err(stopped());
        }
        Ok(())
    }

    pub(super) fn before_swap(root: &Path) {
        if let Some(callback) = BEFORE_SWAP.with(|cell| cell.borrow_mut().take()) {
            callback(root);
        }
    }

    pub(super) fn after_swap() -> Result<(), Error> {
        if STOP_AFTER_SWAP.with(Cell::take) {
            return Err(stopped());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use buffa_types::google::protobuf::Empty;
    use scheda_api::v1::{Collection, OpenOptions, VaultErrorCode};

    use super::{Change, FileMode, check_relative, hooks, pending, read_head};
    use crate::git::git;
    use crate::vault::Vault;

    /// A Vault on disk made by `Vault::open`, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-commit-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }

        fn open(&self) -> Vault {
            Vault::open(&self.0, OpenOptions::default().with_write_timeout_ms(2000)).unwrap()
        }

        fn out(&self, args: &[&str]) -> String {
            let mut cmd = git().current_dir(&self.0);
            for arg in args {
                cmd = cmd.arg(arg);
            }
            cmd.run()
                .unwrap()
                .stdout_str()
                .unwrap()
                .trim_end()
                .to_owned()
        }

        fn status(&self) -> String {
            self.out(&["status", "--porcelain"])
        }

        fn commits(&self) -> usize {
            self.out(&["rev-list", "--count", "HEAD"]).parse().unwrap()
        }

        fn list(&self) -> Vec<u8> {
            std::fs::read(self.0.join(".scheda/commit.lock")).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn standard(name: &str) -> Collection {
        serde_json::from_value(serde_json::json!({"name": name, "schema": {"standard": {}}}))
            .unwrap()
    }

    #[test]
    fn a_write_stopped_after_the_list_stores_nothing_and_touches_nothing() {
        let scratch = Scratch::new("list");
        let vault = scratch.open();
        let before = scratch.out(&["rev-parse", "HEAD"]);
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        hooks::stop_after_list();
        let error = vault.create_collection(standard("notes")).unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        assert_eq!(
            scratch.out(&["rev-parse", "HEAD"]),
            before,
            "nothing stored"
        );
        assert!(
            !scratch.0.join("notes").exists(),
            "the work tree is as it was"
        );
        assert_eq!(scratch.list(), b"notes/.collection.yaml\0", "paths only");
        // The next write of the running handle recovers the note: the path
        // goes to what HEAD holds, which is nothing, and the file stays out.
        vault.create_collection(standard("log")).unwrap();
        assert!(scratch.list().is_empty());
        assert!(!scratch.0.join("notes").exists());
        assert!(scratch.0.join("log/.collection.yaml").is_file());
        assert_eq!(
            scratch.status(),
            "?? person.txt",
            "a person's file is not ours"
        );
        // Nothing of the stopped write was declared.
        vault.create_collection(standard("notes")).unwrap();
        assert_eq!(scratch.commits(), 3);
    }

    #[test]
    fn a_write_stopped_after_the_swap_is_completed_by_the_next_open() {
        let scratch = Scratch::new("swap-open");
        let vault = scratch.open();
        let commits = scratch.commits();
        std::fs::write(
            scratch.0.join(".gitignore"),
            ".scheda/\n.isolated/\n# edited\n",
        )
        .unwrap();
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        hooks::stop_after_swap();
        let error = vault.create_collection(standard("notes")).unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        assert_eq!(scratch.commits(), commits + 1, "the commit landed");
        assert!(!scratch.0.join("notes").exists(), "the files lag");
        assert!(pending(&scratch.0).unwrap());
        drop(vault);
        let vault = scratch.open();
        assert!(scratch.0.join("notes/.collection.yaml").is_file());
        assert!(scratch.list().is_empty());
        assert!(vault.get_collection(key("notes")).is_ok());
        assert_eq!(
            scratch.status(),
            " M .gitignore\n?? person.txt",
            "exactly the paths of the write were brought to HEAD"
        );
    }

    #[test]
    fn a_write_stopped_after_the_swap_is_completed_by_the_next_write_of_the_handle() {
        let scratch = Scratch::new("swap-write");
        let vault = scratch.open();
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        hooks::stop_after_swap();
        assert!(vault.create_collection(standard("notes")).is_err());
        assert!(!scratch.0.join("notes").exists());
        vault.create_collection(standard("log")).unwrap();
        assert!(scratch.0.join("notes/.collection.yaml").is_file());
        assert!(scratch.0.join("log/.collection.yaml").is_file());
        assert!(scratch.list().is_empty());
        assert_eq!(scratch.status(), "?? person.txt");
        // The handle read the collections again when HEAD had moved.
        let error = vault.create_collection(standard("notes")).unwrap_err();
        assert!(error.is(VaultErrorCode::AlreadyExists));
        assert_eq!(
            vault
                .list_collections(Empty::default())
                .unwrap()
                .collections
                .len(),
            2
        );
    }

    #[test]
    fn a_git_commit_between_the_read_and_the_swap_aborts_the_write() {
        let scratch = Scratch::new("race");
        let vault = scratch.open();
        let commits = scratch.commits();
        hooks::run_before_swap(|root| {
            git()
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "Hand")
                .env("GIT_AUTHOR_EMAIL", "hand@example.test")
                .env("GIT_COMMITTER_NAME", "Hand")
                .env("GIT_COMMITTER_EMAIL", "hand@example.test")
                .arg("commit")
                .arg("--quiet")
                .arg("--allow-empty")
                .arg("--message")
                .arg("by hand")
                .run()
                .unwrap();
        });
        let error = vault.create_collection(standard("notes")).unwrap_err();
        assert!(error.is(VaultErrorCode::Aborted));
        assert!(error.retryable());
        assert_eq!(scratch.commits(), commits + 1, "only the hand-made commit");
        assert!(!scratch.0.join("notes").exists());
        assert!(scratch.list().is_empty());
        assert_eq!(scratch.status(), "");
        vault.create_collection(standard("notes")).unwrap();
        assert_eq!(scratch.commits(), commits + 2);
        assert_eq!(scratch.status(), "");
    }

    #[test]
    fn recovery_restores_listed_paths_only_and_passes_over_unsafe_entries() {
        let scratch = Scratch::new("restore");
        let vault = scratch.open();
        drop(vault);
        // A person's edits: one of a listed path, one of another.
        std::fs::write(scratch.0.join(".gitignore"), "edited\n").unwrap();
        std::fs::write(scratch.0.join(".scheda.yaml"), "version: 2\n# edited\n").unwrap();
        std::fs::create_dir_all(scratch.0.join("dir")).unwrap();
        std::fs::write(scratch.0.join("dir/ghost.txt"), "not in HEAD").unwrap();
        std::fs::write(scratch.0.join("kept.txt"), "kept").unwrap();
        let outside = scratch
            .0
            .parent()
            .unwrap()
            .join("scheda-commit-outside.txt");
        std::fs::write(&outside, "outside").unwrap();
        let mut note = Vec::new();
        for path in [
            ".gitignore",
            "dir/ghost.txt",
            "../scheda-commit-outside.txt",
            ".git/config",
        ] {
            note.extend_from_slice(path.as_bytes());
            note.push(0);
        }
        note.extend_from_slice(b".scheda.yaml"); // no NUL: not finished
        std::fs::write(scratch.0.join(".scheda/commit.lock"), &note).unwrap();
        let _vault = scratch.open();
        assert!(scratch.list().is_empty());
        assert_eq!(
            std::fs::read_to_string(scratch.0.join(".gitignore")).unwrap(),
            ".scheda/\n.isolated/\n"
        );
        assert!(
            !scratch.0.join("dir").exists(),
            "the path and its empty folder go"
        );
        assert!(scratch.0.join("kept.txt").is_file());
        assert!(
            std::fs::read_to_string(scratch.0.join(".scheda.yaml"))
                .unwrap()
                .contains("# edited"),
            "an unfinished entry is not a path"
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside");
        std::fs::remove_file(outside).unwrap();
        assert_eq!(scratch.status(), " M .scheda.yaml\n?? kept.txt");
    }

    #[test]
    fn a_commit_stores_modes_removals_and_nothing_twice() {
        let scratch = Scratch::new("changes");
        let vault = scratch.open();
        let committer = vault.committer();
        let first = {
            let lock = committer.lock().unwrap();
            lock.commit(
                "two files",
                &[
                    Change::Put {
                        path: "a/b/run.sh".to_owned(),
                        bytes: b"#!/bin/sh\n".to_vec(),
                        mode: FileMode::Executable,
                    },
                    Change::put("a/plain.txt", b"x\r\ny".to_vec()),
                ],
            )
            .unwrap()
        };
        assert_eq!(first, read_head(&scratch.0).unwrap());
        assert_eq!(scratch.status(), "");
        let listing = scratch.out(&["ls-files", "--stage"]);
        assert!(listing.contains("100755"), "{listing}");
        assert_eq!(
            std::fs::read(scratch.0.join("a/plain.txt")).unwrap(),
            b"x\r\ny",
            "bytes are stored as they are"
        );
        // The same bytes again change nothing and make no commit.
        let same = {
            let lock = committer.lock().unwrap();
            lock.commit("again", &[Change::put("a/plain.txt", b"x\r\ny".to_vec())])
                .unwrap()
        };
        assert_eq!(same, first);
        assert!(scratch.list().is_empty());
        // A removal takes the file and the folders it empties.
        {
            let lock = committer.lock().unwrap();
            lock.commit(
                "gone",
                &[Change::Remove {
                    path: "a/b/run.sh".to_owned(),
                }],
            )
            .unwrap();
        }
        assert!(!scratch.0.join("a/b").exists());
        assert!(scratch.0.join("a/plain.txt").is_file());
        assert_eq!(scratch.status(), "");
        assert_eq!(scratch.out(&["log", "-1", "--format=%s"]), "gone");
        assert_eq!(
            scratch.out(&["log", "-1", "--format=%an <%ae>"]),
            "Scheda <scheda@invalid>"
        );
    }

    #[test]
    fn paths_that_step_out_or_name_git_are_refused_before_anything_is_written() {
        let scratch = Scratch::new("paths");
        let vault = scratch.open();
        let committer = vault.committer();
        let lock = committer.lock().unwrap();
        for bad in ["../x", "/x", "a//b", ".git/config", "a/./b", "", "a\\b"] {
            let error = lock
                .commit("bad", &[Change::put(bad, Vec::new())])
                .unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument), "{bad}");
        }
        assert!(check_relative("a/b").is_ok());
        assert!(scratch.list().is_empty());
        assert_eq!(scratch.status(), "");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_on_the_path_of_a_write_is_refused_and_nothing_is_stored() {
        let scratch = Scratch::new("symlink");
        let vault = scratch.open();
        let commits = scratch.commits();
        let target = scratch
            .0
            .parent()
            .unwrap()
            .join("scheda-commit-link-target");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, scratch.0.join("notes")).unwrap();
        let error = vault.create_collection(standard("notes")).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        assert_eq!(scratch.commits(), commits);
        assert!(scratch.list().is_empty());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        std::fs::remove_dir_all(target).unwrap();
    }

    fn key(name: &str) -> scheda_api::v1::CollectionKey {
        scheda_api::v1::CollectionKey::default().with_name(name)
    }
}
