//! An open Vault: opening one, making one where the mode allows, and
//! closing it. The services of the contract (`service.rs`) are made over
//! this state.

use std::fs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use git2::Oid;
use scheda_api::v1::embedder_ref::EmbedderRef;
use scheda_api::v1::projector_ref::ProjectorRef;
use scheda_api::v1::reranker_ref::RerankerRef;
use scheda_api::v1::{Encryption, OpenMode, OpenOptions, VaultErrorCode, VaultInfo};
use scheda_blocking::{Blocking, blocking};
use tokio::time::Instant;

use crate::collection::{self, Declarations};
use crate::commit::{self, Change, Committer, Held, WriteLock};
use crate::error::{self, Context, Error, Fault, io, redact_remote_text};
use crate::fs::restrict_dir;
use crate::gate::{Gate, Pass};
use crate::idmap::IdMaps;
use crate::index::{Index, State};
use crate::indexer::Indexer;
use crate::ports::{Installed, Ports};
use crate::repo::{self, Repo, Repos, Snap};
use crate::{git, marker, stored, writer};

/// The message of the commit that makes a work tree a Vault.
const MARK_MESSAGE: &str = "mark the work tree as a Vault";

/// How long `open` waits for another `open` that is making the same Vault.
const OTHER_OPEN_WAIT: Duration = Duration::from_secs(5);

/// How often the indexer looks at `HEAD` when `index_poll_ms` is unset.
const DEFAULT_POLL_MS: u32 = 1000;

/// Numbers the staging folders of the clones this process makes.
static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

/// An open Vault: a git work tree that holds the marker `.scheda.yaml`, with
/// the collections its root directories declare.
///
/// [`Vault::open`] opens one that exists or makes one, and [`Vault::close`]
/// ends it. It may be shared between tasks: the writes of one Vault take
/// turns, and the writes of all Vaults open on one path, in this process or
/// another, take turns on the lock file `.scheda/commit.lock`.
///
/// Its methods that read or write files take a [`Blocking`] token: they run
/// on the blocking pool, one step at a time, and what waits between two
/// steps (the write lock, a `git` process, another `open`) is the async
/// layer's.
///
/// An open Vault runs one task, its indexer, that keeps the Index in
/// `.scheda/index/` at the state of `HEAD`: the maps from the id of a
/// journal line to its segment. The task ends with [`Vault::close`], or
/// after its current round when the Vault is dropped.
#[derive(Debug)]
pub(crate) struct Vault {
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
    /// The ports the Vault holds now.
    ports: Arc<Installed>,
    /// The thread that keeps the Index at the state of `HEAD`.
    indexer: Indexer,
    /// The id maps of the journals, as readers use them.
    id_maps: IdMaps,
    /// What every call passes, and `Close` shuts.
    gate: Gate,
    /// Held by a `Close` while it runs, so a second one waits for it.
    closing: tokio::sync::Mutex<()>,
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
    /// in a build without the `encrypt` feature is `UNIMPLEMENTED`. The
    /// `ports` are looked at before the path is, so a refusal of a port
    /// writes nothing: this version of Scheda has no Index to use an
    /// embedder, a reranker, or a Projector, and any of them is
    /// `UNIMPLEMENTED`. Then `options.mode` decides what the path may be
    /// (unset is `OPEN_MODE_OPEN_OR_CREATE`):
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
    /// Vault keeps it. Otherwise `open` pulls nothing. A repository
    /// that uses SHA-256 objects or reftable references, which the bundled
    /// git library cannot read, is `UNIMPLEMENTED` in every mode.
    ///
    /// # Errors
    ///
    /// The codes above; `ABORTED` when the write lock is not free within
    /// `write_timeout_ms`; `STORAGE` or `REPLICATION` when the path or git
    /// cannot be read or written.
    pub(crate) async fn open(
        path: PathBuf,
        options: OpenOptions,
        ports: Ports,
    ) -> Result<Arc<Vault>, Error> {
        let ports = Installed::new(ports)
            .await
            .context("Vault::open: install the ports")?;
        check_options(&options)?;
        let mode = options
            .mode
            .and_then(|mode| mode.as_known())
            .unwrap_or(OpenMode::OPEN_MODE_UNSPECIFIED);
        let (mut vault, existed) = open_at(path, options, mode).await?;
        vault.ports = Arc::new(ports);
        let vault = Arc::new(vault);
        if existed {
            crate::mirror::pull_at_open(&Pass::enter(&vault)?).await;
        }
        let poll = Duration::from_millis(u64::from(
            vault.options.index_poll_ms.unwrap_or(DEFAULT_POLL_MS),
        ));
        vault
            .indexer
            .start(Arc::clone(&vault.repos), Arc::clone(&vault.ports), poll)
            .context("Vault::open: start the indexer")?;
        Ok(vault)
    }

