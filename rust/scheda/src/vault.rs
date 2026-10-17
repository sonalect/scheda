//! The `Vault` handle: opening a Vault, making one where the mode allows,
//! and closing it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use scheda_api::v1::{Encryption, OpenMode, OpenOptions, VaultErrorCode};

use crate::collection::{self, Declarations};
use crate::commit::{self, Change, Committer, WriteLock};
use crate::error::{self, Context, Error, Fault, io, redact_remote_text};
use crate::fs::restrict_dir;
use crate::{git, marker, stored, writer};

/// The message of the commit that makes a work tree a Vault.
const MARK_MESSAGE: &str = "mark the work tree as a Vault";

/// How long `open` waits for another `open` that is making the same Vault.
const OTHER_OPEN_WAIT: Duration = Duration::from_secs(5);

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
    /// The declarations as of the `HEAD` they were loaded at.
    loaded: Mutex<Option<Loaded>>,
    writer: String,
}

/// The declarations of the work tree, with the `HEAD` they were read at.
#[derive(Debug)]
struct Loaded {
    head: Option<String>,
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
    /// - A Vault, a git work tree with the marker `.scheda.yaml`: the marker
    ///   is read before any other file, and a marker of another generation
    ///   is `GENERATION` in every mode. `OPEN_MODE_CREATE_NEW` is
    ///   `ALREADY_EXISTS`; the other modes open it.
    /// - A git work tree without the marker: `OPEN_MODE_OPEN_EXISTING` is
    ///   `NOT_A_VAULT`, `OPEN_MODE_CREATE_NEW` is `ALREADY_EXISTS`, and
    ///   `OPEN_MODE_OPEN_OR_CREATE` adds the marker `.scheda.yaml` and the
    ///   missing entries of `.gitignore` in one commit on top of `HEAD`.
    /// - A folder that is neither empty nor a git work tree:
    ///   `OPEN_MODE_OPEN_EXISTING` is `NOT_A_VAULT`,
    ///   `OPEN_MODE_CREATE_NEW` is `ALREADY_EXISTS`, and
    ///   `OPEN_MODE_OPEN_OR_CREATE` is `INVALID_ARGUMENT`.
    /// - An empty folder: `OPEN_MODE_OPEN_EXISTING` is `NOT_A_VAULT`; the
    ///   other modes make a new Vault there: `git init`, then one commit
    ///   with the marker and a `.gitignore` that ignores `.scheda/` and
    ///   `.isolated/`.
    /// - A missing path: `OPEN_MODE_OPEN_EXISTING` is `NOT_FOUND`; the other
    ///   modes create the folder and make a new Vault as above.
    ///
    /// A refusal writes nothing. Two `open`s of the same new path at once,
    /// in threads or in processes, make one Vault and both open it (with
    /// `OPEN_MODE_CREATE_NEW`, the one that lost is `ALREADY_EXISTS`).
    /// Making the Vault, and the marker of a work tree, are written under the
    /// write lock, after a second look at the path.
    ///
    /// When it opens a Vault, `options.mirror`, if set, must be the fetch
    /// URL of the remote `origin` (`INVALID_ARGUMENT` otherwise, without
    /// contacting the network). The collection files are read (an invalid
    /// one fails `open`), `.scheda/` is created owner-only, and the writer
    /// identity is read or made. If a writer died between its commit and its
    /// files, `open` takes the write lock and brings those files to what
    /// `HEAD` holds; otherwise it takes no write lock. `open` pulls nothing.
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

    /// The collections the root directories declare, as the Vault holds
    /// them now. They are kept with the `HEAD` they were read at and read
    /// again when `HEAD` has moved (another handle or process wrote).
    pub(crate) fn declarations_now(&self) -> Result<Arc<Declarations>, Error> {
        let head = commit::read_head(&self.path)?;
        if let Some(declarations) = self.cached(&head)? {
            return Ok(declarations);
        }
        let (loaded, settled) = snapshot(&self.path)?;
        let declarations = Arc::clone(&loaded.declarations);
        if settled {
            *error::mutex(&self.loaded, "keep the declarations")? = Some(loaded);
        }
        Ok(declarations)
    }

    /// The collections as this handle last read them, without asking git
    /// whether `HEAD` has moved since: a read does not pay a process for
    /// that. A handle that has read none yet reads them now.
    pub(crate) fn declarations_cached(&self) -> Result<Arc<Declarations>, Error> {
        let kept = error::mutex(&self.loaded, "read the declarations")?
            .as_ref()
            .map(|loaded| Arc::clone(&loaded.declarations));
        match kept {
            Some(declarations) => Ok(declarations),
            None => self.declarations_now(),
        }
    }

    /// The collections, with the write lock held: read again when `HEAD`
    /// is not the one they were read at. Nothing writes meanwhile, and the
    /// lock has recovered any unfinished write.
    pub(crate) fn declarations_locked(
        &self,
        lock: &WriteLock<'_>,
    ) -> Result<Arc<Declarations>, Error> {
        let head = lock.head()?;
        if let Some(declarations) = self.cached(&head)? {
            return Ok(declarations);
        }
        let declarations = Arc::new(collection::load(&self.path)?);
        self.remember(head, Arc::clone(&declarations))?;
        Ok(declarations)
    }

