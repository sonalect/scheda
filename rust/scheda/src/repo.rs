//! The handle's access to git: the repository of the Vault and the nested
//! one under `.isolated/`, opened through the git library, never through a
//! process.
//!
//! A `git2::Repository` can move between threads but not be shared by them,
//! so each of the two repositories has a small pool: a call takes a
//! repository, uses it, and hands it back when its [`Lease`] is dropped.
//! When every repository of a pool is busy another is opened, so reads of
//! many threads do not wait for one another.
//!
//! A read does not look at the work tree. It resolves `HEAD`, which takes
//! the commit the branch names now, and reads the trees and blobs of that
//! commit: a [`Snap`]. A snapshot keeps its commit for as long as it lives,
//! so a walk that holds one reads one state of the Vault whatever is
//! committed meanwhile.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use git2::{
    DiffOptions, ErrorClass, ErrorCode, FileMode, Oid, Repository, RepositoryInitOptions,
    StatusOptions,
};
use scheda_api::v1::VaultErrorCode;

use crate::error::{Fault, lib_fault};
use crate::isolated;

/// How many idle repositories a pool keeps. A call that finds the pool
/// empty opens one more; one handed back to a full pool is closed.
const IDLE_MAX: usize = 8;

/// The branch a new repository starts on when `init.defaultBranch` names
/// none.
const FALLBACK_BRANCH: &str = "main";

/// The repository a read or a commit goes to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Repo {
    /// The repository of the Vault.
    Main,
    /// The nested repository under `.isolated/`.
    Isolated,
}

impl Repo {
    /// The work tree of the repository.
    pub(crate) fn root(self, vault: &Path) -> PathBuf {
        match self {
            Repo::Main => vault.to_path_buf(),
            Repo::Isolated => isolated::root(vault),
        }
    }

    /// The path of `path` (Vault-relative) inside this repository, or
    /// `INVALID_ARGUMENT` when it belongs to the other one.
    pub(crate) fn inner(self, path: &str) -> Result<&str, Fault> {
        let nested = isolated::nested_rel(path).filter(|rest| !rest.is_empty());
        match (self, nested) {
            (Repo::Main, None) if path != isolated::DIR => Ok(path),
            (Repo::Isolated, Some(rest)) => Ok(rest),
            _ => Err(Fault::new(
                VaultErrorCode::InvalidArgument,
                "the path is not in the repository the write goes to",
            )),
        }
    }
}

/// Sets the options of the git library that belong to the process, once,
/// before the first repository is opened: the files of git's own directory
/// are flushed to disk when they are written (the library leaves that off,
/// and a commit that a power loss can undo is not a stored write).
///
/// A library that refuses the option fails every call that opens a
/// repository with `STORAGE`: writes that are not flushed are not offered.
pub(crate) fn init_library() -> Result<(), Fault> {
    static OUTCOME: OnceLock<std::ffi::c_int> = OnceLock::new();
    let outcome = *OUTCOME.get_or_init(|| {
        #[cfg(test)]
        crate::git::isolate_test_git_env();
        libgit2_sys::init();
        // SAFETY: the library is initialized, the option takes one `int`,
        // and `OnceLock` keeps this call from racing another thread's.
        unsafe {
            libgit2_sys::git_libgit2_opts(
                libgit2_sys::GIT_OPT_ENABLE_FSYNC_GITDIR as std::ffi::c_int,
                1 as std::ffi::c_int,
            )
        }
    });
    if outcome < 0 {
        return Err(Fault::new(
            VaultErrorCode::Storage,
            "the git library refused to flush its files to disk",
        ));
    }
    Ok(())
}

/// `UNIMPLEMENTED` for the failure the library reports when a repository
/// uses a format it cannot read (SHA-256 objects, reftable refs), the
/// failure itself otherwise.
fn open_fault(err: git2::Error) -> Fault {
    let message = err.message().to_ascii_lowercase();
    let unsupported = err.class() == ErrorClass::Repository
        && (message.contains("unsupported extension") || message.contains("unknown object format"));
    if unsupported {
        Fault::new(
            VaultErrorCode::Unimplemented,
            "the git repository uses a format this build cannot read (SHA-256 objects or \
             reftable references); use a repository of SHA-1 objects and files for references",
        )
        .with_source(err)
    } else {
        lib_fault(err)
    }
}

/// Opens the repository whose work tree or git directory is `root`, without
/// searching the folders above. `None` when there is none there.
fn open_at(root: &Path) -> Result<Option<Repository>, Fault> {
    init_library()?;
    match Repository::open(root) {
        Ok(repo) => Ok(Some(repo)),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(None),
        Err(err) => Err(open_fault(err)),
    }
}

