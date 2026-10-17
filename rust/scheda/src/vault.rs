//! The `Vault` handle: opening a Vault, making one where the mode allows,
//! and closing it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use git2::Oid;
use scheda_api::v1::{Encryption, OpenMode, OpenOptions, VaultErrorCode};

use crate::collection::{self, Declarations};
use crate::commit::{Change, Committer};
use crate::error::{self, Context, Error, Fault, io, redact_remote_text};
use crate::fs::restrict_dir;
use crate::repo::{self, Repo, Repos, Snap};
use crate::{git, marker, stored, writer};

/// The message of the commit that makes a work tree a Vault.
const MARK_MESSAGE: &str = "mark the work tree as a Vault";

/// How long `open` waits for another `open` that is making the same Vault.
const OTHER_OPEN_WAIT: Duration = Duration::from_secs(5);

/// Numbers the staging folders of the clones this process makes.
static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

/// An open Vault: a git work tree that holds the marker `.scheda.yaml`, with
/// the collections its root directories declare.
///
/// [`Vault::open`] opens one that exists or makes one, and [`Vault::close`]
/// ends the handle. A handle may be shared between threads: the writes of
/// one handle take turns, and the writes of all handles of a Vault, in this
/// process or another, take turns on the lock file `.scheda/commit.lock`.
#[derive(Debug)]
pub struct Vault {
    path: PathBuf,
    options: OpenOptions,
    committer: Committer,
    repos: Arc<Repos>,
    /// The declarations as of the commit they were read from.
    loaded: Mutex<Option<Loaded>>,
    writer: String,
    /// The last failure of a pull at `open` or a push after a write, until a
    /// later pull or push succeeds.
    sync_failure: Mutex<Option<Error>>,
}

/// The declarations of a commit, with the commit they were read from.
#[derive(Debug)]
struct Loaded {
    head: Option<Oid>,
    declarations: Arc<Declarations>,
}

/// What `open` found at the path.
enum Spot {
    /// Nothing is there.
    Missing,
    /// An empty folder.
    Empty,
    /// A git work tree that holds a marker of the generation this build
    /// reads.
    Vault,
    /// A git work tree without the marker.
    WorkTree,
    /// A file, or a folder that is neither empty nor a git work tree.
    Other,
}

