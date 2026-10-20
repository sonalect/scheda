//! The write path: one short lock, a commit first, the files after.
//!
//! Every write of a Vault goes through [`Committer::acquire`] and
//! [`WriteLock::commit_in`]. A writer takes its turn among the writes of its
//! handle, first come first served, then `flock` on
//! `{vault}/.scheda/commit.lock`, which it waits for while another handle, of
//! this process or another, holds it. Both waits are `await`s: no thread is
//! held while another writer has the lock, and a writer whose call is
//! dropped while it waits leaves the queue and takes nothing. Holding the
//! lock, in one blocking step, it builds the commit in the object database
//! through the git library:
//! the new blobs, a tree built on the tree of the commit its checks read,
//! and the commit. `HEAD` moves by compare and swap on that commit, so a
//! `git` command run by hand meanwhile makes the write `ABORTED`.
//!
//! Only then are the files written into the work tree, and git's own index
//! brought up to the commit for those paths. A file a person changed and did
//! not commit is left as it is: the write lands in the commit, the index
//! gets the new blob, and git shows the person's file as changed. Scheda
//! never uses the library's checkout: it writes the bytes it stored, with
//! owner-only modes where the folder asks for them, and refuses a symlink on
//! a path.
//!
//! The lock file is also the note of the write in flight. Before it moves
//! `HEAD` the writer writes the Vault-relative paths of its write into the
//! file (each one ended by a NUL byte) and flushes them; once the files are
//! written it empties the file. Whoever takes the lock and finds paths
//! there knows a writer died midway, and brings exactly those paths to what
//! `HEAD` holds, with the same care for a person's edit. If the commit had
//! landed, the files become its files; if not, they were never touched. No
//! other path is ever touched.
//!
//! A write goes to one of two repositories: the Vault's own, or the nested
//! one under `.isolated/` that holds the Documents of isolated collections.
//! Both are written under the one lock and share the one note. Every path in
//! the note is Vault-relative; a path under `.isolated/` belongs to the
//! nested repository, any other to the Vault's, and recovery settles each
//! against the `HEAD` and the index of its own repository.