/// Whether `dir` is the top level of a git work tree. A path that holds no
/// repository, or only the start of one, is "no". A repository of a format
/// this build cannot read is `UNIMPLEMENTED`, and one the library refuses
/// to open for its owner (git's `safe.directory`) is the library's failure.
pub(crate) fn is_work_tree(dir: &Path) -> Result<bool, Fault> {
    if !dir.exists() {
        return Ok(false);
    }
    init_library()?;
    let repo = match Repository::open(dir) {
        Ok(repo) => repo,
        Err(err) => {
            let owner = err.code() == ErrorCode::Owner;
            let fault = open_fault(err);
            return if owner || fault.vault_code() == VaultErrorCode::Unimplemented {
                Err(fault)
            } else {
                Ok(false)
            };
        }
    };
    Ok(is_top_level(&repo, dir))
}

/// Whether the work tree of `repo` is the folder `dir`.
fn is_top_level(repo: &Repository, dir: &Path) -> bool {
    match (repo.workdir().map(Path::canonicalize), dir.canonicalize()) {
        (Some(Ok(work)), Ok(dir)) => work == dir,
        _ => false,
    }
}

/// Succeeds when `dir` is the top level of a git work tree. A bare
/// repository is `FAILED_PRECONDITION`; a folder inside a work tree that is
/// not its top level is `FAILED_PRECONDITION` too; a folder with no
/// repository at all is the library's failure.
pub(crate) fn require_work_tree(dir: &Path) -> Result<(), Fault> {
    init_library()?;
    // The folders above are searched too, to tell "inside a work tree" from
    // "no work tree at all".
    let repo = Repository::discover(dir).map_err(open_fault)?;
    if repo.is_bare() {
        return Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            "the path is a bare git repository, not a work tree",
        ));
    }
    if !is_top_level(&repo, dir) {
        return Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            "the path is not the top level of a git work tree",
        ));
    }
    Ok(())
}

/// The fetch URL of `origin`, read from the configuration of the repository
/// at `dir` without contacting the network. No `origin`, or an empty URL, is
/// `None`.
pub(crate) fn origin_url(dir: &Path) -> Result<Option<String>, Fault> {
    require_work_tree(dir)?;
    let repo = Repository::open(dir).map_err(open_fault)?;
    origin_of(&repo)
}

/// The fetch URL of `origin` in the configuration of `repo`. No `origin`, or
/// an empty URL, is `None`.
pub(crate) fn origin_of(repo: &Repository) -> Result<Option<String>, Fault> {
    let config = repo.config().map_err(lib_fault)?;
    match config.get_string("remote.origin.url") {
        Ok(url) => Ok((!url.trim().is_empty()).then(|| url.trim().to_owned())),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(None),
        Err(err) => Err(lib_fault(err)),
    }
}

/// Sets `core.longpaths` in the repository at `dir`, a clone made by the
/// command line.
pub(crate) fn enable_long_paths_at(dir: &Path) -> Result<(), Fault> {
    init_library()?;
    let repo = Repository::open(dir).map_err(open_fault)?;
    enable_long_paths(&repo)
}

