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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use git2::{
    DiffOptions, ErrorCode, FileMode, Oid, Repository, RepositoryInitOptions, StatusOptions,
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

/// The size of the git library's object cache, in bytes, for the process.
/// The library caches every tree and commit up to 4 KB, up to 256 MiB; a
/// process that reads a lot fills any budget it is given. 16 MiB keeps a
/// random read within 15 % of the speed of 256 MiB, with a sixteenth of the
/// memory.
pub(crate) const OBJECT_CACHE_BYTES: isize = 16 * 1024 * 1024;

/// Sets the options of the git library that belong to the process, once,
/// before the first repository is opened: the files of git's own directory
/// are flushed to disk when they are written (the library leaves that off,
/// and a commit that a power loss can undo is not a stored write), and the
/// object cache is [`OBJECT_CACHE_BYTES`]. The cache budget is set whoever
/// used the library before: a process that read repositories through `git2`
/// first has the budget of the library's default until the first `Open`.
///
/// A library that refuses the first option fails every call that opens a
/// repository with `STORAGE`: writes that are not flushed are not offered.
/// The same for the second.
pub(crate) fn init_library() -> Result<(), Fault> {
    static OUTCOME: OnceLock<std::ffi::c_int> = OnceLock::new();
    let outcome = *OUTCOME.get_or_init(|| {
        #[cfg(test)]
        crate::git::isolate_test_git_env();
        libgit2_sys::init();
        // SAFETY: the library is initialized, the option takes one `int`,
        // and `OnceLock` keeps this call from racing another thread's.
        let flushed = unsafe {
            libgit2_sys::git_libgit2_opts(
                libgit2_sys::GIT_OPT_ENABLE_FSYNC_GITDIR as std::ffi::c_int,
                1 as std::ffi::c_int,
            )
        };
        if flushed < 0 {
            return flushed;
        }
        // SAFETY: the option takes one `ssize_t` and the library guards the
        // cache with a lock of its own; `OnceLock` keeps Scheda's calls from
        // racing each other.
        match unsafe { git2::opts::set_cache_max_size(OBJECT_CACHE_BYTES) } {
            Ok(()) => 0,
            Err(_) => 1,
        }
    });
    match outcome {
        0 => Ok(()),
        1 => Err(Fault::new(
            VaultErrorCode::Storage,
            "the git library refused to set the size of its object cache",
        )),
        _ => Err(Fault::new(
            VaultErrorCode::Storage,
            "the git library refused to flush its files to disk",
        )),
    }
}

/// The failure of a repository the library would not open. The repository's
/// own configuration is read for the two extensions this build cannot read,
/// `extensions.objectformat` other than `sha1` (SHA-256 objects) and
/// `extensions.refstorage` other than `files` (reftable references); with
/// either the failure is `UNIMPLEMENTED`. Any other failure is the
/// library's. `root` is the work tree or git directory that was opened;
/// with `search_above`, the folders above it were searched too.
fn open_fault(root: &Path, search_above: bool, err: git2::Error) -> Fault {
    match unsupported_format(root, search_above) {
        Some(format) => Fault::new(
            VaultErrorCode::Unimplemented,
            format!(
                "the git repository uses {format}, which this build cannot read; use a \
                 repository of SHA-1 objects and files for references"
            ),
        )
        .with_source(err),
        None => lib_fault(err),
    }
}

/// What the configuration of the repository at `root` asks for that this
/// build cannot read, if it does. A repository or a configuration that
/// cannot be found or read says nothing here.
fn unsupported_format(root: &Path, search_above: bool) -> Option<&'static str> {
    let ceiling: Vec<&Path> = match root.parent() {
        Some(parent) if !search_above => vec![parent],
        _ => Vec::new(),
    };
    let git_dir = Repository::discover_path(root, ceiling).ok()?;
    // A linked work tree keeps the repository's configuration in the
    // common directory its `commondir` file names.
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(named) => git_dir.join(named.trim()),
        Err(_) => git_dir,
    };
    let config = git2::Config::open(&common.join("config")).ok()?;
    if config
        .get_string("extensions.objectformat")
        .is_ok_and(|format| !format.trim().eq_ignore_ascii_case("sha1"))
    {
        return Some("a SHA-256 object format");
    }
    if config
        .get_string("extensions.refstorage")
        .is_ok_and(|storage| !storage.trim().eq_ignore_ascii_case("files"))
    {
        return Some("reftable reference storage");
    }
    None
}