impl Vault {
    /// Opens the Vault at `path`, or, as `options.mode` allows, makes one.
    ///
    /// `options` is checked first (`INVALID_ARGUMENT`); `options.encryption`
    /// in a build without the `encrypt` feature is `UNIMPLEMENTED`. Then
    /// `options.mode` decides what the path may be (unset is
    /// `OPEN_MODE_OPEN_OR_CREATE`):
    ///
    /// - A Vault, a git work tree whose commit `HEAD` names holds the marker
    ///   `.scheda.yaml`: the marker is read before any other file, from that
    ///   commit and not from the work tree, and a marker of another
    ///   generation is `GENERATION` in every mode. `OPEN_MODE_CREATE_NEW` is
    ///   `ALREADY_EXISTS`; the other modes open it.
    /// - A git work tree without the marker in the commit `HEAD` names (a
    ///   branch with no commit yet is one): `OPEN_MODE_OPEN_EXISTING` is
    ///   `NOT_A_VAULT`, `OPEN_MODE_CREATE_NEW` is `ALREADY_EXISTS`, and
    ///   `OPEN_MODE_OPEN_OR_CREATE` adds the marker `.scheda.yaml` and the
    ///   missing entries of `.gitignore` in one commit on top of `HEAD`.
    /// - A folder that is neither empty nor a git work tree:
    ///   `OPEN_MODE_OPEN_EXISTING` is `NOT_A_VAULT`,
    ///   `OPEN_MODE_CREATE_NEW` is `ALREADY_EXISTS`, and
    ///   `OPEN_MODE_OPEN_OR_CREATE` is `INVALID_ARGUMENT`.
    /// - An empty folder: `OPEN_MODE_OPEN_EXISTING` is `NOT_A_VAULT`; the
    ///   other modes make a new Vault there: a git repository on the branch
    ///   `init.defaultBranch` names (else `main`), then one commit with the
    ///   marker and a `.gitignore` that ignores `.scheda/` and `.isolated/`.
    /// - A missing path: `OPEN_MODE_OPEN_EXISTING` is `NOT_FOUND`; the other
    ///   modes create the folder and make a new Vault as above.
    ///
    /// A refusal writes nothing. Two `open`s of the same new path at once,
    /// in threads or in processes, make one Vault and both open it (with
    /// `OPEN_MODE_CREATE_NEW`, the one that lost is `ALREADY_EXISTS`).
    /// Making the Vault, and the marker of a work tree, are written under the
    /// write lock, after a second look at the path.
    ///
    /// A missing path or an empty folder with `options.mirror` set, in the
    /// modes that make a Vault, is a clone of the mirror (`git clone`; the
    /// person's SSH configuration and credential helpers apply) instead of
    /// a new repository. The clone is looked at as any path is: a marker of
    /// another generation is `GENERATION`; a clone without a marker (an
    /// empty mirror, or a plain repository) gets the marker and `.gitignore`
    /// in one commit; a clone with the marker opens. The clone is made in a
    /// folder beside the path and moved into place, so a clone that fails
    /// leaves no Vault, and of two `open`s the one that finds the Vault
    /// made opens that one. A mirror that cannot be cloned is
    /// `UNAUTHENTICATED`, `UNAVAILABLE`, or `REPLICATION`.
    ///
    /// When it opens a Vault that exists, `options.mirror`, if set, must be
    /// the fetch URL of the remote `origin` (`INVALID_ARGUMENT` otherwise,
    /// without contacting the network). The collection files of `HEAD` are read (an
    /// invalid one fails `open`), `.scheda/` is created owner-only, and the
    /// writer identity is read or made. If a writer died between its commit
    /// and its files, `open` takes the write lock and brings those files to
    /// what `HEAD` holds, leaving a file a person changed meanwhile as it is;
    /// otherwise it takes no write lock. With `options.auto_sync` and a
    /// mirror, `open` of a Vault that exists then pulls (as
    /// [`Vault::pull`]); a failure of that pull does not fail `open`, the
    /// handle keeps it. Otherwise `open` pulls nothing. A repository
    /// that uses SHA-256 objects or reftable references, which the bundled
    /// git library cannot read, is `UNIMPLEMENTED` in every mode.
    ///
    /// # Errors
    ///
    /// The codes above; `ABORTED` when the write lock is not free within
    /// `write_timeout_ms`; `STORAGE` or `REPLICATION` when the path or git
    /// cannot be read or written.
    pub fn open(path: impl AsRef<Path>, options: OpenOptions) -> Result<Vault, Error> {
        open(path.as_ref(), options)
    }

    /// The canonical path of the work tree.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The collections the root directories declare in the commit `HEAD`
    /// names now. They are kept with the commit they were read from and read
    /// again when `HEAD` names another (another handle or process wrote, or
    /// a person committed); resolving `HEAD` costs no process.
    pub(crate) fn declarations_now(&self) -> Result<Arc<Declarations>, Error> {
        let snap = self.repos.snapshot(Repo::Main)?;
        self.declarations_in(&snap)
    }

    /// The collections declared in the commit of `snap`, a snapshot of the
    /// main repository: the ones this handle kept for that commit, else read
    /// from it and kept.
    pub(crate) fn declarations_in(&self, snap: &Snap) -> Result<Arc<Declarations>, Error> {
        let head = snap.commit();
        if let Some(declarations) = self.cached(head)? {
            return Ok(declarations);
        }
        let declarations = Arc::new(collection::load(snap)?);
        self.remember(head, Arc::clone(&declarations))?;
        Ok(declarations)
    }

    /// Keeps `declarations` as the state at `head`: after a write of this
    /// handle that moved `HEAD` there.
    pub(crate) fn remember(
        &self,
        head: Option<Oid>,
        declarations: Arc<Declarations>,
    ) -> Result<(), Error> {
        *error::mutex(&self.loaded, "keep the declarations")? = Some(Loaded { head, declarations });
        Ok(())
    }

    fn cached(&self, head: Option<Oid>) -> Result<Option<Arc<Declarations>>, Error> {
        let guard = error::mutex(&self.loaded, "read the declarations")?;
        Ok(guard
            .as_ref()
            .filter(|loaded| loaded.head == head)
            .map(|loaded| Arc::clone(&loaded.declarations)))
    }