/// The branch a new repository starts on: `init.defaultBranch` of the
/// configuration the library reads, or `main`.
fn default_branch() -> String {
    git2::Config::open_default()
        .and_then(|config| config.get_string("init.defaultbranch"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| FALLBACK_BRANCH.to_owned())
}

/// Makes a git repository with its work tree at `dir`: SHA-1 objects and
/// files for references, on the branch `init.defaultBranch` names (or
/// `main`), with `core.longpaths` set so that a 255-byte file name fits a
/// long absolute path on Windows. A folder `.git` that is there already and
/// empty is the claim of the caller and is filled in.
pub(crate) fn init_repository(dir: &Path) -> Result<(), Fault> {
    init_library()?;
    let mut options = RepositoryInitOptions::new();
    options.initial_head(&default_branch()).mkpath(true);
    let repo = Repository::init_opts(dir, &options).map_err(lib_fault)?;
    enable_long_paths(&repo)
}

/// Sets `core.longpaths` in the configuration of `repo`.
fn enable_long_paths(repo: &Repository) -> Result<(), Fault> {
    let mut config = repo.config().map_err(lib_fault)?;
    config.set_bool("core.longpaths", true).map_err(lib_fault)
}

/// Whether git's index in `repo` tracks `prefix` (repository-relative) or
/// anything below it.
pub(crate) fn index_has_prefix(repo: &Repository, prefix: &str) -> Result<bool, Fault> {
    let mut index = repo.index().map_err(lib_fault)?;
    index.read(true).map_err(lib_fault)?;
    let prefix = prefix.trim_end_matches('/');
    if index.get_path(Path::new(prefix), 0).is_some() {
        return Ok(true);
    }
    match index.find_prefix(format!("{prefix}/")) {
        Ok(_) => Ok(true),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(false),
        Err(err) => Err(lib_fault(err)),
    }
}

/// The repository-relative paths git's index holds as *unmerged*: a merge,
/// a pull, or a rebase that stopped on a conflict and was not finished.
/// Each path is listed once, in the order of the index. Git's own state is
/// the answer; the bytes of a file say nothing about it.
pub(crate) fn unmerged_paths(repo: &Repository) -> Result<Vec<String>, Fault> {
    let mut index = repo.index().map_err(lib_fault)?;
    index.read(true).map_err(lib_fault)?;
    if !index.has_conflicts() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<String> = Vec::new();
    for conflict in index.conflicts().map_err(lib_fault)? {
        let conflict = conflict.map_err(lib_fault)?;
        let Some(entry) = conflict.our.or(conflict.their).or(conflict.ancestor) else {
            continue;
        };
        let path = String::from_utf8_lossy(&entry.path).into_owned();
        if paths.last() != Some(&path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// The repository-relative paths below `dirs` that differ from what `HEAD`
/// holds: modified, deleted, staged, or new (untracked, not ignored) files,
/// each once, in the order the library reports them. A path git holds as
/// unmerged is left to [`unmerged_paths`].
pub(crate) fn changed_paths(repo: &Repository, dirs: &[String]) -> Result<Vec<String>, Fault> {
    if dirs.is_empty() {
        return Ok(Vec::new());
    }
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .include_unmodified(false)
        .exclude_submodules(true);
    for dir in dirs {
        options.pathspec(dir.trim_end_matches('/'));
    }
    let statuses = repo.statuses(Some(&mut options)).map_err(lib_fault)?;
    let mut paths = Vec::new();
    for entry in statuses.iter() {
        if entry.status().is_conflicted() {
            continue;
        }
        let Ok(path) = entry.path() else { continue };
        paths.push(path.to_owned());
    }
    Ok(paths)
}

/// The repositories of one Vault, with their pools.
#[derive(Debug)]
pub(crate) struct Repos {
    main: Arc<Pool>,
    nested: Arc<Pool>,
}

impl Repos {
    /// The repositories of the Vault at `vault`. Nothing is opened yet.
    pub(crate) fn new(vault: &Path) -> Repos {
        Repos {
            main: Arc::new(Pool::new(Repo::Main.root(vault))),
            nested: Arc::new(Pool::new(Repo::Isolated.root(vault))),
        }
    }

    /// The folder of the Vault: the work tree of the main repository.
    pub(crate) fn root(&self) -> &Path {
        &self.main.root
    }

    /// A repository of `repo` for the caller to use, or `None` when it has
    /// not been made (the nested repository exists once an isolated
    /// Document has been written).
    pub(crate) fn lease(&self, repo: Repo) -> Result<Option<Lease>, Fault> {
        match repo {
            Repo::Main => &self.main,
            Repo::Isolated => &self.nested,
        }
        .lease()
    }

    /// The state of `repo` as `HEAD` names it now. A repository that is not
    /// there reads as an empty one.
    pub(crate) fn snapshot(&self, repo: Repo) -> Result<Snap, Fault> {
        let lease = self.lease(repo)?;
        let head = match &lease {
            Some(lease) => resolve_head(lease)?,
            None => None,
        };
        Ok(Snap {
            lease,
            which: repo,
            head,
        })
    }

    /// Both repositories as `HEAD` names them now: the state a check reads.
    pub(crate) fn views(&self) -> Result<Views, Fault> {
        Ok(Views {
            main: self.snapshot(Repo::Main)?,
            nested: self.snapshot(Repo::Isolated)?,
        })
    }
}

/// Snapshots of both repositories of a Vault, taken together.
#[derive(Debug)]
pub(crate) struct Views {
    main: Snap,
    nested: Snap,
}

impl Views {
    /// The snapshot of `repo`.
    pub(crate) fn of(&self, repo: Repo) -> &Snap {
        match repo {
            Repo::Main => &self.main,
            Repo::Isolated => &self.nested,
        }
    }
}

/// The idle repositories of one path.
struct Pool {
    root: PathBuf,
    idle: Mutex<Vec<Repository>>,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool").field("root", &self.root).finish()
    }
}

impl Pool {
    fn new(root: PathBuf) -> Pool {
        Pool {
            root,
            idle: Mutex::new(Vec::new()),
        }
    }

    /// An idle repository, or a newly opened one.
    fn lease(self: &Arc<Pool>) -> Result<Option<Lease>, Fault> {
        // The list holds no invariant a panic could break, so a poisoned
        // lock is as good as a clean one.
        let idle = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let repo = match idle {
            Some(repo) => repo,
            None => match open_at(&self.root)? {
                Some(repo) => repo,
                None => return Ok(None),
            },
        };
        Ok(Some(Lease {
            pool: Arc::clone(self),
            repo: Some(repo),
        }))
    }
}

/// A repository taken from its pool. It goes back when it is dropped.
pub(crate) struct Lease {
    pool: Arc<Pool>,
    repo: Option<Repository>,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease").field("pool", &self.pool).finish()
    }
}

impl Deref for Lease {
    type Target = Repository;

    fn deref(&self) -> &Repository {
        // `repo` is `Some` from the lease until `drop`.
        match &self.repo {
            Some(repo) => repo,
            None => unreachable!("a lease holds its repository until it is dropped"),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(repo) = self.repo.take() {
            let mut idle = self
                .pool
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if idle.len() < IDLE_MAX {
                idle.push(repo);
            }
        }
    }
}

/// The commit `HEAD` of `repo` names and its tree, or `None` on an unborn
/// branch.
fn resolve_head(repo: &Repository) -> Result<Option<(Oid, Oid)>, Fault> {
    let id = match repo.refname_to_id("HEAD") {
        Ok(id) => id,
        Err(err) if matches!(err.code(), ErrorCode::NotFound | ErrorCode::UnbornBranch) => {
            return Ok(None);
        }
        Err(err) => return Err(lib_fault(err)),
    };
    let commit = repo.find_commit(id).map_err(lib_fault)?;
    Ok(Some((id, commit.tree_id())))
}

/// What a tree entry is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NodeKind {
    /// A file, executable or not.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Link,
    /// Anything else (a submodule).
    Other,
}

/// One entry of a tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Node {
    /// The id of the blob or of the tree.
    pub(crate) id: Oid,
    /// What it is.
    pub(crate) kind: NodeKind,
    /// Whether it is a file with the executable mode.
    pub(crate) executable: bool,
}

impl Node {
    fn of(entry: &git2::TreeEntry<'_>) -> Node {
        let mode = entry.filemode_raw();
        let kind = if mode == i32::from(FileMode::Tree) {
            NodeKind::Dir
        } else if mode == i32::from(FileMode::Link) {
            NodeKind::Link
        } else if mode == i32::from(FileMode::Blob)
            || mode == i32::from(FileMode::BlobExecutable)
            || mode == i32::from(FileMode::BlobGroupWritable)
        {
            NodeKind::File
        } else {
            NodeKind::Other
        };
        Node {
            id: entry.id(),
            kind,
            executable: mode == i32::from(FileMode::BlobExecutable),
        }
    }
}

/// One entry of a directory of a tree, with its name.
#[derive(Clone, Debug)]
pub(crate) struct Child {
    /// The name, one path component.
    pub(crate) name: String,
    /// What it is.
    pub(crate) node: Node,
}

/// A file that differs between two commits of one repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Delta {
    /// The Vault-relative path, POSIX.
    pub(crate) path: String,
    /// Whether the later commit lacks the file. A file that is new or
    /// changed is not deleted.
    pub(crate) deleted: bool,
}