use std::fs::{self, File, OpenOptions as FileOptions, TryLockError as FileLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use git2::build::TreeUpdateBuilder;
use git2::{ErrorCode, Index, IndexEntry, IndexTime, ObjectType, Oid, Repository, Signature};
use scheda_api::v1::{OpenOptions, VaultErrorCode};
use scheda_blocking::{Blocking, blocking};
use tokio::sync::{Mutex as Queue, OwnedMutexGuard};
use tokio::time::Instant;

use crate::error::{Context, Error, Fault, io, lib_fault};
use crate::fs::{Flock, atomic_write, reject_symlinks, restrict_dir};
use crate::index::State;
use crate::indexer::Signal;
use crate::isolated;
use crate::pack;
pub(crate) use crate::repo::Repo;
use crate::repo::{NodeKind, Repos, Snap};

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
    fn git(self) -> git2::FileMode {
        match self {
            FileMode::Regular => git2::FileMode::Blob,
            FileMode::Executable => git2::FileMode::BlobExecutable,
        }
    }

    fn of(executable: bool) -> FileMode {
        if executable {
            FileMode::Executable
        } else {
            FileMode::Regular
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
    /// repository of the Vault (`user.name` and `user.email`), then
    /// `Scheda <scheda@invalid>`. A name or an address the library cannot
    /// sign with is `INVALID_ARGUMENT`.
    fn resolve(options: &OpenOptions, repos: &Repos) -> Result<Identity, Error> {
        let identity = Self::pick(options, repos)?;
        Signature::now(&identity.name, &identity.email).map_err(|err| {
            Fault::new(
                VaultErrorCode::InvalidArgument,
                "the author of the commits needs a name and an address git can sign with",
            )
            .with_source(err)
        })?;
        Ok(identity)
    }

    fn pick(options: &OpenOptions, repos: &Repos) -> Result<Identity, Error> {
        if let Some(author) = options.author.as_option()
            && let (Some(name), Some(email)) = (author.name.as_deref(), author.email.as_deref())
        {
            return Ok(Identity {
                name: name.to_owned(),
                email: email.to_owned(),
            });
        }
        let Some(lease) = repos.lease(Repo::Main)? else {
            return Ok(Identity::fallback());
        };
        let config = lease.config().map_err(lib_fault)?;
        let read = |key: &str| {
            config
                .get_string(key)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        Ok(match (read("user.name"), read("user.email")) {
            (Some(name), Some(email)) => Identity { name, email },
            _ => Identity::fallback(),
        })
    }

    fn fallback() -> Identity {
        Identity {
            name: FALLBACK_NAME.to_owned(),
            email: FALLBACK_EMAIL.to_owned(),
        }
    }

    fn signature(&self) -> Result<Signature<'static>, Fault> {
        Signature::now(&self.name, &self.email).map_err(lib_fault)
    }
}

/// The write path of one handle.
#[derive(Debug)]
pub(crate) struct Committer {
    root: PathBuf,
    timeout: Duration,
    identity: Identity,
    repos: Arc<Repos>,
    /// The turn among the writes of this handle: a fair queue, so the
    /// writes of the handle take the lock in the order they asked for it.
    /// It guards no data: the lock file guards the state of the Vault.
    turn: Arc<Queue<()>>,
    /// Woken when a commit of this handle moves `HEAD`: the indexer looks
    /// at once.
    signal: Arc<Signal>,
}

impl Committer {
    /// The write path of the Vault at `root`, with the author and the
    /// timeout `options` give.
    pub(crate) fn new(
        _cx: &Blocking,
        root: &Path,
        options: &OpenOptions,
        repos: Arc<Repos>,
    ) -> Result<Committer, Error> {
        let timeout = options
            .write_timeout_ms
            .map_or(DEFAULT_TIMEOUT, |ms| Duration::from_millis(u64::from(ms)));
        Ok(Committer {
            root: root.to_path_buf(),
            timeout,
            identity: Identity::resolve(options, &repos).context("resolve the author")?,
            repos,
            turn: Arc::new(Queue::new(())),
            signal: Arc::new(Signal::default()),
        })
    }

    /// What this handle's commits wake: the indexer's signal.
    pub(crate) fn signal(&self) -> Arc<Signal> {
        Arc::clone(&self.signal)
    }

    /// Waits for the write lock: the turn among the writes of this handle,
    /// then `flock` on `commit.lock`, tried without blocking with a pause
    /// between two tries (1 ms, doubling up to 25 ms), until the timeout
    /// runs out. Nothing is recovered yet: [`Committer::adopt`] does that,
    /// in the step that writes. Dropped while it waits, it takes nothing.
    ///
    /// # Errors
    ///
    /// `ABORTED` (retryable) when the lock is not free within the timeout;
    /// nothing is written then. `STORAGE` when the lock file cannot be
    /// opened.
    pub(crate) async fn acquire(&self) -> Result<Held, Error> {
        let deadline = Instant::now() + self.timeout;
        let turn = tokio::time::timeout_at(deadline, Arc::clone(&self.turn).lock_owned())
            .await
            .map_err(|_| lock_busy())?;
        let flock = flock_until(&self.root, LOCK_FILE, deadline, lock_busy).await?;
        Ok(Held { _turn: turn, flock })
    }

    /// Waits for the fetch lock, `flock` on `{vault}/.scheda/fetch.lock`,
    /// tried the way [`Committer::acquire`] tries the write lock, until the
    /// write timeout runs out. The pulls of every handle and process of the
    /// Vault take it one at a time for the `git fetch` they run, so two
    /// fetches never update the same reference; a write never takes it, and
    /// so is never held up by the network. The lock is let go when the
    /// returned value is dropped.
    ///
    /// # Errors
    ///
    /// `ABORTED` (retryable) when another pull's fetch holds the lock past
    /// the timeout. `STORAGE` when the lock file cannot be opened.
    pub(crate) async fn acquire_fetch(&self) -> Result<Flock, Error> {
        let deadline = Instant::now() + self.timeout;
        flock_until(&self.root, FETCH_LOCK_FILE, deadline, fetch_busy).await
    }

    /// The write lock for a turn and a file lock already taken: recovers
    /// the unfinished write a dead writer left, if one did.
    ///
    /// # Errors
    ///
    /// `STORAGE` or `REPLICATION` when the recovery fails.
    pub(crate) fn adopt(&self, _cx: &Blocking, held: Held) -> Result<WriteLock<'_>, Error> {
        recover(&self.root, &self.repos, held.flock.file())
            .context("recover an unfinished write")?;
        Ok(WriteLock {
            committer: self,
            held,
        })
    }
}

/// The turn and the file lock of a write: it owns everything it holds, so
/// it can wait in a task and cross to the thread that writes, where it
/// becomes a [`WriteLock`] with [`Committer::adopt`]. Dropping it lets the
/// lock go.
#[derive(Debug)]
pub(crate) struct Held {
    _turn: OwnedMutexGuard<()>,
    flock: Flock,
}

/// Takes `flock` on `{vault}/.scheda/{name}`, tried without blocking with a
/// pause between two tries (1 ms, doubling up to 25 ms), until `deadline`.
/// `busy` makes the failure of a lock that stayed taken until then.
async fn flock_until(
    root: &Path,
    name: &'static str,
    deadline: Instant,
    busy: fn() -> Error,
) -> Result<Flock, Error> {
    let root = root.to_path_buf();
    let file = blocking(move |cx| open_lock_file(cx, &root, name)).await?;
    let mut pause = FIRST_POLL;
    loop {
        // A try never waits: `flock` without blocking returns at once.
        match file.try_lock() {
            Ok(()) => return Ok(Flock::held(file)),
            Err(FileLockError::WouldBlock) => {}
            Err(FileLockError::Error(err)) => {
                return Err(io(err)).context("take the lock");
            }
        }
        if Instant::now() >= deadline {
            return Err(busy());
        }
        tokio::time::sleep(grow(&mut pause)).await;
    }
}

/// The `ABORTED` failure of a lock that stayed taken until the deadline.
fn lock_busy() -> Error {
    Fault::new(
        VaultErrorCode::Aborted,
        "another write holds the write lock; try again",
    )
    .into()
}

/// The `ABORTED` failure of a fetch lock that stayed taken until the
/// deadline.
fn fetch_busy() -> Error {
    Fault::new(
        VaultErrorCode::Aborted,
        "another pull is fetching from the mirror; try again",
    )
    .into()
}

/// The pause to take now; the next one is twice as long, up to
/// [`LAST_POLL`].
fn grow(pause: &mut Duration) -> Duration {
    let now = *pause;
    *pause = (now * 2).min(LAST_POLL);
    now
}

/// The name of the write lock's file under `.scheda/`.
const LOCK_FILE: &str = "commit.lock";

/// The name of the fetch lock's file under `.scheda/`.
const FETCH_LOCK_FILE: &str = "fetch.lock";

/// `{vault}/.scheda/commit.lock`.
fn lock_path(root: &Path) -> PathBuf {
    root.join(".scheda").join(LOCK_FILE)
}

/// Opens `{vault}/.scheda/{name}`, owner-only, creating it and `.scheda/`
/// on first use.
fn open_lock_file(_cx: &Blocking, root: &Path, name: &str) -> Result<File, Error> {
    let path = root.join(".scheda").join(name);
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

/// Whether the lock file holds a list of paths: a write died midway, or one
/// is in flight. Reads the file without the lock.
pub(crate) fn pending(_cx: &Blocking, root: &Path) -> Result<bool, Error> {
    match fs::metadata(lock_path(root)) {
        Ok(meta) => Ok(meta.len() > 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(io(err)).context("look at the lock file"),
    }
}

/// The write lock, held, inside a blocking step. Dropping it lets the lock
/// go; it leaves the list of paths alone, so a write that ended in the
/// middle is recovered by the next holder.
#[derive(Debug)]
pub(crate) struct WriteLock<'a> {
    committer: &'a Committer,
    held: Held,
}

impl WriteLock<'_> {
    /// The Vault the lock is for.
    pub(crate) fn root(&self) -> &Path {
        &self.committer.root
    }

    /// The state of `repo` as `HEAD` names it now: what the checks of a
    /// write read, and what its commit is built on.
    pub(crate) fn snapshot(&self, repo: Repo) -> Result<Snap, Fault> {
        self.committer.repos.snapshot(repo)
    }

    /// The state of the Vault after a write that left the repository `repo`
    /// at `commit`: that commit, and `HEAD` of the other repository, read
    /// now, while this lock is held. Scheda moves `HEAD` only under this
    /// lock (a write, a pull), so the state names exactly what the write
    /// made; a `git` command run by hand is not bound by it, and shows in
    /// the state as the commit it left. For a write that stored nothing,
    /// `commit` is the commit its checks read, and the state is the one the
    /// write found.
    pub(crate) fn state_after(&self, repo: Repo, commit: Option<Oid>) -> Result<State, Fault> {
        Ok(match repo {
            Repo::Main => State::new(commit, self.snapshot(Repo::Isolated)?.commit()),
            Repo::Isolated => State::new(self.snapshot(Repo::Main)?.commit(), commit),
        })
    }

    /// Stores `changes` as one commit with `message` on top of the commit of
    /// `snap`, which is a snapshot of `repo`, then writes the files.
    ///
    /// The paths are Vault-relative and all belong to the repository of
    /// `snap` (a path under `.isolated/` is the nested repository's). They
    /// are checked first (a path that steps out of the Vault, or a symlink
    /// on a path, is `INVALID_ARGUMENT`; so is a file where a folder is, or
    /// the reverse), and a `HEAD` that does not name a branch is
    /// `FAILED_PRECONDITION`. The new blobs, the tree, and the commit are
    /// written to the object database, no process is started, and `HEAD`
    /// moves by compare and swap: a branch that moved from the commit of
    /// `snap` (a `git` command run by hand) makes the call `ABORTED` with
    /// nothing written to the work tree. A set of changes that leaves the
    /// tree as `snap` holds it makes no commit and touches no file. The
    /// nested repository must exist already ([`isolated::ensure`]).
    ///
    /// Then each path is settled in the work tree and in git's index: a file
    /// that is what `snap` held (the same bytes, or absent where `snap` held
    /// nothing) becomes the new file, or is removed; a file a person changed
    /// is left as it is, and the index gets the new state, so git shows it as
    /// changed.
    ///
    /// Returns the commit `HEAD` of the repository names afterwards.
    ///
    /// # Errors
    ///
    /// The codes above, and `STORAGE` or `REPLICATION` when a file or git
    /// fails.
    pub(crate) fn commit_in(
        &self,
        snap: &Snap,
        message: &str,
        changes: &[Change],
    ) -> Result<Option<Oid>, Error> {
        let vault = self.root();
        let repo = snap.repo();
        let old = snap.commit();
        if changes.is_empty() {
            return Ok(old);
        }
        let Some(git) = snap.repository() else {
            return Err(Fault::new(
                VaultErrorCode::Replication,
                "the repository to write to is not there",
            )
            .into());
        };
        for change in changes {
            check_relative(change.path())?;
            repo.inner(change.path())?;
            reject_symlinks(vault, &vault.join(change.path())).context("check the path")?;
        }
        let branch = branch_of(git)?;
        let Some((new, blobs)) = build_commit(
            git,
            snap,
            message,
            changes,
            &self.committer.identity.signature()?,
        )?
        else {
            return Ok(old);
        };
        let paths: Vec<&str> = changes.iter().map(Change::path).collect();
        write_list(self.held.flock.file(), &paths)?;
        #[cfg(test)]
        hooks::after_list(vault)?;
        #[cfg(test)]
        hooks::before_swap(vault, &repo.root(vault));
        if let Err(err) = swap(git, &branch, old, new, message) {
            return Err(after_failed_swap(self.held.flock.file(), err));
        }
        #[cfg(test)]
        hooks::after_swap(vault)?;
        self.committer.signal.moved();
        let plan = plan_of(snap, changes, &blobs)?;
        // The commit is stored. A git index that cannot be brought up now (a
        // person's `git add` holds its lock) keeps the note, and the next
        // holder of the lock does it.
        if settle(vault, snap, &plan, Mode::Write)? {
            empty_list(self.held.flock.file())?;
        }
        let stored = changes
            .iter()
            .map(|change| match change {
                Change::Put { bytes, .. } => bytes.len() as u64,
                Change::Remove { .. } => 0,
            })
            .sum();
        pack_loose(git, stored);
        Ok(Some(new))
    }

    /// The author and committer a commit of this handle is made with.
    pub(crate) fn signature(&self) -> Result<Signature<'static>, Fault> {
        self.committer.identity.signature()
    }

    /// Moves the branch `HEAD` names, in the repository of `snap`, from the
    /// commit of `snap` to `new`, a commit the object database holds
    /// already, then brings the work tree and git's index to it as
    /// [`WriteLock::commit_in`] does.
    ///
    /// The paths the two trees differ in go into the lock file first; `HEAD`
    /// moves by compare and swap on the commit of `snap` (a `git` command
    /// run by hand meanwhile makes the call `ABORTED` with nothing written to
    /// the work tree); then each path is settled: a file that is what `snap`
    /// held becomes what `new` holds, a file a person changed is left as it
    /// is, and the index gets the new state. Only regular files reach the
    /// work tree, and never a path below `.git`, `.scheda/`, or
    /// `.isolated/`: a tree that came from a mirror may hold any name.
    ///
    /// # Errors
    ///
    /// `FAILED_PRECONDITION` for a detached `HEAD`; `ABORTED` for a branch
    /// that moved; `REPLICATION` for a path that is not UTF-8; `STORAGE` or
    /// `REPLICATION` when a file or git fails.
    pub(crate) fn advance_to(&self, snap: &Snap, new: Oid, message: &str) -> Result<(), Error> {
        let vault = self.root();
        let Some(git) = snap.repository() else {
            return Err(Fault::new(
                VaultErrorCode::Replication,
                "the repository to write to is not there",
            )
            .into());
        };
        let branch = branch_of(git)?;
        let target = git
            .find_commit(new)
            .and_then(|commit| commit.tree())
            .map_err(lib_fault)?;
        let before = match snap.tree() {
            Some(id) => Some(git.find_tree(id).map_err(lib_fault)?),
            None => None,
        };
        let diff = git
            .diff_tree_to_tree(before.as_ref(), Some(&target), None)
            .map_err(lib_fault)?;
        let mut moved: Vec<(String, Option<(Oid, FileMode)>)> = Vec::new();
        for delta in diff.deltas() {
            let deleted = delta.status() == git2::Delta::Deleted;
            let file = if deleted {
                delta.old_file()
            } else {
                delta.new_file()
            };
            let Some(path) = file.path().and_then(Path::to_str) else {
                return Err(Fault::new(
                    VaultErrorCode::Replication,
                    "the mirror holds a path that is not UTF-8",
                )
                .into());
            };
            if !reaches_work_tree(path) {
                continue;
            }
            let want = match file.mode() {
                git2::FileMode::Blob | git2::FileMode::BlobGroupWritable if !deleted => {
                    Some((file.id(), FileMode::Regular))
                }
                git2::FileMode::BlobExecutable if !deleted => {
                    Some((file.id(), FileMode::Executable))
                }
                _ => None,
            };
            moved.push((path.to_owned(), want));
        }
        let paths: Vec<&str> = moved.iter().map(|(path, _)| path.as_str()).collect();
        write_list(self.held.flock.file(), &paths)?;
        if let Err(err) = swap(git, &branch, snap.commit(), new, message) {
            return Err(after_failed_swap(self.held.flock.file(), err));
        }
        self.committer.signal.moved();
        let mut plan = Vec::with_capacity(moved.len());
        for (path, want) in &moved {
            let was = snap
                .lookup(path)
                .ok()
                .flatten()
                .filter(|node| node.kind == NodeKind::File)
                .map(|node| node.id);
            plan.push(Item {
                rel: path,
                want: want.map(|(id, mode)| Want {
                    id,
                    mode,
                    bytes: None,
                }),
                was,
            });
        }
        if settle(vault, snap, &plan, Mode::Write)? {
            empty_list(self.held.flock.file())?;
        }
        pack_loose(git, 0);
        Ok(())
    }
}