    /// The repositories of the Vault.
    pub(crate) fn repos(&self) -> &Repos {
        &self.repos
    }

    /// The write path of this handle.
    pub(crate) fn committer(&self) -> &Committer {
        &self.committer
    }

    /// The writer identity of this handle: the name its journal segments
    /// carry.
    pub(crate) fn writer(&self) -> &str {
        &self.writer
    }

    /// Whether the handle pulls at `open` and pushes after each write
    /// (`OpenOptions.auto_sync`).
    pub(crate) fn auto_sync(&self) -> bool {
        self.options.auto_sync.unwrap_or(false)
    }

    /// Keeps the outcome of an `auto_sync` pull or push: a failure replaces
    /// the one kept, a success clears it.
    pub(crate) fn record_sync(&self, outcome: Result<(), Error>) {
        // A poisoned lock holds a plain value no panic could break.
        let mut kept = self
            .sync_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *kept = outcome.err();
    }

    /// Forgets the failure kept by [`Vault::record_sync`]: a pull or push
    /// asked for by the caller succeeded.
    pub(crate) fn clear_sync_failure(&self) {
        self.record_sync(Ok(()));
    }

    /// The last failure of a pull at `open` or of a push after a write
    /// (`auto_sync`), or `None` when the last one succeeded or none ran.
    #[cfg_attr(not(test), expect(dead_code, reason = "the Vault service shows it"))]
    pub(crate) fn sync_failure(&self) -> Option<Error> {
        self.sync_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The age secret the handle was opened with, if any.
    pub(crate) fn encryption(&self) -> Option<&Encryption> {
        self.options.encryption.as_option()
    }

    /// Ends the handle. The handle holds no lock and no background work yet,
    /// so nothing can fail; a later version of the handle may flush before
    /// it returns.
    ///
    /// # Errors
    ///
    /// None at present.
    pub fn close(self) -> Result<(), Error> {
        Ok(())
    }
}

fn open(path: &Path, options: OpenOptions) -> Result<Vault, Error> {
    stored::validate(&options)
        .map_err(|fault| {
            Fault::new(
                VaultErrorCode::InvalidArgument,
                format!("the open options are not valid: {}", fault.message()),
            )
            .with_source(fault)
        })
        .context("Vault::open: check the options")?;
    #[cfg(not(feature = "encrypt"))]
    if options.encryption.is_set() {
        return Err(Fault::new(
            VaultErrorCode::Unimplemented,
            "encryption needs a build with the `encrypt` feature",
        )
        .into());
    }
    if options.signing.is_set() {
        return Err(Fault::new(
            VaultErrorCode::Unimplemented,
            "signing commits is not built yet; leave `signing` unset",
        )
        .into());
    }
    let mode = options
        .mode
        .and_then(|mode| mode.as_known())
        .unwrap_or(OpenMode::OPEN_MODE_UNSPECIFIED);
    match classify(path).context("Vault::open: look at the path")? {
        Spot::Vault => {
            if mode == OpenMode::OPEN_MODE_CREATE_NEW {
                return Err(refusal(
                    VaultErrorCode::AlreadyExists,
                    "the path holds a Vault",
                ));
            }
            let vault = open_existing(path, options).context("Vault::open: open the Vault")?;
            vault.pull_at_open();
            Ok(vault)
        }
        Spot::WorkTree => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => Err(refusal(
                VaultErrorCode::NotAVault,
                "the path is a git work tree without the Vault marker",
            )),
            OpenMode::OPEN_MODE_CREATE_NEW => Err(refusal(
                VaultErrorCode::AlreadyExists,
                "the path is a git work tree",
            )),
            _ => mark_and_open(path, options).context("Vault::open: mark the work tree"),
        },
        Spot::Other => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => Err(refusal(
                VaultErrorCode::NotAVault,
                "the path is not a Vault",
            )),
            OpenMode::OPEN_MODE_CREATE_NEW => Err(refusal(
                VaultErrorCode::AlreadyExists,
                "the path is not empty",
            )),
            _ => Err(refusal(
                VaultErrorCode::InvalidArgument,
                "the path is neither an empty folder, a git work tree, nor a Vault",
            )),
        },
        Spot::Empty => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => Err(refusal(
                VaultErrorCode::NotAVault,
                "the folder is empty and holds no Vault",
            )),
            _ => make_new(path, options, mode).context("Vault::open: make a Vault"),
        },
        Spot::Missing => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => {
                Err(refusal(VaultErrorCode::NotFound, "the path does not exist"))
            }
            _ => make_new(path, options, mode).context("Vault::open: make a Vault"),
        },
    }
}