/// Opens the repository whose work tree or git directory is `root`, without
/// searching the folders above. `None` when there is none there.
fn open_at(root: &Path) -> Result<Option<Repository>, Fault> {
    init_library()?;
    match Repository::open(root) {
        Ok(repo) => {
            crate::fallback::register(&repo)?;
            Ok(Some(repo))
        }
        Err(err) if err.code() == ErrorCode::NotFound => Ok(None),
        Err(err) => Err(open_fault(root, false, err)),
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
            let fault = open_fault(dir, false, err);
            return if owner || fault.vault_code() == VaultErrorCode::Unimplemented {
                Err(fault)
            } else {
                Ok(false)
            };
        }
    };
    Ok(is_top_level(&repo, dir))
}

/// Whether `repo` is the repository of the folder `dir`: its work tree is
/// `dir`, or, for a repository without a work tree (`core.bare`), its git
/// folder is `dir/.git`. A bare repository whose own folder is `dir` is not.
fn is_top_level(repo: &Repository, dir: &Path) -> bool {
    let Ok(dir) = dir.canonicalize() else {
        return false;
    };
    match repo.workdir() {
        Some(work) => work.canonicalize().is_ok_and(|work| work == dir),
        None => repo
            .path()
            .canonicalize()
            .is_ok_and(|git| git == dir.join(".git")),
    }
}