/// Packs the loose objects of `git` when there are too many. The commit is
/// stored by now, and packing only changes how the objects are kept, so a
/// failure here is not the write's: the objects stay loose and the next
/// write tries again. `stored` is the bytes of blobs the write stored.
fn pack_loose(git: &Repository, stored: u64) {
    let _ = pack::pack_if_due(git, &pack::Limits::DEFAULT, stored);
}

/// Whether a path of a tree that came from a mirror may be written into the
/// work tree.
fn reaches_work_tree(path: &str) -> bool {
    check_relative(path).is_ok()
        && path != isolated::DIR
        && path != ".scheda"
        && isolated::nested_rel(path).is_none()
        && !path.starts_with(".scheda/")
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

/// The reference `HEAD` names: the branch a commit moves. A `HEAD` that
/// names a commit directly is `FAILED_PRECONDITION`.
pub(crate) fn branch_of(git: &Repository) -> Result<String, Error> {
    let head = git.find_reference("HEAD").map_err(lib_fault)?;
    match head.symbolic_target().map_err(lib_fault)? {
        Some(branch) => Ok(branch.to_owned()),
        None => Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            "HEAD is detached: it names a commit, not a branch; check a branch out before \
             writing",
        )
        .into()),
    }
}

/// What a commit stored for a path: the id of the blob of a `Put`.
type Blobs = Vec<Option<Oid>>;