    /// What this Vault holds: its path, the Index path, its writer identity,
    /// and the state token of the stored state now ([`Vault::state_now`]).
    /// Reads no Document.
    ///
    /// # Errors
    ///
    /// `STORAGE` or `REPLICATION` when `HEAD` cannot be read.
    pub(crate) fn info(&self, cx: &Blocking) -> Result<VaultInfo, Error> {
        Ok(VaultInfo::default()
            .with_path(self.path.to_string_lossy().into_owned())
            .with_index_path(self.index().root().to_string_lossy().into_owned())
            .with_writer(self.writer.clone())
            .with_version(self.state_now(cx)?.token()))
    }

    /// The stored state of the Vault now: `HEAD` of the Vault's repository
    /// and of the isolated one, read in turn, without the write lock. A
    /// writer may move one of them between the two reads; every commit the
    /// state names was stored before this call returned.
    ///
    /// # Errors
    ///
    /// `STORAGE` or `REPLICATION` when `HEAD` cannot be read.
    pub(crate) fn state_now(&self, _cx: &Blocking) -> Result<State, Error> {
        Ok(State::of(&self.repos.views().context("read HEAD")?))
    }

    /// Installs `projector`, or removes the Projector when it is `None`,
    /// which a Vault that has none takes as nothing to do. The indexer is
    /// woken: its next round is with the new port, not after the poll or
    /// the pause of a failed round.
    ///
    /// # Errors
    ///
    /// In a build with the feature `index`, `INVALID_ARGUMENT` for a
    /// reference that holds no implementation or a descriptor that breaks
    /// its rules, and the failure of the Projector's `describe`. In a build
    /// without it, `UNIMPLEMENTED` for a Projector: there is no Index to use
    /// one.
    pub(crate) async fn set_projector(&self, projector: Option<ProjectorRef>) -> Result<(), Error> {
        self.ports.set_projector(projector).await?;
        self.committer.signal().ports_changed();
        Ok(())
    }

    /// Installs `embedder`, or removes the embedder when it is `None`,
    /// which a Vault that has none takes as nothing to do. The indexer is
    /// woken, as by [`Vault::set_projector`].
    ///
    /// # Errors
    ///
    /// As [`Vault::set_projector`], for an embedder.
    pub(crate) async fn set_embedder(&self, embedder: Option<EmbedderRef>) -> Result<(), Error> {
        self.ports.set_embedder(embedder).await?;
        self.committer.signal().ports_changed();
        Ok(())
    }

    /// Installs `reranker`, or removes the reranker when it is `None`,
    /// which a Vault that has none takes as nothing to do. The indexer is
    /// woken, as by [`Vault::set_projector`].
    ///
    /// # Errors
    ///
    /// As [`Vault::set_projector`], for a reranker.
    pub(crate) async fn set_reranker(&self, reranker: Option<RerankerRef>) -> Result<(), Error> {
        self.ports.set_reranker(reranker).await?;
        self.committer.signal().ports_changed();
        Ok(())
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
        Ok(self.declarations_at_head()?.1)
    }