fn refusal(code: VaultErrorCode, message: &str) -> Error {
    Fault::new(code, message).into()
}

/// What is at `path`. A git work tree is asked for its marker before
/// anything else of it is read: a marker of another generation fails here.
fn classify(path: &Path) -> Result<Spot, Error> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Spot::Missing),
        Err(err) => return Err(io(err)).context("stat the path"),
    };
    if !meta.is_dir() {
        return Ok(Spot::Other);
    }
    let mut entries = fs::read_dir(path).map_err(io).context("read the folder")?;
    if entries.next().is_none() {
        return Ok(Spot::Empty);
    }
    if !repo::is_work_tree(path)? && !becomes_work_tree(path)? {
        return Ok(Spot::Other);
    }
    match marker::read(&Repos::new(path).snapshot(Repo::Main)?)? {
        Some(_) => Ok(Spot::Vault),
        None => Ok(Spot::WorkTree),
    }
}

/// Whether the folder, which holds a `.git` that is not a repository yet,
/// becomes a work tree within a moment: another `open` is running
/// `git init` in it.
fn becomes_work_tree(path: &Path) -> Result<bool, Error> {
    if !path.join(".git").is_dir() {
        return Ok(false);
    }
    match wait_for_work_tree(path) {
        Ok(()) => Ok(true),
        Err(error) if error.is(VaultErrorCode::Aborted) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Waits until `path` is a git work tree, which another `open` is making.
fn wait_for_work_tree(path: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + OTHER_OPEN_WAIT;
    while !repo::is_work_tree(path)? {
        if Instant::now() >= deadline {
            return Err(refusal(
                VaultErrorCode::Aborted,
                "another open is making a Vault at this path; try again",
            ));
        }
        sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// Makes a new Vault at `path`, a missing path or an empty folder: the
/// folder, `git init`, then the marker and the `.gitignore` in one commit
/// under the write lock. The folder's `.git` is created first as the claim
/// on the path: of two `open`s at once, one makes the repository and the
/// other waits for it.
fn make_new(path: &Path, options: OpenOptions, mode: OpenMode) -> Result<Vault, Error> {
    if let Some(url) = options.mirror.clone() {
        return clone_new(path, &url, options).context("clone the mirror");
    }
    fs::create_dir_all(path)
        .map_err(io)
        .context("create the folder")?;
    match fs::create_dir(path.join(".git")) {
        Ok(()) => init_repository(path)?,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if mode == OpenMode::OPEN_MODE_CREATE_NEW {
                return Err(refusal(
                    VaultErrorCode::AlreadyExists,
                    "another open is making a Vault at this path",
                ));
            }
            wait_for_work_tree(path)?;
        }
        Err(err) => return Err(io(err)).context("claim the folder"),
    }
    mark_and_open(path, options)
}

/// A folder beside the path of a new Vault that a clone is made in, removed
/// when it is dropped (after the move into place it is not there to remove).
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Makes a new Vault at `path`, a missing path or an empty folder, by
/// cloning the mirror at `url`.
///
/// The clone is made in a staging folder in the same parent, classified like
/// any path (a marker of another generation is `GENERATION`), given its
/// marker in one commit when it has none, and only then moved to `path`. A
/// failure at any step leaves nothing at `path`. When the move finds that
/// another `open` made the Vault meanwhile, this one opens that Vault.
fn clone_new(path: &Path, url: &str, options: OpenOptions) -> Result<Vault, Error> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)
        .map_err(io)
        .context("create the folder")?;
    let name = path.file_name().map_or_else(
        || String::from("vault"),
        |name| name.to_string_lossy().into_owned(),
    );
    let staging = Staging(parent.join(format!(
        ".{name}.scheda-clone-{}-{}",
        std::process::id(),
        NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
    )));
    let _ = fs::remove_dir_all(&staging.0);
    git::clone(url, &staging.0).context("clone")?;
    repo::enable_long_paths_at(&staging.0).context("set up the clone")?;
    match classify(&staging.0).context("look at the clone")? {
        Spot::Vault => {}
        Spot::WorkTree => {
            drop(mark_and_open(&staging.0, options.clone()).context("mark the clone")?)
        }
        _ => {
            return Err(refusal(
                VaultErrorCode::Replication,
                "the mirror did not clone into a git work tree",
            ));
        }
    }
    match fs::rename(&staging.0, path) {
        Ok(()) => open_existing(path, options),
        Err(err) => {
            drop(staging);
            match classify(path) {
                Ok(Spot::Vault | Spot::WorkTree | Spot::Other) => open(path, options),
                _ => Err(io(err)).context("move the clone into place"),
            }
        }
    }
}

/// Makes the git repository in `path`, whose `.git` this call has just made.
fn init_repository(path: &Path) -> Result<(), Error> {
    let made = repo::init_repository(path);
    if made.is_err() {
        let _ = fs::remove_dir_all(path.join(".git"));
    }
    made.map_err(Error::from).context("make the repository")
}

/// Gives the git work tree at `path` its marker and the missing entries of
/// `.gitignore` in one commit, unless another `open` did already; then opens
/// it. The mirror a caller names is checked first, so a refusal writes
/// nothing.
fn mark_and_open(path: &Path, options: OpenOptions) -> Result<Vault, Error> {
    let canonical = path
        .canonicalize()
        .map_err(io)
        .context("resolve the path")?;
    if let Some(mirror) = options.mirror.as_deref() {
        check_mirror(&canonical, mirror)?;
    }
    prepare_scheda(&canonical)?;
    let repos = Arc::new(Repos::new(&canonical));
    let committer = Committer::new(&canonical, &options, Arc::clone(&repos))?;
    {
        let lock = committer.lock().context("take the write lock")?;
        // A second look, now that nobody else writes: another `open` may
        // have made the Vault meanwhile.
        let snap = lock.snapshot(Repo::Main)?;
        if marker::read(&snap)?.is_none() {
            let changes = service_changes(&snap)?;
            lock.commit_in(&snap, MARK_MESSAGE, &changes)
                .context("commit the marker")?;
        }
    }
    finish(canonical, options, committer, repos)
}

/// The marker, and `.gitignore` with the entries Scheda needs, as the commit
/// of `snap` holds it.
fn service_changes(snap: &Snap) -> Result<Vec<Change>, Error> {
    let mut changes = vec![Change::put(marker::FILE, marker::make()?)];
    let current = snap
        .read(".gitignore")
        .map_err(|fault| fault.with_path(".gitignore"))?
        .map(|blob| blob.bytes)
        .unwrap_or_default();
    if let Some(updated) = with_ignore_entries(&current) {
        changes.push(Change::put(".gitignore", updated));
    }
    Ok(changes)
}

/// `gitignore` with the lines `.scheda/` and `.isolated/` it lacks added at
/// the end, or `None` when it has both.
fn with_ignore_entries(gitignore: &[u8]) -> Option<Vec<u8>> {
    let has = |dir: &str| {
        let slashed = format!("{dir}/");
        gitignore.split(|byte| *byte == b'\n').any(|line| {
            let line = line.trim_ascii();
            line == dir.as_bytes() || line == slashed.as_bytes()
        })
    };
    let missing: Vec<&str> = [".scheda/", ".isolated/"]
        .into_iter()
        .filter(|entry| !has(entry.trim_end_matches('/')))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut out = gitignore.to_vec();
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    for entry in missing {
        out.extend_from_slice(entry.as_bytes());
        out.push(b'\n');
    }
    Some(out)
}

/// Creates `{root}/.scheda/`, owner-only.
fn prepare_scheda(root: &Path) -> Result<(), Error> {
    let scheda = root.join(".scheda");
    fs::create_dir_all(&scheda)
        .map_err(io)
        .context("create .scheda")?;
    restrict_dir(&scheda)
}

/// Opens the Vault at `path`, whose marker `classify` has accepted.
fn open_existing(path: &Path, options: OpenOptions) -> Result<Vault, Error> {
    let canonical = path
        .canonicalize()
        .map_err(io)
        .context("resolve the path")?;
    if let Some(mirror) = options.mirror.as_deref() {
        check_mirror(&canonical, mirror)?;
    }
    let repos = Arc::new(Repos::new(&canonical));
    let committer = Committer::new(&canonical, &options, Arc::clone(&repos))?;
    finish(canonical, options, committer, repos)
}

/// Completes the handle of the Vault at `canonical`: recovers a write a
/// dead writer left, reads the collection files of `HEAD` (an invalid one
/// fails here, before anything is created), then creates `.scheda/` and the
/// writer identity.
fn finish(
    canonical: PathBuf,
    options: OpenOptions,
    committer: Committer,
    repos: Arc<Repos>,
) -> Result<Vault, Error> {
    committer.recover_pending()?;
    let snap = repos.snapshot(Repo::Main)?;
    let declarations = Arc::new(collection::load(&snap)?);
    prepare_scheda(&canonical)?;
    let writer = writer::load_or_create(&canonical, options.writer.as_deref())?;
    Ok(Vault {
        path: canonical,
        options,
        committer,
        repos,
        loaded: Mutex::new(Some(Loaded {
            head: snap.commit(),
            declarations,
        })),
        writer,
        sync_failure: Mutex::new(None),
    })
}

/// The mirror a caller names must be the fetch URL of `origin`. Reads the
/// local configuration only.
fn check_mirror(vault: &Path, mirror: &str) -> Result<(), Error> {
    let origin = repo::origin_url(vault).context("read the mirror of the Vault")?;
    match origin {
        Some(url) if url == mirror => Ok(()),
        Some(url) => Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            format!(
                "the mirror {} is not the mirror of this Vault, {}",
                redact_remote_text(mirror),
                redact_remote_text(&url)
            ),
        )
        .into()),
        None => Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            format!(
                "the mirror {} is named, but this Vault has no mirror",
                redact_remote_text(mirror)
            ),
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use scheda_api::v1::{OpenOptions, VaultErrorCode};

    use super::Vault;
    use crate::git::{git, isolate_test_git_env};

    /// A Vault on disk, made with plain git, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn vault(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-vault-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("notes")).unwrap();
            std::fs::write(dir.join(".scheda.yaml"), "version: 2\nfuture: 1\n").unwrap();
            std::fs::write(
                dir.join("notes/.collection.yaml"),
                "name: notes\nschema:\n  standard: {}\n",
            )
            .unwrap();
            crate::repo::fixture::commit_all(&dir);
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_existing_vault_opens_with_its_collections_and_a_writer_and_closes() {
        let scratch = Scratch::vault("open");
        let vault = Vault::open(&scratch.0, OpenOptions::default()).unwrap();
        assert_eq!(vault.path, scratch.0.canonicalize().unwrap());
        let declarations = vault.declarations_now().unwrap();
        assert_eq!(declarations.len(), 1);
        assert!(declarations.get("notes").is_some());
        assert_eq!(vault.writer.len(), 16);
        assert_eq!(vault.options.mirror, None);
        vault.close().unwrap();
        assert!(scratch.0.join(".scheda/writer").is_file());
        let again = Vault::open(&scratch.0, OpenOptions::default().with_writer("alice")).unwrap();
        assert_eq!(again.writer, "alice");
    }

    #[test]
    fn a_mirror_must_be_the_origin_of_the_vault() {
        let scratch = Scratch::vault("mirror");
        let url = "https://example.test/org/repo.git";
        let options = || OpenOptions::default().with_mirror(url);
        let error = Vault::open(&scratch.0, options()).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument), "no origin yet");
        git()
            .current_dir(Path::new(&scratch.0))
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg(url)
            .run()
            .unwrap();
        Vault::open(&scratch.0, options()).unwrap();
        let other = OpenOptions::default().with_mirror("https://example.test/org/other.git");
        let error = Vault::open(&scratch.0, other).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn options_that_break_a_rule_are_invalid_argument() {
        let scratch = Scratch::vault("options");
        for options in [
            OpenOptions::default().with_writer("Not Valid"),
            OpenOptions::default().with_mirror("not a url"),
            OpenOptions::default().with_index_poll_ms(1),
        ] {
            let error = Vault::open(&scratch.0, options).unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
        assert!(
            !scratch.0.join(".scheda").exists(),
            "a refusal writes nothing"
        );
    }
}