/// Succeeds when `dir` is the top level of a git work tree, or of a Vault
/// without one (its `.git` folder holds `core.bare = true`). A bare
/// repository is `FAILED_PRECONDITION`; a folder inside a work tree that is
/// not its top level is `FAILED_PRECONDITION` too; a folder with no
/// repository at all is the library's failure.
pub(crate) fn require_work_tree(dir: &Path) -> Result<(), Fault> {
    init_library()?;
    // The folders above are searched too, to tell "inside a work tree" from
    // "no work tree at all".
    let repo = Repository::discover(dir).map_err(|err| open_fault(dir, true, err))?;
    if repo.is_bare() && !is_top_level(&repo, dir) {
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
    let repo = Repository::open(dir).map_err(|err| open_fault(dir, false, err))?;
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

/// Whether the repository at `dir` has no folder of files: `core.bare` in
/// its own configuration (the local level), `true` meaning none. A missing
/// key is a repository with files.
pub(crate) fn has_no_files(dir: &Path) -> Result<bool, Fault> {
    init_library()?;
    let repo = Repository::open(dir).map_err(|err| open_fault(dir, false, err))?;
    let config = repo
        .config()
        .and_then(|config| config.open_level(git2::ConfigLevel::Local))
        .map_err(lib_fault)?;
    match config.get_bool("core.bare") {
        Ok(bare) => Ok(bare),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(false),
        Err(err) => Err(lib_fault(err)),
    }
}

/// Sets `core.longpaths` in the repository at `dir`, a clone made by the
/// command line.
pub(crate) fn enable_long_paths_at(dir: &Path) -> Result<(), Fault> {
    init_library()?;
    let repo = Repository::open(dir).map_err(|err| open_fault(dir, false, err))?;
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
///
/// With `no_files` the repository is marked as one without a work tree:
/// `core.bare = true` and `core.logAllRefUpdates = false` in its local
/// configuration, so git refuses the commands that need files and keeps no
/// reflog. Its git folder stays `dir/.git`.
pub(crate) fn init_repository(dir: &Path, no_files: bool) -> Result<(), Fault> {
    init_repository_on(dir, no_files, None)
}

/// [`init_repository`] on the branch `branch` when it is set (already
/// checked to be a valid name), else on the default one.
pub(crate) fn init_repository_on(
    dir: &Path,
    no_files: bool,
    branch: Option<&str>,
) -> Result<(), Fault> {
    init_library()?;
    let mut options = RepositoryInitOptions::new();
    let name = branch.map_or_else(default_branch, str::to_owned);
    options.initial_head(&name).mkpath(true);
    let repo = Repository::init_opts(dir, &options).map_err(lib_fault)?;
    enable_long_paths(&repo)?;
    if no_files {
        set_no_files(&repo)?;
    }
    Ok(())
}

/// Marks `repo` as a repository without a work tree: `core.bare = true` and
/// no reflog, in its local configuration.
fn set_no_files(repo: &Repository) -> Result<(), Fault> {
    let mut config = repo
        .config()
        .and_then(|config| config.open_level(git2::ConfigLevel::Local))
        .map_err(lib_fault)?;
    config.set_bool("core.bare", true).map_err(lib_fault)?;
    config
        .set_bool("core.logAllRefUpdates", false)
        .map_err(lib_fault)
}

/// Gives the bare clone whose git folder is `git_dir` the configuration of a
/// Vault without a work tree.
pub(crate) fn mark_no_files_at(git_dir: &Path) -> Result<(), Fault> {
    init_library()?;
    let repo = Repository::open_bare(git_dir).map_err(|err| open_fault(git_dir, false, err))?;
    set_no_files(&repo)
}

/// Puts the fresh clone at `dir` on the branch `name`, from its own
/// references: a bare clone (`bare`) holds the mirror's branches as
/// `refs/heads/*`, one with files as `refs/remotes/origin/*`.
///
/// - The mirror has the branch: it is checked out (files) or `HEAD` names
///   it (bare), whatever the mirror's `HEAD` named. The local branch the
///   clone made for the mirror's `HEAD` is removed.
/// - The mirror has no branch at all: `HEAD` names `name`, unborn.
/// - The mirror holds branches but not `name`: `INVALID_ARGUMENT`.
pub(crate) fn select_clone_branch(dir: &Path, bare: bool, name: &str) -> Result<(), Fault> {
    init_library()?;
    let repo = if bare {
        Repository::open_bare(dir)
    } else {
        Repository::open(dir)
    }
    .map_err(|err| open_fault(dir, false, err))?;
    let local = format!("refs/heads/{name}");
    let remote = format!("refs/remotes/origin/{name}");
    let heads_glob = if bare {
        "refs/heads/*"
    } else {
        "refs/remotes/origin/*"
    };
    let holds_any = repo
        .references_glob(heads_glob)
        .map_err(lib_fault)?
        .any(|reference| {
            reference.is_ok_and(|reference| {
                reference
                    .name()
                    .is_ok_and(|name| name != "refs/remotes/origin/HEAD")
            })
        });
    if !holds_any {
        return repo.set_head(&local).map_err(lib_fault);
    }
    let has = |reference: &str| match repo.find_reference(reference) {
        Ok(_) => Ok(true),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(false),
        Err(err) => Err(lib_fault(err)),
    };
    if bare {
        if !has(&local)? {
            return Err(missing_branch(name));
        }
        return repo.set_head(&local).map_err(lib_fault);
    }
    if !has(&remote)? {
        return Err(missing_branch(name));
    }
    let head_name = repo
        .find_reference("HEAD")
        .ok()
        .and_then(|head| head.symbolic_target().ok().flatten().map(ToOwned::to_owned));
    if head_name.as_deref() == Some(local.as_str()) {
        return Ok(());
    }
    let commit = repo
        .find_reference(&remote)
        .and_then(|reference| reference.peel_to_commit())
        .map_err(lib_fault)?;
    let mut branch = repo.branch(name, &commit, true).map_err(lib_fault)?;
    branch
        .set_upstream(Some(&format!("origin/{name}")))
        .map_err(lib_fault)?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(commit.as_object(), Some(&mut checkout))
        .map_err(lib_fault)?;
    repo.set_head(&local).map_err(lib_fault)?;
    if let Some(old) = head_name.filter(|old| old.starts_with("refs/heads/")) {
        let mut old = repo.find_reference(&old).map_err(lib_fault)?;
        old.delete().map_err(lib_fault)?;
    }
    Ok(())
}

/// The refusal of a mirror that holds commits but not the branch asked for.
fn missing_branch(name: &str) -> Fault {
    Fault::new(
        VaultErrorCode::InvalidArgument,
        format!("the mirror holds commits but not the initial branch {name:?}"),
    )
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
    /// Counts the times a merge removed packs: see [`Repos::pack_generation`].
    generation: Arc<AtomicU64>,
}

impl Repos {
    /// The repositories of the Vault at `vault`. Nothing is opened yet.
    pub(crate) fn new(vault: &Path) -> Repos {
        let generation = Arc::new(AtomicU64::new(0));
        Repos {
            main: Arc::new(Pool::new(Repo::Main.root(vault), Arc::clone(&generation))),
            nested: Arc::new(Pool::new(
                Repo::Isolated.root(vault),
                Arc::clone(&generation),
            )),
            generation,
        }
    }

    /// The counter a merge of packs raises after it removed packs.
    ///
    /// The git library never forgets a pack: one a merge unlinked stays
    /// open and mapped in each repository handle that had seen it, and every
    /// lookup of an object on that handle walks the whole list, so a handle
    /// that lives through many merges gets slower with the history. A
    /// repository that went back to its pool under an older count is
    /// dropped, not leased again, and the next lease opens a new one. A
    /// handle a reader still holds keeps working until it is dropped.
    pub(crate) fn pack_generation(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.generation)
    }

    /// How many handles of the main repository wait in the pool.
    #[cfg(test)]
    pub(crate) fn idle_main(&self) -> usize {
        self.main
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
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

    /// The state of `repo` at `commit` (`None`: no commit), whatever `HEAD`
    /// names now. `None` when the repository lacks that commit: the history
    /// was rewritten and cleaned up.
    pub(crate) fn snapshot_at(
        &self,
        repo: Repo,
        commit: Option<Oid>,
    ) -> Result<Option<Snap>, Fault> {
        let lease = self.lease(repo)?;
        let head = match (commit, &lease) {
            (None, _) => None,
            (Some(_), None) => return Ok(None),
            (Some(id), Some(lease)) => match lease.find_commit(id) {
                Ok(found) => Some((id, found.tree_id())),
                Err(err) if err.code() == ErrorCode::NotFound => return Ok(None),
                Err(err) => return Err(lib_fault(err)),
            },
        };
        Ok(Some(Snap {
            lease,
            which: repo,
            head,
        }))
    }

    /// Both repositories at the commits `main` and `nested` name, as
    /// [`Repos::snapshot_at`]; `None` when either commit is gone.
    #[cfg(feature = "index")]
    pub(crate) fn views_at(
        &self,
        main: Option<Oid>,
        nested: Option<Oid>,
    ) -> Result<Option<Views>, Fault> {
        let Some(main) = self.snapshot_at(Repo::Main, main)? else {
            return Ok(None);
        };
        let Some(nested) = self.snapshot_at(Repo::Isolated, nested)? else {
            return Ok(None);
        };
        Ok(Some(Views { main, nested }))
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
    /// Each with the count of [`Repos::pack_generation`] it was opened at.
    idle: Mutex<Vec<(u64, Repository)>>,
    generation: Arc<AtomicU64>,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool").field("root", &self.root).finish()
    }
}

impl Pool {
    fn new(root: PathBuf, generation: Arc<AtomicU64>) -> Pool {
        Pool {
            root,
            idle: Mutex::new(Vec::new()),
            generation,
        }
    }

    /// An idle repository, or a newly opened one.
    fn lease(self: &Arc<Pool>) -> Result<Option<Lease>, Fault> {
        let now = self.generation.load(Ordering::Acquire);
        // The list holds no invariant a panic could break, so a poisoned
        // lock is as good as a clean one.
        let idle = {
            let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            // A repository that saw packs a merge removed since is let go.
            idle.retain(|(generation, _)| *generation == now);
            idle.pop()
        };
        let repo = match idle {
            Some((_, repo)) => repo,
            None => match open_at(&self.root)? {
                Some(repo) => repo,
                None => return Ok(None),
            },
        };
        Ok(Some(Lease {
            pool: Arc::clone(self),
            repo: Some(repo),
            generation: now,
        }))
    }
}

/// A repository taken from its pool. It goes back when it is dropped.
pub(crate) struct Lease {
    pool: Arc<Pool>,
    repo: Option<Repository>,
    /// The count of [`Repos::pack_generation`] it was leased at.
    generation: u64,
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
            if idle.len() < IDLE_MAX
                && self.generation == self.pool.generation.load(Ordering::Acquire)
            {
                idle.push((self.generation, repo));
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
        Node::of_mode(entry.id(), entry.filemode_raw())
    }

    /// The entry of the object `id` with the git file mode `mode`.
    fn of_mode(id: Oid, mode: i32) -> Node {
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
            id,
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
    /// The entry at `path` in the later commit, as the comparison of the
    /// two trees gave it; `None` when the later commit lacks the file.
    pub(crate) node: Option<Node>,
}

impl Delta {
    /// The blob of the regular file at `path` in the later commit, as
    /// [`Snap::lookup`] finds it in that commit, with no tree read: `None`
    /// when the commit lacks the file or holds something else there.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when it is a symbolic link.
    #[cfg(feature = "index")]
    pub(crate) fn file(&self) -> Result<Option<Oid>, Fault> {
        match self.node {
            Some(Node {
                kind: NodeKind::Link,
                ..
            }) => Err(symlink(self.path.clone())),
            Some(Node {
                kind: NodeKind::File,
                id,
                ..
            }) => Ok(Some(id)),
            _ => Ok(None),
        }
    }
}

/// The failure of a path that is, or goes through, the symbolic link
/// `link`, a Vault-relative path.
fn symlink(link: String) -> Fault {
    Fault::new(
        VaultErrorCode::InvalidArgument,
        format!("{link} is a symlink"),
    )
    .with_path(link)
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
                return Err(symlink(self.outer(&parts[..=at].join("/"))));
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
                node: (!deleted).then(|| Node::of_mode(file.id(), i32::from(file.mode()))),
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
        let (size, _) = odb.read_header(node.id).map_err(lib_fault)?;
        Ok(Some(size as u64))
    }

    /// The names of `names` that the directory `rel` holds as files (not
    /// folders or links), in the order of `names`: each looked up in the
    /// directory's tree, which is read once and never listed, so a
    /// directory of a million entries costs no more than one of ten.
    pub(crate) fn files_named<'n>(
        &self,
        rel: &str,
        names: &[&'n str],
    ) -> Result<Vec<&'n str>, Fault> {
        let Some(node) = self.lookup(rel)? else {
            return Ok(Vec::new());
        };
        let Some(repo) = self.repository() else {
            return Ok(Vec::new());
        };
        if node.kind != NodeKind::Dir {
            return Ok(Vec::new());
        }
        let tree = repo.find_tree(node.id).map_err(lib_fault)?;
        Ok(names
            .iter()
            .copied()
            .filter(|name| {
                tree.get_name(name)
                    .is_some_and(|entry| Node::of(&entry).kind == NodeKind::File)
            })
            .collect())
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

    #[cfg(all(unix, feature = "index"))]
    use super::NodeKind;
    use super::{
        Repo, Repos, changed_paths, fixture, index_has_prefix, init_repository, is_work_tree,
        origin_url, require_work_tree, unmerged_paths,
    };
    #[cfg(all(unix, feature = "index"))]
    use crate::error::Error;
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
            crate::testing::remove_when_the_test_ends(&dir);
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
            // A linked work tree of it: the format is in the common
            // directory's configuration.
            assert!(scratch.git(&[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "--quiet",
                "--allow-empty",
                "--message",
                "first",
            ]));
            assert!(scratch.git(&["worktree", "add", "--quiet", "linked"]));
            let error = is_work_tree(&scratch.0.join("linked")).unwrap_err();
            assert_eq!(
                error.vault_code(),
                VaultErrorCode::Unimplemented,
                "{name}, linked"
            );
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
        init_repository(&scratch.0.join("made"), false).unwrap();
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

    #[cfg(all(unix, feature = "index"))]
    #[test]
    fn a_delta_names_the_file_a_lookup_finds_in_the_later_commit() {
        let scratch = Scratch::new("deltas");
        scratch.write("notes/kept.md", b"kept");
        scratch.write("notes/changed.md", b"one");
        scratch.write("notes/gone.md", b"gone");
        fixture::commit_all(&scratch.0);
        let repos = Repos::new(&scratch.0);
        let old = repos.snapshot(Repo::Main).unwrap().commit();
        scratch.write("notes/changed.md", b"two");
        scratch.write("notes/new.md", b"new");
        std::fs::remove_file(scratch.0.join("notes/gone.md")).unwrap();
        std::os::unix::fs::symlink("kept.md", scratch.0.join("notes/link.md")).unwrap();
        fixture::commit_all(&scratch.0);
        let snap = repos.snapshot(Repo::Main).unwrap();
        let deltas = snap.changed_since(old, None).unwrap().unwrap();
        let paths: Vec<&str> = deltas.iter().map(|delta| delta.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "notes/changed.md",
                "notes/gone.md",
                "notes/link.md",
                "notes/new.md"
            ]
        );
        for delta in &deltas {
            let found = snap.lookup(&delta.path).map(|node| {
                node.filter(|node| node.kind == NodeKind::File)
                    .map(|node| node.id)
            });
            match (delta.file(), found) {
                (Ok(file), Ok(found)) => assert_eq!(file, found, "{}", delta.path),
                (Err(file), Err(found)) => {
                    assert_eq!(delta.path, "notes/link.md");
                    assert_eq!(file.path(), found.path());
                    assert_eq!(file.message(), found.message());
                    assert!(Error::from(file).is(VaultErrorCode::InvalidArgument));
                }
                (file, found) => panic!("{}: {file:?} and {found:?}", delta.path),
            }
        }
        assert_eq!(deltas[1].file().unwrap(), None, "a deleted file");
    }
}