/// Writes the blobs, the tree on the tree of `snap`, and the commit, and
/// returns the id of the commit with the blob ids in the order of
/// `changes` (`None` for a removal); `None` when the tree does not change.
fn build_commit(
    git: &Repository,
    snap: &Snap,
    message: &str,
    changes: &[Change],
    who: &Signature<'_>,
) -> Result<Option<(Oid, Blobs)>, Error> {
    let repo = snap.repo();
    let mut blobs: Blobs = Vec::with_capacity(changes.len());
    let tree = if let Some(old) = snap.tree() {
        let base = git.find_tree(old).map_err(lib_fault)?;
        let mut update = TreeUpdateBuilder::new();
        for change in changes {
            let inner = repo.inner(change.path())?;
            match change {
                Change::Put { bytes, mode, path } => {
                    refuse_collision(snap, path)?;
                    let id = git.blob(bytes).map_err(lib_fault)?;
                    update.upsert(inner, id, mode.git());
                    blobs.push(Some(id));
                }
                Change::Remove { path } => {
                    // Removing what the tree does not hold is nothing to do.
                    if snap.lookup(path)?.is_some() {
                        update.remove(inner);
                    }
                    blobs.push(None);
                }
            }
        }
        let tree = update.create_updated(git, &base).map_err(lib_fault)?;
        if tree == old {
            return Ok(None);
        }
        tree
    } else {
        // An unborn branch has no tree to build on. The tree is made in an
        // index in memory, so that no empty tree is left in the object
        // database.
        let mut index = Index::new().map_err(lib_fault)?;
        for change in changes {
            match change {
                Change::Put { bytes, mode, path } => {
                    let id = git.blob(bytes).map_err(lib_fault)?;
                    let want = Want {
                        id,
                        mode: *mode,
                        bytes: None,
                    };
                    index
                        .add(&index_entry(repo.inner(path)?, &want, None))
                        .map_err(lib_fault)?;
                    blobs.push(Some(id));
                }
                Change::Remove { .. } => blobs.push(None),
            }
        }
        if index.is_empty() {
            return Ok(None);
        }
        index.write_tree_to(git).map_err(lib_fault)?
    };
    let tree = git.find_tree(tree).map_err(lib_fault)?;
    let mut text = message.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let parent = match snap.commit() {
        Some(id) => Some(git.find_commit(id).map_err(lib_fault)?),
        None => None,
    };
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    // No reference is given: the commit is only written to the object
    // database, and `swap` moves the branch.
    let id = git
        .commit(None, who, who, &text, &tree, &parents)
        .map_err(lib_fault)?;
    Ok(Some((id, blobs)))
}

/// `INVALID_ARGUMENT` when `path` would put a file where `snap` holds a
/// folder, or below a file.
fn refuse_collision(snap: &Snap, path: &str) -> Result<(), Fault> {
    let collision = |what: &str| {
        Fault::new(VaultErrorCode::InvalidArgument, format!("{path}: {what}")).with_path(path)
    };
    if snap
        .lookup(path)?
        .is_some_and(|node| node.kind != NodeKind::File)
    {
        return Err(collision("a folder is where the file would be"));
    }
    let mut at = 0;
    while let Some(slash) = path[at..].find('/') {
        at += slash;
        if snap
            .lookup(&path[..at])?
            .is_some_and(|node| node.kind != NodeKind::Dir)
        {
            return Err(collision("a file is where a folder would be"));
        }
        at += 1;
    }
    Ok(())
}

/// Moves `branch` from `old` to `new` only if it still names `old`; an
/// unborn branch is created only if it still does not exist. A branch that
/// moved is `ABORTED`.
fn swap(
    git: &Repository,
    branch: &str,
    old: Option<Oid>,
    new: Oid,
    message: &str,
) -> Result<(), Error> {
    let first = message.lines().next().unwrap_or("");
    let log = format!("scheda: {first}");
    let moved = match old {
        Some(old) => git
            .reference_matching(branch, new, true, old, &log)
            .map(|_| ()),
        None => git.reference(branch, new, false, &log).map(|_| ()),
    };
    match moved {
        Ok(()) => Ok(()),
        Err(err)
            if matches!(err.code(), ErrorCode::Modified | ErrorCode::Exists)
                || (old.is_some() && err.code() == ErrorCode::NotFound) =>
        {
            Err(Fault::new(
                VaultErrorCode::Aborted,
                "HEAD moved while the write was being stored; try again",
            )
            .with_source(err)
            .into())
        }
        Err(err) => Err(lib_fault(err).into()),
    }
}

/// Writes the paths into the lock file, each ended by a NUL
/// byte, and flushes them to disk.
fn write_list(file: &File, paths: &[&str]) -> Result<(), Error> {
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path.as_bytes());
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

/// Empties the note after the swap of `HEAD` failed with `err`, since
/// nothing was written to the work tree, and returns `err`: the reason the
/// write failed. A note that cannot be emptied is no reason to lose it; the
/// failure rides on `err` as a frame, and the next holder of the lock does
/// what the note says.
fn after_failed_swap(file: &File, err: Error) -> Error {
    match empty_list(file) {
        Ok(()) => err,
        Err(emptying) => err.context(format_args!("the note was not emptied either: {emptying}")),
    }
}

/// What the work tree and the index are to hold for one path.
struct Item<'a> {
    /// The Vault-relative path.
    rel: &'a str,
    /// What the stored commit holds there: the blob, its mode, and its
    /// bytes when the caller has them.
    want: Option<Want<'a>>,
    /// The state the file is expected to be in when nobody has touched it
    /// since: the blob of the old `HEAD`, or the one the index holds.
    was: Option<Oid>,
}

/// A file the commit holds.
struct Want<'a> {
    id: Oid,
    mode: FileMode,
    /// The bytes of the blob, if the caller holds them already.
    bytes: Option<&'a [u8]>,
}

/// Whether a settle is the end of a write (the old `HEAD` is the file's
/// last known state) or a recovery (the index is).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Write,
    Recover,
}

/// The work-tree items of a write, from its changes and the blob ids the
/// commit stored. `snap` is the state the write was built on.
fn plan_of<'a>(snap: &Snap, changes: &'a [Change], blobs: &Blobs) -> Result<Vec<Item<'a>>, Error> {
    let mut plan = Vec::with_capacity(changes.len());
    for (change, blob) in changes.iter().zip(blobs) {
        let was = snap
            .lookup(change.path())?
            .filter(|node| node.kind == NodeKind::File)
            .map(|node| node.id);
        let want = match (change, blob) {
            (Change::Put { bytes, mode, .. }, Some(id)) => Some(Want {
                id: *id,
                mode: *mode,
                bytes: Some(bytes),
            }),
            _ => None,
        };
        plan.push(Item {
            rel: change.path(),
            want,
            was,
        });
    }
    Ok(plan)
}

/// What is on disk at a path of the work tree.
enum Disk {
    Absent,
    File(Oid),
    Other,
}