/// One commit, as a history walk reads it.
#[derive(Clone, Debug)]
pub(crate) struct CommitInfo {
    /// The id of its tree.
    pub(crate) tree: Oid,
    /// Its parents, in order.
    pub(crate) parents: Vec<Oid>,
    /// The commit time, in seconds since the epoch (UTC).
    pub(crate) seconds: i64,
    /// The name of its author.
    pub(crate) author_name: String,
    /// The email of its author.
    pub(crate) author_email: String,
    /// Its whole message.
    pub(crate) message: String,
}

#[cfg(test)]
thread_local! {
    /// How many blobs this thread has read through a [`Snap`], for the tests
    /// that show a stream reads no more than it hands out.
    static BLOB_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The number of blobs this thread has read through a [`Snap`] so far.
#[cfg(test)]
pub(crate) fn blob_reads() -> u64 {
    BLOB_READS.with(std::cell::Cell::get)
}

/// The bytes of a file of a tree with the id of its blob.
#[derive(Debug)]
pub(crate) struct Blob {
    /// The id of the blob: the revision of a Document.
    pub(crate) id: Oid,
    /// The bytes.
    pub(crate) bytes: Vec<u8>,
}

/// One state of a repository: the commit `HEAD` named when the snapshot was
/// taken, and the tree of that commit. Reads through it see that state and
/// no other, whatever is committed or edited meanwhile.
///
/// The paths it takes are Vault-relative and POSIX, whichever repository it
/// is of: a snapshot of the nested repository strips `.isolated/`. A
/// symbolic link on the way to a path is `INVALID_ARGUMENT`.
#[derive(Debug)]
pub(crate) struct Snap {
    lease: Option<Lease>,
    which: Repo,
    head: Option<(Oid, Oid)>,
}

impl Snap {
    /// Which repository this is a snapshot of.
    pub(crate) fn repo(&self) -> Repo {
        self.which
    }

    /// The commit of the snapshot, `None` for an unborn branch or a
    /// repository that is not there.
    pub(crate) fn commit(&self) -> Option<Oid> {
        self.head.map(|(commit, _)| commit)
    }

    /// The id of the tree of the commit.
    pub(crate) fn tree(&self) -> Option<Oid> {
        self.head.map(|(_, tree)| tree)
    }

    /// The repository, when there is one.
    pub(crate) fn repository(&self) -> Option<&Repository> {
        self.lease.as_deref()
    }

    /// The path inside this snapshot's repository.
    fn inner<'a>(&self, rel: &'a str) -> &'a str {
        let rel = rel.trim_end_matches('/');
        match self.which {
            Repo::Main => rel,
            Repo::Isolated => {
                if rel == isolated::DIR {
                    ""
                } else {
                    isolated::nested_rel(rel).unwrap_or(rel)
                }
            }
        }
    }

    /// The Vault-relative form of the repository-relative `inner`.
    fn outer(&self, inner: &str) -> String {
        match self.which {
            Repo::Main => inner.to_owned(),
            Repo::Isolated => format!("{}/{inner}", isolated::DIR),
        }
    }

    /// The entry at `rel`, or `None` when the commit holds nothing there.
    /// `rel` empty names the root.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the path, or a folder on the way to it, is a
    /// symbolic link; the library's failure when an object cannot be read.
    pub(crate) fn lookup(&self, rel: &str) -> Result<Option<Node>, Fault> {
        match self.tree() {
            Some(root) => self.lookup_in(root, rel),
            None => Ok(None),
        }
    }

    /// The entry at `rel` in the tree `root` of this snapshot's repository
    /// (the tree of an older commit, say), as [`lookup`](Self::lookup) finds
    /// it in the tree of the snapshot.
    pub(crate) fn lookup_in(&self, root: Oid, rel: &str) -> Result<Option<Node>, Fault> {
        let Some(repo) = self.repository() else {
            return Ok(None);
        };
        let inner = self.inner(rel);
        if inner.is_empty() {
            return Ok(Some(Node {
                id: root,
                kind: NodeKind::Dir,
                executable: false,
            }));
        }
        let mut tree = repo.find_tree(root).map_err(lib_fault)?;
        let parts: Vec<&str> = inner.split('/').collect();
        for (at, part) in parts.iter().enumerate() {
            let node = match tree.get_name(part) {
                Some(entry) => Node::of(&entry),
                None => return Ok(None),
            };
            if node.kind == NodeKind::Link {
                let link = self.outer(&parts[..=at].join("/"));
                return Err(Fault::new(
                    VaultErrorCode::InvalidArgument,
                    format!("{link} is a symlink"),
                )
                .with_path(link));
            }
            if at + 1 == parts.len() {
                return Ok(Some(node));
            }
            if node.kind != NodeKind::Dir {
                return Ok(None);
            }
            tree = repo.find_tree(node.id).map_err(lib_fault)?;
        }
        Ok(None)
    }

    /// The file at `rel`: `None` when the commit holds nothing there or a
    /// directory.
    pub(crate) fn read(&self, rel: &str) -> Result<Option<Blob>, Fault> {
        let Some(node) = self.lookup(rel)? else {
            return Ok(None);
        };
        if node.kind != NodeKind::File {
            return Ok(None);
        }
        let bytes = self.blob(node.id).map_err(|fault| fault.with_path(rel))?;
        Ok(Some(Blob { id: node.id, bytes }))
    }

    /// The files that differ between the commit `old` names (`None`: an empty
    /// tree) and the commit of this snapshot, below the Vault-relative
    /// directory `under` or anywhere when it is `None`. Pure tree
    /// comparison: no process, no work tree. A renamed file is a deleted one
    /// and a new one; a name that is not UTF-8 is left out.
    ///
    /// `None` when the object database lacks the commit `old` names (the
    /// history was rewritten and cleaned up): the difference is not known.
    pub(crate) fn changed_since(
        &self,
        old: Option<Oid>,
        under: Option<&str>,
    ) -> Result<Option<Vec<Delta>>, Fault> {
        let Some(repo) = self.repository() else {
            return Ok(old.is_none().then(Vec::new));
        };
        let old_tree = match old {
            None => None,
            Some(id) => match repo.find_commit(id) {
                Ok(commit) => Some(commit.tree().map_err(lib_fault)?),
                Err(err) if err.code() == ErrorCode::NotFound => return Ok(None),
                Err(err) => return Err(lib_fault(err)),
            },
        };
        let new_tree = match self.tree() {
            Some(id) => Some(repo.find_tree(id).map_err(lib_fault)?),
            None => None,
        };
        let mut options = DiffOptions::new();
        if let Some(dir) = under {
            let inner = self.inner(dir);
            if !inner.is_empty() {
                options.pathspec(inner);
            }
        }
        let diff = repo
            .diff_tree_to_tree(old_tree.as_ref(), new_tree.as_ref(), Some(&mut options))
            .map_err(lib_fault)?;
        let mut out = Vec::new();
        for delta in diff.deltas() {
            let deleted = delta.status() == git2::Delta::Deleted;
            let file = if deleted {
                delta.old_file()
            } else {
                delta.new_file()
            };
            let Some(path) = file.path().and_then(Path::to_str) else {
                continue;
            };
            out.push(Delta {
                path: self.outer(path),
                deleted,
            });
        }
        Ok(Some(out))
    }

    /// What a commit of this snapshot's repository holds that a history
    /// walk needs: its tree, parents, time, author, and message.
    pub(crate) fn commit_info(&self, id: Oid) -> Result<CommitInfo, Fault> {
        let Some(repo) = self.repository() else {
            return Err(Fault::new(
                VaultErrorCode::Replication,
                "the repository is not there",
            ));
        };
        let commit = repo.find_commit(id).map_err(lib_fault)?;
        let author = commit.author();
        Ok(CommitInfo {
            tree: commit.tree_id(),
            parents: commit.parent_ids().collect(),
            seconds: commit.time().seconds(),
            author_name: author.name().unwrap_or_default().to_owned(),
            author_email: author.email().unwrap_or_default().to_owned(),
            message: commit.message().unwrap_or_default().to_owned(),
        })
    }

    /// The bytes of the blob `id`.
    pub(crate) fn blob(&self, id: Oid) -> Result<Vec<u8>, Fault> {
        #[cfg(test)]
        BLOB_READS.with(|reads| reads.set(reads.get() + 1));
        let Some(repo) = self.repository() else {
            return Err(Fault::new(
                VaultErrorCode::Replication,
                "the repository is not there",
            ));
        };
        let blob = repo.find_blob(id).map_err(lib_fault)?;
        Ok(blob.content().to_vec())
    }

    /// The size in bytes of the file at `rel`, without reading it; `None`
    /// when the commit holds no file there.
    pub(crate) fn size(&self, rel: &str) -> Result<Option<u64>, Fault> {
        let Some(node) = self.lookup(rel)? else {
            return Ok(None);
        };
        let Some(repo) = self.repository() else {
            return Ok(None);
        };
        if node.kind != NodeKind::File {
            return Ok(None);
        }
        let odb = repo.odb().map_err(lib_fault)?;
        let (size, kind) = odb.read_header(node.id).map_err(lib_fault)?;
        let _ = kind;
        Ok(Some(size as u64))
    }

    /// The entries of the directory at `rel`, in the order of the tree.
    /// `None` when the commit holds no directory there. An entry whose name
    /// is not UTF-8 is not listed.
    pub(crate) fn list(&self, rel: &str) -> Result<Option<Vec<Child>>, Fault> {
        let Some(node) = self.lookup(rel)? else {
            return Ok(None);
        };
        let Some(repo) = self.repository() else {
            return Ok(None);
        };
        if node.kind != NodeKind::Dir {
            return Ok(None);
        }
        let tree = repo.find_tree(node.id).map_err(lib_fault)?;
        let children = tree
            .iter()
            .filter_map(|entry| {
                entry.name().ok().map(|name| Child {
                    name: name.to_owned(),
                    node: Node::of(&entry),
                })
            })
            .collect();
        Ok(Some(children))
    }

    /// Every symbolic link in the tree below the directory `rel`, as
    /// Vault-relative paths in the order of the trees. A link on the way to
    /// `rel` is the only one reported.
    pub(crate) fn symlinks_below(&self, rel: &str) -> Result<Vec<String>, Fault> {
        let mut found = Vec::new();
        let node = match self.lookup(rel) {
            Ok(node) => node,
            Err(fault) => {
                if let Some(link) = fault.path() {
                    found.push(link.to_owned());
                }
                return Ok(found);
            }
        };
        if node.is_some_and(|node| node.kind == NodeKind::Dir) {
            self.collect_links(rel.trim_end_matches('/'), &mut found)?;
        } else if node.is_some_and(|node| node.kind == NodeKind::Link) {
            found.push(rel.to_owned());
        }
        Ok(found)
    }

    fn collect_links(&self, dir: &str, found: &mut Vec<String>) -> Result<(), Fault> {
        let Some(children) = self.list(dir)? else {
            return Ok(());
        };
        for child in children {
            let path = if dir.is_empty() {
                child.name.clone()
            } else {
                format!("{dir}/{}", child.name)
            };
            match child.node.kind {
                NodeKind::Link => found.push(path),
                NodeKind::Dir => self.collect_links(&path, found)?,
                NodeKind::File | NodeKind::Other => {}
            }
        }
        Ok(())
    }
}