    /// Keeps `declarations` as the state at `head`: after a write of this
    /// handle that moved `HEAD` there.
    pub(crate) fn remember(
        &self,
        head: Option<String>,
        declarations: Arc<Declarations>,
    ) -> Result<(), Error> {
        *error::mutex(&self.loaded, "keep the declarations")? = Some(Loaded { head, declarations });
        Ok(())
    }

    fn cached(&self, head: &Option<String>) -> Result<Option<Arc<Declarations>>, Error> {
        let guard = error::mutex(&self.loaded, "read the declarations")?;
        Ok(guard
            .as_ref()
            .filter(|loaded| loaded.head == *head)
            .map(|loaded| Arc::clone(&loaded.declarations)))
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

/// Reads the declarations of `root` with the `HEAD` they belong to. The
/// second value says whether the reading is a settled state to keep: no
/// write was in flight (the lock file held no paths) and `HEAD` did not move
/// meanwhile.
fn snapshot(root: &Path) -> Result<(Loaded, bool), Error> {
    let before = commit::pending(root)?;
    let head = commit::read_head(root)?;
    let declarations = Arc::new(collection::load(root)?);
    let settled = !before && !commit::pending(root)? && commit::read_head(root)? == head;
    Ok((Loaded { head, declarations }, settled))
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
            open_existing(path, options).context("Vault::open: open the Vault")
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
    if !git::is_work_tree(path) && !becomes_work_tree(path) {
        return Ok(Spot::Other);
    }
    match marker::read(path)? {
        Some(_) => Ok(Spot::Vault),
        None => Ok(Spot::WorkTree),
    }
}

/// Whether the folder, which holds a `.git` that is not a repository yet,
/// becomes a work tree within a moment: another `open` is running
/// `git init` in it.
fn becomes_work_tree(path: &Path) -> bool {
    path.join(".git").is_dir() && wait_for_work_tree(path).is_ok()
}

/// Waits until `path` is a git work tree, which another `open` is making.
fn wait_for_work_tree(path: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + OTHER_OPEN_WAIT;
    while !git::is_work_tree(path) {
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
    if options.mirror.is_some() {
        todo!("P4c clones the mirror into a new Vault");
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

/// `git init` in `path`, whose `.git` this call has just made.
fn init_repository(path: &Path) -> Result<(), Error> {
    let made = git::git()
        .arg("init")
        .arg("--")
        .arg(path)
        .run()
        .context("run git init")
        .and_then(|_| git::enable_long_paths(path));
    if made.is_err() {
        let _ = fs::remove_dir_all(path.join(".git"));
    }
    made
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
    let committer = Committer::new(&canonical, &options)?;
    {
        let lock = committer.lock().context("take the write lock")?;
        // A second look, now that nobody else writes: another `open` may
        // have made the Vault meanwhile.
        if marker::read(&canonical)?.is_none() {
            let changes = service_changes(&canonical)?;
            lock.commit(MARK_MESSAGE, &changes)
                .context("commit the marker")?;
        }
    }
    finish(canonical, options, committer)
}

/// The marker, and `.gitignore` with the entries Scheda needs.
fn service_changes(root: &Path) -> Result<Vec<Change>, Error> {
    let mut changes = vec![Change::put(marker::FILE, marker::make()?)];
    let current = match fs::read(root.join(".gitignore")) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(io(err).with_path(".gitignore").into()),
    };
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
    let committer = Committer::new(&canonical, &options)?;
    finish(canonical, options, committer)
}

/// Completes the handle of the Vault at `canonical`: recovers a write a
/// dead writer left, reads the collection files (an invalid one fails here,
/// before anything is created), then creates `.scheda/` and the writer
/// identity.
fn finish(canonical: PathBuf, options: OpenOptions, committer: Committer) -> Result<Vault, Error> {
    committer.recover_pending()?;
    let (loaded, settled) = snapshot(&canonical)?;
    prepare_scheda(&canonical)?;
    let writer = writer::load_or_create(&canonical, options.writer.as_deref())?;
    Ok(Vault {
        path: canonical,
        options,
        committer,
        loaded: Mutex::new(settled.then_some(loaded)),
        writer,
    })
}

/// The mirror a caller names must be the fetch URL of `origin`. Reads the
/// local configuration only.
fn check_mirror(vault: &Path, mirror: &str) -> Result<(), Error> {
    let origin = git::origin_url(vault).context("read the mirror of the Vault")?;
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
            git().arg("init").arg("--").arg(&dir).run().unwrap();
            std::fs::write(dir.join(".scheda.yaml"), "version: 2\nfuture: 1\n").unwrap();
            std::fs::write(
                dir.join("notes/.collection.yaml"),
                "name: notes\nschema:\n  standard: {}\n",
            )
            .unwrap();
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