impl Disk {
    /// The state of the file at `target`: its blob id when it is a regular
    /// file (its raw bytes, no filters), `Other` for a folder, a link, or
    /// anything unreadable, which are not Scheda's to touch.
    fn of(target: &Path) -> Result<Disk, Error> {
        match fs::symlink_metadata(target) {
            Ok(meta) if meta.is_file() => {
                Ok(Oid::hash_file(ObjectType::Blob, target).map_or(Disk::Other, Disk::File))
            }
            Ok(_) => Ok(Disk::Other),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Disk::Absent),
            // A file where a folder of the path should be: not ours.
            Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => Ok(Disk::Other),
            Err(err) => Err(io(err)).context("look at a file of the work tree"),
        }
    }

    /// Whether the file is the one blob `state` names, or is absent where
    /// `state` names none.
    fn is(&self, state: Option<Oid>) -> bool {
        match (self, state) {
            (Disk::Absent, None) => true,
            (Disk::File(on_disk), Some(id)) => *on_disk == id,
            _ => false,
        }
    }
}

/// What to do about one path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    /// The file is the wanted one already: only the index moves.
    Index,
    /// The file is as it was last known: write the wanted bytes.
    Write,
    /// The file is as it was last known and the path is to be gone.
    Remove,
    /// A person changed the file: leave it, and give the index the wanted
    /// state.
    Leave,
}

fn decide(disk: &Disk, want: Option<Oid>, was: Option<Oid>) -> Action {
    if disk.is(want) {
        Action::Index
    } else if disk.is(was) {
        if want.is_some() {
            Action::Write
        } else {
            Action::Remove
        }
    } else {
        Action::Leave
    }
}

/// Brings the work tree and git's index of the repository of `snap` to the
/// items: see [`WriteLock::commit_in`] and `recover`. Returns whether the
/// index was written; a file failure is an error. `Mode::Recover` takes
/// what the file was last known to be from the index, not from the item.
fn settle(vault: &Path, snap: &Snap, items: &[Item<'_>], mode: Mode) -> Result<bool, Error> {
    let repo = snap.repo();
    let Some(git) = snap.repository() else {
        return Ok(true);
    };
    let work = repo.root(vault);
    let mut index = git.index().map_err(lib_fault)?;
    index.read(true).map_err(lib_fault)?;
    // Removals first, with the folders they empty, then the new files.
    let mut edits: Vec<(String, Edit)> = Vec::new();
    let mut ordered: Vec<&Item<'_>> = items.iter().filter(|item| item.want.is_none()).collect();
    ordered.extend(items.iter().filter(|item| item.want.is_some()));
    for item in ordered {
        let inner = repo.inner(item.rel)?;
        let target = work.join(inner);
        if reject_symlinks(vault, &vault.join(item.rel)).is_err() {
            continue;
        }
        let was = match mode {
            Mode::Write => item.was,
            Mode::Recover => index.get_path(Path::new(inner), 0).map(|entry| entry.id),
        };
        let want = item.want.as_ref().map(|want| want.id);
        let disk = Disk::of(&target)?;
        let action = decide(&disk, want, was);
        let edit = apply(vault, snap, item, &target, action)?;
        edits.push((inner.to_owned(), edit));
    }
    for (inner, edit) in edits {
        if index.has_conflicts() {
            let _ = index.conflict_remove(Path::new(&inner));
        }
        match edit {
            Edit::Remove => match index.remove_path(Path::new(&inner)) {
                Ok(()) => {}
                Err(err) if err.code() == ErrorCode::NotFound => {}
                Err(err) => return Err(lib_fault(err).into()),
            },
            Edit::Set { entry } => index.add(&entry).map_err(lib_fault)?,
        }
    }
    match index.write() {
        Ok(()) => Ok(true),
        Err(err) if err.code() == ErrorCode::Locked => Ok(false),
        Err(err) => Err(lib_fault(err).into()),
    }
}

/// What the index gets for a path.
enum Edit {
    Remove,
    Set { entry: IndexEntry },
}

/// Does `action` to the file at `target` and returns the index edit.
fn apply(
    vault: &Path,
    snap: &Snap,
    item: &Item<'_>,
    target: &Path,
    action: Action,
) -> Result<Edit, Error> {
    let repo = snap.repo();
    let inner = repo.inner(item.rel)?;
    let Some(want) = &item.want else {
        if action == Action::Remove {
            remove_path(&repo.root(vault), inner)?;
        }
        return Ok(Edit::Remove);
    };
    let stat = match action {
        Action::Write => {
            let loaded;
            let bytes = match want.bytes {
                Some(bytes) => bytes,
                None => {
                    loaded = snap.blob(want.id)?;
                    &loaded
                }
            };
            atomic_write(target, bytes, want.mode == FileMode::Executable)
                .context("write a file")?;
            // A file of the nested repository, and the folders above it, are
            // for the owner alone.
            isolated::restrict_record(vault, &vault.join(item.rel))?;
            Some(fs::metadata(target).map_err(io).context("look at a file")?)
        }
        Action::Index => Some(fs::metadata(target).map_err(io).context("look at a file")?),
        Action::Leave | Action::Remove => None,
    };
    Ok(Edit::Set {
        entry: index_entry(inner, want, stat.as_ref()),
    })
}

/// The index entry for `path` holding the blob of `want`. With the `stat` of
/// the file it stands for, git takes the file as unchanged until its size or
/// time changes; without one (a file a person changed), git has to read the
/// file and sees it differs.
fn index_entry(path: &str, want: &Want<'_>, stat: Option<&fs::Metadata>) -> IndexEntry {
    let mode = match want.mode {
        FileMode::Regular => 0o100_644,
        FileMode::Executable => 0o100_755,
    };
    let mut entry = IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: want.id,
        flags: u16::try_from(path.len().min(0xFFF)).unwrap_or(0xFFF),
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    };
    if let Some(meta) = stat {
        stamp(&mut entry, meta);
    }
    entry
}

#[cfg(unix)]
fn stamp(entry: &mut IndexEntry, meta: &fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    let secs = |value: i64| i32::try_from(value).unwrap_or(i32::MAX);
    let nanos = |value: i64| u32::try_from(value).unwrap_or(0);
    entry.ctime = IndexTime::new(secs(meta.ctime()), nanos(meta.ctime_nsec()));
    entry.mtime = IndexTime::new(secs(meta.mtime()), nanos(meta.mtime_nsec()));
    entry.dev = meta.dev() as u32;
    entry.ino = meta.ino() as u32;
    entry.uid = meta.uid();
    entry.gid = meta.gid();
    entry.file_size = u32::try_from(meta.size()).unwrap_or(u32::MAX);
}

#[cfg(not(unix))]
fn stamp(entry: &mut IndexEntry, meta: &fs::Metadata) {
    if let Ok(since) = meta.modified().and_then(|time| {
        time.duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)
    }) {
        let secs = i32::try_from(since.as_secs()).unwrap_or(i32::MAX);
        entry.mtime = IndexTime::new(secs, since.subsec_nanos());
        entry.ctime = entry.mtime;
    }
    entry.file_size = u32::try_from(meta.len()).unwrap_or(u32::MAX);
}