/// Helpers for the unit tests that need a repository with files in it.
#[cfg(test)]
pub(crate) mod fixture {
    use std::path::Path;

    use git2::{IndexAddOption, Repository, Signature};

    use super::{Repo, Repos, Snap, init_library};

    /// Makes a git repository at `dir`, empty.
    pub(crate) fn init(dir: &Path) {
        init_library().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        Repository::init(dir).unwrap();
    }

    /// A snapshot of `HEAD` of the repository at `dir`.
    pub(crate) fn snap(dir: &Path) -> Snap {
        Repos::new(dir).snapshot(Repo::Main).unwrap()
    }

    /// A git work tree at `dir` (made if it is not one), whatever the files
    /// in it are: `git add --all` and `git commit`, as the library does
    /// them. Returns nothing; a failure fails the test.
    pub(crate) fn commit_all(dir: &Path) {
        init_library().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let repo = match Repository::open(dir) {
            Ok(repo) => repo,
            Err(_) => Repository::init(dir).unwrap(),
        };
        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
        index.update_all(["*"], None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Test", "test@example.test").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &parents)
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::VaultErrorCode;

    use super::{
        Repo, Repos, changed_paths, fixture, index_has_prefix, init_repository, is_work_tree,
        origin_url, require_work_tree, unmerged_paths,
    };
    use crate::git::{git, isolate_test_git_env};

    /// A scratch directory that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-repo-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn git(&self, args: &[&str]) -> bool {
            let mut cmd = git().current_dir(&self.0);
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
        assert!(!is_work_tree(&scratch.0).unwrap());
        assert!(!is_work_tree(&scratch.0.join("missing")).unwrap());
        assert!(require_work_tree(&scratch.0).is_err());
    }

    #[test]
    fn a_folder_inside_a_work_tree_is_not_its_top_level() {
        let scratch = Scratch::new("inner");
        fixture::init(&scratch.0);
        std::fs::create_dir(scratch.0.join("sub")).unwrap();
        assert!(is_work_tree(&scratch.0).unwrap());
        require_work_tree(&scratch.0).unwrap();
        assert!(!is_work_tree(&scratch.0.join("sub")).unwrap());
        let error = require_work_tree(&scratch.0.join("sub")).unwrap_err();
        assert_eq!(error.vault_code(), VaultErrorCode::FailedPrecondition);
    }

    #[test]
    fn a_bare_repository_is_not_a_work_tree() {
        let scratch = Scratch::new("bare");
        assert!(scratch.git(&["init", "--quiet", "--bare", "."]));
        assert!(!is_work_tree(&scratch.0).unwrap());
        let error = require_work_tree(&scratch.0).unwrap_err();
        assert_eq!(error.vault_code(), VaultErrorCode::FailedPrecondition);
    }

    #[test]
    fn a_repository_the_library_cannot_read_is_unimplemented() {
        for (name, flag) in [
            ("sha256", "--object-format=sha256"),
            ("reftable", "--ref-format=reftable"),
        ] {
            let scratch = Scratch::new(name);
            if !scratch.git(&["init", "--quiet", flag, "."]) {
                // This git cannot make such a repository.
                continue;
            }
            let error = is_work_tree(&scratch.0).unwrap_err();
            assert_eq!(error.vault_code(), VaultErrorCode::Unimplemented, "{name}");
        }
    }

    #[test]
    fn origin_url_is_the_fetch_url_or_none() {
        let scratch = Scratch::new("origin");
        fixture::init(&scratch.0);
        assert_eq!(origin_url(&scratch.0).unwrap(), None);
        assert!(scratch.git(&[
            "remote",
            "add",
            "origin",
            "https://example.test/org/repo.git"
        ]));
        assert_eq!(
            origin_url(&scratch.0).unwrap().as_deref(),
            Some("https://example.test/org/repo.git")
        );
    }

    #[test]
    fn a_new_repository_starts_on_main_with_long_paths_and_sha1_objects() {
        let scratch = Scratch::new("init");
        init_repository(&scratch.0.join("made")).unwrap();
        let made = scratch.0.join("made");
        assert!(is_work_tree(&made).unwrap());
        let head = std::fs::read_to_string(made.join(".git/HEAD")).unwrap();
        assert_eq!(head.trim(), "ref: refs/heads/main");
        let config = std::fs::read_to_string(made.join(".git/config")).unwrap();
        assert!(config.contains("longpaths = true"), "{config}");
        assert!(!config.contains("objectformat"), "{config}");
        assert!(
            Repos::new(&made)
                .snapshot(Repo::Main)
                .unwrap()
                .commit()
                .is_none()
        );
    }

    #[test]
    fn the_index_prefix_is_a_directory_or_a_file_of_that_name() {
        let scratch = Scratch::new("prefix");
        fixture::init(&scratch.0);
        for rel in [".isolated/a/b", ".isolatedx/c", "other"] {
            scratch.write(rel, b"x");
        }
        assert!(scratch.git(&["add", "other", ".isolatedx"]));
        let repos = Repos::new(&scratch.0);
        let lease = repos.lease(Repo::Main).unwrap().unwrap();
        assert!(!index_has_prefix(&lease, ".isolated").unwrap());
        assert!(index_has_prefix(&lease, ".isolatedx/").unwrap());
        assert!(scratch.git(&["add", "--force", ".isolated"]));
        assert!(index_has_prefix(&lease, ".isolated").unwrap());
        assert!(index_has_prefix(&lease, "other").unwrap());
    }

    #[test]
    fn unmerged_paths_are_git_state_listed_once_each() {
        let scratch = Scratch::new("unmerged");
        fixture::init(&scratch.0);
        let repos = Repos::new(&scratch.0);
        let lease = repos.lease(Repo::Main).unwrap().unwrap();
        assert!(unmerged_paths(&lease).unwrap().is_empty());
        scratch.write("a", b"base\n");
        scratch.write("b", b"base\n");
        assert!(scratch.git(&["add", "--all"]));
        assert!(scratch.git(&["commit", "--quiet", "--message", "base"]));
        assert!(scratch.git(&["checkout", "--quiet", "-b", "other"]));
        scratch.write("a", b"theirs\n");
        scratch.write("b", b"theirs\n");
        assert!(scratch.git(&["commit", "--quiet", "--all", "--message", "theirs"]));
        assert!(scratch.git(&["checkout", "--quiet", "-"]));
        scratch.write("a", b"ours\n");
        scratch.write("b", b"ours\n");
        assert!(scratch.git(&["commit", "--quiet", "--all", "--message", "ours"]));
        assert!(!scratch.git(&["merge", "--quiet", "--no-edit", "other"]));
        assert_eq!(unmerged_paths(&lease).unwrap(), ["a", "b"]);
        // Resolving one path leaves the other.
        scratch.write("a", b"resolved\n");
        assert!(scratch.git(&["add", "a"]));
        assert_eq!(unmerged_paths(&lease).unwrap(), ["b"]);
    }

    #[test]
    fn changed_paths_lists_modified_deleted_staged_and_new_files_below_the_folders() {
        let scratch = Scratch::new("changed");
        for rel in ["notes/a", "notes/b", "notes/c", "other/x"] {
            scratch.write(rel, b"one");
        }
        scratch.write(".gitignore", b"notes/ignored\n");
        fixture::commit_all(&scratch.0);
        let repos = Repos::new(&scratch.0);
        let lease = repos.lease(Repo::Main).unwrap().unwrap();
        let dirs = ["notes".to_owned()];
        assert!(changed_paths(&lease, &dirs).unwrap().is_empty());
        scratch.write("notes/a", b"two");
        std::fs::remove_file(scratch.0.join("notes/b")).unwrap();
        scratch.write("notes/c", b"three");
        assert!(scratch.git(&["add", "notes/c"]));
        scratch.write("notes/new/d", b"new");
        scratch.write("notes/ignored", b"ignored");
        scratch.write("other/x", b"two");
        let mut got = changed_paths(&lease, &dirs).unwrap();
        got.sort();
        assert_eq!(got, ["notes/a", "notes/b", "notes/c", "notes/new/d"]);
        assert!(changed_paths(&lease, &[]).unwrap().is_empty());
    }

    #[test]
    fn a_snapshot_keeps_its_commit_and_the_pool_hands_out_several_repositories() {
        let scratch = Scratch::new("snap");
        scratch.write("a", b"one");
        fixture::commit_all(&scratch.0);
        let repos = Repos::new(&scratch.0);
        let old = repos.snapshot(Repo::Main).unwrap();
        scratch.write("a", b"two");
        fixture::commit_all(&scratch.0);
        let new = repos.snapshot(Repo::Main).unwrap();
        assert_ne!(old.commit(), new.commit());
        assert_eq!(old.read("a").unwrap().unwrap().bytes, b"one");
        assert_eq!(new.read("a").unwrap().unwrap().bytes, b"two");
        // Both snapshots hold a repository at the same time, and a thread
        // of its own may take another.
        let other = std::thread::scope(|scope| {
            scope
                .spawn(|| repos.snapshot(Repo::Main).unwrap().commit())
                .join()
                .unwrap()
        });
        assert_eq!(other, new.commit());
        // The nested repository is not there: it reads as an empty one.
        let nested = repos.snapshot(Repo::Isolated).unwrap();
        assert!(nested.commit().is_none());
        assert!(nested.read(".isolated/x").unwrap().is_none());
    }
}