    /// [`Vault::declarations_now`] with the commit of the main repository
    /// they were read from: what the Projector's columns are kept by.
    pub(crate) fn declarations_at_head(&self) -> Result<(Option<Oid>, Arc<Declarations>), Error> {
        let snap = self.repos.snapshot(Repo::Main)?;
        let declarations = self.declarations_in(&snap)?;
        Ok((snap.commit(), declarations))
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

    /// The write lock `held` stands for, which the caller waited for, in
    /// the step that writes: an unfinished write a dead writer left is
    /// recovered first.
    ///
    /// # Errors
    ///
    /// As [`Committer::adopt`].
    pub(crate) fn lock(&self, cx: &Blocking, held: Held) -> Result<WriteLock<'_>, Error> {
        self.committer
            .adopt(cx, held)
            .context("take the write lock")
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
    pub(crate) fn sync_failure(&self) -> Option<Error> {
        self.sync_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How deep a `Filter` may be (`OpenOptions.filter_max_depth`), when
    /// set.
    #[cfg(feature = "index")]
    pub(crate) fn filter_max_depth(&self) -> Option<u32> {
        self.options.filter_max_depth
    }

    /// How long a call with `not_before` waits for the Index
    /// (`OpenOptions.index_wait_timeout_ms`, 60 seconds when unset).
    #[cfg(feature = "index")]
    pub(crate) fn index_wait(&self) -> Duration {
        Duration::from_millis(u64::from(
            self.options.index_wait_timeout_ms.unwrap_or(60_000),
        ))
    }

    /// The age secret the handle was opened with, if any.
    pub(crate) fn encryption(&self) -> Option<&Encryption> {
        self.options.encryption.as_option()
    }

    /// The indexer of the Vault.
    #[cfg(any(test, feature = "index"))]
    pub(crate) fn indexer(&self) -> &Indexer {
        &self.indexer
    }

    /// The ports the Vault holds now.
    #[cfg(feature = "index")]
    pub(crate) fn ports(&self) -> &Installed {
        &self.ports
    }

    /// The id maps of the journals.
    pub(crate) fn id_maps(&self) -> &IdMaps {
        &self.id_maps
    }

    /// The Index of the Vault on disk.
    pub(crate) fn index(&self) -> Index {
        Index::new(&self.path)
    }

    /// The gate every call of the Vault passes.
    pub(crate) fn gate(&self) -> &Gate {
        &self.gate
    }

    /// Ends the Vault: new calls fail at once (`FAILED_PRECONDITION`); the
    /// calls inside, and the steps of calls that were dropped, are waited
    /// for; then the indexer stops and is waited for (a round in progress
    /// ends at its next step; nothing it left half done is believed,
    /// because the Index's `commit` is written last). The Vault holds no
    /// lock after that. A second call is nothing to do; two at once, the
    /// second waits for the first.
    ///
    /// # Errors
    ///
    /// `PANICKED` when the indexer task died by a panic outside a round.
    pub(crate) async fn close(&self) -> Result<(), Error> {
        let _closing = self.closing.lock().await;
        self.gate.shut().await;
        self.indexer
            .stop()
            .await
            .context("Vault::close: stop the indexer")
    }
}

/// The checks of `options` that need no file: they validate, and what
/// this build or version lacks is `UNIMPLEMENTED`.
fn check_options(options: &OpenOptions) -> Result<(), Error> {
    stored::validate(options)
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
    Ok(())
}

/// What `open_at` returns: the Vault, and whether it existed before this
/// call (only then does `open` pull).
type Opened = Result<(Vault, bool), Error>;

/// Opens or makes the Vault at `path` as `mode` allows. Boxed: a clone that
/// finds the Vault made by another `open` meanwhile opens it from the top.
fn open_at(
    path: PathBuf,
    options: OpenOptions,
    mode: OpenMode,
) -> Pin<Box<dyn Future<Output = Opened> + Send>> {
    Box::pin(async move {
        match classify(&path)
            .await
            .context("Vault::open: look at the path")?
        {
            Spot::Vault => {
                if mode == OpenMode::OPEN_MODE_CREATE_NEW {
                    return Err(refusal(
                        VaultErrorCode::AlreadyExists,
                        "the path holds a Vault",
                    ));
                }
                let vault = open_existing(path, options)
                    .await
                    .context("Vault::open: open the Vault")?;
                Ok((vault, true))
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
                _ => mark_and_open(path, options)
                    .await
                    .context("Vault::open: mark the work tree")
                    .map(|vault| (vault, false)),
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
                _ => make_new(path, options, mode)
                    .await
                    .context("Vault::open: make a Vault"),
            },
            Spot::Missing => match mode {
                OpenMode::OPEN_MODE_OPEN_EXISTING => {
                    Err(refusal(VaultErrorCode::NotFound, "the path does not exist"))
                }
                _ => make_new(path, options, mode)
                    .await
                    .context("Vault::open: make a Vault"),
            },
        }
    })
}

fn refusal(code: VaultErrorCode, message: &str) -> Error {
    Fault::new(code, message).into()
}

/// What one look at a path found: a place, or a folder whose `.git` is not
/// a repository yet (another `open` is running `git init` in it).
enum Look {
    At(Spot),
    Becoming,
}

/// What is at `path`. A git work tree is asked for its marker before
/// anything else of it is read: a marker of another generation fails here.
/// A folder another `open` is making a repository in is waited for, up to
/// a moment; one that does not become a work tree is `Spot::Other`.
async fn classify(path: &Path) -> Result<Spot, Error> {
    let at = path.to_path_buf();
    match blocking(move |cx| look_at(cx, &at)).await? {
        Look::At(spot) => Ok(spot),
        Look::Becoming => match wait_for_work_tree(path).await {
            Ok(()) => {
                let at = path.to_path_buf();
                blocking(move |cx| marked(cx, &at)).await
            }
            Err(error) if error.is(VaultErrorCode::Aborted) => Ok(Spot::Other),
            Err(error) => Err(error),
        },
    }
}

/// One look at `path`.
fn look_at(cx: &Blocking, path: &Path) -> Result<Look, Error> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Look::At(Spot::Missing));
        }
        Err(err) => return Err(io(err)).context("stat the path"),
    };
    if !meta.is_dir() {
        return Ok(Look::At(Spot::Other));
    }
    let mut entries = fs::read_dir(path).map_err(io).context("read the folder")?;
    if entries.next().is_none() {
        return Ok(Look::At(Spot::Empty));
    }
    if repo::is_work_tree(path)? {
        return marked(cx, path).map(Look::At);
    }
    if path.join(".git").is_dir() {
        return Ok(Look::Becoming);
    }
    Ok(Look::At(Spot::Other))
}