/// Removes the file at `rel` below `root` and the directories it leaves
/// empty, up to but not including `root`. A file that is not there is
/// nothing to do; a directory of that name is left alone.
fn remove_path(root: &Path, rel: &str) -> Result<(), Error> {
    let target = root.join(rel);
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

/// Recovers the write a dead writer left, if the lock file holds paths:
/// each path of each repository is settled against what the `HEAD` of that
/// repository holds, and the file is emptied once the index took it. Called
/// with the lock held.
///
/// A path is brought to `HEAD` only when nobody touched it since the last
/// state Scheda knew (the file equals the index entry, or is absent where
/// the index has none); when it already equals `HEAD`, only the index moves;
/// otherwise a person changed it, and the file stays while the index gets
/// `HEAD`'s blob.
fn recover(root: &Path, repos: &Repos, lock: &File) -> Result<(), Error> {
    let (len, paths) = read_list(lock)?;
    let mut done = true;
    if !paths.is_empty() {
        let (nested, main): (Vec<String>, Vec<String>) = paths
            .into_iter()
            .partition(|path| isolated::nested_rel(path).is_some());
        done &= restore(root, repos, Repo::Main, &main)?;
        if !nested.is_empty() {
            done &= restore(root, repos, Repo::Isolated, &nested)?;
        }
    }
    if len > 0 && done {
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

/// Settles the Vault-relative `paths` of `repo` against its `HEAD`. A
/// nested repository that is not there, or whose folder is a link, was never
/// written to: its paths are passed over. Returns whether git's index took
/// the change.
fn restore(vault: &Path, repos: &Repos, repo: Repo, paths: &[String]) -> Result<bool, Error> {
    if paths.is_empty() {
        return Ok(true);
    }
    if repo == Repo::Isolated && reject_symlinks(vault, &isolated::root(vault)).is_err() {
        return Ok(true);
    }
    let snap = repos.snapshot(repo)?;
    if snap.repository().is_none() {
        return Ok(true);
    }
    let mut items = Vec::with_capacity(paths.len());
    for path in paths {
        // A link on the way in `HEAD` is not a file to restore.
        let node = snap
            .lookup(path)
            .ok()
            .flatten()
            .filter(|node| node.kind == NodeKind::File);
        items.push(Item {
            rel: path,
            want: node.map(|node| Want {
                id: node.id,
                mode: FileMode::of(node.executable),
                bytes: None,
            }),
            was: None,
        });
    }
    settle(vault, &snap, &items, Mode::Recover)
}

/// Test hooks that stop a write where a kill would, and run a plain `git`
/// command in the middle of one. They exist in test builds only.
#[cfg(test)]
pub(crate) mod hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    use scheda_api::v1::VaultErrorCode;

    use crate::error::{Error, Fault};

    type Callback = Box<dyn FnOnce(&Path) + Send>;

    /// What the next write to one Vault does at its seams.
    #[derive(Default)]
    struct Hook {
        stop_after_list: bool,
        stop_after_swap: bool,
        before_swap: Option<Callback>,
    }

    /// The hooks of each Vault, by its canonical path: a write runs on the
    /// blocking pool, not on the thread of the test.
    static HOOKS: Mutex<Option<HashMap<PathBuf, Hook>>> = Mutex::new(None);

    fn with<T>(vault: &Path, edit: impl FnOnce(&mut Hook) -> T) -> T {
        let mut guard = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
        let hooks = guard.get_or_insert_with(HashMap::new);
        edit(hooks.entry(vault.to_path_buf()).or_default())
    }

    /// The next write to `vault` stops right after it wrote the list of its
    /// paths, before it moves `HEAD`.
    pub(crate) fn stop_after_list(vault: &Path) {
        with(vault, |hook| hook.stop_after_list = true);
    }

    /// The next write to `vault` stops right after it moved `HEAD`, before
    /// it writes the files.
    pub(crate) fn stop_after_swap(vault: &Path) {
        with(vault, |hook| hook.stop_after_swap = true);
    }

    /// The next write to `vault` runs `callback` with the root of the
    /// repository it writes, between the read of `HEAD` and the swap.
    pub(crate) fn run_before_swap(vault: &Path, callback: impl FnOnce(&Path) + Send + 'static) {
        with(vault, |hook| hook.before_swap = Some(Box::new(callback)));
    }

    fn stopped() -> Error {
        Fault::new(
            VaultErrorCode::Storage,
            "the write was stopped by a test hook",
        )
        .into()
    }

    pub(super) fn after_list(vault: &Path) -> Result<(), Error> {
        if with(vault, |hook| std::mem::take(&mut hook.stop_after_list)) {
            return Err(stopped());
        }
        Ok(())
    }

    pub(super) fn before_swap(vault: &Path, root: &Path) {
        if let Some(callback) = with(vault, |hook| hook.before_swap.take()) {
            callback(root);
        }
    }

    pub(super) fn after_swap(vault: &Path) -> Result<(), Error> {
        if with(vault, |hook| std::mem::take(&mut hook.stop_after_swap)) {
            return Err(stopped());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use buffa_types::google::protobuf::Empty;
    use scheda_api::v1::{OpenOptions, VaultErrorCode};

    use super::{
        Change, FileMode, Repo, WriteLock, after_failed_swap, check_relative, hooks, pending,
    };
    use crate::error::Error;
    use crate::git::git;
    use crate::testing::{create_collection, open, step};
    use crate::vault::Vault;

    #[test]
    fn a_note_that_cannot_be_emptied_keeps_the_error_of_the_swap() {
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let path = base.join(format!("scheda-commit-note-{}", std::process::id()));
        std::fs::write(&path, b"note").unwrap();
        // A file opened for reading only: emptying it fails.
        let read_only = std::fs::File::open(&path).unwrap();
        let swap = Error::new(VaultErrorCode::Aborted, "HEAD moved");
        let error = after_failed_swap(&read_only, swap);
        assert!(error.is(VaultErrorCode::Aborted), "{error}");
        assert!(
            error
                .stack()
                .iter()
                .any(|frame| frame.contains("the note was not emptied either")),
            "{:?}",
            error.stack()
        );

        // A note that can be emptied adds no frame.
        let writable = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let swap = Error::new(VaultErrorCode::Aborted, "HEAD moved");
        let error = after_failed_swap(&writable, swap);
        assert!(error.is(VaultErrorCode::Aborted), "{error}");
        assert!(error.stack().is_empty(), "{:?}", error.stack());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        std::fs::remove_file(&path).unwrap();
    }

    /// Runs `work` inside a blocking step that holds the write lock of
    /// `vault`.
    async fn locked<T, F>(vault: &Arc<Vault>, work: F) -> Result<T, Error>
    where
        F: FnOnce(&WriteLock<'_>) -> Result<T, Error> + Send + 'static,
        T: Send + 'static,
    {
        let held = vault.committer().acquire().await?;
        step(vault, move |cx, vault| {
            let lock = vault.lock(cx, held)?;
            work(&lock)
        })
        .await
    }

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

        async fn open(&self) -> Arc<Vault> {
            open(&self.0, OpenOptions::default().with_write_timeout_ms(2000))
                .await
                .unwrap()
        }

        fn canonical(&self) -> PathBuf {
            self.0.canonicalize().unwrap()
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

    fn standard(name: &str) -> serde_json::Value {
        serde_json::json!({"name": name, "schema": {"standard": {}}})
    }

    #[tokio::test]
    async fn a_write_stopped_after_the_list_stores_nothing_and_touches_nothing() {
        let scratch = Scratch::new("list");
        let vault = scratch.open().await;
        let before = scratch.out(&["rev-parse", "HEAD"]);
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        hooks::stop_after_list(&scratch.canonical());
        let error = create_collection(&vault, standard("notes"))
            .await
            .unwrap_err();
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
        create_collection(&vault, standard("log")).await.unwrap();
        assert!(scratch.list().is_empty());
        assert!(!scratch.0.join("notes").exists());
        assert!(scratch.0.join("log/.collection.yaml").is_file());
        assert_eq!(
            scratch.status(),
            "?? person.txt",
            "a person's file is not ours"
        );
        // Nothing of the stopped write was declared.
        create_collection(&vault, standard("notes")).await.unwrap();
        assert_eq!(scratch.commits(), 3);
    }

    #[tokio::test]
    async fn a_write_stopped_after_the_swap_is_completed_by_the_next_open() {
        let scratch = Scratch::new("swap-open");
        let vault = scratch.open().await;
        let commits = scratch.commits();
        std::fs::write(
            scratch.0.join(".gitignore"),
            ".scheda/\n.isolated/\n# edited\n",
        )
        .unwrap();
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        hooks::stop_after_swap(&scratch.canonical());
        let error = create_collection(&vault, standard("notes"))
            .await
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        assert_eq!(scratch.commits(), commits + 1, "the commit landed");
        assert!(!scratch.0.join("notes").exists(), "the files lag");
        let root = scratch.canonical();
        assert!(step(&vault, move |cx, _| pending(cx, &root)).await.unwrap());
        vault.close().await.unwrap();
        let vault = scratch.open().await;
        assert!(scratch.0.join("notes/.collection.yaml").is_file());
        assert!(scratch.list().is_empty());
        assert!(
            step(&vault, |cx, vault| vault.get_collection(cx, key("notes")))
                .await
                .is_ok()
        );
        assert_eq!(
            scratch.status(),
            " M .gitignore\n?? person.txt",
            "exactly the paths of the write were brought to HEAD"
        );
    }

    #[tokio::test]
    async fn a_write_stopped_after_the_swap_is_completed_by_the_next_write_of_the_handle() {
        let scratch = Scratch::new("swap-write");
        let vault = scratch.open().await;
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        hooks::stop_after_swap(&scratch.canonical());
        assert!(create_collection(&vault, standard("notes")).await.is_err());
        assert!(!scratch.0.join("notes").exists());
        create_collection(&vault, standard("log")).await.unwrap();
        assert!(scratch.0.join("notes/.collection.yaml").is_file());
        assert!(scratch.0.join("log/.collection.yaml").is_file());
        assert!(scratch.list().is_empty());
        assert_eq!(scratch.status(), "?? person.txt");
        // The handle read the collections again when HEAD had moved.
        let error = create_collection(&vault, standard("notes"))
            .await
            .unwrap_err();
        assert!(error.is(VaultErrorCode::AlreadyExists));
        let listed = step(&vault, |cx, vault| {
            vault.list_collections(cx, Empty::default())
        })
        .await
        .unwrap();
        assert_eq!(listed.collections.len(), 2);
    }

    #[tokio::test]
    async fn a_git_commit_between_the_read_and_the_swap_aborts_the_write() {
        let scratch = Scratch::new("race");
        let vault = scratch.open().await;
        let commits = scratch.commits();
        hooks::run_before_swap(&scratch.canonical(), |root| {
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
        let error = create_collection(&vault, standard("notes"))
            .await
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Aborted));
        assert!(error.retryable());
        assert_eq!(scratch.commits(), commits + 1, "only the hand-made commit");
        assert!(!scratch.0.join("notes").exists());
        assert!(scratch.list().is_empty());
        assert_eq!(scratch.status(), "");
        create_collection(&vault, standard("notes")).await.unwrap();
        assert_eq!(scratch.commits(), commits + 2);
        assert_eq!(scratch.status(), "");
    }

    #[tokio::test]
    async fn recovery_settles_listed_paths_only_and_passes_over_unsafe_entries() {
        let scratch = Scratch::new("restore");
        let vault = scratch.open().await;
        vault.close().await.unwrap();
        // A person's edits: one of a listed path, one of another.
        std::fs::write(scratch.0.join(".gitignore"), "edited\n").unwrap();
        std::fs::write(scratch.0.join(".scheda.yaml"), "version: 3\n# edited\n").unwrap();
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
        let _vault = scratch.open().await;
        assert!(scratch.list().is_empty());
        // A listed file a person changed is theirs, even one HEAD never had.
        assert_eq!(
            std::fs::read_to_string(scratch.0.join(".gitignore")).unwrap(),
            "edited\n"
        );
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("dir/ghost.txt")).unwrap(),
            "not in HEAD"
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
        assert_eq!(
            scratch.status(),
            " M .gitignore\n M .scheda.yaml\n?? .scheda/\n?? dir/\n?? kept.txt"
        );
    }

    #[tokio::test]
    async fn a_write_cut_after_the_swap_is_finished_by_the_next_open_and_a_persons_edit_survives() {
        let scratch = Scratch::new("cut");
        let vault = scratch.open().await;
        hooks::stop_after_swap(&scratch.canonical());
        let error = locked(&vault, |lock| {
            let snap = lock.snapshot(Repo::Main)?;
            lock.commit_in(
                &snap,
                "two files",
                &[
                    Change::put("a.txt", b"new a".to_vec()),
                    Change::put("sub/b.txt", b"new b".to_vec()),
                ],
            )
        })
        .await
        .unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        assert_eq!(scratch.out(&["show", "HEAD:a.txt"]), "new a");
        assert!(!scratch.0.join("a.txt").exists(), "the files lag");
        assert_eq!(scratch.list(), b"a.txt\0sub/b.txt\0");
        // Meanwhile a person writes one of the listed files.
        std::fs::create_dir_all(scratch.0.join("sub")).unwrap();
        std::fs::write(scratch.0.join("sub/b.txt"), "mine").unwrap();
        vault.close().await.unwrap();
        let _vault = scratch.open().await;
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).unwrap(),
            "new a"
        );
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("sub/b.txt")).unwrap(),
            "mine",
            "the person's file survives the recovery"
        );
        assert!(scratch.list().is_empty());
        assert_eq!(scratch.out(&["show", "HEAD:sub/b.txt"]), "new b");
        assert_eq!(scratch.status(), " M sub/b.txt");
    }

    #[tokio::test]
    async fn a_persons_edit_survives_a_write_of_the_same_path_and_git_shows_it() {
        let scratch = Scratch::new("edit");
        let vault = scratch.open().await;
        let put = async |bytes: &[u8]| {
            let bytes = bytes.to_vec();
            locked(&vault, move |lock| {
                let snap = lock.snapshot(Repo::Main)?;
                lock.commit_in(&snap, "put", &[Change::put("a.txt", bytes)])
            })
            .await
            .unwrap()
        };
        put(b"one").await;
        assert_eq!(scratch.status(), "");
        std::fs::write(scratch.0.join("a.txt"), "edited").unwrap();
        put(b"two").await;
        assert_eq!(scratch.out(&["show", "HEAD:a.txt"]), "two");
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).unwrap(),
            "edited"
        );
        assert_eq!(scratch.status(), " M a.txt");
        assert!(scratch.list().is_empty());
        // Nobody touched it: the next write replaces the file.
        std::fs::write(scratch.0.join("a.txt"), "two").unwrap();
        assert_eq!(scratch.status(), "", "the file is what HEAD holds");
        put(b"three").await;
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).unwrap(),
            "three"
        );
        assert_eq!(scratch.status(), "");
        // A file deleted by hand stays deleted; git shows it.
        std::fs::remove_file(scratch.0.join("a.txt")).unwrap();
        put(b"four").await;
        assert!(!scratch.0.join("a.txt").exists());
        assert_eq!(scratch.status(), " D a.txt");
    }

    #[tokio::test]
    async fn a_detached_head_is_failed_precondition_and_nothing_is_stored() {
        let scratch = Scratch::new("detached");
        let vault = scratch.open().await;
        scratch.out(&["checkout", "--quiet", "--detach"]);
        let commits = scratch.commits();
        let error = create_collection(&vault, standard("notes"))
            .await
            .unwrap_err();
        assert!(error.is(VaultErrorCode::FailedPrecondition));
        assert_eq!(scratch.commits(), commits);
        assert!(scratch.list().is_empty());
        assert!(!scratch.0.join("notes").exists());
    }

    #[tokio::test]
    async fn a_repository_with_gpgsign_gets_unsigned_commits() {
        let scratch = Scratch::new("gpgsign");
        let vault = scratch.open().await;
        scratch.out(&["config", "commit.gpgSign", "true"]);
        scratch.out(&["config", "gpg.program", "/nonexistent/gpg"]);
        create_collection(&vault, standard("notes")).await.unwrap();
        assert!(scratch.0.join("notes/.collection.yaml").is_file());
        let raw = scratch.out(&["cat-file", "commit", "HEAD"]);
        assert!(!raw.contains("gpgsig"), "{raw}");
        assert_eq!(scratch.status(), "");
    }

    /// The umask is process-wide, so the check runs in a child process that
    /// this test starts through `sh` with the umask set: the shared test
    /// process, where other tests create files, keeps its own.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_umask_decides_the_modes_of_the_files_a_write_puts_in_the_tree() {
        use std::os::unix::fs::PermissionsExt;

        const CHILD: &str = "SCHEDA_TEST_UMASK_CHILD";
        let name =
            "commit::tests::the_umask_decides_the_modes_of_the_files_a_write_puts_in_the_tree";
        if std::env::var_os(CHILD).is_none() {
            let exe = std::env::current_exe().unwrap();
            let status = std::process::Command::new("sh")
                .args(["-c", "umask 077 && exec \"$0\" \"$@\""])
                .arg(exe)
                .args(["--exact", name, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let report = String::from_utf8_lossy(&status.stdout);
            assert!(status.status.success(), "the child with umask 077 failed");
            // A name that matches no test would also exit 0.
            assert!(
                report.contains("1 passed"),
                "the child ran no test: {report}"
            );
            return;
        }
        let scratch = Scratch::new("umask");
        let vault = scratch.open().await;
        locked(&vault, |lock| {
            let snap = lock.snapshot(Repo::Main)?;
            lock.commit_in(
                &snap,
                "two files",
                &[
                    Change::Put {
                        path: "a/run.sh".to_owned(),
                        bytes: b"#!/bin/sh\n".to_vec(),
                        mode: FileMode::Executable,
                    },
                    Change::put("a/plain.txt", b"x".to_vec()),
                ],
            )
        })
        .await
        .unwrap();
        let mode = |rel: &str| {
            std::fs::metadata(scratch.0.join(rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("a/plain.txt"), 0o600);
        assert_eq!(mode("a/run.sh"), 0o700);
        assert_eq!(scratch.status(), "");
    }

    #[tokio::test]
    async fn a_commit_stores_modes_removals_and_nothing_twice() {
        let scratch = Scratch::new("changes");
        let vault = scratch.open().await;
        let first = locked(&vault, |lock| {
            let snap = lock.snapshot(Repo::Main)?;
            lock.commit_in(
                &snap,
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
        })
        .await
        .unwrap();
        assert_eq!(
            first.unwrap().to_string(),
            scratch.out(&["rev-parse", "HEAD"])
        );
        assert_eq!(scratch.status(), "");
        let listing = scratch.out(&["ls-files", "--stage"]);
        assert!(listing.contains("100755"), "{listing}");
        assert_eq!(
            std::fs::read(scratch.0.join("a/plain.txt")).unwrap(),
            b"x\r\ny",
            "bytes are stored as they are"
        );
        // The same bytes again change nothing and make no commit.
        let same = locked(&vault, |lock| {
            let snap = lock.snapshot(Repo::Main)?;
            lock.commit_in(
                &snap,
                "again",
                &[Change::put("a/plain.txt", b"x\r\ny".to_vec())],
            )
        })
        .await
        .unwrap();
        assert_eq!(same, first);
        assert!(scratch.list().is_empty());
        // A removal takes the file and the folders it empties.
        locked(&vault, |lock| {
            let snap = lock.snapshot(Repo::Main)?;
            lock.commit_in(
                &snap,
                "gone",
                &[Change::Remove {
                    path: "a/b/run.sh".to_owned(),
                }],
            )
        })
        .await
        .unwrap();
        assert!(!scratch.0.join("a/b").exists());
        assert!(scratch.0.join("a/plain.txt").is_file());
        assert_eq!(scratch.status(), "");
        // The folder the removal emptied is gone from the tree too.
        assert!(
            !scratch
                .out(&["ls-tree", "-r", "-t", "--name-only", "HEAD"])
                .contains("a/b")
        );
        scratch.out(&["fsck", "--no-progress", "--strict"]);
        assert_eq!(scratch.out(&["log", "-1", "--format=%s"]), "gone");
        assert_eq!(
            scratch.out(&["log", "-1", "--format=%an <%ae>"]),
            "Scheda <scheda@invalid>"
        );
    }

    #[tokio::test]
    async fn paths_that_step_out_or_name_git_are_refused_before_anything_is_written() {
        let scratch = Scratch::new("paths");
        let vault = scratch.open().await;
        locked(&vault, |lock| {
            let snap = lock.snapshot(Repo::Main)?;
            for bad in ["../x", "/x", "a//b", ".git/config", "a/./b", "", "a\\b"] {
                let error = lock
                    .commit_in(&snap, "bad", &[Change::put(bad, Vec::new())])
                    .unwrap_err();
                assert!(error.is(VaultErrorCode::InvalidArgument), "{bad}");
            }
            Ok(())
        })
        .await
        .unwrap();
        assert!(check_relative("a/b").is_ok());
        assert!(scratch.list().is_empty());
        assert_eq!(scratch.status(), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_on_the_path_of_a_write_is_refused_and_nothing_is_stored() {
        let scratch = Scratch::new("symlink");
        let vault = scratch.open().await;
        let commits = scratch.commits();
        let target = scratch
            .0
            .parent()
            .unwrap()
            .join("scheda-commit-link-target");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, scratch.0.join("notes")).unwrap();
        let error = create_collection(&vault, standard("notes"))
            .await
            .unwrap_err();
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