/// The place of the git work tree at `path`: a Vault when the commit `HEAD`
/// names holds the marker, a plain work tree otherwise.
fn marked(_cx: &Blocking, path: &Path) -> Result<Spot, Error> {
    match marker::read(&Repos::new(path).snapshot(Repo::Main)?)? {
        Some(_) => Ok(Spot::Vault),
        None => Ok(Spot::WorkTree),
    }
}

/// Waits until `path` is a git work tree, which another `open` is making:
/// a look every 10 ms, for [`OTHER_OPEN_WAIT`] at most.
async fn wait_for_work_tree(path: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + OTHER_OPEN_WAIT;
    loop {
        let at = path.to_path_buf();
        if blocking(move |_cx| Ok(repo::is_work_tree(&at)?)).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(refusal(
                VaultErrorCode::Aborted,
                "another open is making a Vault at this path; try again",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// What claiming the folder of a new Vault found.
enum Claim {
    /// This call made the repository.
    Made,
    /// Another `open` claimed the folder first.
    Taken,
}

/// Makes a new Vault at `path`, a missing path or an empty folder: the
/// folder, `git init`, then the marker and the `.gitignore` in one commit
/// under the write lock. The folder's `.git` is created first as the claim
/// on the path: of two `open`s at once, one makes the repository and the
/// other waits for it.
async fn make_new(path: PathBuf, options: OpenOptions, mode: OpenMode) -> Opened {
    if let Some(url) = options.mirror.clone() {
        return clone_new(path, url, options, mode)
            .await
            .context("clone the mirror");
    }
    let at = path.clone();
    match blocking(move |cx| claim(cx, &at, mode)).await? {
        Claim::Made => {}
        Claim::Taken => wait_for_work_tree(&path).await?,
    }
    Ok((mark_and_open(path, options).await?, false))
}

/// Creates the folder and its `.git`, the claim on the path, and makes the
/// repository when the claim is this call's.
fn claim(_cx: &Blocking, path: &Path, mode: OpenMode) -> Result<Claim, Error> {
    fs::create_dir_all(path)
        .map_err(io)
        .context("create the folder")?;
    match fs::create_dir(path.join(".git")) {
        Ok(()) => {
            init_repository(path)?;
            Ok(Claim::Made)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if mode == OpenMode::OPEN_MODE_CREATE_NEW {
                return Err(refusal(
                    VaultErrorCode::AlreadyExists,
                    "another open is making a Vault at this path",
                ));
            }
            Ok(Claim::Taken)
        }
        Err(err) => Err(io(err)).context("claim the folder"),
    }
}

/// A folder beside the path of a new Vault that a clone is made in. It is
/// removed when it is dropped before it was moved into place; a drop on a
/// thread of the runtime hands the removal to the blocking pool.
struct Staging {
    path: PathBuf,
    /// Moved into place, or removed already: nothing to clean.
    done: bool,
}

impl Drop for Staging {
    /// Only a call dropped midway gets here with a folder left: every
    /// return of `clone_new` settles the folder first. Nobody waits for the
    /// dropped call, so the removal runs on the blocking pool; a clone
    /// killed with its call is gone by then, or goes in a moment, and what
    /// it wrote after the removal stays, as after a crash.
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let path = std::mem::take(&mut self.path);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(move || fs::remove_dir_all(&path))),
            Err(_) => {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
}

/// Makes a new Vault at `path`, a missing path or an empty folder, by
/// cloning the mirror at `url`.
///
/// The clone is made in a staging folder in the same parent, classified like
/// any path (a marker of another generation is `GENERATION`), given its
/// marker in one commit when it has none, and only then moved to `path`. A
/// failure at any step removes the staging folder before the call returns,
/// and leaves nothing at `path`. When the move finds that another `open`
/// made the Vault meanwhile, this one opens that Vault.
async fn clone_new(path: PathBuf, url: String, options: OpenOptions, mode: OpenMode) -> Opened {
    let at = path.clone();
    let staging = blocking(move |cx| stage(cx, &at)).await?;
    if let Err(error) = prepare_clone(&staging.path, &url, &options).await {
        // The step owns the folder: if it never runs, its drop removes it.
        let _ = blocking(move |cx| {
            discard(cx, staging);
            Ok(())
        })
        .await;
        return Err(error);
    }
    let target = path.clone();
    match blocking(move |cx| place_clone(cx, staging, &target)).await? {
        None => Ok((open_existing(path, options).await?, false)),
        Some(err) => match classify(&path).await {
            Ok(Spot::Vault | Spot::WorkTree | Spot::Other) => open_at(path, options, mode).await,
            _ => Err(io(err)).context("move the clone into place"),
        },
    }
}

/// The clone of `url` in the staging folder `place`, looked at, and given
/// its marker when it has none.
async fn prepare_clone(place: &Path, url: &str, options: &OpenOptions) -> Result<(), Error> {
    git::clone(url, place).await.context("clone")?;
    let at = place.to_path_buf();
    let spot = blocking(move |cx| {
        repo::enable_long_paths_at(&at).context("set up the clone")?;
        match look_at(cx, &at).context("look at the clone")? {
            Look::At(spot) => Ok(spot),
            Look::Becoming => Ok(Spot::Other),
        }
    })
    .await?;
    match spot {
        Spot::Vault => Ok(()),
        Spot::WorkTree => {
            drop(
                mark_and_open(place.to_path_buf(), options.clone())
                    .await
                    .context("mark the clone")?,
            );
            Ok(())
        }
        _ => Err(refusal(
            VaultErrorCode::Replication,
            "the mirror did not clone into a git work tree",
        )),
    }
}

/// A new staging folder for a clone of the Vault at `path`.
fn stage(_cx: &Blocking, path: &Path) -> Result<Staging, Error> {
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
    let staging = parent.join(format!(
        ".{name}.scheda-clone-{}-{}",
        std::process::id(),
        NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&staging);
    Ok(Staging {
        path: staging,
        done: false,
    })
}

/// Removes the staging folder of a clone that failed.
fn discard(_cx: &Blocking, mut staging: Staging) {
    staging.done = true;
    let _ = fs::remove_dir_all(&staging.path);
}

/// Moves the clone in `staging` to `path`: `None` when it moved, the
/// failure of the move otherwise, with the staging folder removed.
fn place_clone(
    cx: &Blocking,
    mut staging: Staging,
    path: &Path,
) -> Result<Option<std::io::Error>, Error> {
    match fs::rename(&staging.path, path) {
        Ok(()) => {
            staging.done = true;
            Ok(None)
        }
        Err(err) => {
            discard(cx, staging);
            Ok(Some(err))
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

/// The pieces of a Vault before it is complete: its canonical path, its
/// repositories, and its write path.
struct Parts {
    canonical: PathBuf,
    repos: Arc<Repos>,
    committer: Committer,
}

/// The parts of the Vault at `path`, which a caller's mirror must be the
/// origin of; with `scheda`, `.scheda/` is created first.
fn parts(cx: &Blocking, path: &Path, options: &OpenOptions, scheda: bool) -> Result<Parts, Error> {
    let canonical = path
        .canonicalize()
        .map_err(io)
        .context("resolve the path")?;
    if let Some(mirror) = options.mirror.as_deref() {
        check_mirror(&canonical, mirror)?;
    }
    if scheda {
        prepare_scheda(&canonical)?;
    }
    let repos = Arc::new(Repos::new(&canonical));
    let committer = Committer::new(cx, &canonical, options, Arc::clone(&repos))?;
    Ok(Parts {
        canonical,
        repos,
        committer,
    })
}

/// Gives the git work tree at `path` its marker and the missing entries of
/// `.gitignore` in one commit, unless another `open` did already; then opens
/// it. The mirror a caller names is checked first, so a refusal writes
/// nothing.
async fn mark_and_open(path: PathBuf, options: OpenOptions) -> Result<Vault, Error> {
    let checked = options.clone();
    let parts = blocking(move |cx| parts(cx, &path, &checked, true)).await?;
    let held = parts
        .committer
        .acquire()
        .await
        .context("take the write lock")?;
    let parts = blocking(move |cx| {
        mark(cx, &parts.committer, held)?;
        Ok(parts)
    })
    .await?;
    finish(parts, options).await
}

/// The marker and `.gitignore` committed under the write lock `held`, after
/// a second look: another `open` may have made the Vault meanwhile.
fn mark(cx: &Blocking, committer: &Committer, held: Held) -> Result<(), Error> {
    let lock = committer.adopt(cx, held).context("take the write lock")?;
    let snap = lock.snapshot(Repo::Main)?;
    if marker::read(&snap)?.is_none() {
        let changes = service_changes(&snap)?;
        lock.commit_in(&snap, MARK_MESSAGE, &changes)
            .context("commit the marker")?;
    }
    Ok(())
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
async fn open_existing(path: PathBuf, options: OpenOptions) -> Result<Vault, Error> {
    let checked = options.clone();
    let parts = blocking(move |cx| parts(cx, &path, &checked, false)).await?;
    finish(parts, options).await
}

/// Completes the handle of the Vault whose `parts` are made: recovers a
/// write a dead writer left (taking the write lock for that only when the
/// lock file holds paths), reads the collection files of `HEAD` (an invalid
/// one fails here, before anything is created), then creates `.scheda/` and
/// the writer identity.
async fn finish(mut parts: Parts, options: OpenOptions) -> Result<Vault, Error> {
    let root = parts.canonical.clone();
    if blocking(move |cx| commit::pending(cx, &root)).await? {
        let held = parts
            .committer
            .acquire()
            .await
            .context("take the write lock")?;
        parts = blocking(move |cx| {
            drop(parts.committer.adopt(cx, held)?);
            Ok(parts)
        })
        .await?;
    }
    blocking(move |cx| complete(cx, parts, options)).await
}

/// The Vault of `parts`, with its collections read.
fn complete(_cx: &Blocking, parts: Parts, options: OpenOptions) -> Result<Vault, Error> {
    let Parts {
        canonical,
        repos,
        committer,
    } = parts;
    let snap = repos.snapshot(Repo::Main)?;
    let declarations = Arc::new(collection::load(&snap)?);
    prepare_scheda(&canonical)?;
    let writer = writer::load_or_create(&canonical, options.writer.as_deref())?;
    let indexer = Indexer::new(committer.signal(), &canonical);
    let id_maps = IdMaps::new(Index::new(&canonical));
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
        ports: Arc::new(Installed::default()),
        indexer,
        id_maps,
        gate: Gate::default(),
        closing: tokio::sync::Mutex::new(()),
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

    use scheda_api::v1::embedder_ref::EmbedderRef;
    use scheda_api::v1::{OpenOptions, VaultErrorCode};

    use super::Vault;
    use crate::git::{git, isolate_test_git_env};
    use crate::ports::Ports;
    use crate::testing::{open, step};

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

    #[tokio::test]
    async fn an_existing_vault_opens_with_its_collections_and_a_writer_and_closes() {
        let scratch = Scratch::vault("open");
        let vault = open(&scratch.0, OpenOptions::default()).await.unwrap();
        assert_eq!(vault.path, scratch.0.canonicalize().unwrap());
        let declarations = step(&vault, |_cx, vault| vault.declarations_now())
            .await
            .unwrap();
        assert_eq!(declarations.len(), 1);
        assert!(declarations.get("notes").is_some());
        assert_eq!(vault.writer.len(), 16);
        assert_eq!(vault.options.mirror, None);
        vault.close().await.unwrap();
        assert!(scratch.0.join(".scheda/writer").is_file());
        let again = open(&scratch.0, OpenOptions::default().with_writer("alice"))
            .await
            .unwrap();
        assert_eq!(again.writer, "alice");
    }

    #[tokio::test]
    async fn removing_a_port_that_is_not_there_is_nothing_to_do() {
        let scratch = Scratch::vault("no-ports");
        let vault = open(&scratch.0, OpenOptions::default()).await.unwrap();
        vault.set_embedder(None).await.unwrap();
        vault.set_reranker(None).await.unwrap();
        vault.set_projector(None).await.unwrap();
        let error = vault
            .set_embedder(Some(EmbedderRef::from_sync(AnyEmbedder)))
            .await
            .unwrap_err();
        // Without the Index no port is taken; with it, this one's descriptor
        // breaks its rules.
        let code = if cfg!(feature = "index") {
            VaultErrorCode::InvalidArgument
        } else {
            VaultErrorCode::Unimplemented
        };
        assert!(error.is(code), "{error}");
    }

    #[tokio::test]
    async fn a_port_is_refused_by_open_before_the_path_is_touched() {
        let scratch = Scratch::vault("bad-ports");
        let missing = scratch.0.join("nowhere");
        let ports = Ports {
            embedder: Some(EmbedderRef::from_sync(AnyEmbedder)),
            ..Ports::default()
        };
        let error = Vault::open(missing.clone(), OpenOptions::default(), ports)
            .await
            .unwrap_err();
        let code = if cfg!(feature = "index") {
            VaultErrorCode::InvalidArgument
        } else {
            VaultErrorCode::Unimplemented
        };
        assert!(error.is(code), "{error}");
        assert!(!missing.exists());
    }

    /// An embedder whose descriptor is empty, which breaks its rules.
    struct AnyEmbedder;

    impl scheda_api::v1::EmbedderSync for AnyEmbedder {
        fn describe(
            &self,
            _empty: buffa_types::google::protobuf::Empty,
        ) -> Result<scheda_api::v1::EmbedderDescriptor, crate::error::Error> {
            Ok(scheda_api::v1::EmbedderDescriptor::default())
        }

        fn embed(
            &self,
            _texts: protocontract::BoxIter<
                'static,
                Result<scheda_api::v1::Text, crate::error::Error>,
            >,
        ) -> Result<
            protocontract::BoxIter<'static, Result<scheda_api::v1::Vector, crate::error::Error>>,
            crate::error::Error,
        > {
            Ok(Box::new(std::iter::empty()))
        }

        fn embed_query(
            &self,
            _query: scheda_api::v1::Query,
        ) -> Result<scheda_api::v1::Vector, crate::error::Error> {
            Ok(scheda_api::v1::Vector::default())
        }
    }

    #[tokio::test]
    async fn a_mirror_must_be_the_origin_of_the_vault() {
        let scratch = Scratch::vault("mirror");
        let url = "https://example.test/org/repo.git";
        let options = || OpenOptions::default().with_mirror(url);
        let error = open(&scratch.0, options()).await.unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument), "no origin yet");
        git()
            .current_dir(Path::new(&scratch.0))
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg(url)
            .run()
            .unwrap();
        open(&scratch.0, options()).await.unwrap();
        let other = OpenOptions::default().with_mirror("https://example.test/org/other.git");
        let error = open(&scratch.0, other).await.unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[tokio::test]
    async fn options_that_break_a_rule_are_invalid_argument() {
        let scratch = Scratch::vault("options");
        for options in [
            OpenOptions::default().with_writer("Not Valid"),
            OpenOptions::default().with_mirror("not a url"),
            OpenOptions::default().with_index_poll_ms(1),
            OpenOptions::default().with_index_wait_timeout_ms(0),
            OpenOptions::default().with_filter_max_depth(0),
            OpenOptions::default().with_filter_max_depth(101),
        ] {
            let error = open(&scratch.0, options).await.unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
        assert!(
            !scratch.0.join(".scheda").exists(),
            "a refusal writes nothing"
        );
    }

    #[tokio::test]
    async fn the_options_of_the_index_wait_and_the_filter_depth_are_accepted_within_their_bounds() {
        let scratch = Scratch::vault("options-bounds");
        for options in [
            OpenOptions::default().with_index_wait_timeout_ms(1),
            OpenOptions::default().with_index_wait_timeout_ms(60_000),
            OpenOptions::default().with_filter_max_depth(1),
            OpenOptions::default().with_filter_max_depth(100),
        ] {
            let vault = open(&scratch.0, options).await.unwrap();
            vault.close().await.unwrap();
        }
    }
}
