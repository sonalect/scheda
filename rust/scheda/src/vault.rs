use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
#[cfg(feature = "find")]
use std::time::Instant;

#[cfg(feature = "search")]
use crate::Reranker;
#[cfg(feature = "search")]
use crate::error::ensure;
use crate::error::{Code, Context, ContextAs, ContextIo, bail, io_code};
use crate::git;
use crate::index::{self, JournalIdMaps};
use crate::isolated;
use crate::journal;
use crate::marker;
use crate::marks;
#[cfg(feature = "find")]
use crate::query;
use crate::schema::{self, CollectionDecl, Decls, Kind};
#[cfg(feature = "find")]
use crate::search::SearchEngine;
use crate::store;
use crate::{
    Embedder, Encryption, Error, History, OpenOptions, Projector, Record, ReindexResult, Schema,
    VaultInfo,
};
#[cfg(feature = "find")]
use crate::{Filter, FindQuery, FindResult};
#[cfg(feature = "search")]
use crate::{SearchHits, SearchQuery, search_hits};

const AUTO_COMMIT_MESSAGE: &str = "scheda";

/// In-process write lock plus the thread that currently holds it.
struct WriteLock {
    file: Option<File>,
    owner: Option<thread::ThreadId>,
}

impl WriteLock {
    fn new() -> Self {
        Self {
            file: None,
            owner: None,
        }
    }

    fn is_held(&self) -> bool {
        self.file.is_some()
    }
}

#[cfg(feature = "find")]
type ReindexWalkHook = Arc<dyn Fn(&str) + Send + Sync>;

/// A git working tree that holds collections of records.
pub struct Vault {
    path: PathBuf,
    index: Arc<index::IndexHandle>,
    remotes: Vec<String>,
    auto_sync: bool,
    write_lock: Arc<Mutex<WriteLock>>,
    /// Collection declarations, read again from the work tree when a
    /// name misses and when the Index passes a commit.
    declared: Arc<schema::Declared>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    writer: Arc<str>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    #[cfg(feature = "find")]
    search: Arc<SearchEngine>,
    /// Swapped after `open` by [`Self::set_embedder`]. A `collection`
    /// copies the value it sees at that call; the background indexer reads
    /// the current one every round.
    embedder: Arc<Mutex<Option<Arc<dyn Embedder>>>>,
    projector: Option<Arc<dyn Projector>>,
    identity: Arc<git::CommitIdentity>,
    encryption: Option<Encryption>,
    #[cfg(all(test, feature = "find"))]
    walk_hook: Mutex<Option<ReindexWalkHook>>,
    /// Background indexer; stopped and joined when the last of this
    /// handle and its collections drops.
    #[cfg(feature = "find")]
    indexer: Option<Arc<crate::indexer::Indexer>>,
}

/// A named set of records with one write mode (`document`, `journal`, or `singleton`).
pub struct Collection {
    vault: PathBuf,
    index: Arc<index::IndexHandle>,
    write_lock: Arc<Mutex<WriteLock>>,
    auto_sync: bool,
    remotes: Vec<String>,
    decl: CollectionDecl,
    decls: Arc<Decls>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    writer: Arc<str>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    #[cfg(feature = "find")]
    search: Arc<SearchEngine>,
    #[cfg(feature = "search")]
    embedder: Option<Arc<dyn Embedder>>,
    projector: Option<Arc<dyn Projector>>,
    identity: Arc<git::CommitIdentity>,
    encryption: Option<Encryption>,
    #[cfg(feature = "find")]
    indexer: Option<Arc<crate::indexer::Indexer>>,
}

impl Vault {
    /// Open, create, or clone a Vault.
    ///
    /// Path exists → open. A set `remote_repo` must match the fetch URL of
    /// `origin` (`invalid` if there is no `origin` or the URL differs); `origin`
    /// is not added or rewritten. Unset `remote_repo` attaches and does not
    /// change `origin`. Path missing + `remote_repo` → clone. A set
    /// `remote_repo` is an absolute URI or an scp-style `git@host:path`. Path
    /// missing → `git init`. Path exists but is not a git work tree →
    /// `invalid`.
    ///
    /// A Vault is a git work tree with the marker file `{vault}/.scheda.yaml`
    /// (`version: 1`), committed to git so a clone carries it. By default
    /// `open` writes the marker into the first commit of a new or cloned Vault,
    /// and adds it in one commit to a git work tree that lacks it (a Vault
    /// made before the marker existed, a clone of such a Vault, or an ordinary
    /// repository), then opens. A marker that cannot be read, or whose
    /// `version` is not 1, is `invalid` and nothing is written.
    /// `OpenOptions.existing_only` opens only a work tree that already has the
    /// marker: a missing path is `not_found`, a folder without the marker or
    /// that is not a git work tree is `not_a_vault`, and it never runs
    /// `git init` or `git clone`. A refused folder is left exactly as it was.
    ///
    /// Optional caller embedder. Turns on the vector leg of `search`.
    /// Without the `search` crate feature, a `Some` value fails
    /// (`index_vector`): that leg is absent. The signature is the same in
    /// every build.
    ///
    /// No projector: the Index holds identity only. `find` `all` and `term`
    /// on `id` work; any other Filter field is `invalid`. `search` has no
    /// FTS or vector slots; the embedder is not called on write. `add` does
    /// not call `project` to accept or reject the write. Same as
    /// [`open_with`](Self::open_with) with `projector` `None`.
    ///
    /// `OpenOptions.author` is the git author and committer for every commit
    /// on this handle (main tree and `{vault}/.isolated/`). Unset `name` or
    /// `email` on a set Author, or characters git cannot carry, fail here
    /// before the first commit. Unset Author → local git config, then
    /// `Scheda <scheda@invalid>`.
    ///
    /// `OpenOptions.encryption` is stored on this handle for I/O of collections
    /// whose declaration marks `encrypted`. A build without the `encrypt` crate
    /// feature refuses a set value (`unsupported_encryption`) before any
    /// directory is created. Unset `encryption` is unchanged: `open` of the
    /// Vault still succeeds.
    ///
    /// `OpenOptions.writer` is the machine-local identity of this clone. Unset
    /// → generated on first `open` and stored in `{vault}/.scheda/writer`.
    /// That file is gitignored with the rest of `.scheda/`. A set value must
    /// be lowercase alphanumeric plus `-`, 1–32 bytes, and must not start
    /// with eight hex digits (`invalid` otherwise).
    pub fn open(
        path: impl AsRef<Path>,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, Error> {
        Self::open_with(path, options, embedder, None)
    }

    /// Open, create, or clone a Vault, with an optional caller [`Projector`].
    ///
    /// Path, options, and embedder are the same as [`open`](Self::open).
    /// Optional caller projector: `find` columns and `search` FTS / vector
    /// slots come from [`Projector::schema`] / [`Projector::project`], not
    /// collection-declaration fields. Without it the Index holds identity
    /// only; `find` `all` and `term` on `id` work; any other Filter field is
    /// `invalid`; `search` has no FTS or vector slots and the embedder is not
    /// called on write. `add` does not call `project` to accept or reject the
    /// write. Changing projector schema is an Index format change
    /// (`index_version` until `reindex`).
    pub fn open_with(
        path: impl AsRef<Path>,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
        projector: Option<Arc<dyn Projector>>,
    ) -> Result<Self, Error> {
        options.require_valid()?;
        #[cfg(not(feature = "encrypt"))]
        if options.encryption.is_set() {
            bail!(
                Error::unsupported_encryption("encryption is not available in this build")
                    .context("[Vault::open] encryption")
            );
        }
        let path = path.as_ref();
        let existing_only = options.existing_only.unwrap_or(false);
        if path.exists() {
            if !git::is_work_tree(path) {
                if existing_only {
                    bail!(
                        Error::not_a_vault(format!(
                            "path is not a git work tree: {}",
                            path.display()
                        ))
                        .context("[Vault::open] existing only")
                    );
                }
                bail!(Error::invalid(format!(
                    "path exists but is not a Vault: {}",
                    path.display()
                )));
            }
            // Everything up to `open_existing` only reads, so a refused
            // folder is left as it was.
            let marked = marker::read(path)?;
            if existing_only && !marked {
                bail!(
                    Error::not_a_vault(format!("no {} marker in {}", marker::FILE, path.display()))
                        .context("[Vault::open] existing only")
                );
            }
            require_matching_origin(path, &options)?;
            let identity = git::identity_from_open(&options, path)?;
            let marker_pending =
                !existing_only && (!marked || !git::is_tracked(path, marker::FILE)?);
            return Self::open_existing(
                path,
                options,
                embedder,
                projector,
                identity,
                marker_pending,
            );
        }
        if existing_only {
            bail!(
                Error::not_found(format!("no Vault at {}", path.display()))
                    .context("[Vault::open] existing only")
            );
        }
        let remote = remote_repo_url(&options).map(str::to_owned);
        if let Some(url) = remote {
            return Self::clone_into(&url, path, options, embedder, projector);
        }
        Self::init_at(path, options, embedder, projector)
    }

    fn init_at(
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
        projector: Option<Arc<dyn Projector>>,
    ) -> Result<Self, Error> {
        prepare_new_work_tree(path)?;
        git::init(path)?;
        let identity = git::identity_from_open(&options, path)?;
        commit_service_files(path, &identity)?;
        Self::open_existing(path, options, embedder, projector, identity, false)
    }

    fn clone_into(
        url: &str,
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
        projector: Option<Arc<dyn Projector>>,
    ) -> Result<Self, Error> {
        let url = url.trim();
        if url.is_empty() {
            bail!(Error::invalid("clone URL is empty"));
        }
        prepare_new_work_tree(path)?;
        git::clone(url, path)?;
        let identity = git::identity_from_open(&options, path)?;
        commit_service_files(path, &identity)?;
        Self::open_existing(
            path,
            without_remote_repo(options),
            embedder,
            projector,
            identity,
            false,
        )
    }

    fn open_existing(
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
        projector: Option<Arc<dyn Projector>>,
        identity: git::CommitIdentity,
        marker_pending: bool,
    ) -> Result<Self, Error> {
        #[cfg(not(feature = "search"))]
        if embedder.is_some() {
            bail!(
                Error::index_vector("vector index is not available")
                    .context("[Vault::open] embedder")
            );
        }
        let auto_sync = options.auto_sync.unwrap_or(false);
        #[cfg(feature = "find")]
        let poll = options
            .index_poll_ms
            .map_or(crate::indexer::DEFAULT_POLL, |ms| {
                std::time::Duration::from_millis(u64::from(ms))
            });
        let path = fs::canonicalize(path).context("[Vault::open] canonicalize vault")?;
        let gitignore_pending = gitignore_needs_write(&path)?;
        let index_root = path.join(".scheda").join("index");
        fs::create_dir_all(&index_root).context("[Vault::open] create index dir")?;
        store::restrict_dir(&path.join(".scheda"))?;
        store::restrict_dir(&index_root)?;
        let index_root =
            fs::canonicalize(&index_root).context("[Vault::open] canonicalize index")?;
        let remotes = git::remotes(&path)?;
        if auto_sync && !remotes.is_empty() {
            pull_under_write_lock(&path)?;
        }
        try_prune_orphans(&path, &index_root)?;
        let index = Arc::new(index::bind(&index_root)?);
        let generation = index.dir()?;
        index::restrict_index_tree(&generation)?;
        let declared = Arc::new(schema::Declared::open(&path, &generation)?);
        let decls = declared.current()?;
        let journal_ids = Arc::new(Mutex::new(index::load_journal_ids(&generation, &decls)?));
        let written = Arc::new(Mutex::new(load_written(&path)?));
        let in_txn = Arc::new(Mutex::new(false));
        let writer = Arc::<str>::from(crate::writer::load_or_create(
            &path,
            options.writer.as_deref(),
        )?);
        #[cfg(feature = "find")]
        let search = Arc::new(SearchEngine::open(&generation, false, projector.clone())?);
        let encryption = options.encryption.as_option().cloned();
        let vault = Self {
            path,
            index,
            remotes,
            auto_sync,
            write_lock: Arc::new(Mutex::new(WriteLock::new())),
            declared,
            written,
            in_txn,
            writer,
            journal_ids,
            #[cfg(feature = "find")]
            search,
            embedder: Arc::new(Mutex::new(embedder)),
            projector,
            identity: Arc::new(identity),
            encryption,
            #[cfg(all(test, feature = "find"))]
            walk_hook: Mutex::new(None),
            #[cfg(feature = "find")]
            indexer: None,
        };
        if gitignore_pending || marker_pending {
            vault.write_service_files_under_lock(marker_pending, gitignore_pending)?;
        }
        #[cfg(feature = "find")]
        let vault = {
            let mut vault = vault;
            vault.indexer = Some(Arc::new(crate::indexer::Indexer::start(
                crate::indexer::Context {
                    vault: vault.path.clone(),
                    index: Arc::clone(&vault.index),
                    search: Arc::clone(&vault.search),
                    journal_ids: Arc::clone(&vault.journal_ids),
                    declared: Arc::clone(&vault.declared),
                    embedder: Arc::clone(&vault.embedder),
                    projector: vault.projector.clone(),
                    poll,
                },
            )?));
            vault
        };
        Ok(vault)
    }

    /// Where the Index stands against git `HEAD`.
    ///
    /// `head` and `isolated_head` are read now; the marks are those of the
    /// generation this handle answers queries from, whichever process
    /// wrote them. When another handle publishes a new generation, this one
    /// reports it once its indexer has switched to it. The roles and the
    /// last error are this handle's own indexer.
    ///
    /// The pending counts come from a `git diff` of each leg's mark against
    /// `HEAD`, so a call costs about what that diff costs; poll
    /// [`wait_indexed`](Self::wait_indexed) rather than this in a loop.
    #[cfg(feature = "find")]
    pub fn index_status(&self) -> Result<crate::IndexStatus, Error> {
        self.status(true)
    }

    /// [`index_status`](Self::index_status); `pending` `false` leaves the
    /// pending counts `None` and skips the diffs.
    #[cfg(feature = "find")]
    fn status(&self, pending: bool) -> Result<crate::IndexStatus, Error> {
        let generation = self.generation_dir()?;
        let heads = crate::indexer::Heads::read(&self.path)?;
        let head = heads.as_ref().map(|heads| heads.main().to_owned());
        let isolated_head = heads
            .as_ref()
            .and_then(|heads| heads.nested().map(str::to_owned));
        let state = self
            .indexer
            .as_ref()
            .map(|indexer| indexer.state())
            .unwrap_or_default();
        // Marks of a generation in another format say nothing: the indexer
        // is about to replace it.
        let usable = matches!(
            index::format_version(&generation)?,
            None | Some(index::FORMAT_VERSION)
        );
        let mark = |leg, repo| {
            if usable {
                marks::read(&generation, leg, repo)
            } else {
                Ok(None)
            }
        };
        let (lexical_pending, vector_pending) = match &heads {
            _ if !pending => (None, None),
            None => (Some(0), Some(0)),
            // A commit whose declarations do not load is the indexer's
            // error to report; the status still answers.
            Some(heads) => match schema::load_at(&self.path, heads.main()) {
                Ok(decls) => {
                    let count =
                        |leg| crate::indexer::pending(&generation, heads, &decls, leg, usable).ok();
                    (count(marks::Leg::Lexical), count(marks::Leg::Vector))
                }
                Err(_) => (None, None),
            },
        };
        let embedder = self.embedder_arc()?;
        Ok(crate::IndexStatus {
            head,
            isolated_head,
            lexical: mark(marks::Leg::Lexical, marks::Repo::Main)?,
            vector: mark(marks::Leg::Vector, marks::Repo::Main)?,
            isolated_lexical: mark(marks::Leg::Lexical, marks::Repo::Isolated)?,
            isolated_vector: mark(marks::Leg::Vector, marks::Repo::Isolated)?,
            journal_ids: mark(marks::Leg::Ids, marks::Repo::Main)?,
            isolated_journal_ids: mark(marks::Leg::Ids, marks::Repo::Isolated)?,
            lexical_pending,
            vector_pending,
            model_mismatch: crate::indexer::model_mismatch(&generation, embedder.as_deref())?,
            lexical_role: state.lexical_role,
            vector_role: state.vector_role,
            last_error: state.last_error,
        })
    }

    /// Wait until the Index has caught up with the `HEAD` seen when this
    /// call starts.
    ///
    /// Each leg that has an owner must reach that commit (or a later one):
    /// the lexical leg, and with it the journal `id` maps, when this handle
    /// runs an indexer or another handle holds `{vault}/.scheda/index.lock`;
    /// the vector leg when this handle's
    /// embedder may write it or another handle holds
    /// `{vault}/.scheda/vector.lock`. A leg nobody can write does not make
    /// the call wait; the returned status shows it behind. Past `timeout`
    /// the call fails with `io_timed_out`, and the message lists the heads
    /// and marks it saw last. Before it returns, the collection declarations
    /// are read again when the Index passed a commit since the last read,
    /// so `info` shows a declaration another handle changed.
    #[cfg(feature = "find")]
    pub fn wait_indexed(&self, timeout: std::time::Duration) -> Result<crate::IndexStatus, Error> {
        let deadline = Instant::now().checked_add(timeout);
        let target = self.wait_target()?;
        loop {
            let Some(status) = self.wait_pass(&target)? else {
                return self.wait_done();
            };
            let now = Instant::now();
            let left = match deadline {
                Some(deadline) if now >= deadline => {
                    return Err(
                        wait_timed_out(timeout, &status).context("[Vault::wait_indexed] wait")
                    );
                }
                Some(deadline) => deadline - now,
                None => WAIT_SLICE,
            };
            let slice = left.min(WAIT_SLICE);
            match &self.indexer {
                Some(indexer) => {
                    let rounds = indexer.state().rounds;
                    indexer.nudge();
                    indexer.wait_round(rounds, slice);
                }
                None => thread::sleep(slice),
            }
        }
    }

    /// The end of a wait that reached its target: declarations a passed
    /// commit changed are read again, then the status is taken. Counted
    /// once, after the wait: a leg nobody writes shows its lag here.
    #[cfg(feature = "find")]
    pub(crate) fn wait_done(&self) -> Result<crate::IndexStatus, Error> {
        self.declared.follow(&self.generation_dir()?)?;
        self.index_status()
    }

    /// The heads a wait has to reach: those of `HEAD` now.
    #[cfg(feature = "find")]
    pub(crate) fn wait_target(&self) -> Result<crate::IndexStatus, Error> {
        self.status(false)
    }

    /// One look at the Index on behalf of a wait for `target`: `None` once
    /// every leg with an owner has reached it, else the status seen.
    #[cfg(feature = "find")]
    pub(crate) fn wait_pass(
        &self,
        target: &crate::IndexStatus,
    ) -> Result<Option<crate::IndexStatus>, Error> {
        let nested = isolated::root(&self.path);
        // Owners are looked at again on every pass: a probe of a role lock
        // from another thread can make one pass see it taken.
        let lexical = (self.indexer.is_some()
            && crate::indexer::lexical_allowed(&self.search, &self.generation_dir()?)?)
            || crate::indexer::role_taken(&self.path, false)?;
        let own_vector = match self.embedder_arc()? {
            Some(embedder) if self.indexer.is_some() => {
                crate::indexer::vector_allowed(&self.generation_dir()?, Some(embedder.as_ref()))?
            }
            _ => false,
        };
        let vector = own_vector || crate::indexer::role_taken(&self.path, true)?;
        let status = self.status(false)?;
        let main = |mark: &Option<String>, active| {
            reached(&self.path, target.head.as_deref(), mark.as_deref(), active)
        };
        let nested_reached = |mark: &Option<String>, active| {
            reached(
                &nested,
                target.isolated_head.as_deref(),
                mark.as_deref(),
                active,
            )
        };
        // The journal `id` maps move with the lexical role.
        let done = main(&status.lexical, lexical)?
            && main(&status.journal_ids, lexical)?
            && main(&status.vector, vector)?
            && nested_reached(&status.isolated_lexical, lexical)?
            && nested_reached(&status.isolated_journal_ids, lexical)?
            && nested_reached(&status.isolated_vector, vector)?;
        Ok((!done).then_some(status))
    }

    /// Ask this handle's indexer for a round now, unless a failure holds it
    /// back.
    #[cfg(all(feature = "find", feature = "tokio"))]
    pub(crate) fn nudge_indexer(&self) {
        if let Some(indexer) = &self.indexer {
            indexer.nudge();
        }
    }

    /// Bumped by this handle's indexer at the end of every round.
    #[cfg(all(feature = "find", feature = "tokio"))]
    pub(crate) fn index_rounds(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.indexer
            .as_ref()
            .map(|indexer| indexer.subscribe_rounds())
    }

    /// Close this handle: stop and join the background indexer, then drop
    /// the handle.
    ///
    /// The indexer finishes the Record it is indexing — at most one
    /// embedder call, which Scheda cannot interrupt, so bound it with the
    /// embedder's own timeout — and keeps every mark where its last durable
    /// write put it; the next `open` resumes from there. `Collection`
    /// handles made from this Vault keep reading and writing afterwards,
    /// but nothing indexes their commits until a handle is opened again.
    /// An open transaction ends without a commit, as on drop. Failure means
    /// the indexer thread could not be joined.
    ///
    /// Scheda installs no signal handler: a program that wants a clean exit
    /// on `SIGINT` / `SIGTERM` catches the signal and calls this. Being
    /// killed instead is safe too: the next `open` re-applies whatever the
    /// Index had not recorded as done.
    pub fn close(self) -> Result<(), Error> {
        #[cfg(feature = "find")]
        if let Some(indexer) = &self.indexer {
            indexer.shutdown()?;
        }
        drop(self);
        Ok(())
    }

    /// Absolute path of the Vault working tree.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Index directory `{vault}/.scheda/index`.
    ///
    /// Holds the `current` pointer and generation subdirectories. This handle
    /// binds one generation at `open` and keeps using it. A reader that opened
    /// before `reindex` keeps answering `find` / `search` from that snapshot.
    /// A reader older than one `reindex` cycle may lose its files; those
    /// methods then fail with `index_missing` or `index_lexical`.
    #[must_use]
    pub fn index_dir(&self) -> &Path {
        &self.index.root
    }

    fn generation_dir(&self) -> Result<PathBuf, Error> {
        self.index.dir()
    }

    /// Whether this handle currently holds `{vault}/.scheda/write.lock`.
    ///
    /// True during an open transaction and while a mutating call is in
    /// progress. False after `commit`, `discard`, a mutating call that
    /// autocommits, and `open`. Dropping the handle releases the lock
    /// before the rest of the handle is torn down. Only git writes take the
    /// lock: the background indexer holds no write lock, so another handle
    /// can write as soon as it is released.
    #[must_use]
    pub fn holds_write_lock(&self) -> bool {
        match self.write_lock.lock() {
            Ok(slot) => slot.is_held(),
            Err(_) => true,
        }
    }

    /// Machine-local identity of the process that writes this clone.
    ///
    /// Stored in `{vault}/.scheda/writer` (gitignored with the rest of
    /// `.scheda/`). Independent clones of the same Vault keep distinct
    /// identities, so journal segments named with this token cannot collide
    /// across clones. Override at `open` with `OpenOptions.writer`. Generated
    /// on first `open` when the file is absent: sixteen lowercase hex digits
    /// unique to this clone.
    #[must_use]
    pub fn writer(&self) -> &str {
        &self.writer
    }

    #[cfg(all(test, feature = "find"))]
    fn lexical_writer_opens(&self) -> u64 {
        self.search.lexical_writer_opens()
    }

    /// Whether git reports at least one Remote (usually `origin`).
    #[must_use]
    pub fn has_remote(&self) -> bool {
        !self.remotes.is_empty()
    }

    /// Git remote names on this Vault. Empty when there is no Remote.
    #[must_use]
    pub fn remote_names(&self) -> &[String] {
        &self.remotes
    }

    /// `auto_sync` on this handle: after `commit`, `push` when a Remote exists.
    #[must_use]
    pub fn auto_sync(&self) -> bool {
        self.auto_sync
    }

    /// Collection generation values present in this Vault.
    ///
    /// A poisoned decls lock is `io_other`, same as the other decls reads.
    pub fn versions(&self) -> Result<Vec<u32>, Error> {
        Ok(self.decls_arc()?.versions())
    }

    fn decls_arc(&self) -> Result<Arc<Decls>, Error> {
        self.declared.current()
    }

    /// Declarations that know every name in `names`: read again from the
    /// work tree once when one is missing, since another handle or process
    /// may have declared it after this handle last read them.
    fn decls_naming(&self, names: &[&str]) -> Result<Arc<Decls>, Error> {
        let decls = self.decls_arc()?;
        if names.iter().all(|name| decls.get(name).is_some()) {
            return Ok(decls);
        }
        self.declared.reload()
    }

    /// [`decls_naming`](Self::decls_naming) for a query: the collections it
    /// names and those its Filter leaves qualify.
    #[cfg(feature = "find")]
    fn decls_for_query(&self, collections: &[&str], filter: &Filter) -> Result<Arc<Decls>, Error> {
        let mut names = collections.to_vec();
        names.extend(query::leaf_collections(filter));
        self.decls_naming(&names)
    }

    /// Install `embedder` for later [`collection`](Self::collection) handles
    /// and for `search` on this handle.
    ///
    /// Returns the embedder that was installed before this call. A
    /// collection opened before the swap keeps the embedder it copied.
    /// `None` turns the vector leg off until another embedder is installed.
    /// The lexical index is unchanged.
    ///
    /// An embedder of another `model_id` or dimensions than the Index
    /// vectors does not re-embed anything by itself: the handle writes no
    /// vectors, its `search` runs without the vector leg, and
    /// `index_status().model_mismatch` is true. Moving to a new model is the
    /// caller's migration: call [`reindex`](Self::reindex) with the new
    /// embedder installed, which re-embeds every slot.
    pub fn set_embedder(
        &self,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Option<Arc<dyn Embedder>>, Error> {
        let previous = {
            let mut slot = crate::error::mutex(&self.embedder, "[Vault] lock embedder")?;
            std::mem::replace(&mut *slot, embedder)
        };
        #[cfg(feature = "find")]
        if let Some(indexer) = &self.indexer {
            indexer.embedder_changed();
        }
        Ok(previous)
    }

    /// Ask the background indexer for a round now.
    fn wake_indexer(&self) {
        #[cfg(feature = "find")]
        if let Some(indexer) = &self.indexer {
            indexer.wake();
        }
    }

    #[cfg(feature = "find")]
    fn embedder_arc(&self) -> Result<Option<Arc<dyn Embedder>>, Error> {
        Ok(crate::error::mutex(&self.embedder, "[Vault] lock embedder")?.clone())
    }

    /// Handle for one collection.
    ///
    /// A name this handle does not know reads the declarations again from
    /// the work tree once, so a collection another handle or process
    /// declared after `open` is found; still unknown → `not_found`. The
    /// returned handle keeps the declaration it was made with. Isolated collections live under
    /// `{vault}/.isolated/{name}/` on this same handle. A build without the
    /// `encrypt` crate feature refuses an encrypted collection
    /// (`unsupported_encryption`); `open` of the Vault still succeeds.
    pub fn collection(&self, name: &str) -> Result<Collection, Error> {
        let decls = self.decls_naming(&[name])?;
        let Some(decl) = decls.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in the declaration"
            )));
        };
        #[cfg(not(feature = "encrypt"))]
        if decl.encrypted {
            bail!(
                Error::unsupported_encryption(format!(
                    "collection {name} is encrypted; this build cannot open it"
                ))
                .context("[Vault::collection] encryption")
            );
        }
        Ok(Collection {
            vault: self.path.clone(),
            index: Arc::clone(&self.index),
            write_lock: Arc::clone(&self.write_lock),
            auto_sync: self.auto_sync,
            remotes: self.remotes.clone(),
            decl: decl.clone(),
            decls,
            written: Arc::clone(&self.written),
            in_txn: Arc::clone(&self.in_txn),
            writer: Arc::clone(&self.writer),
            journal_ids: Arc::clone(&self.journal_ids),
            #[cfg(feature = "find")]
            search: Arc::clone(&self.search),
            #[cfg(feature = "find")]
            indexer: self.indexer.clone(),
            #[cfg(feature = "search")]
            embedder: self.embedder_arc()?,
            projector: self.projector.clone(),
            identity: Arc::clone(&self.identity),
            encryption: self.encryption.clone(),
        })
    }

    /// Run a Tantivy Query on the Index, then open matching Record files.
    /// Empty `collections` is every non-isolated, non-encrypted collection
    /// that is in the Tantivy Index (`exclude_lexical_index` collections are
    /// omitted). Naming an isolated collection is an ordinary query.
    /// Naming an encrypted collection or one marked `exclude_lexical_index`
    /// is `unsupported`.
    ///
    /// Page order is score descending. Equal scores break by the Index `ord`
    /// fast field, ascending (earlier indexed first). `ord` is unique within
    /// a collection; two Records in different collections that share both
    /// score and `ord` stay unordered relative to each other. One `journal`
    /// collection uses append order instead.
    ///
    /// Filter field names are [`Projector`] find-columns plus identity `id`.
    /// Projector-column leaves must set proto `collection`. Without a
    /// projector, only `all` and Filter on `id` are valid. A collection name
    /// or Filter qualifier this handle does not know reads the declarations
    /// again once, as [`collection`](Self::collection) does.
    #[cfg(feature = "find")]
    pub fn find(
        &self,
        collections: &[&str],
        query: &FindQuery,
        offset: u32,
        count: u32,
    ) -> Result<FindResult, Error> {
        let decls = self.decls_for_query(collections, &query.filter)?;
        QueryCtx {
            engine: &self.search,
            decls: &decls,
            vault: &self.path,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
            scope: crate::query::FilterScope::Vault,
        }
        .find(collections, query, offset, count)
    }

    /// Hybrid retrieval: BM25 and ANN when both are on, then fusion, then an
    /// optional reranker. Empty `collections` is every non-isolated,
    /// non-encrypted collection that has at least one Index leg. Naming an
    /// isolated collection is an ordinary query. A collection with only one
    /// leg runs that leg; hits set `lexical_score` and/or `vector_score` for
    /// the legs that ran. Naming an encrypted collection or one with both
    /// `exclude_lexical_index` and `exclude_vector_index` is `unsupported`.
    ///
    /// `lexical_weight` / `vector_weight` unset → 1; **zero turns that leg
    /// off**. Without a reranker the weights also scale Reciprocal Rank
    /// Fusion, which both orders the page and truncates it to the pool.
    /// With a reranker the fused order is discarded, so membership is
    /// filled by alternating vector and lexical ranks (vector rank 1,
    /// lexical rank 1, vector rank 2, …), skipping a document already
    /// taken, until the pool is full or both legs are exhausted. A short
    /// leg does not leave the pool short while the other still has hits.
    /// Non-zero weights then no longer decide who enters the pool; they
    /// still appear in the reported `rrf_score`.
    ///
    /// `SearchQuery.rerank_limit` bounds how many fused hits the reranker
    /// scores. Unset scores every fused hit; the fusion pool stays
    /// `max(k, 50)`. Set to N, both Index legs retrieve `max(k, 50, N)` so a
    /// candidate in the fused prefix of length N can actually be scored; the
    /// library then keeps that prefix, reads and scores only those,
    /// and drops the rest. N larger than the corpus scores every hit that
    /// exists. A large N is more BM25 and ANN retrieval and more Record
    /// reads before the model runs. Zero is `invalid`. N below the effective
    /// `k` (unset or 0 means 10) is `invalid` and names both numbers.
    /// `rerank_limit` is ignored, not rejected, when no reranker is passed.
    ///
    /// `SearchHits.vector_leg` is set on every reply: `VECTOR_LEG_RAN`, or
    /// why the vector leg did not run (not asked, no collection with vector
    /// slots, no embedder, no vectors yet, an embedder of another model),
    /// decided from what this handle holds without extra I/O. A generation
    /// another Projector built is `index_version` with `repeat: forbidden`,
    /// Filter or not; `reindex` from a handle with the right Projector is
    /// the way over. Collection names and Filter qualifiers this handle
    /// does not know read the declarations again once, as
    /// [`collection`](Self::collection) does.
    #[cfg(feature = "search")]
    pub fn search(
        &self,
        collections: &[&str],
        query: &SearchQuery,
        reranker: Option<&dyn Reranker>,
    ) -> Result<SearchHits, Error> {
        let decls = self.decls_for_query(collections, &query.filter)?;
        QueryCtx {
            engine: &self.search,
            decls: &decls,
            vault: &self.path,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
            scope: crate::query::FilterScope::Vault,
        }
        .search(
            self.embedder_arc()?.as_deref(),
            collections,
            query,
            reranker,
        )
    }

    /// End the transaction on this handle and create one git commit per
    /// repository that has recorded paths (main tree and/or
    /// `{vault}/.isolated/`). Nested is committed first; if the main commit
    /// then fails, nested `HEAD` is reset to the commit it had before that
    /// nested commit. The transaction flag is only in this process:
    /// a new `open` does not resume it. `invalid` if none is open, the
    /// message is empty, or there is nothing to commit.
    ///
    /// Holds the write lock through `auto_sync` push, then releases it
    /// before returning. A later `open` on this path can write
    /// immediately, including from a second handle that is still
    /// concurrent with this one.
    ///
    /// The commit does not write the lexical or vector Index. The
    /// background indexer applies it afterwards; `wait_indexed` returns
    /// once it has.
    pub fn commit(&self, message: &str) -> Result<(), Error> {
        self.require_write_lock("commit")?;
        self.conclude_write((|| {
            if !flag(&self.in_txn)? {
                bail!(Error::invalid("commit requires an open transaction"));
            }
            self.commit_recorded(message)?;
            set_in_txn(&self.in_txn, false)?;
            self.push_if_auto_sync()
        })())
    }

    /// Start a transaction on this handle. Later writes on this handle do
    /// not autocommit until `commit` or `discard`. Dropping the handle ends
    /// the transaction without a git commit; recorded paths stay in
    /// `{vault}/.scheda/tx.log` until `begin`, `discard`, or a mutating
    /// call outside a transaction restores them.
    ///
    /// Takes the write lock and holds it until `commit`, `discard`, or drop.
    /// Writes inside the transaction only change files; the lexical and
    /// vector Index see them after `commit` creates the git commit. Discards leftover
    /// Scheda writes first.
    /// `invalid` if a transaction is already open on this handle.
    /// A mutating call, `commit`, or `discard` from another thread while a
    /// transaction is open is `locked` (retryable); it does not join this
    /// transaction.
    pub fn begin(&self) -> Result<(), Error> {
        self.require_write_lock("begin")?;
        self.conclude_write((|| {
            if flag(&self.in_txn)? {
                bail!(Error::invalid("already in a transaction"));
            }
            self.restore_recorded()?;
            set_in_txn(&self.in_txn, true)?;
            Ok(())
        })())
    }

    /// Restore Scheda-recorded paths to HEAD in the repository that owns
    /// each path, end the transaction if any on this handle, and reload
    /// Schema. The lexical and vector Index are not rebuilt: they only ever
    /// reflect commits, and the discarded writes were never committed. Allowed without an open
    /// transaction so a later process can clear crash leftovers.
    ///
    /// Releases the write lock before returning.
    pub fn discard(&self) -> Result<(), Error> {
        self.require_write_lock("discard")?;
        self.conclude_write((|| {
            self.restore_recorded()?;
            set_in_txn(&self.in_txn, false)?;
            Ok(())
        })())
    }

    /// Run `f` as one transaction on this thread: [`begin`](Self::begin),
    /// `f(self)`, then one git commit per repository the writes changed,
    /// with `message` as the subject. Every write `f` makes through this
    /// `Vault`, or through a collection from it, joins the transaction.
    ///
    /// When `f` succeeds and its writes leave both repositories as at
    /// `HEAD` (it wrote nothing, or only bytes `HEAD` already holds), no
    /// commit is made and `f`'s value is returned: a batch replayed after a
    /// crash is `Ok`. [`commit`](Self::commit) on its own stays strict.
    ///
    /// `f` returns the caller's own error type. When it returns an error,
    /// the writes are discarded and that error is returned as is. Scheda's
    /// own failures (`begin`, the commit, `locked`, an empty `message`,
    /// which fails before `f` runs) reach the caller through
    /// `E::from`; a failed commit discards the writes first. When `f`
    /// panics, the writes are discarded and the panic resumes.
    ///
    /// While this `Vault` holds a transaction on another thread, the result
    /// is `E::from(locked)` at once and `f` does not run; on this thread,
    /// `invalid` (a transaction is already open).
    pub fn transaction<T, E, F>(&self, message: &str, f: F) -> Result<T, E>
    where
        F: FnOnce(&Self) -> Result<T, E>,
        E: From<Error>,
    {
        const FRAME: &str = "[Vault::transaction]";
        let scheda = |err: Error| E::from(err.context(FRAME));
        require_commit_message(Some(message)).map_err(scheda)?;
        self.begin().map_err(scheda)?;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match outcome {
            Ok(Ok(value)) => match self.end_transaction(message) {
                Ok(()) => Ok(value),
                Err(err) => {
                    // The error the caller sees is the one that ended the
                    // transaction; a failed discard leaves leftovers that
                    // the next `begin` or mutating call restores.
                    let _ = self.discard();
                    Err(scheda(err))
                }
            },
            Ok(Err(err)) => {
                let _ = self.discard();
                Err(err)
            }
            Err(panic) => {
                let _ = self.discard();
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Close the transaction `transaction` opened: commit what changed,
    /// nothing when nothing did, then release the write lock.
    fn end_transaction(&self, message: &str) -> Result<(), Error> {
        self.require_write_lock("transaction")?;
        self.conclude_write((|| {
            if !flag(&self.in_txn)? {
                bail!(Error::invalid(
                    "the transaction was ended inside its closure"
                ));
            }
            let changed =
                commit_written_if_changed(&self.path, &self.written, message, &self.identity)?;
            set_in_txn(&self.in_txn, false)?;
            if !changed {
                return Ok(());
            }
            self.wake_indexer();
            self.push_if_auto_sync()
        })())
    }

    fn commit_recorded(&self, message: &str) -> Result<(), Error> {
        commit_written(&self.path, &self.written, message, &self.identity)?;
        self.wake_indexer();
        Ok(())
    }

    fn restore_recorded(&self) -> Result<(), Error> {
        let decls = self.decls_arc()?;
        let restored = restore_written(
            &self.path,
            &self.written,
            &decls,
            &self.generation_dir()?,
            &self.journal_ids,
        )?;
        if let Some(loaded) = restored {
            self.declared.replace(loaded)?;
        }
        Ok(())
    }

    /// Reload collection declarations after git moved the work tree, and
    /// wake the indexer: it brings the Index, journal `id` maps included,
    /// up to the new `HEAD`.
    fn reload_after_restore(&self) -> Result<(), Error> {
        self.wake_indexer();
        self.declared.reload()?;
        Ok(())
    }

    fn drain_abandoned(&self) -> Result<(), Error> {
        if flag(&self.in_txn)? {
            return Ok(());
        }
        self.restore_recorded()
    }

    fn finish_mutation(&self, message: Option<&str>) -> Result<(), Error> {
        if flag(&self.in_txn)? {
            return Ok(());
        }
        match self.commit_recorded(autocommit_subject(message)) {
            Ok(()) => self.push_if_auto_sync(),
            Err(err) => {
                let _ = self.restore_recorded();
                Err(err)
            }
        }
    }

    fn push_if_auto_sync(&self) -> Result<(), Error> {
        if self.auto_sync && !self.remotes.is_empty() {
            git::push(&self.path)?;
        }
        Ok(())
    }

    fn note_write(&self, rel: String) -> Result<(), Error> {
        let mut set = crate::error::mutex(&self.written, "[Vault] lock written")?;
        set.insert(rel);
        persist_written(&self.path, &set)
    }

    /// `git push` to `origin`. Takes the write lock for this call. Outside
    /// a transaction the lock is released before return.
    pub fn push(&self) -> Result<(), Error> {
        self.require_write_lock("push")?;
        self.conclude_write(git::push(&self.path))
    }

    /// `git pull` from `origin`, then reload Schema. The background
    /// indexer brings the Index up to the new `HEAD`. Takes the write lock
    /// for this call. Outside a transaction the lock
    /// is released before return.
    pub fn pull(&self) -> Result<(), Error> {
        self.require_write_lock("pull")?;
        self.conclude_write((|| {
            git::pull(&self.path)?;
            self.reload_after_restore()
        })())
    }

    /// `pull` then `push`. Takes the write lock for this call. Outside a
    /// transaction the lock is released before return.
    pub fn sync(&self) -> Result<(), Error> {
        self.require_write_lock("sync")?;
        self.conclude_write((|| {
            git::pull(&self.path)?;
            self.reload_after_restore()?;
            git::push(&self.path)
        })())
    }

    /// `git push` of `{vault}/.isolated/` to its own `origin`.
    ///
    /// Vault `push` never reaches this nested repository. Missing nested
    /// work tree → `invalid`. No Remote → `invalid`. Takes the write lock
    /// for this call.
    pub fn push_isolated(&self) -> Result<(), Error> {
        self.require_write_lock("push_isolated")?;
        self.conclude_write(git::push(&self.isolated_work_tree()?))
    }

    /// `git pull` of `{vault}/.isolated/` from its own `origin`, then reload
    /// Schema. The background indexer brings the Index up to the new
    /// `HEAD`. Takes the write lock for this call; without the `find`
    /// feature `reindex` holds it too, so this call is `locked` while that
    /// runs. Missing nested work tree → `invalid`.
    pub fn pull_isolated(&self) -> Result<(), Error> {
        self.require_write_lock("pull_isolated")?;
        self.conclude_write((|| {
            git::pull(&self.isolated_work_tree()?)?;
            self.reload_after_restore()
        })())
    }

    /// Isolated `pull` then `push`. Takes the write lock for this call.
    pub fn sync_isolated(&self) -> Result<(), Error> {
        self.require_write_lock("sync_isolated")?;
        self.conclude_write((|| {
            let nested = self.isolated_work_tree()?;
            git::pull(&nested)?;
            self.reload_after_restore()?;
            git::push(&nested)
        })())
    }

    fn isolated_work_tree(&self) -> Result<PathBuf, Error> {
        let root = isolated::root(&self.path);
        if !git::is_work_tree(&root) {
            bail!(Error::invalid("isolated nested git is not a work tree"));
        }
        Ok(root)
    }

    fn require_write_lock(&self, op: &str) -> Result<(), Error> {
        let mut slot = crate::error::mutex(&self.write_lock, "[Vault] lock write lock")
            .with_context(|| format!("[Vault::{op}] take write lock"))?;
        take_write_slot(&mut slot, &self.path, &self.written, "Vault", op)
    }

    fn conclude_write<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        release_lock_unless_in_transaction(&self.write_lock, &self.in_txn, result)
    }

    /// Write the marker and patch `.gitignore` on an existing Vault, in one
    /// commit. Takes `write.lock` for that write, then `unlock`s so `open`
    /// does not keep the writer lock.
    fn write_service_files_under_lock(&self, marker: bool, gitignore: bool) -> Result<(), Error> {
        {
            let mut slot = crate::error::mutex(&self.write_lock, "[Vault::open] lock write lock")?;
            if !slot.is_held() {
                take_write_slot(&mut slot, &self.path, &self.written, "Vault", "open")?;
            }
        }
        let result = (|| {
            let mut noted = false;
            if marker {
                marker::ensure(&self.path)?;
                self.note_write(String::from(marker::FILE))?;
                noted = true;
            }
            if gitignore && ensure_gitignore(&self.path)? {
                self.note_write(String::from(".gitignore"))?;
                noted = true;
            }
            if noted {
                self.finish_mutation(None)?;
            }
            Ok(())
        })();
        {
            let mut slot =
                crate::error::mutex(&self.write_lock, "[Vault::open] release write lock")?;
            if let Some(file) = slot.file.take() {
                slot.owner = None;
                if let Err(err) = file.unlock()
                    && result.is_ok()
                {
                    return Err(err).context("[Vault::open] unlock write lock");
                }
            }
        }
        result
    }

    /// Validate Schema, encodings, journal lines, `id` uniqueness, and conflict
    /// markers. Isolated Record files are scanned when `{vault}/.isolated/`
    /// exists. Parent git must not track `.isolated/`. A nested Remote of its
    /// own is not a defect. On Unix, `{vault}/.scheda/` must be owner-only
    /// (`0700` directories, `0600` files); a wider mode is reported with that
    /// path. A journal `id` map line whose path is outside that collection is
    /// `index_corrupt`. Does not write Records. An
    /// encrypted collection whose secret is absent is not checked
    /// (`crypto`); a build without `encrypt` reports `unsupported_encryption`.
    /// With the secret, encrypted collections are validated like any other.
    pub fn check(&self) -> Result<(), Error> {
        // Let this handle's indexer finish its batch and make its files
        // owner-only first, so a file Tantivy just created is not read as
        // a wide mode. An indexer in another process can still be writing.
        #[cfg(feature = "find")]
        let _paused = self.indexer.as_ref().map(|indexer| indexer.pause());
        isolated::check_perimeter(&self.path)?;
        store::check_scheda_owner_only(&self.path)?;
        let decls = schema::load(&self.path)?;
        index::check_journal_id_maps(&self.generation_dir()?, &decls)?;
        store::check_documents(&self.path, &decls, self.encryption.as_ref())?;
        journal::check_journals(&self.path, &decls)
    }

    /// Proto snapshot (`api.v1.VaultInfo`): vault attrs plus the collection list.
    ///
    /// The list is the declarations this handle holds. They are read again
    /// from the work tree when a name lookup misses and, with `find`,
    /// whenever the Index passes a commit (and before `wait_indexed`
    /// returns), so a declaration another handle or process committed shows
    /// here without a reopen. This handle's own uncommitted declaration
    /// stays.
    /// `remote` is `origin` when that remote exists, otherwise empty, whatever
    /// other remotes are configured.
    pub fn info(&self) -> Result<VaultInfo, Error> {
        let remote = self
            .remotes
            .iter()
            .find(|name| name.as_str() == "origin")
            .cloned()
            .unwrap_or_default();
        let info = schema::to_vault_info(
            &self.path,
            &self.index.root,
            &remote,
            self.auto_sync,
            &*self.decls_arc()?,
        )?;
        Ok(info)
    }

    /// Write one collection Schema (`api.collection.v1.Schema`) as
    /// `{name}/.collection.yaml`. Reloads declarations on this handle.
    /// `isolated: true` stores Records under
    /// `{vault}/.isolated/` and creates the nested git if needed. Setting or
    /// clearing `encrypted` on a collection that already holds Record files is
    /// `invalid`; existing files are left untouched. A build without the
    /// `encrypt` crate feature refuses a Schema that marks `encrypted`
    /// (`unsupported_encryption`) and writes nothing.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject. Takes the write lock for this call; outside a transaction
    /// the lock is released before return.
    pub fn create_collection(
        &self,
        name: &str,
        schema: Schema,
        message: Option<&str>,
    ) -> Result<Collection, Error> {
        schema.require_valid()?;
        #[cfg(not(feature = "encrypt"))]
        if schema.encrypted == Some(true) {
            bail!(
                Error::unsupported_encryption("this build cannot create an encrypted collection")
                    .context("[Vault::create_collection] encryption")
            );
        }
        let message = require_commit_message(message)?;
        self.require_write_lock("create_collection")?;
        self.conclude_write((|| {
            self.drain_abandoned()?;
            let result = (|| {
                if schema.isolated == Some(true)
                    && let Some(rel) = isolated::ensure(&self.path)?
                {
                    self.note_write(rel)?;
                }
                let rel = schema::put_collection(&self.path, name, &schema)?;
                self.note_write(rel)?;
                let loaded = schema::load(&self.path)?;
                let dir = self.generation_dir()?;
                *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? =
                    index::load_journal_ids(&dir, &loaded)?;
                self.declared.replace(loaded)?;
                self.finish_mutation(message)?;
                Ok(())
            })();
            // Autocommit only: a later step can fail after the declaration file is
            // recorded. Inside a transaction the written set also holds earlier
            // Record paths; restoring them here would drop work that `commit`
            // or `discard` still owns.
            if result.is_err() && flag(&self.in_txn).ok() == Some(false) {
                let _ = self.restore_recorded();
            }
            result
        })())?;
        self.collection(name)
    }

    /// Rebuild the Index into a new generation directory, then point
    /// `{vault}/.scheda/index/current` at it with one atomic write.
    ///
    /// With the `find` feature the new generation is filled from the
    /// commits at `HEAD` (of the Vault and of `{vault}/.isolated/`), the
    /// same way the background indexer rebuilds an Index it cannot use:
    /// git objects only, so a write no commit holds yet is not read. Its
    /// marks name those commits, and the indexer applies whatever reached
    /// `HEAD` during the build once the generation is live. No `write.lock`
    /// is taken: `add` / `replace` / `remove` and transactions keep going
    /// in other handles throughout.
    ///
    /// Vectors: with this handle's embedder, every slot of `HEAD` is filled
    /// with it, copying unchanged slots from a previous dump of the same
    /// `model_id` and dimensions; another model starts the vector table
    /// empty and re-embeds every slot, which is how the Index moves to a
    /// new model. Without an embedder the previous vectors are kept as they
    /// are.
    ///
    /// Without `find` only the journal `id` maps are rebuilt, from the
    /// files, holding `write.lock` for the whole call.
    ///
    /// The journal `id` map of every non-encrypted `journal` is rebuilt,
    /// isolated included; encrypted collections are skipped. Removes a
    /// leftover `{vault}/.isolated/.scheda/` directory. Holds
    /// `{vault}/.scheda/reindex.lock` for the whole call: a second
    /// `reindex` is `locked`. A reader that opened before the swap keeps
    /// its generation until its indexer follows `current`.
    pub fn reindex(&self) -> Result<ReindexResult, Error> {
        #[cfg(feature = "find")]
        {
            self.reindex_from_commits()
        }
        #[cfg(not(feature = "find"))]
        {
            self.reindex_files()
        }
    }

    #[cfg(feature = "find")]
    fn reindex_from_commits(&self) -> Result<ReindexResult, Error> {
        let _reindex_lock = crate::reindex::acquire_lock(&self.path)?;
        let heads = crate::indexer::Heads::read(&self.path)?;
        let previous = self.generation_dir()?;
        let embedder = self.embedder_arc()?;
        let hook = self.walk_hook();
        let never = || false;
        let spec = crate::indexer::BuildSpec {
            vault: &self.path,
            root: &self.index.root,
            previous: &previous,
            heads: heads.as_ref(),
            projector: self.projector.clone(),
            embedder: embedder.as_deref(),
            stop: &never,
            hook: hook.as_deref().map(|hook| hook as &dyn Fn(&str)),
        };
        let crate::indexer::Built {
            dir,
            engine,
            mut maps,
            counts,
        } = crate::indexer::build_generation(&spec)?.ok_or_else(|| {
            Error::new(Code::IndexLexical, "the generation build stopped")
                .context("[Vault::reindex] build")
        })?;
        index::remove_legacy_isolated_index(&self.path)?;
        // No round of this handle's indexer may keep writing the previous
        // generation's diff into the engine this swaps.
        let _paused = self.indexer.as_ref().map(|indexer| indexer.pause());
        // Journal lines any handle appended to the live generation after
        // `heads` was read: copied before the swap, so `get` finds them at
        // once, and again after it for the ones that landed in between. A
        // later append to the retired generation is forwarded by its writer.
        let retired = index::current_dir(&self.index.root)?.filter(|live| live.is_dir());
        if let Some(retired) = &retired {
            index::carry_retired(retired, &dir, &mut maps)?;
        }
        index::publish_generation(&self.index.root, &dir)?;
        if let Some(retired) = &retired {
            index::carry_retired(retired, &dir, &mut maps)?;
        }
        crate::indexer::install(
            &self.index,
            &self.search,
            &self.journal_ids,
            dir,
            engine,
            maps,
        )?;
        Ok(counts)
    }

    #[cfg(not(feature = "find"))]
    fn reindex_files(&self) -> Result<ReindexResult, Error> {
        let in_txn = flag(&self.in_txn)?;
        self.require_write_lock("reindex")?;
        let result = (|| {
            let _reindex_lock = crate::reindex::acquire_lock(&self.path)?;
            let decls = schema::load(&self.path)?;
            let base = self.reindex_base()?;
            let new_dir = index::start_generation(&self.index.root, &self.generation_dir()?)?;
            let (counts, maps) = index::rebuild(&self.path, &decls, &new_dir)?;
            self.stamp_marks(&new_dir, base.as_deref())?;
            index::publish_generation(&self.index.root, &new_dir)?;
            self.index.set_dir(new_dir)?;
            *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? = maps;
            Ok(counts)
        })();
        if in_txn {
            result
        } else {
            self.conclude_write(result)
        }
    }

    #[cfg(feature = "find")]
    fn walk_hook(&self) -> Option<ReindexWalkHook> {
        #[cfg(test)]
        {
            crate::error::mutex(&self.walk_hook, "[Vault] walk hook")
                .ok()
                .and_then(|slot| slot.clone())
        }
        #[cfg(not(test))]
        {
            None
        }
    }

    #[cfg(all(test, feature = "find"))]
    pub(crate) fn set_reindex_walk_hook(
        &self,
        hook: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<(), Error> {
        *crate::error::mutex(&self.walk_hook, "[Vault] walk hook")? = Some(hook);
        Ok(())
    }

    /// `HEAD` of the Vault and of the nested repository, read under
    /// `write.lock`. `None` while `tx.log` names an uncommitted Scheda
    /// write: the files then hold bytes no commit has.
    #[cfg(not(feature = "find"))]
    fn reindex_base(&self) -> Result<Option<Vec<(marks::Repo, String)>>, Error> {
        if !load_written(&self.path)?.is_empty() {
            return Ok(None);
        }
        let mut base = Vec::new();
        if let Some(head) = git::peel_head_oid(&self.path)? {
            base.push((marks::Repo::Main, head));
        }
        let nested = isolated::root(&self.path);
        if git::is_work_tree(&nested)
            && let Some(head) = git::peel_head_oid(&nested)?
        {
            base.push((marks::Repo::Isolated, head));
        }
        Ok(Some(base))
    }

    /// Record the commit the journal `id` maps of a generation rebuilt from
    /// the files reflect, so a build with `find` that opens it later does
    /// not apply those lines again. No base writes no mark.
    #[cfg(not(feature = "find"))]
    fn stamp_marks(
        &self,
        new_dir: &Path,
        base: Option<&[(marks::Repo, String)]>,
    ) -> Result<(), Error> {
        for (repo, head) in base.unwrap_or_default() {
            marks::write(new_dir, marks::Leg::Ids, *repo, head)?;
        }
        Ok(())
    }

    /// Fill a new generation without swapping `current`. Test seam for a
    /// crash between rebuild and the pointer write.
    #[cfg(all(test, feature = "find"))]
    pub(crate) fn reindex_without_publish(&self) -> Result<PathBuf, Error> {
        let heads = crate::indexer::Heads::read(&self.path)?;
        let previous = self.generation_dir()?;
        let embedder = self.embedder_arc()?;
        let never = || false;
        let spec = crate::indexer::BuildSpec {
            vault: &self.path,
            root: &self.index.root,
            previous: &previous,
            heads: heads.as_ref(),
            projector: self.projector.clone(),
            embedder: embedder.as_deref(),
            stop: &never,
            hook: None,
        };
        let built = crate::indexer::build_generation(&spec)?
            .ok_or_else(|| Error::new(Code::IndexLexical, "the generation build stopped"))?;
        Ok(built.dir)
    }
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("index_dir", &self.index.root)
            .field("has_projector", &self.projector.is_some())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collection")
            .field("name", &self.decl.name)
            .field("has_projector", &self.projector.is_some())
            .finish_non_exhaustive()
    }
}

/// Releases `{vault}/.scheda/write.lock` when this is the last handle that
/// shares it, before the rest of the handle is torn down. An open
/// transaction ends without a git commit. The unlock is explicit: a process
/// that forks while the descriptor is open would otherwise keep the lock a
/// moment after the handle is gone.
impl Drop for Vault {
    fn drop(&mut self) {
        if Arc::strong_count(&self.write_lock) > 1 {
            return;
        }
        release_write_lock_on_drop(&self.write_lock);
    }
}

/// Releases `{vault}/.scheda/write.lock` when this is the last handle that
/// shares it, before the rest of the handle is torn down.
impl Drop for Collection {
    fn drop(&mut self) {
        if Arc::strong_count(&self.write_lock) > 1 {
            return;
        }
        release_write_lock_on_drop(&self.write_lock);
    }
}

impl Collection {
    #[cfg(test)]
    fn stub() -> Self {
        Self {
            vault: PathBuf::new(),
            index: Arc::new(index::IndexHandle::empty()),
            write_lock: Arc::new(Mutex::new(WriteLock::new())),
            auto_sync: false,
            remotes: Vec::new(),
            decl: CollectionDecl {
                name: String::from("c"),
                kind: Kind::Document,
                version: 1,
                segment_max_bytes: None,
                segment_max_lines: None,
                segment_rotate: None,
                singleton_id: None,
                isolated: false,
                encrypted: false,
                exclude_lexical_index: false,
                exclude_vector_index: false,
            },
            decls: Arc::new(Decls::default()),
            written: Arc::new(Mutex::new(BTreeSet::new())),
            in_txn: Arc::new(Mutex::new(false)),
            writer: Arc::from("stub"),
            journal_ids: Arc::new(Mutex::new(JournalIdMaps::default())),
            #[cfg(feature = "find")]
            search: Arc::new(SearchEngine::empty()),
            #[cfg(feature = "find")]
            indexer: None,
            #[cfg(feature = "search")]
            embedder: None,
            projector: None,
            identity: Arc::new(git::CommitIdentity::fallback()),
            encryption: None,
        }
    }

    /// Collection directory name (same as declaration `name`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.decl.name
    }

    /// This collection's Schema (`api.collection.v1.Schema`). Not a Vault listing (`info`).
    pub fn schema(&self) -> Result<Schema, Error> {
        schema::to_proto_schema(&self.decl)
    }

    /// Write mode: `document`, `journal`, or `singleton`.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.decl.kind
    }

    /// Collection generation: declaration shape and on-disk Record layout.
    ///
    /// Required on the collection file. Only generation `1` is implemented.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.decl.version
    }

    /// Journal segment size cap in bytes. `None` on `document` / `singleton`.
    ///
    /// Rust helper for Schema inspection. The collection catalog is
    /// [`schema`](Self::schema).
    #[must_use]
    pub fn segment_max_bytes(&self) -> Option<u64> {
        self.decl.segment_max_bytes
    }

    /// Fixed `id` of a `singleton` collection. `None` otherwise.
    ///
    /// Rust helper for Schema inspection. The collection catalog is
    /// [`schema`](Self::schema).
    #[must_use]
    pub fn singleton_id(&self) -> Option<&str> {
        self.decl.singleton_id.as_deref()
    }

    fn generation_dir(&self) -> Result<PathBuf, Error> {
        self.index.dir()
    }

    /// Whether `id` names a Record file of this collection. The write path
    /// asks the file, never the Index: the Index lags the last commit.
    fn record_exists(&self, id: &str) -> Result<bool, Error> {
        Ok(store::locate_file(&self.vault, &self.decl, id)?.is_some())
    }

    fn require_write(&self, op: &str) -> Result<(), Error> {
        let mut slot = crate::error::mutex(&self.write_lock, "[Collection] lock write lock")
            .with_context(|| format!("[Collection::{op}] take write lock"))?;
        take_write_slot(&mut slot, &self.vault, &self.written, "Collection", op)
    }

    fn conclude_write<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        release_lock_unless_in_transaction(&self.write_lock, &self.in_txn, result)
    }

    fn note_write(&self, rel: String) -> Result<(), Error> {
        let mut set = crate::error::mutex(&self.written, "[Collection] lock written")?;
        set.insert(rel);
        persist_written(&self.vault, &set)
    }

    fn drain_abandoned(&self) -> Result<(), Error> {
        if flag(&self.in_txn)? {
            return Ok(());
        }
        self.drain_recorded()
    }

    fn prepare_isolated_write(&self) -> Result<(), Error> {
        if !self.decl.isolated {
            return Ok(());
        }
        if let Some(rel) = isolated::ensure(&self.vault)? {
            self.note_write(rel)?;
        }
        Ok(())
    }

    fn finish_mutation(&self, message: Option<&str>) -> Result<(), Error> {
        if flag(&self.in_txn)? {
            return Ok(());
        }
        match commit_written(
            &self.vault,
            &self.written,
            autocommit_subject(message),
            &self.identity,
        ) {
            Ok(()) => {
                #[cfg(feature = "find")]
                if let Some(indexer) = &self.indexer {
                    indexer.wake();
                }
                if self.auto_sync && !self.remotes.is_empty() {
                    git::push(&self.vault)?;
                }
                Ok(())
            }
            Err(err) => {
                // A failed restore leaves the paths in `tx.log` for the next
                // mutating call; the commit failure is what the caller sees.
                let _ = self.drain_recorded();
                Err(err)
            }
        }
    }

    /// Restore what this handle recorded, whether or not a transaction is open.
    fn drain_recorded(&self) -> Result<(), Error> {
        restore_written(
            &self.vault,
            &self.written,
            &self.decls,
            &self.generation_dir()?,
            &self.journal_ids,
        )?;
        Ok(())
    }

    fn require_encryption_secret(&self) -> Result<(), Error> {
        if !self.decl.encrypted {
            return Ok(());
        }
        #[cfg(not(feature = "encrypt"))]
        {
            bail!(
                Error::unsupported_encryption(format!(
                    "collection {} is encrypted; this build cannot open it",
                    self.decl.name
                ))
                .context("[Collection] encryption")
            );
        }
        #[cfg(feature = "encrypt")]
        match self.encryption.as_ref() {
            Some(_) => Ok(()),
            None => bail!(
                Error::crypto(format!(
                    "encrypted collection {} requires a secret on the handle",
                    self.decl.name
                ))
                .context("[Collection] encryption")
            ),
        }
    }

    /// Create a Record. Same `id` already present → `duplicate_id`.
    ///
    /// For `document`, `id` may be `/`-separated slugs (`work/meeting.toml` is
    /// `{name}/work/meeting.toml`). The library does not append `.md`. `journal`
    /// and `singleton` remain one slug. A `journal` `add` appends
    /// `{id}: {payload}\n`; `get` / `lines` `raw` is the payload. A `singleton`
    /// file is `{name}/{declared id}` (an extension, if any, is part of that
    /// id); contents are the caller bytes and are not parsed as YAML.
    ///
    /// Encrypted collections write age armor after the UTF-8 plaintext
    /// is built. A missing secret is `crypto`. The temp file never holds
    /// plaintext. The write commits to git and returns; the background
    /// indexer applies the commit later. `exclude_lexical_index` keeps the
    /// collection out of the lexical leg of that work and
    /// `exclude_vector_index` out of the vector leg (the embedder is not
    /// called for it).
    ///
    /// `raw` is the UTF-8 file / payload bytes. Empty is allowed. Non-UTF-8
    /// is refused and writes no file.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject. Outside a transaction the write lock is held only for this
    /// call.
    pub fn add(&self, id: &str, raw: impl AsRef<[u8]>, message: Option<&str>) -> Result<(), Error> {
        require_caller_id(self.decl.kind, id)?;
        self.require_encryption_secret()?;
        let message = require_commit_message(message)?;
        let record = record_from_raw(id, &self.decl.name, raw.as_ref());
        self.require_write("add")?;
        self.conclude_write((|| {
            self.drain_abandoned()?;
            self.prepare_isolated_write()?;
            match self.decl.kind {
                Kind::Journal => {
                    let mut maps = crate::error::mutex(
                        &self.journal_ids,
                        "[Collection::add] lock journal ids",
                    )?;
                    let dir = self.generation_dir()?;
                    let (_id, rel) = journal::insert_journal(
                        &self.vault,
                        &self.decl,
                        &record,
                        &dir,
                        &mut maps,
                        &self.writer,
                    )?;
                    drop(maps);
                    self.note_write(rel.clone())?;
                    self.finish_mutation(message)?;
                    Ok(())
                }
                Kind::Document | Kind::Singleton => {
                    let (_id, rel) = store::insert_document(
                        &self.vault,
                        &self.decl,
                        &record,
                        self.encryption.as_ref(),
                    )?;
                    self.note_write(rel.clone())?;
                    self.finish_mutation(message)?;
                    Ok(())
                }
            }
        })())
    }

    /// Read one Record by `id`. Missing → `not_found`.
    ///
    /// Encrypted collections decrypt the file. A missing or wrong secret,
    /// or bytes that will not decrypt, is `crypto`. `raw` is the file
    /// bytes (decrypted plaintext when encrypted). The library does not
    /// parse document contents.
    pub fn get(&self, id: &str) -> Result<Record, Error> {
        require_caller_id(self.decl.kind, id)?;
        self.require_encryption_secret()?;
        match self.decl.kind {
            Kind::Journal => {
                let mut maps =
                    crate::error::mutex(&self.journal_ids, "[Collection::get] lock journal ids")?;
                index::refresh_ids(&self.generation_dir()?, &mut maps, &self.decl)?;
                journal::find_journal(&self.vault, &self.decl, id, &maps)
            }
            Kind::Document | Kind::Singleton => {
                store::find_document(&self.vault, &self.decl, id, self.encryption.as_ref())
            }
        }
    }

    /// Walk this `journal` in append order without opening the Index.
    ///
    /// Closed segments in declaration sequence, each file from first line to last,
    /// then the open segment. Rotation does not reorder earlier lines. Each
    /// item is one Record; `raw` is the envelope payload after `: `, not the
    /// line bytes and not including the trailing newline. `document` /
    /// `singleton` → `unsupported`. A missing Index
    /// directory does not stop the walk.
    pub fn lines(&self) -> Result<crate::JournalLines, Error> {
        journal::journal_lines(&self.vault, &self.decl)
    }

    /// Walk this `document` or `singleton` in `id` order without opening the Index.
    ///
    /// Files the library names, in the byte order of the full `id` (the
    /// relative POSIX path under the collection, including any extension;
    /// `/` in a `document` `id` is a directory). A `singleton` yields its
    /// one Record, or nothing before the first write.
    /// Each item is what `get` returns for that `id`, `raw` included.
    /// Names that are not Record files (dotfiles, not a regular file,
    /// symlinks) are skipped. Directories whose names are slugs are descended;
    /// other subdirectories are skipped. `journal` → `unsupported`. A
    /// missing Index directory does not stop the walk. Encrypted collections
    /// decrypt each file. A file that will not decrypt is `crypto`.
    pub fn documents(&self) -> Result<crate::CollectionDocuments, Error> {
        self.require_encryption_secret()?;
        store::collection_documents(&self.vault, &self.decl, self.encryption.as_ref())
    }

    /// Ids of this `document` or `singleton` in the same order as `documents`.
    ///
    /// Derived from the file names; no Record file is opened. `journal` →
    /// `unsupported`. A `singleton` before the first write is empty; after,
    /// one id equal to the declared `singleton_id`. A missing collection
    /// directory is an empty list. Encrypted collections do not need the
    /// handle secret.
    pub fn ids(&self) -> Result<Vec<String>, Error> {
        store::collection_ids(&self.vault, &self.decl)
    }

    /// Replace a whole `document` or `singleton`. `journal` → `unsupported`.
    ///
    /// The write commits to git and returns; the background indexer applies
    /// the commit later, leaving the collection out of the lexical or vector
    /// leg per `exclude_lexical_index` / `exclude_vector_index`.
    ///
    /// `raw` is the UTF-8 file bytes. Empty is allowed. Non-UTF-8 is
    /// refused and writes no file.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject. Outside a transaction the write lock is held only for this
    /// call.
    pub fn replace(
        &self,
        id: &str,
        raw: impl AsRef<[u8]>,
        message: Option<&str>,
    ) -> Result<(), Error> {
        require_caller_id(self.decl.kind, id)?;
        self.require_encryption_secret()?;
        let message = require_commit_message(message)?;
        if self.decl.kind == Kind::Journal {
            bail!(Error::unsupported("replace is not offered for journal"));
        }
        let record = record_from_raw(id, &self.decl.name, raw.as_ref());
        self.require_write("replace")?;
        self.conclude_write((|| {
            self.drain_abandoned()?;
            let known = self.record_exists(id)?;
            if !known {
                bail!(Error::not_found(format!(
                    "record {id} is not in {}",
                    self.decl.name
                )));
            }
            self.prepare_isolated_write()?;
            let (_id, rel) = store::replace_document(
                &self.vault,
                &self.decl,
                id,
                &record,
                self.encryption.as_ref(),
            )?;
            self.note_write(rel.clone())?;
            self.finish_mutation(message)?;
            Ok(())
        })())
    }

    /// Delete a `document` or `singleton` Record. `journal` → `unsupported`.
    ///
    /// Unknown `id` → `not_found`. An illegal slug → `invalid` before any
    /// file is touched. The file is unlinked as a whole and staged like
    /// `add` / `replace`: autocommit outside a transaction, part of the
    /// batch inside one. Isolated collections use the nested git. The
    /// Record leaves the Index when the background indexer applies that
    /// commit. An `id` is looked up as a file, not in the Index.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject. Outside a transaction the write lock is held only for this
    /// call.
    pub fn remove(&self, id: &str, message: Option<&str>) -> Result<(), Error> {
        require_caller_id(self.decl.kind, id)?;
        self.require_encryption_secret()?;
        let message = require_commit_message(message)?;
        if self.decl.kind == Kind::Journal {
            bail!(Error::unsupported("remove is not offered for journal"));
        }
        self.require_write("remove")?;
        self.conclude_write((|| {
            self.drain_abandoned()?;
            let known = self.record_exists(id)?;
            if !known {
                bail!(Error::not_found(format!(
                    "record {id} is not in {}",
                    self.decl.name
                )));
            }
            self.prepare_isolated_write()?;
            let rel = store::record_rel(&self.decl, id)?;
            if !self.tracked_or_noted(&rel)? {
                // A file git never saw: deleting it is the whole change, and
                // there is nothing to commit.
                store::remove_document(&self.vault, &self.decl, id)?;
                return Ok(());
            }
            self.note_write(rel.clone())?;
            store::remove_document(&self.vault, &self.decl, id)?;
            self.finish_mutation(message)?;
            Ok(())
        })())
    }

    /// Run a Tantivy Query on this collection's Index, then open matching files.
    ///
    /// Isolated collections are in the Vault Index. An encrypted collection
    /// or one marked `exclude_lexical_index` is `unsupported`: Tantivy does
    /// not hold its Records.
    ///
    /// Page order is score descending. Equal scores break by the Index `ord`
    /// fast field, ascending (earlier indexed first). `ord` is unique within
    /// a collection; two Records in different collections that share both
    /// score and `ord` stay unordered relative to each other. One `journal`
    /// collection uses append order instead.
    ///
    /// Filter field names are [`Projector`] find-columns plus identity `id`.
    /// Unset proto `collection` on a projector leaf means this collection.
    /// A qualifier for another collection is `invalid`. Without a projector,
    /// only `all` and Filter on `id` are valid.
    #[cfg(feature = "find")]
    pub fn find(&self, query: &FindQuery, offset: u32, count: u32) -> Result<FindResult, Error> {
        if self.decl.encrypted {
            bail!(Error::unsupported(format!(
                "encrypted collection {} cannot be found",
                self.decl.name
            )));
        }
        if self.decl.exclude_lexical_index {
            bail!(Error::unsupported(format!(
                "collection {} cannot be found: exclude_lexical_index",
                self.decl.name
            )));
        }
        let name = self.decl.name.as_str();
        QueryCtx {
            engine: &self.search,
            decls: &self.decls,
            vault: &self.vault,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
            scope: crate::query::FilterScope::Collection(name),
        }
        .find(&[name], query, offset, count)
    }

    /// Hybrid retrieval on this collection: BM25 and ANN when both are on,
    /// then fusion, then an optional reranker.
    ///
    /// Isolated collections are in the Vault Index. A collection with only
    /// one Index leg runs that leg; hits set `lexical_score` and/or
    /// `vector_score` for the legs that ran. An encrypted collection or one
    /// with both legs excluded is `unsupported`.
    ///
    /// Weights, pool membership, and `SearchQuery.rerank_limit` are the same
    /// as on [`Vault::search`]: without a reranker, weighted Reciprocal Rank
    /// Fusion orders and truncates the page; with a reranker, each leg's
    /// ranks fill the pool by alternation and non-zero weights no longer
    /// decide membership. Unset `rerank_limit` scores every fused hit at
    /// pool `max(k, 50)`; set to N deepens both legs to `max(k, 50, N)` and
    /// scores the fused prefix of length N; zero and a value below `k` are
    /// `invalid`. Ignored when no reranker is passed.
    #[cfg(feature = "search")]
    pub fn search(
        &self,
        query: &SearchQuery,
        reranker: Option<&dyn Reranker>,
    ) -> Result<SearchHits, Error> {
        if self.decl.encrypted {
            bail!(Error::unsupported(format!(
                "encrypted collection {} cannot be searched",
                self.decl.name
            )));
        }
        if !self.decl.any_indexed() {
            bail!(Error::unsupported(format!(
                "collection {} cannot be searched: both Index legs are excluded",
                self.decl.name
            )));
        }
        let name = self.decl.name.as_str();
        QueryCtx {
            engine: &self.search,
            decls: &self.decls,
            vault: &self.vault,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
            scope: crate::query::FilterScope::Collection(name),
        }
        .search(self.embedder.as_deref(), &[name], query, reranker)
    }

    /// Git history of the Record file, newest first.
    ///
    /// Same identity path as `get`. Does not ask the Index which ids exist.
    /// `journal`: history of the segment file (whole file in `Entry.raw`), not
    /// line-level blame. `count` `0` → `invalid`. After `remove`, commits
    /// that stored the file are still returned. An `id` that never had a
    /// commit and has no file → `not_found`. A missing Index directory
    /// does not stop this call.
    ///
    /// On an encrypted collection the returned `raw` is decrypted plaintext,
    /// the same bytes a `get` of that revision would have produced. The
    /// handle secret is required. An empty `raw` (a deletion revision) is
    /// returned as is.
    pub fn history(&self, id: &str, offset: u32, count: u32) -> Result<History, Error> {
        require_caller_id(self.decl.kind, id)?;
        self.require_encryption_secret()?;
        if count == 0 {
            bail!(Error::invalid("history count must be > 0"));
        }
        let rel = match self.decl.kind {
            Kind::Journal => {
                let mut maps = crate::error::mutex(
                    &self.journal_ids,
                    "[Collection::history] lock journal ids",
                )?;
                index::refresh_ids(&self.generation_dir()?, &mut maps, &self.decl)?;
                journal::segment_rel(&self.vault, &self.decl, id, &maps)?
            }
            Kind::Document | Kind::Singleton => {
                if self
                    .decl
                    .singleton_id
                    .as_deref()
                    .is_some_and(|fixed| fixed != id)
                {
                    bail!(Error::not_found(format!(
                        "record {id} is not in {}",
                        self.decl.name
                    )));
                }
                let rel = store::record_rel(&self.decl, id)?;
                let on_disk = store::locate_file(&self.vault, &self.decl, id)?.is_some();
                if !on_disk {
                    let (repo, git_path) = isolated::git_addr(&self.vault, &rel);
                    if !git::is_work_tree(&repo) || !git::path_has_history(&repo, &git_path)? {
                        bail!(Error::not_found(format!(
                            "record {id} is not in {}",
                            self.decl.name
                        )));
                    }
                }
                rel
            }
        };
        let (repo, git_path) = isolated::git_addr(&self.vault, &rel);
        if self.decl.isolated && !git::is_work_tree(&repo) {
            return Ok(History {
                entries: Vec::new(),
                ..History::default()
            });
        }
        let mut entries = git::history(&repo, &git_path, offset, count)?;
        if self.decl.encrypted {
            for entry in &mut entries {
                if let Some(raw) = entry.raw.take_if(|raw| !raw.is_empty()) {
                    entry.raw = Some(store::decode_record_file(
                        raw,
                        &self.decl,
                        self.encryption.as_ref(),
                        &rel,
                    )?);
                }
            }
        }
        Ok(History {
            entries,
            ..History::default()
        })
    }

    /// Whether `rel` is in git or among this handle's uncommitted writes.
    fn tracked_or_noted(&self, rel: &str) -> Result<bool, Error> {
        if crate::error::mutex(&self.written, "[Collection] lock written")?.contains(rel) {
            return Ok(true);
        }
        let (repo, path) = isolated::git_addr(&self.vault, rel);
        if !git::is_work_tree(&repo) {
            return Ok(false);
        }
        git::is_tracked(&repo, &path)
    }
}

fn require_caller_id(kind: Kind, id: &str) -> Result<(), Error> {
    match kind {
        Kind::Document => crate::id::require_document_id(id),
        Kind::Journal | Kind::Singleton => crate::id::require_slug("id", id),
    }
}

fn record_from_raw(id: &str, collection: &str, raw: &[u8]) -> Record {
    Record::default()
        .with_id(id)
        .with_collection(collection)
        .with_raw(raw.to_vec())
}

#[cfg(feature = "find")]
fn resolve_collections(
    decls: &Decls,
    names: &[&str],
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if names.is_empty() {
        return Ok(decls
            .collections()
            .filter(|decl| !decl.isolated && decl.lexical_indexed())
            .cloned()
            .collect());
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for name in names {
        if !seen.insert(*name) {
            continue;
        }
        let Some(decl) = decls.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in the declaration"
            )));
        };
        if decl.encrypted {
            bail!(Error::unsupported(format!(
                "encrypted collection {name} cannot be found"
            )));
        }
        if decl.exclude_lexical_index {
            bail!(Error::unsupported(format!(
                "collection {name} cannot be found: exclude_lexical_index"
            )));
        }
        out.push(decl.clone());
    }
    Ok(out)
}

#[cfg(feature = "search")]
fn resolve_search_collections(
    decls: &Decls,
    names: &[&str],
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if names.is_empty() {
        return Ok(decls
            .collections()
            .filter(|decl| !decl.isolated && decl.any_indexed())
            .cloned()
            .collect());
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for name in names {
        if !seen.insert(*name) {
            continue;
        }
        let Some(decl) = decls.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in the declaration"
            )));
        };
        if decl.encrypted {
            bail!(Error::unsupported(format!(
                "encrypted collection {name} cannot be searched"
            )));
        }
        if !decl.any_indexed() {
            bail!(Error::unsupported(format!(
                "collection {name} cannot be searched: both Index legs are excluded"
            )));
        }
        out.push(decl.clone());
    }
    Ok(out)
}

#[cfg(feature = "find")]
fn apply_filter_decls(
    decls: Vec<schema::CollectionDecl>,
    filter: &Filter,
    explicit: bool,
    projector: Option<&dyn crate::Projector>,
    scope: query::FilterScope<'_>,
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if explicit {
        for decl in &decls {
            query::validate(decl, filter, projector, scope)?;
        }
        return Ok(decls);
    }
    Ok(decls
        .into_iter()
        .filter(|decl| query::validate(decl, filter, projector, scope).is_ok())
        .collect())
}

/// Full-text blob of a `moreLikeThis` source `id`, same text the Index holds.
///
/// Missing `id` is `not_found`. An encrypted or lexically excluded
/// collection never reaches `find` (`unsupported` at the collection set);
/// if this path still sees one, it returns `unsupported` rather than
/// `crypto`.
#[cfg(feature = "find")]
fn more_like_this_source_text(
    ctx: &QueryCtx<'_>,
    decl: &CollectionDecl,
    id: &str,
) -> Result<String, Error> {
    if decl.encrypted {
        bail!(Error::unsupported(format!(
            "encrypted collection {} cannot be found",
            decl.name
        )));
    }
    if !decl.lexical_indexed() {
        bail!(Error::unsupported(format!(
            "collection {} cannot be found: exclude_lexical_index",
            decl.name
        )));
    }
    let record = match ctx.read_record(&decl.name, id) {
        Ok(record) => record,
        Err(err)
            if matches!(
                err.known_code(),
                Some(Code::Crypto | Code::UnsupportedEncryption)
            ) =>
        {
            bail!(Error::unsupported(format!(
                "encrypted collection {} cannot be found",
                decl.name
            )));
        }
        Err(err) => return Err(err),
    };
    let Some(projector) = ctx.projector else {
        return Ok(String::new());
    };
    let index_schema = match projector.schema(&decl.name) {
        Ok(schema) => {
            schema.require_valid()?;
            schema
        }
        Err(_) => return Ok(String::new()),
    };
    let raw = record.raw.as_deref().unwrap_or(&[]);
    let doc = match projector.project(&decl.name, id, raw) {
        Ok(doc) if !doc.values.is_empty() => doc,
        _ => return Ok(String::new()),
    };
    Ok(crate::projector::fts_blob(&index_schema, &doc))
}

#[cfg(feature = "find")]
struct QueryCtx<'a> {
    engine: &'a SearchEngine,
    decls: &'a Decls,
    vault: &'a Path,
    journal_ids: &'a Mutex<JournalIdMaps>,
    projector: Option<&'a dyn crate::Projector>,
    scope: query::FilterScope<'a>,
}

#[cfg(feature = "find")]
impl QueryCtx<'_> {
    /// Read one Record of `collection` as the files hold it. A journal line
    /// another handle or process appended is found: the `id` map is read
    /// again when its file changed.
    fn read_record(&self, collection: &str, id: &str) -> Result<Record, Error> {
        let Some(decl) = self.decls.get(collection) else {
            bail!(Error::not_found(format!(
                "collection {collection} is not in the declaration"
            )));
        };
        match decl.kind {
            Kind::Journal => {
                let mut maps = crate::error::mutex(self.journal_ids, "[Vault] lock journal ids")?;
                index::refresh_ids(&self.engine.dir()?, &mut maps, decl)?;
                journal::find_journal(self.vault, decl, id, &maps)
            }
            Kind::Document | Kind::Singleton => store::find_document(self.vault, decl, id, None),
        }
    }

    fn find(
        &self,
        names: &[&str],
        query: &FindQuery,
        offset: u32,
        count: u32,
    ) -> Result<FindResult, Error> {
        if count == 0 {
            bail!(Error::invalid("find count must be > 0"));
        }
        query.require_valid()?;
        let filter = &*query.filter;
        query::check_scope(
            filter,
            self.scope,
            |name| self.decls.get(name),
            self.projector,
        )?;
        let include_raw = query.include_raw == Some(true);
        let explicit = !names.is_empty();
        let decls = apply_filter_decls(
            resolve_collections(self.decls, names)?,
            filter,
            explicit,
            self.projector,
            self.scope,
        )?;
        if decls.is_empty() {
            return Ok(FindResult {
                total: Some(0),
                ..FindResult::default()
            });
        }
        let journal_ord =
            decls.len() == 1 && decls[0].kind == Kind::Journal && !query::uses_score(filter);
        let collections: Vec<&str> = decls.iter().map(|decl| decl.name.as_str()).collect();
        let page = self.engine.find(
            &collections,
            journal_ord,
            &decls,
            filter,
            (offset, count),
            |decl, id| more_like_this_source_text(self, decl, id),
        )?;
        let mut records = Vec::new();
        for (collection, id) in page.ids {
            match self.read_record(&collection, &id) {
                Ok(record) => records.push(if include_raw {
                    record
                } else {
                    without_raw(record)
                }),
                Err(err) if err.known_code() == Some(crate::Code::NotFound) => {}
                Err(err) => bail!(err),
            }
        }
        Ok(FindResult {
            records,
            total: Some(page.total),
            ..FindResult::default()
        })
    }

    #[cfg(feature = "search")]
    fn search(
        &self,
        embedder: Option<&dyn Embedder>,
        names: &[&str],
        query: &SearchQuery,
        reranker: Option<&dyn Reranker>,
    ) -> Result<SearchHits, Error> {
        query.require_valid()?;
        let explicit = !names.is_empty();
        let filter = &*query.filter;
        if filter.kind.is_some() {
            query::check_scope(
                filter,
                self.scope,
                |name| self.decls.get(name),
                self.projector,
            )?;
        }
        let mut decls = resolve_search_collections(self.decls, names)?;
        if filter.kind.is_some() {
            decls = apply_filter_decls(decls, filter, explicit, self.projector, self.scope)?;
        }
        if decls.is_empty() {
            return Ok(crate::search::hits_without_vectors(query));
        }
        let constrained = filter.kind.is_some() && !query::is_unconstrained(filter);
        if explicit && constrained {
            for decl in &decls {
                if !decl.lexical_indexed() {
                    bail!(Error::unsupported(format!(
                        "collection {} cannot apply a Filter: exclude_lexical_index",
                        decl.name
                    )));
                }
            }
        }
        if !explicit && constrained {
            decls.retain(|decl| decl.lexical_indexed());
            if decls.is_empty() {
                return Ok(crate::search::hits_without_vectors(query));
            }
        }
        let k = match query.k {
            None | Some(0) => 10,
            Some(n) => n as usize,
        };
        if reranker.is_some()
            && let Some(limit) = query.rerank_limit
        {
            let limit = limit as usize;
            ensure!(
                limit >= k,
                Error::invalid(format!("rerank_limit {limit} is below k {k}"))
            );
        }
        let collections: Vec<&str> = decls.iter().map(|decl| decl.name.as_str()).collect();
        let allowed = if filter.kind.is_none() || query::is_unconstrained(filter) {
            None
        } else {
            Some(
                self.engine
                    .find_ids(&collections, &decls, filter, |decl, id| {
                        more_like_this_source_text(self, decl, id)
                    })?,
            )
        };
        let mlt_fts = |decl: &CollectionDecl, id: &str| more_like_this_source_text(self, decl, id);
        let narrowed = allowed.as_ref().map(|allowed| crate::search::SearchFilter {
            filter,
            allowed,
            mlt_fts: &mlt_fts,
        });
        let mut hits = self.engine.search(
            &decls,
            query,
            embedder,
            narrowed.as_ref(),
            reranker.is_some(),
        )?;
        let include_raw = query.include_raw == Some(true);
        let Some(reranker) = reranker else {
            hits.hits.truncate(k);
            if include_raw {
                self.fill_hit_raw(&mut hits)?;
            }
            return Ok(hits);
        };
        if let Some(limit) = query.rerank_limit {
            hits.hits.truncate(limit as usize);
        }
        let text = query.text.as_deref().unwrap_or("");
        // The Index may lag a `remove`: a hit whose file is gone is dropped
        // before scoring, as `find` drops it.
        let mut records = Vec::with_capacity(hits.hits.len());
        let mut candidates = Vec::with_capacity(hits.hits.len());
        for hit in hits.hits {
            match self.read_record(hit_collection(&hit), hit.id.as_deref().unwrap_or("")) {
                Ok(record) => {
                    records.push(record);
                    candidates.push(hit);
                }
                Err(err) if err.known_code() == Some(crate::Code::NotFound) => {}
                Err(err) => bail!(err),
            }
        }
        let refs: Vec<&Record> = records.iter().collect();
        let scores = crate::embed::score_all(reranker, text, &refs)?;
        let mut scored: Vec<_> = scores
            .into_iter()
            .zip(candidates)
            .zip(records)
            .map(|((score, mut hit), record)| {
                if include_raw {
                    hit.raw = record.raw;
                }
                (score, hit)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        let mut out = SearchHits {
            vector_leg: hits.vector_leg,
            ..SearchHits::default()
        };
        for (score, mut hit) in scored.into_iter().take(k) {
            hit.rerank_score = Some(score);
            out.hits.push(hit);
        }
        Ok(out)
    }

    #[cfg(feature = "search")]
    fn fill_hit_raw(&self, hits: &mut SearchHits) -> Result<(), Error> {
        for hit in &mut hits.hits {
            let collection = hit_collection(hit);
            match self.read_record(collection, hit.id.as_deref().unwrap_or("")) {
                Ok(record) => hit.raw = record.raw,
                Err(err) if err.known_code() == Some(crate::Code::NotFound) => {}
                Err(err) => bail!(err),
            }
        }
        Ok(())
    }
}

#[cfg(feature = "find")]
fn without_raw(mut record: Record) -> Record {
    record.raw = None;
    record
}

#[cfg(feature = "search")]
fn hit_collection(hit: &search_hits::Hit) -> &str {
    hit.collection.as_deref().unwrap_or("")
}

fn autocommit_subject(message: Option<&str>) -> &str {
    message.unwrap_or(AUTO_COMMIT_MESSAGE)
}

fn require_commit_message(message: Option<&str>) -> Result<Option<&str>, Error> {
    match message {
        None => Ok(None),
        Some(text) if text.trim().is_empty() => {
            bail!(Error::invalid("commit message is empty"))
        }
        Some(text) => Ok(Some(text)),
    }
}

fn remote_repo_url(options: &OpenOptions) -> Option<&str> {
    options
        .remote_repo
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
}

/// On an existing Vault, a set `remote_repo` must equal `origin`'s fetch URL.
fn require_matching_origin(path: &Path, options: &OpenOptions) -> Result<(), Error> {
    let Some(requested) = remote_repo_url(options) else {
        return Ok(());
    };
    let Some(origin) = git::origin_url(path)? else {
        bail!(
            Error::invalid(format!(
                "existing Vault has no origin; remote_repo is {}",
                remote_url_for_message(requested)
            ))
            .context("[Vault::open] match origin")
        );
    };
    if origin == requested {
        return Ok(());
    }
    bail!(
        Error::invalid(format!(
            "existing Vault origin {} does not match remote_repo {}",
            remote_url_for_message(&origin),
            remote_url_for_message(requested)
        ))
        .context("[Vault::open] match origin")
    )
}

/// Drop URL userinfo from an error message; keep scheme, host, and path.
fn remote_url_for_message(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    match rest.rsplit_once('@') {
        Some((_, host_and_path)) if !host_and_path.is_empty() => {
            format!("{scheme}://{host_and_path}")
        }
        _ => url.to_owned(),
    }
}

fn without_remote_repo(mut options: OpenOptions) -> OpenOptions {
    options.remote_repo = None;
    options
}

fn prepare_new_work_tree(path: &Path) -> Result<(), Error> {
    if git::is_work_tree(path) {
        bail!(Error::invalid(format!(
            "path is already a git working tree: {}",
            path.display()
        )));
    }
    if path.exists() {
        if !path.is_dir() {
            bail!(Error::invalid(format!(
                "init path is not a directory: {}",
                path.display()
            )));
        }
        let mut entries = fs::read_dir(path).context("[Vault::init] read directory")?;
        if entries
            .next()
            .transpose()
            .context("[Vault::init] read directory entry")?
            .is_some()
        {
            bail!(Error::invalid(format!(
                "init path is not empty: {}",
                path.display()
            )));
        }
    } else {
        fs::create_dir_all(path).context("[Vault::init] create vault")?;
    }
    Ok(())
}

/// Which of the `.scheda` and `.isolated` entries `text` already ignores.
fn gitignore_entries(text: &str) -> (bool, bool) {
    let has = |dir: &str, slashed: &str| {
        text.lines().any(|line| {
            let line = line.trim();
            line == slashed || line == dir
        })
    };
    (has(".scheda", ".scheda/"), has(".isolated", ".isolated/"))
}

fn gitignore_needs_write(vault: &Path) -> Result<bool, Error> {
    let path = vault.join(".gitignore");
    if !path.exists() {
        return Ok(true);
    }
    let text = fs::read_to_string(&path).context("[Vault::open] read gitignore")?;
    let (already_scheda, already_isolated) = gitignore_entries(&text);
    Ok(!(already_scheda && already_isolated))
}

fn ensure_gitignore(vault: &Path) -> Result<bool, Error> {
    let path = vault.join(".gitignore");
    let mut text = if path.exists() {
        fs::read_to_string(&path).context("[Vault::open] read gitignore")?
    } else {
        String::new()
    };
    let (already_scheda, already_isolated) = gitignore_entries(&text);
    if already_scheda && already_isolated {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !already_scheda {
        text.push_str(".scheda/\n");
    }
    if !already_isolated {
        text.push_str(".isolated/\n");
    }
    fs::write(&path, text).context("[Vault::open] write gitignore")?;
    Ok(true)
}

/// Write the marker, and `.scheda/` and `.isolated/` into `.gitignore`, and
/// commit what changed in one commit, when this tree is new (init or clone).
/// A marker that came with a clone is validated first and kept. No writer
/// exists yet, so this does not take `write.lock`. A later mutating call is
/// then the first `try_lock`.
fn commit_service_files(vault: &Path, identity: &git::CommitIdentity) -> Result<(), Error> {
    marker::read(vault)?;
    let mut paths = Vec::new();
    if marker::ensure(vault)? {
        paths.push(String::from(marker::FILE));
    }
    if ensure_gitignore(vault)? {
        paths.push(String::from(".gitignore"));
    }
    if !paths.is_empty() {
        git::commit(vault, AUTO_COMMIT_MESSAGE, &paths, identity)?;
    }
    Ok(())
}

/// Longest single sleep inside `wait_indexed`.
#[cfg(feature = "find")]
const WAIT_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

/// `wait_indexed` ran out of time; the message lists the heads and marks
/// seen last.
#[cfg(feature = "find")]
pub(crate) fn wait_timed_out(timeout: std::time::Duration, status: &crate::IndexStatus) -> Error {
    Error::io_timed_out(format!(
        "Index did not reach HEAD within {timeout:?}: head {:?}, lexical {:?}, \
         vector {:?}, isolated head {:?}, isolated lexical {:?}, \
         isolated vector {:?}",
        status.head,
        status.lexical,
        status.vector,
        status.isolated_head,
        status.isolated_lexical,
        status.isolated_vector,
    ))
}

/// Whether a leg's `mark` has reached `target` in the repository at `dir`.
/// An inactive leg, or no commit to reach, counts as reached.
#[cfg(feature = "find")]
fn reached(
    dir: &Path,
    target: Option<&str>,
    mark: Option<&str>,
    active: bool,
) -> Result<bool, Error> {
    let (Some(target), true) = (target, active) else {
        return Ok(true);
    };
    match mark {
        None => Ok(false),
        Some(mark) if mark == target => Ok(true),
        Some(mark) => git::tree::is_ancestor(dir, target, mark),
    }
}

fn tx_log(vault: &Path) -> PathBuf {
    vault.join(".scheda").join("tx.log")
}

fn flag(slot: &Mutex<bool>) -> Result<bool, Error> {
    Ok(*crate::error::mutex(slot, "[Vault] lock transaction flag")?)
}

fn set_in_txn(slot: &Mutex<bool>, on: bool) -> Result<(), Error> {
    *crate::error::mutex(slot, "[Vault] lock transaction flag")? = on;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).context("[Vault] remove tx.log"),
    }
}

/// Restore Scheda-recorded `paths` to `HEAD` and repair what writes keep
/// inline: the journal `id` map forgets every line the restore removed.
/// The Index legs are not touched: they only ever held committed state.
/// Returns the declarations as the files hold them afterwards.
fn restore_paths(
    vault: &Path,
    decls: &Decls,
    paths: &[String],
    index_dir: &Path,
    journal_ids: &Mutex<JournalIdMaps>,
) -> Result<Decls, Error> {
    let before = crate::reindex::journal_ids_in(vault, decls, paths)?;
    isolated::discard_recorded(vault, paths)?;
    let loaded = schema::load(vault)?;
    let after = crate::reindex::journal_ids_in(vault, &loaded, paths)?;
    {
        let mut maps = crate::error::mutex(journal_ids, "[Vault] lock journal ids")?;
        maps.clear_segment_lines();
        // `difference` is sorted by collection, so each collection's ids
        // are one run: one lock and one rewrite of its `ids` file.
        let mut removed: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (collection, id) in before.difference(&after) {
            removed.entry(collection).or_default().push(id.clone());
        }
        for (collection, ids) in removed {
            index::apply_id_changes(index_dir, &mut maps, collection, &[], &ids)?;
        }
    }
    Ok(loaded)
}

/// Commit every path in the `written` set, then empty the set and `tx.log`.
/// A failed commit leaves both as they were.
fn commit_written(
    vault: &Path,
    written: &Mutex<BTreeSet<String>>,
    message: &str,
    identity: &git::CommitIdentity,
) -> Result<(), Error> {
    let paths = written_paths(written)?;
    isolated::commit_recorded(vault, message, &paths, identity)?;
    clear_written(vault, written)
}

/// [`commit_written`] where nothing to commit is no failure: `false` when
/// the set is empty or its paths leave both repositories as at `HEAD`.
/// The set and `tx.log` are emptied either way.
fn commit_written_if_changed(
    vault: &Path,
    written: &Mutex<BTreeSet<String>>,
    message: &str,
    identity: &git::CommitIdentity,
) -> Result<bool, Error> {
    let paths = written_paths(written)?;
    let changed = isolated::commit_recorded_if_changed(vault, message, &paths, identity)?;
    clear_written(vault, written)?;
    Ok(changed)
}

fn written_paths(written: &Mutex<BTreeSet<String>>) -> Result<Vec<String>, Error> {
    let set = crate::error::mutex(written, "[Vault] lock written")?;
    Ok(set.iter().cloned().collect())
}

fn clear_written(vault: &Path, written: &Mutex<BTreeSet<String>>) -> Result<(), Error> {
    let mut set = crate::error::mutex(written, "[Vault] lock written")?;
    set.clear();
    persist_written(vault, &set)
}

/// Restore every path in the `written` set to `HEAD`, then empty the set and
/// `tx.log`. The list is cleared only after the files are back: a restore
/// that fails keeps the paths for the next attempt. Returns the declarations
/// as the files hold them afterwards, `None` when nothing was recorded.
fn restore_written(
    vault: &Path,
    written: &Mutex<BTreeSet<String>>,
    decls: &Decls,
    index_dir: &Path,
    journal_ids: &Mutex<JournalIdMaps>,
) -> Result<Option<Decls>, Error> {
    let paths: Vec<String> = {
        let set = crate::error::mutex(written, "[Vault] lock written")?;
        set.iter().cloned().collect()
    };
    if paths.is_empty() {
        return Ok(None);
    }
    let loaded = restore_paths(vault, decls, &paths, index_dir, journal_ids)?;
    let mut set = crate::error::mutex(written, "[Vault] lock written")?;
    set.clear();
    persist_written(vault, &set)?;
    Ok(Some(loaded))
}

fn load_written(vault: &Path) -> Result<BTreeSet<String>, Error> {
    match fs::read(tx_log(vault)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(err) => Err(err).context("[Vault::open] read tx.log"),
        Ok(bytes) if bytes.is_empty() => Ok(BTreeSet::new()),
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)
                .context(Code::NotUtf8, "[Vault::open] decode tx.log")?;
            Ok(text
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect())
        }
    }
}

fn persist_written(vault: &Path, set: &BTreeSet<String>) -> Result<(), Error> {
    let path = tx_log(vault);
    if set.is_empty() {
        remove_if_exists(&path)?;
    } else {
        let mut bytes = Vec::new();
        for rel in set {
            bytes.extend_from_slice(rel.as_bytes());
            bytes.push(b'\n');
        }
        store::atomic_write(&path, &bytes)?;
        store::restrict_file(&path)?;
    }
    let scheda = vault.join(".scheda");
    if scheda.is_dir() {
        store::restrict_dir(&scheda)?;
    }
    Ok(())
}

/// Prune orphan generations when no writer holds the lock and no
/// `reindex` is filling `N+1`. Skip if `write.lock` or `reindex.lock` is held.
fn try_prune_orphans(vault: &Path, root: &Path) -> Result<(), Error> {
    let scheda = vault.join(".scheda");
    fs::create_dir_all(&scheda).context("[Vault::open] create .scheda for prune")?;
    store::restrict_dir(&scheda)?;
    let lock_path = scheda.join("write.lock");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .context("[Vault::open] open write lock for prune")?;
    store::restrict_file(&lock_path)?;
    match file.try_lock() {
        Ok(()) => {
            let result = match crate::reindex::try_lock(vault)? {
                None => Ok(()),
                Some(reindex_lock) => {
                    let result = crate::reindex::remove_leftovers(vault)
                        .and_then(|()| index::prune_generations(root));
                    drop(reindex_lock);
                    result
                }
            };
            if let Err(err) = file.unlock()
                && result.is_ok()
            {
                return Err(err).context("[Vault::open] unlock write lock after prune");
            }
            result
        }
        Err(TryLockError::WouldBlock) => Ok(()),
        Err(TryLockError::Error(err)) => Err(Error::new(io_code(&err), err.to_string())
            .context("[Vault::open] lock write lock for prune")),
    }
}

fn release_held_write_lock(slot: &Mutex<WriteLock>) -> Result<(), Error> {
    let mut guard = crate::error::mutex(slot, "[Vault] release write lock")?;
    release_held_write_lock_in(&mut guard)
}

fn release_held_write_lock_in(slot: &mut WriteLock) -> Result<(), Error> {
    slot.owner = None;
    if let Some(file) = slot.file.take()
        && let Err(err) = file.unlock()
    {
        return Err(err).context("[Vault] unlock write lock");
    }
    Ok(())
}

fn release_write_lock_on_drop(slot: &Mutex<WriteLock>) {
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    let _ = release_held_write_lock_in(&mut guard);
}

/// Take `write.lock` for this handle. When the lock is newly acquired, the
/// `written` set is re-read from `tx.log`: another process may have left
/// recorded paths there since this handle opened, and the set a call
/// restores or persists must be the file's. A call that finds the lock
/// already held by this thread (inside a transaction) keeps its own set.
fn take_write_slot(
    slot: &mut WriteLock,
    vault: &Path,
    written: &Mutex<BTreeSet<String>>,
    kind: &str,
    op: &str,
) -> Result<(), Error> {
    let me = thread::current().id();
    if slot.is_held() {
        if slot.owner != Some(me) {
            return Err(
                Error::locked("another thread holds the write lock on this handle")
                    .with_context(|| format!("[{kind}::{op}] take write lock")),
            );
        }
        return Ok(());
    }
    let file =
        acquire_write_lock(vault).with_context(|| format!("[{kind}::{op}] take write lock"))?;
    // Dropping `file` on error releases the lock.
    let on_disk = load_written(vault).with_context(|| format!("[{kind}::{op}] read tx.log"))?;
    *crate::error::mutex(written, "[Vault] lock written")? = on_disk;
    slot.file = Some(file);
    slot.owner = Some(me);
    Ok(())
}

fn release_lock_unless_in_transaction<T>(
    write_lock: &Mutex<WriteLock>,
    in_txn: &Mutex<bool>,
    result: Result<T, Error>,
) -> Result<T, Error> {
    let in_txn = match flag(in_txn) {
        Ok(v) => v,
        Err(err) => {
            let _ = release_held_write_lock(write_lock);
            return result.and(Err(err));
        }
    };
    if in_txn {
        return result;
    }
    match release_held_write_lock(write_lock) {
        Ok(()) => result,
        Err(err) => result.and(Err(err)),
    }
}

fn acquire_write_lock(vault: &Path) -> Result<File, Error> {
    match try_acquire_write_lock(vault)? {
        Some(file) => Ok(file),
        None => {
            Err(Error::locked("vault write lock is held").context("[Vault] acquire write lock"))
        }
    }
}

fn try_acquire_write_lock(vault: &Path) -> Result<Option<File>, Error> {
    let scheda = vault.join(".scheda");
    fs::create_dir_all(&scheda).context("[Vault] create .scheda for write lock")?;
    store::restrict_dir(&scheda)?;
    drain_legacy_index_lock(&scheda)?;
    let lock_path = scheda.join("write.lock");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .context("[Vault] open write lock")?;
    store::restrict_file(&lock_path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => {
            Err(Error::new(io_code(&err), err.to_string()).context("[Vault] lock write lock"))
        }
    }
}

/// `auto_sync` pull on `open`: take `write.lock` for the fetch/checkout, then
/// release it so `open` does not keep the writer.
fn pull_under_write_lock(path: &Path) -> Result<(), Error> {
    let file = acquire_write_lock(path)?;
    let result = git::pull(path);
    if let Err(err) = file.unlock()
        && result.is_ok()
    {
        return Err(err).context("[Vault::open] unlock write lock after auto_sync pull");
    }
    result
}

fn drain_legacy_index_lock(scheda: &Path) -> Result<(), Error> {
    let old_path = scheda.join("index").join("write.lock");
    if !old_path.is_file() {
        return Ok(());
    }
    let old = File::options()
        .read(true)
        .write(true)
        .open(&old_path)
        .context("[Vault] open legacy write lock")?;
    match old.try_lock() {
        Ok(()) => {
            drop(old);
            match fs::remove_file(&old_path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err).context("[Vault] remove legacy write lock"),
            }
        }
        Err(TryLockError::WouldBlock) => {
            Err(Error::locked("vault write lock is held")
                .context("[Vault] acquire legacy write lock"))
        }
        Err(TryLockError::Error(err)) => {
            Err(Error::new(io_code(&err), err.to_string())
                .context("[Vault] lock legacy write lock"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(feature = "search")]
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::process;
    #[cfg(feature = "search")]
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Collection, OpenOptions, Vault};
    use crate::{Code, Encryption, Error, Schema, VaultInfo};
    #[cfg(feature = "search")]
    use crate::{Embedder, SearchQuery};
    use crate::{git, isolated, marks};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            crate::git::isolate_test_git_env();
            let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let base = std::env::var_os("TEST_TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            let tid = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
            let root = base.join(format!("scheda-c1-{}-{n}-{tid}", process::id()));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn vault(&self) -> PathBuf {
            self.root.join("vault")
        }

        fn options(&self) -> OpenOptions {
            OpenOptions::new()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn init_writes_gitignore_for_scheda_index() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        let gitignore = fs::read_to_string(scratch.vault().join(".gitignore")).unwrap();
        assert!(gitignore.lines().any(|line| line.trim() == ".scheda/"));
        assert!(gitignore.lines().any(|line| line.trim() == ".isolated/"));
        assert!(git::is_ignored(vault.path(), Path::new(".scheda/index")).unwrap());
        assert!(git::is_ignored(vault.path(), Path::new(".scheda/writer")).unwrap());
        let token = vault.writer().to_owned();
        assert!(!token.is_empty());
        assert_eq!(
            fs::read_to_string(scratch.vault().join(".scheda").join("writer"))
                .unwrap()
                .trim(),
            token
        );
        assert!(vault.index_dir().starts_with(vault.path()));
        assert!(vault.index_dir().ends_with("index"));
        fs::remove_dir_all(vault.index_dir()).unwrap();
        assert!(scratch.vault().join(".git").exists());
        drop(vault);
        let reopened = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        assert_eq!(reopened.writer(), token);
    }

    #[test]
    fn empty_remote_repo_option_is_invalid() {
        let scratch = Scratch::new();
        let err =
            Vault::open(scratch.vault(), OpenOptions::new().remote_repo(""), None).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(!scratch.vault().exists() || !scratch.vault().join(".git").exists());
    }

    #[cfg(not(feature = "encrypt"))]
    #[test]
    fn open_with_passphrase_fails_and_creates_nothing() {
        let scratch = Scratch::new();
        let err = Vault::open(
            scratch.vault(),
            OpenOptions::new().encryption(Encryption::new().passphrase("test-passphrase")),
            None,
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::UnsupportedEncryption));
        assert!(
            err.message
                .as_deref()
                .is_some_and(|m| m.contains("not available in this build"))
        );
        assert!(!scratch.vault().exists());
    }

    #[cfg(not(feature = "encrypt"))]
    #[test]
    fn open_with_identity_fails_and_creates_nothing() {
        let scratch = Scratch::new();
        let err = Vault::open(
            scratch.vault(),
            OpenOptions::new().encryption(Encryption::new().identity("AGE-SECRET-KEY-TEST")),
            None,
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::UnsupportedEncryption));
        assert!(
            err.message
                .as_deref()
                .is_some_and(|m| m.contains("not available in this build"))
        );
        assert!(!scratch.vault().exists());
    }

    #[cfg(feature = "encrypt")]
    #[test]
    fn open_with_passphrase_succeeds_when_encrypt_is_on() {
        let scratch = Scratch::new();
        let vault = Vault::open(
            scratch.vault(),
            OpenOptions::new().encryption(Encryption::new().passphrase("test-passphrase")),
            None,
        )
        .unwrap();
        assert!(scratch.vault().join(".git").is_dir());
        drop(vault);
    }

    #[cfg(feature = "encrypt")]
    fn encrypted_document_schema() -> Schema {
        Schema {
            kind: crate::Unit::default().into(),
            ..Schema::default()
        }
        .with_encrypted(true)
    }

    #[cfg(feature = "encrypt")]
    #[test]
    fn identity_add_get_round_trip() {
        let scratch = Scratch::new();
        let secret = crate::crypto::test_identity_secret();
        let vault =
            Vault::open(scratch.vault(), OpenOptions::new().encryption(secret), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("c", encrypted_document_schema(), None)
            .unwrap();
        let add = b"";
        vault
            .collection("c")
            .unwrap()
            .add("alpha", add, None)
            .unwrap();
        let got = vault.collection("c").unwrap().get("alpha").unwrap();
        assert_eq!(got.id.as_deref(), Some("alpha"));
        assert_eq!(got.raw.as_deref(), Some(b"".as_slice()));
    }

    #[cfg(feature = "encrypt")]
    #[test]
    fn documents_file_that_decrypts_but_does_not_parse_is_valid_false() {
        let scratch = Scratch::new();
        let secret = Encryption::new().passphrase("unit-passphrase");
        let vault = Vault::open(
            scratch.vault(),
            OpenOptions::new().encryption(secret.clone()),
            None,
        )
        .unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("c", encrypted_document_schema(), None)
            .unwrap();
        let garbage = crate::crypto::encrypt(b"not-yaml", Some(&secret)).unwrap();
        fs::write(scratch.vault().join("c").join("zulu.md"), garbage).unwrap();
        let records: Vec<_> = vault
            .collection("c")
            .unwrap()
            .documents()
            .unwrap()
            .map(|record| record.unwrap())
            .collect();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id.as_deref(), Some("zulu.md"));
        assert_eq!(records[0].raw.as_deref(), Some(b"not-yaml".as_slice()));
    }

    #[test]
    fn open_with_encryption_unset_succeeds_on_new_and_existing_vault() {
        let scratch = Scratch::new();
        let created = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        assert!(scratch.vault().join(".git").is_dir());
        assert!(!created.holds_write_lock());
        drop(created);
        let opened = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        assert!(scratch.vault().join(".git").is_dir());
        assert!(!opened.holds_write_lock());
        assert_eq!(opened.path(), scratch.vault().canonicalize().unwrap());
    }

    #[test]
    fn insert_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .add("", b"", None)
            .expect_err("empty id must fail slug rules");
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn replace_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .replace("", b"", None)
            .expect_err("empty id must fail slug rules");
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn unsupported_code_is_not_only_the_message() {
        let err = Error::unsupported("replace_one on journal");
        assert_eq!(err.known_code(), Some(Code::Unsupported));
        assert_ne!(err.to_string(), "unsupported");
    }

    #[test]
    fn init_creates_git_work_tree_without_remote() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(scratch.vault().join(".git").is_dir());
        assert!(!vault.has_remote());
        assert!(vault.remote_names().is_empty());
        assert!(!vault.holds_write_lock());
        assert_eq!(git::remotes(vault.path()).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn open_keeps_index_inside_tree_and_gitignored() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(vault.index_dir().starts_with(vault.path()));
        assert!(git::is_ignored(vault.path(), Path::new(".scheda")).unwrap());
        let index_dir = vault.index_dir().to_path_buf();
        // The background indexer writes there until the handle closes.
        vault.close().unwrap();
        fs::remove_dir_all(index_dir).unwrap();
        assert!(scratch.vault().join(".git").exists());
    }

    #[test]
    fn write_lock_file_is_under_scheda_not_index() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let scheda = scratch.vault().join(".scheda");
        let legacy = scheda.join("index").join("write.lock");
        fs::write(&legacy, []).unwrap();
        vault.begin().unwrap();
        assert!(scheda.join("write.lock").is_file());
        assert!(!legacy.is_file());
    }

    #[test]
    fn two_opens_on_distinct_roots() {
        let a = Scratch::new();
        let b = Scratch::new();
        let vault_a = Vault::open(a.vault(), a.options(), None).unwrap();
        let vault_b = Vault::open(b.vault(), b.options(), None).unwrap();
        assert_ne!(vault_a.path(), vault_b.path());
        assert!(!vault_a.holds_write_lock());
        assert!(!vault_b.holds_write_lock());
    }

    /// `replace` with the bytes `HEAD` already holds, inside a
    /// transaction: `commit` stages a tree equal to `HEAD`'s and makes no
    /// commit.
    #[test]
    fn identical_replace_in_a_transaction_commits_nothing() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let c = vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        c.add("alpha", b"one", None).unwrap();
        let before = head(vault.path());
        vault.begin().unwrap();
        c.replace("alpha", b"one", None).unwrap();
        vault.commit("same bytes").unwrap();
        assert_eq!(head(vault.path()), before);
        assert!(!vault.holds_write_lock());
        assert!(!super::tx_log(vault.path()).exists());
    }

    /// A caller's own error type for `transaction`.
    #[derive(Debug)]
    enum CallerError {
        Refused,
        Scheda(Error),
    }

    impl From<Error> for CallerError {
        fn from(err: Error) -> Self {
            Self::Scheda(err)
        }
    }

    fn with_notes(scratch: &Scratch) -> (Vault, Collection) {
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let c = vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        c.add("alpha", b"one", None).unwrap();
        (vault, c)
    }

    #[test]
    fn transaction_that_writes_nothing_is_ok_and_commits_nothing() {
        let scratch = Scratch::new();
        let (vault, _) = with_notes(&scratch);
        let before = head(vault.path());
        let value = vault.transaction("nothing", |_| Ok::<_, Error>(5)).unwrap();
        assert_eq!(value, 5);
        assert_eq!(head(vault.path()), before);
        assert!(!vault.holds_write_lock());
        // A strict `commit` still refuses an empty transaction.
        vault.begin().unwrap();
        let err = vault.commit("empty").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        vault.discard().unwrap();
    }

    #[test]
    fn transaction_with_identical_bytes_commits_nothing_in_either_repository() {
        let scratch = Scratch::new();
        let (vault, c) = with_notes(&scratch);
        let hidden = Schema {
            isolated: Some(true),
            ..document_schema()
        };
        vault
            .create_collection("hidden", hidden, None)
            .unwrap()
            .add("beta", b"two", None)
            .unwrap();
        let nested = isolated::root(vault.path());
        let before = (head(vault.path()), head(&nested));
        vault
            .transaction("same bytes", |vault| {
                c.replace("alpha", b"one", None)?;
                vault.collection("hidden")?.replace("beta", b"two", None)
            })
            .unwrap();
        assert_eq!((head(vault.path()), head(&nested)), before);
        assert!(!super::tx_log(vault.path()).exists());
        // A batch that changes something still commits, once.
        vault
            .transaction("changed", |_| c.replace("alpha", b"uno", None))
            .unwrap();
        assert_ne!(head(vault.path()), before.0);
        assert_eq!(last_commit_files(vault.path()), ["c/alpha"]);
    }

    #[test]
    fn transaction_returns_the_callers_error_and_discards_its_writes() {
        let scratch = Scratch::new();
        let (vault, c) = with_notes(&scratch);
        let before = head(vault.path());
        let err = vault
            .transaction("refused", |_| -> Result<(), CallerError> {
                c.add("beta", b"two", None)?;
                Err(CallerError::Refused)
            })
            .unwrap_err();
        assert!(matches!(err, CallerError::Refused), "{err:?}");
        assert_eq!(head(vault.path()), before);
        assert_eq!(
            c.get("beta").unwrap_err().known_code(),
            Some(Code::NotFound)
        );
        assert!(!vault.holds_write_lock());
        // Scheda's own failure arrives through `From`.
        let err = vault
            .transaction("", |_| Ok::<_, CallerError>(()))
            .unwrap_err();
        match err {
            CallerError::Scheda(err) => assert_eq!(err.known_code(), Some(Code::Invalid)),
            CallerError::Refused => panic!("an empty message is Scheda's failure"),
        }
    }

    #[test]
    fn transaction_while_another_thread_holds_one_is_locked_through_from() {
        let scratch = Scratch::new();
        let (vault, _) = with_notes(&scratch);
        let vault = std::sync::Arc::new(vault);
        let (begun_tx, begun_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let vault = std::sync::Arc::clone(&vault);
            std::thread::spawn(move || {
                vault
                    .transaction("held", |vault| {
                        vault.collection("c")?.add("held", b"x", None)?;
                        begun_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok::<_, Error>(())
                    })
                    .unwrap();
            })
        };
        begun_rx.recv().unwrap();
        let mut ran = false;
        let err = vault
            .transaction("second", |_| {
                ran = true;
                Ok::<_, CallerError>(())
            })
            .unwrap_err();
        match err {
            CallerError::Scheda(err) => assert_eq!(err.known_code(), Some(Code::Locked)),
            CallerError::Refused => panic!("locked is Scheda's failure"),
        }
        assert!(!ran);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(last_commit_files(vault.path()), ["c/held"]);
    }

    #[test]
    fn transaction_panic_discards_then_resumes() {
        let scratch = Scratch::new();
        let (vault, c) = with_notes(&scratch);
        let before = head(vault.path());
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vault.transaction("panics", |_| -> Result<(), Error> {
                c.add("beta", b"two", None)?;
                panic!("caller bug");
            })
        }));
        let payload = caught.unwrap_err();
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"caller bug"));
        assert_eq!(head(vault.path()), before);
        assert_eq!(
            c.get("beta").unwrap_err().known_code(),
            Some(Code::NotFound)
        );
        assert!(!vault.holds_write_lock());
        vault
            .transaction("after", |_| c.add("gamma", b"three", None))
            .unwrap();
        assert_eq!(last_commit_files(vault.path()), ["c/gamma"]);
    }

    #[test]
    fn begin_after_create_is_not_locked() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(!vault.holds_write_lock());
        vault.begin().unwrap();
        assert!(vault.holds_write_lock());
    }

    #[test]
    fn drop_vault_while_collection_holds_transaction_keeps_write_lock() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        let collection = vault.collection("c").unwrap();
        drop(vault);
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let err = other.begin().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        drop(collection);
        other.begin().unwrap();
    }

    #[test]
    fn collection_outliving_vault_can_autocommit() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        vault.commit("schema").unwrap();
        let collection = vault.collection("c").unwrap();
        drop(vault);
        collection.add("alpha", empty_add(), None).unwrap();
        let opened = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert_eq!(
            opened
                .collection("c")
                .unwrap()
                .get("alpha")
                .unwrap()
                .id
                .as_deref(),
            Some("alpha")
        );
    }

    #[test]
    fn begin_after_gitignore_repair_is_not_locked() {
        let scratch = Scratch::new();
        let created = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        drop(created);
        fs::write(scratch.vault().join(".gitignore"), "other\n").unwrap();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(!vault.holds_write_lock());
        vault.begin().unwrap();
        assert!(vault.holds_write_lock());
        let gitignore = fs::read_to_string(scratch.vault().join(".gitignore")).unwrap();
        assert!(gitignore.lines().any(|line| line.trim() == ".scheda/"));
        assert!(gitignore.lines().any(|line| line.trim() == ".isolated/"));
        assert!(gitignore.lines().any(|line| line.trim() == "other"));
    }

    fn document_schema() -> Schema {
        Schema {
            kind: crate::Unit::default().into(),
            ..Schema::default()
        }
    }

    fn journal_schema() -> Schema {
        serde_json::from_value(serde_json::json!({
            "journal": {}
        }))
        .unwrap()
    }

    #[cfg(feature = "search")]
    fn journal_vector_schema() -> Schema {
        serde_json::from_value(serde_json::json!({
            "journal": {}
        }))
        .unwrap()
    }

    fn empty_add() -> &'static [u8] {
        b""
    }

    #[cfg(feature = "search")]
    fn rec_tag(text: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "tag": text })).unwrap()
    }

    #[cfg(feature = "search")]
    struct TagProjector;

    #[cfg(feature = "search")]
    impl crate::Projector for TagProjector {
        fn schema(&self, _collection: &str) -> Result<crate::IndexSchema, Error> {
            Ok(crate::IndexSchema {
                columns: vec![
                    crate::IndexColumn::default()
                        .with_name("tag".to_owned())
                        .with_kind(crate::ColumnKind::Text)
                        .with_find(true)
                        .with_fts(true)
                        .with_vector(true),
                ],
                ..crate::IndexSchema::default()
            })
        }

        fn project(
            &self,
            _collection: &str,
            _id: &str,
            raw: &[u8],
        ) -> Result<crate::IndexDoc, Error> {
            let Ok(text) = std::str::from_utf8(raw) else {
                return Ok(crate::IndexDoc::default());
            };
            let Ok(serde_json::Value::Object(map)) = serde_json::from_str(text) else {
                return Ok(crate::IndexDoc::default());
            };
            let mut values = std::collections::BTreeMap::new();
            if let Some(serde_json::Value::String(tag)) = map.get("tag") {
                values.insert("tag".to_owned(), crate::IndexValue::Text(tag.clone()));
            }
            Ok(crate::IndexDoc { values })
        }
    }

    #[cfg(feature = "search")]
    fn tag_projector() -> Arc<dyn crate::Projector> {
        Arc::new(TagProjector)
    }

    #[cfg(feature = "search")]
    fn open_search(
        path: impl AsRef<Path>,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Vault {
        Vault::open_with(path, options, embedder, Some(tag_projector())).unwrap()
    }

    #[cfg(feature = "search")]
    fn counting_vector(text: &str) -> Vec<f32> {
        let mut acc = [0.0f32; 4];
        for (i, b) in text.as_bytes().iter().enumerate() {
            acc[i % 4] += f32::from(*b) / 255.0;
        }
        let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut acc {
                *x /= norm;
            }
        } else {
            acc[0] = 1.0;
        }
        acc.to_vec()
    }

    #[cfg(feature = "search")]
    struct CountingEmbedder {
        model_id: &'static str,
        slots: AtomicU64,
    }

    #[cfg(feature = "search")]
    impl CountingEmbedder {
        fn new(model_id: &'static str) -> Arc<Self> {
            Arc::new(Self {
                model_id,
                slots: AtomicU64::new(0),
            })
        }

        fn slots(&self) -> u64 {
            self.slots.load(Ordering::Relaxed)
        }
    }

    #[cfg(feature = "search")]
    impl Embedder for CountingEmbedder {
        fn model_id(&self) -> &str {
            self.model_id
        }

        fn dimensions(&self) -> NonZeroUsize {
            NonZeroUsize::new(4).unwrap()
        }

        fn many_limit(&self) -> NonZeroUsize {
            NonZeroUsize::new(32).unwrap()
        }

        fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            self.slots.fetch_add(
                u64::try_from(texts.len()).expect("embed batch fits u64"),
                Ordering::Relaxed,
            );
            Ok(texts.iter().copied().map(counting_vector).collect())
        }
    }

    #[test]
    fn write_commit_drop_then_write_again_is_not_locked() {
        let scratch = Scratch::new();
        let path = scratch.vault();
        {
            let vault = Vault::open(&path, scratch.options(), None).unwrap();
            vault.begin().unwrap();
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            vault.commit("schema").unwrap();
        }
        for i in 0..32 {
            let vault = Vault::open(&path, scratch.options(), None).unwrap();
            let collection = vault.collection("c").unwrap();
            vault.begin().unwrap();
            collection
                .add(&format!("row-{i}"), empty_add(), None)
                .unwrap();
            vault.commit("row").unwrap();
            assert!(
                !vault.holds_write_lock(),
                "iteration {i}: write lock still held after commit"
            );
            drop(collection);
            drop(vault);
        }
    }

    /// After `commit` the first handle stays alive with no transaction.
    /// A second handle must be able to write without waiting out Tantivy's
    /// `LockBusy` backoff (~1.3s, then `index_lexical`).
    #[cfg(feature = "find")]
    #[test]
    fn second_handle_writes_after_commit_without_lexical_lock_wait() {
        let scratch = Scratch::new();
        let writer = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        writer.begin().unwrap();
        writer
            .create_collection("c", document_schema(), None)
            .unwrap();
        writer.commit("schema").unwrap();

        writer.begin().unwrap();
        writer
            .collection("c")
            .unwrap()
            .add("batch-a", empty_add(), None)
            .unwrap();
        writer.commit("batch").unwrap();
        assert!(!writer.holds_write_lock());

        let started = std::time::Instant::now();
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        other.begin().unwrap();
        other
            .collection("c")
            .unwrap()
            .add("batch-b", empty_add(), None)
            .unwrap();
        other.commit("other").unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "second handle waited out lexical LockBusy backoff: {elapsed:?}"
        );
        assert!(writer.collection("c").unwrap().get("batch-b").is_ok());
    }

    #[cfg(feature = "find")]
    #[test]
    fn writes_inside_a_transaction_open_no_lexical_writer() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        vault.commit("schema").unwrap();
        indexed(&vault);

        vault.begin().unwrap();
        let before = vault.lexical_writer_opens();
        let collection = vault.collection("c").unwrap();
        for i in 0..8 {
            collection
                .add(&format!("row-{i}"), empty_add(), None)
                .unwrap();
        }
        assert_eq!(
            vault.lexical_writer_opens(),
            before,
            "the write path never opens the Tantivy writer"
        );
        vault.commit("batch").unwrap();
        indexed(&vault);
        assert_eq!(find_ids(&vault, "c").len(), 8);
    }

    #[test]
    fn two_opens_do_not_take_the_write_lock() {
        let scratch = Scratch::new();
        let a = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let b = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(!a.holds_write_lock());
        assert!(!b.holds_write_lock());
        a.begin().unwrap();
        assert!(a.holds_write_lock());
        let err = b.begin().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::begin]")),
            "operation missing from stack: {:?}",
            err.stack
        );
    }

    #[test]
    fn second_handle_mutating_call_is_locked() {
        let scratch = Scratch::new();
        let writer = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        writer.begin().unwrap();
        writer
            .create_collection("c", document_schema(), None)
            .unwrap();
        assert!(writer.holds_write_lock());
        let reader = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let err = reader
            .create_collection("other", document_schema(), None)
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::create_collection]")),
            "operation missing from stack: {:?}",
            err.stack
        );
        let err = reader.commit("no").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::commit]")),
            "operation missing from stack: {:?}",
            err.stack
        );
        let err = reader.discard().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::discard]")),
            "operation missing from stack: {:?}",
            err.stack
        );
        let err = reader.push().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::push]")),
            "operation missing from stack: {:?}",
            err.stack
        );
        let err = reader.pull().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::pull]")),
            "operation missing from stack: {:?}",
            err.stack
        );
        let err = reader.sync().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::sync]")),
            "operation missing from stack: {:?}",
            err.stack
        );
        // With `find`, `reindex` builds from commits and takes no
        // `write.lock`; without it, it holds the lock for the call.
        let reindex = reader.reindex();
        if cfg!(feature = "find") {
            reindex.unwrap();
        } else {
            let err = reindex.unwrap_err();
            assert_eq!(err.known_code(), Some(Code::Locked));
            assert!(
                err.stack
                    .iter()
                    .any(|frame| frame.contains("[Vault::reindex]")),
                "operation missing from stack: {:?}",
                err.stack
            );
        }
    }

    #[test]
    fn open_does_not_take_write_lock() {
        let scratch = Scratch::new();
        let writer = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let reader = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(!writer.holds_write_lock());
        assert!(!reader.holds_write_lock());
        assert_eq!(reader.path(), writer.path());
    }

    #[test]
    fn open_reports_origin_without_network() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        git::add_remote(vault.path(), "origin", "https://example.invalid/scheda.git").unwrap();
        drop(vault);
        let opened = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(opened.has_remote());
        assert_eq!(opened.remote_names(), &["origin".to_owned()]);
        assert!(!opened.auto_sync());
    }

    #[test]
    fn open_existing_without_origin_rejects_remote_repo() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let path = vault.path().to_path_buf();
        drop(vault);
        let err = Vault::open(
            &path,
            OpenOptions::new().remote_repo("https://example.invalid/scheda.git"),
            None,
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(
            err.message
                .as_deref()
                .is_some_and(|m| m.contains("has no origin"))
        );
        assert!(git::origin_url(&path).unwrap().is_none());
        assert!(git::remotes(&path).unwrap().is_empty());
    }

    #[test]
    fn open_existing_rejects_different_remote_repo() {
        const ORIGIN: &str = "https://example.invalid/scheda.git";
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        git::add_remote(vault.path(), "origin", ORIGIN).unwrap();
        let path = vault.path().to_path_buf();
        drop(vault);
        let err = Vault::open(
            &path,
            OpenOptions::new().remote_repo("https://example.invalid/other.git"),
            None,
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(
            err.message
                .as_deref()
                .is_some_and(|m| m.contains("does not match remote_repo"))
        );
        assert_eq!(git::origin_url(&path).unwrap().as_deref(), Some(ORIGIN));
        assert_eq!(git::remotes(&path).unwrap(), vec!["origin".to_owned()]);
    }

    #[test]
    fn remote_url_for_message_drops_userinfo_keeps_path() {
        assert_eq!(
            super::remote_url_for_message("https://token@github.com/org/repo.git"),
            "https://github.com/org/repo.git"
        );
        assert_eq!(
            super::remote_url_for_message("https://github.com/org/repo.git"),
            "https://github.com/org/repo.git"
        );
    }

    #[test]
    fn open_existing_accepts_matching_remote_repo() {
        const ORIGIN: &str = "https://example.invalid/scheda.git";
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        git::add_remote(vault.path(), "origin", ORIGIN).unwrap();
        drop(vault);
        let opened = Vault::open(
            scratch.vault(),
            OpenOptions::new().remote_repo(ORIGIN),
            None,
        )
        .unwrap();
        assert_eq!(opened.remote_names(), &["origin".to_owned()]);
        assert_eq!(
            git::origin_url(opened.path()).unwrap().as_deref(),
            Some(ORIGIN)
        );
    }

    #[test]
    fn init_does_not_configure_lfs() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(!git::local_config_mentions_lfs(vault.path()).unwrap());
    }

    #[test]
    fn auto_sync_open_does_not_require_a_held_lock() {
        let scratch = Scratch::new();
        let opened = Vault::open(scratch.vault(), scratch.options().auto_sync(true), None).unwrap();
        assert!(opened.auto_sync());
        assert!(!opened.holds_write_lock());
    }

    #[test]
    fn git_works_without_an_async_runtime() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(git::is_work_tree(vault.path()));
    }

    #[test]
    fn info_is_on_the_vault_surface() {
        let _: fn(&Vault) -> Result<VaultInfo, Error> = Vault::info;
        let _: fn(&Vault, &str, Schema, Option<&str>) -> Result<Collection, Error> =
            Vault::create_collection;
        let _: fn(&Vault) -> Result<(), Error> = Vault::begin;
        let _: fn(&Vault) -> Result<(), Error> = Vault::discard;
    }

    #[cfg(feature = "find")]
    #[test]
    fn reindex_without_publish_keeps_old_generation_live() {
        use crate::FindQuery;

        let scratch = Scratch::new();
        let writer = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        writer.begin().unwrap();
        let declaration = Schema {
            kind: crate::Unit::default().into(),
            ..Schema::default()
        };
        writer.create_collection("c", declaration, None).unwrap();
        writer.commit("schema").unwrap();
        writer
            .collection("c")
            .unwrap()
            .add("alpha", b"", None)
            .unwrap();
        writer.reindex().unwrap();
        let current = fs::read_to_string(writer.index_dir().join("current")).unwrap();
        let reader = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        let before = reader
            .collection("c")
            .unwrap()
            .find(&FindQuery::all(), 0, 10)
            .unwrap();
        assert_eq!(before.records.len(), 1);
        let orphan = writer.reindex_without_publish().unwrap();
        assert!(orphan.is_dir());
        assert_eq!(
            fs::read_to_string(writer.index_dir().join("current")).unwrap(),
            current
        );
        let after = reader
            .collection("c")
            .unwrap()
            .find(&FindQuery::all(), 0, 10)
            .unwrap();
        assert_eq!(after.records.len(), 1);
        assert_eq!(
            after.records[0].id.as_deref(),
            before.records[0].id.as_deref()
        );
        drop(writer);
        drop(reader);
        let _ = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        assert!(
            !orphan.exists(),
            "unpublished generation must be removed on the next open"
        );
    }

    #[cfg(feature = "find")]
    fn live_generation(vault: &Vault) -> u64 {
        fs::read_to_string(vault.index_dir().join("current"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[cfg(feature = "find")]
    fn assert_locked_reindex(err: &Error) {
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack.iter().any(|frame| frame.contains("reindex")),
            "reindex missing from stack: {:?}",
            err.stack
        );
    }

    fn seeded(scratch: &Scratch) -> Vault {
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        vault.commit("schema").unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("alpha", empty_add(), None)
            .unwrap();
        indexed(&vault);
        vault
    }

    /// Wait for the background indexer to reach `HEAD`. A build without
    /// `find` has no Index to wait for.
    fn indexed(vault: &Vault) {
        #[cfg(feature = "find")]
        vault
            .wait_indexed(std::time::Duration::from_secs(30))
            .unwrap();
        #[cfg(not(feature = "find"))]
        let _ = vault;
    }

    #[cfg(feature = "find")]
    fn find_ids(vault: &Vault, name: &str) -> Vec<String> {
        vault
            .collection(name)
            .unwrap()
            .find(&crate::FindQuery::all(), 0, 10)
            .unwrap()
            .records
            .into_iter()
            .filter_map(|record| record.id)
            .collect()
    }

    /// The build reads commits and takes no `write.lock`; a write made
    /// during it reaches `N+1` through the marks after publish.
    #[cfg(feature = "find")]
    #[test]
    fn reindex_takes_no_write_lock_and_writes_during_the_build_follow() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                assert!(
                    !vault.holds_write_lock(),
                    "write.lock must be free during the build"
                );
                other
                    .collection("c")
                    .unwrap()
                    .add("beta", empty_add(), None)
                    .unwrap();
                done.wait();
            });
            vault.reindex().unwrap();
        });
        indexed(&vault);
        let mut ids = find_ids(&vault, "c");
        ids.sort();
        assert_eq!(ids, ["alpha", "beta"]);
        assert!(
            !scratch.vault().join(".scheda").join("reindex").exists(),
            "no catch-up queue is written"
        );
    }

    #[cfg(feature = "find")]
    #[test]
    fn reindex_publishes_while_another_handle_holds_a_transaction() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let live = live_generation(&vault);
        let holder = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            s.spawn(move || {
                start.wait();
                holder.begin().unwrap();
                holder
                    .collection("c")
                    .unwrap()
                    .add("pending", empty_add(), None)
                    .unwrap();
                done.wait();
                // The write lock belongs to this thread.
                release_rx.recv().unwrap();
                holder.discard().unwrap();
            });
            vault.reindex().unwrap();
            assert_eq!(live_generation(&vault), live + 1);
            indexed(&vault);
            assert_eq!(
                find_ids(&vault, "c"),
                ["alpha"],
                "a write no commit holds is not indexed"
            );
            release_tx.send(()).unwrap();
        });
    }

    #[cfg(feature = "search")]
    #[test]
    fn a_write_during_the_build_is_applied_after_publish_on_both_legs() {
        let scratch = Scratch::new();
        let embedder = CountingEmbedder::new("model-x");
        let vault = open_search(
            scratch.vault(),
            scratch.options(),
            Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
        );
        vault.begin().unwrap();
        vault
            .create_collection("c", journal_vector_schema(), None)
            .unwrap();
        vault.commit("schema").unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("keep", rec_tag("alpha mineral"), None)
            .unwrap();
        indexed(&vault);
        let after_add = embedder.slots();
        let other = open_search(
            scratch.vault(),
            scratch.options(),
            Some(CountingEmbedder::new("model-x") as Arc<dyn Embedder>),
        );
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                other
                    .collection("c")
                    .unwrap()
                    .add("queued", rec_tag("alpha mineral"), None)
                    .unwrap();
                done.wait();
            });
            vault.reindex().unwrap();
        });
        indexed(&vault);
        assert!(
            embedder.slots() <= after_add + 2,
            "`keep` is copied from the same-model dump, not embedded again"
        );
        let mut ids = find_ids(&vault, "c");
        ids.sort();
        assert_eq!(ids, ["keep", "queued"]);
        let hits = vault
            .collection("c")
            .unwrap()
            .search(
                &SearchQuery::default()
                    .with_text("alpha mineral")
                    .with_k(10)
                    .with_lexical_weight(0.0),
                None,
            )
            .unwrap()
            .hits
            .into_iter()
            .filter_map(|hit| hit.id)
            .collect::<Vec<_>>();
        assert!(hits.contains(&"queued".to_owned()), "{hits:?}");
    }

    /// A handle without an embedder (a CLI, say) must not drop vectors
    /// another process paid for: they move to `N+1` with their mark.
    #[cfg(feature = "search")]
    #[test]
    fn reindex_without_an_embedder_keeps_the_vectors_and_their_mark() {
        let scratch = Scratch::new();
        let seeded = open_search(
            scratch.vault(),
            scratch.options(),
            Some(CountingEmbedder::new("model-x") as Arc<dyn Embedder>),
        );
        seeded.begin().unwrap();
        seeded
            .create_collection("c", journal_vector_schema(), None)
            .unwrap();
        seeded.commit("schema").unwrap();
        seeded
            .collection("c")
            .unwrap()
            .add("keep", rec_tag("alpha mineral"), None)
            .unwrap();
        indexed(&seeded);
        let vector_mark = mark(&seeded, marks::Leg::Vector, marks::Repo::Main);
        assert!(vector_mark.is_some());
        drop(seeded);
        let vault = open_search(scratch.vault(), scratch.options(), None);
        let before = live_generation(&vault);
        vault.reindex().unwrap();
        assert_eq!(live_generation(&vault), before + 1);
        assert_eq!(
            mark(&vault, marks::Leg::Vector, marks::Repo::Main),
            vector_mark
        );
        drop(vault);
        let embedder = CountingEmbedder::new("model-x");
        let reader = open_search(
            scratch.vault(),
            scratch.options(),
            Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
        );
        indexed(&reader);
        let hits = reader
            .collection("c")
            .unwrap()
            .search(
                &SearchQuery::default()
                    .with_text("alpha mineral")
                    .with_k(10)
                    .with_lexical_weight(0.0),
                None,
            )
            .unwrap()
            .hits;
        assert_eq!(hits.len(), 1);
        assert!(embedder.slots() <= 1, "only the query is embedded");
    }

    #[cfg(feature = "find")]
    #[test]
    fn second_reindex_during_walk_is_locked() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                let err = other.reindex().unwrap_err();
                assert_locked_reindex(&err);
                done.wait();
            });
            vault.reindex().unwrap();
        });
    }

    #[cfg(feature = "find")]
    #[test]
    fn remove_during_the_build_is_applied_after_publish() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                other
                    .collection("c")
                    .unwrap()
                    .remove("alpha", None)
                    .unwrap();
                done.wait();
            });
            vault.reindex().unwrap();
        });
        indexed(&vault);
        assert!(find_ids(&vault, "c").is_empty());
    }

    #[cfg(feature = "find")]
    #[test]
    fn open_during_walk_does_not_prune_new_generation() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let before = live_generation(&vault);
        let next = scratch
            .vault()
            .join(".scheda")
            .join("index")
            .join((before + 1).to_string());
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                let opened = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
                assert!(
                    next.is_dir(),
                    "open must not prune N+1 while reindex.lock is held"
                );
                assert_eq!(live_generation(&opened), before);
                let ids = find_ids(&opened, "c");
                assert_eq!(ids, ["alpha"]);
                done.wait();
            });
            vault.reindex().unwrap();
        });
        assert_eq!(live_generation(&vault), before + 1);
        assert_eq!(find_ids(&vault, "c"), ["alpha"]);
    }

    /// The build reads commits: nothing another handle does to the files
    /// can race it, so no operation is refused while it runs.
    #[cfg(feature = "find")]
    #[test]
    fn admin_ops_run_during_reindex() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                other
                    .create_collection("x", document_schema(), None)
                    .unwrap();
                other.discard().unwrap();
                for err in [other.pull().unwrap_err(), other.sync().unwrap_err()] {
                    assert_ne!(err.known_code(), Some(Code::Locked), "{:?}", err.stack);
                }
                done.wait();
            });
            vault.reindex().unwrap();
        });
        drop(vault);
        let reopened = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        indexed(&reopened);
        assert!(reopened.collection("x").is_ok());
    }

    #[cfg(feature = "find")]
    #[test]
    fn reindex_inside_a_transaction_keeps_it_open() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.begin().unwrap();
        vault.reindex().unwrap();
        assert!(vault.holds_write_lock());
        vault.discard().unwrap();
        assert_eq!(find_ids(&vault, "c"), ["alpha"]);
    }

    fn live_dir(vault: &Vault) -> PathBuf {
        let current = fs::read_to_string(vault.index_dir().join("current")).unwrap();
        vault.index_dir().join(current.trim())
    }

    fn mark(vault: &Vault, leg: marks::Leg, repo: marks::Repo) -> Option<String> {
        marks::read(&live_dir(vault), leg, repo).unwrap()
    }

    fn head(dir: &Path) -> String {
        git::peel_head_oid(dir).unwrap().unwrap()
    }

    #[test]
    fn reindex_marks_each_built_leg_with_head() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.reindex().unwrap();
        let head = head(scratch.vault().as_path());
        assert_eq!(
            mark(&vault, marks::Leg::Ids, marks::Repo::Main),
            Some(head.clone())
        );
        let lexical = mark(&vault, marks::Leg::Lexical, marks::Repo::Main);
        if cfg!(feature = "find") {
            assert_eq!(lexical, Some(head));
        } else {
            assert_eq!(lexical, None);
        }
        assert_eq!(mark(&vault, marks::Leg::Vector, marks::Repo::Main), None);
        assert_eq!(mark(&vault, marks::Leg::Ids, marks::Repo::Isolated), None);
        assert_eq!(
            fs::read_to_string(live_dir(&vault).join("format"))
                .unwrap()
                .trim(),
            "6"
        );
    }

    #[test]
    fn reindex_marks_isolated_legs_with_nested_head() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let schema = Schema {
            isolated: Some(true),
            ..document_schema()
        };
        vault.create_collection("secret", schema, None).unwrap();
        vault
            .collection("secret")
            .unwrap()
            .add("alpha", empty_add(), None)
            .unwrap();
        vault.reindex().unwrap();
        let nested = head(&isolated::root(scratch.vault().as_path()));
        assert_ne!(nested, head(scratch.vault().as_path()));
        assert_eq!(
            mark(&vault, marks::Leg::Ids, marks::Repo::Isolated),
            Some(nested)
        );
    }

    #[cfg(feature = "search")]
    #[test]
    fn reindex_marks_vector_leg_only_with_an_embedder() {
        let scratch = Scratch::new();
        drop(seeded(&scratch));
        let embedder: Arc<dyn Embedder> = CountingEmbedder::new("m1");
        let vault = open_search(scratch.vault(), scratch.options(), Some(embedder));
        vault.reindex().unwrap();
        assert_eq!(
            mark(&vault, marks::Leg::Vector, marks::Repo::Main),
            Some(head(scratch.vault().as_path()))
        );
    }

    #[cfg(not(feature = "find"))]
    #[test]
    fn reindex_inside_transaction_writes_no_marks() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.begin().unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("beta", empty_add(), None)
            .unwrap();
        vault.reindex().unwrap();
        assert!(!live_dir(&vault).join("marks").exists());
        vault.discard().unwrap();
    }

    /// With `find` the build reads `HEAD`, so an open transaction's writes
    /// are neither indexed nor a reason to leave the marks out.
    #[cfg(feature = "find")]
    #[test]
    fn reindex_inside_a_transaction_indexes_committed_state_only() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.begin().unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("beta", empty_add(), None)
            .unwrap();
        vault.reindex().unwrap();
        let head = head(scratch.vault().as_path());
        assert_eq!(
            mark(&vault, marks::Leg::Lexical, marks::Repo::Main),
            Some(head)
        );
        assert_eq!(find_ids(&vault, "c"), ["alpha"]);
        vault.discard().unwrap();
    }

    #[cfg(not(feature = "find"))]
    #[test]
    fn reindex_with_uncommitted_leftovers_writes_no_marks() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.begin().unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("beta", empty_add(), None)
            .unwrap();
        drop(vault);
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(scratch.vault().join(".scheda").join("tx.log").is_file());
        vault.reindex().unwrap();
        assert!(!live_dir(&vault).join("marks").exists());
    }

    #[cfg(feature = "find")]
    #[test]
    fn journal_add_during_the_build_is_in_the_map_once_indexed() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("j", journal_schema(), None)
            .unwrap();
        vault.commit("schema").unwrap();
        vault
            .collection("j")
            .unwrap()
            .add("keep", empty_add(), None)
            .unwrap();
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                other
                    .collection("j")
                    .unwrap()
                    .add("queued", empty_add(), None)
                    .unwrap();
                done.wait();
            });
            vault.reindex().unwrap();
        });
        // Another handle's line: in the map once the indexer applied the
        // commit, as for any line that reached `HEAD` another way.
        indexed(&vault);
        let got = vault.collection("j").unwrap().get("queued").unwrap();
        assert_eq!(got.id.as_deref(), Some("queued"));
        let ids_path = vault
            .index_dir()
            .join(live_generation(&vault).to_string())
            .join("journal")
            .join("j")
            .join("ids");
        let text = fs::read_to_string(&ids_path).unwrap();
        let queued = text
            .lines()
            .filter(|line| line.contains("\"queued\""))
            .count();
        let keep = text
            .lines()
            .filter(|line| line.contains("\"keep\""))
            .count();
        assert_eq!(queued, 1, "{text}");
        assert_eq!(keep, 1, "{text}");
    }

    /// A consumer may delete `{vault}/.scheda/` under an open handle: the
    /// Index is derived data. `reindex` then builds a whole new one.
    #[test]
    fn reindex_after_deleting_the_scheda_directory_rebuilds_it() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault
            .create_collection("j", journal_schema(), None)
            .unwrap();
        vault
            .collection("j")
            .unwrap()
            .add("line", empty_add(), None)
            .unwrap();
        // The indexer writes under `.scheda/` after a commit: a caller that
        // deletes the directory waits for it first, or the delete races it.
        indexed(&vault);
        fs::remove_dir_all(scratch.vault().join(".scheda")).unwrap();
        let counts = vault.reindex().unwrap();
        assert_eq!(counts.records_read, Some(1));
        assert_eq!(counts.skipped_invalid, Some(0));
        assert!(live_dir(&vault).join("format").is_file());
        let got = vault.collection("j").unwrap().get("line").unwrap();
        assert_eq!(got.id.as_deref(), Some("line"));
    }

    #[test]
    fn reindex_of_a_vault_without_commits_writes_an_empty_generation() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let counts = vault.reindex().unwrap();
        assert_eq!(counts.records_read, Some(0));
        assert!(live_dir(&vault).join("format").is_file());
    }

    #[test]
    fn leftover_isolated_scheda_is_removed_by_reindex() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let leftover = scratch.vault().join(".isolated").join(".scheda");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("stale"), b"old").unwrap();
        vault.reindex().unwrap();
        assert!(!leftover.exists());
    }

    #[test]
    fn reindex_does_not_delete_tx_log() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.begin().unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("beta", empty_add(), None)
            .unwrap();
        let tx = scratch.vault().join(".scheda").join("tx.log");
        assert!(tx.is_file());
        vault.reindex().unwrap();
        assert!(tx.is_file(), "reindex must not delete tx.log");
        vault.discard().unwrap();
    }

    #[cfg(feature = "find")]
    #[test]
    fn leftover_queue_and_orphan_are_removed_on_open() {
        let scratch = Scratch::new();
        let writer = seeded(&scratch);
        writer.reindex().unwrap();
        let live = fs::read_to_string(writer.index_dir().join("current")).unwrap();
        let orphan = writer.reindex_without_publish().unwrap();
        let reindex = scratch.vault().join(".scheda").join("reindex");
        fs::create_dir_all(reindex.join("queue")).unwrap();
        fs::write(reindex.join("active"), b"99\n").unwrap();
        fs::write(
            reindex.join("queue").join("deadbeef"),
            b"{\"collection\":\"c\",\"id\":\"alpha\"}",
        )
        .unwrap();
        drop(writer);
        let opened = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        assert!(!orphan.exists());
        assert!(!reindex.exists(), "an older release's queue is dropped");
        assert_eq!(
            fs::read_to_string(opened.index_dir().join("current")).unwrap(),
            live
        );
    }

    #[cfg(unix)]
    #[cfg(feature = "find")]
    #[test]
    fn published_generation_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.reindex().unwrap();
        let generation = vault.index_dir().join(live_generation(&vault).to_string());
        let mode = fs::metadata(&generation).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn restore_of_an_uncommitted_journal_segment_forgets_its_lines() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("j", journal_schema(), None)
            .unwrap();
        vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        vault.commit("schema").unwrap();
        vault.begin().unwrap();
        vault
            .collection("j")
            .unwrap()
            .add("lost", empty_add(), None)
            .unwrap();
        let rel = format!("j/{:08x}-{}.jsonl", 1, vault.writer());
        let segment = scratch.vault().join(&rel);
        assert!(segment.is_file(), "uncommitted journal segment must exist");
        drop(vault);

        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("beta", empty_add(), None)
            .unwrap();
        assert!(
            !segment.is_file(),
            "restore must unlink the uncommitted journal segment"
        );
        let err = vault.collection("j").unwrap().get("lost").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::NotFound));
    }

    /// A writer in another process that died mid-transaction: it recorded
    /// `c/{id}` in `tx.log` and wrote the file, but never committed. The
    /// handle is dropped, which releases `write.lock` and keeps `tx.log`.
    fn leave_crash_leftover(scratch: &Scratch, id: &str) {
        let dead = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        dead.begin().unwrap();
        dead.collection("c")
            .unwrap()
            .add(id, empty_add(), None)
            .unwrap();
        drop(dead);
        let tx = scratch.vault().join(".scheda").join("tx.log");
        assert_eq!(fs::read_to_string(tx).unwrap(), format!("c/{id}\n"));
    }

    /// Files the last commit of the Vault touched.
    fn last_commit_files(dir: &Path) -> Vec<String> {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["show", "--name-only", "--format=", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// A handle opened before another process crashed still restores that
    /// process's leftovers on its next write, and commits only its own path.
    #[test]
    fn crash_leftover_of_another_handle_is_restored_by_the_next_write() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        leave_crash_leftover(&scratch, "ghost");
        let ghost = scratch.vault().join("c").join("ghost");
        assert!(
            ghost.is_file(),
            "the dead writer's file is in the work tree"
        );
        let c = vault.collection("c").unwrap();
        assert!(c.get("ghost").is_ok(), "uncommitted bytes are readable");
        c.add("beta", empty_add(), None).unwrap();
        assert!(!ghost.exists(), "the leftover is restored");
        assert!(
            !scratch.vault().join(".scheda").join("tx.log").exists(),
            "tx.log is emptied once the files are back"
        );
        assert_eq!(last_commit_files(&scratch.vault()), ["c/beta"]);
        assert_eq!(
            c.get("ghost").unwrap_err().known_code(),
            Some(Code::NotFound)
        );
    }

    /// `begin` on a handle opened before the crash restores the leftover too.
    #[test]
    fn begin_restores_the_leftover_of_another_handle() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        leave_crash_leftover(&scratch, "ghost");
        vault.begin().unwrap();
        assert!(!scratch.vault().join("c").join("ghost").exists());
        vault.discard().unwrap();
    }

    /// A restore that fails keeps the paths in `tx.log`; the next call that
    /// can restore them does.
    #[cfg(unix)]
    #[test]
    fn failed_restore_keeps_the_paths_in_tx_log() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        leave_crash_leftover(&scratch, "ghost");
        let dir = scratch.vault().join("c");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(dir.join("probe"), b"").is_ok() {
            // A superuser ignores the mode: the restore cannot be made to fail.
            fs::remove_file(dir.join("probe")).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let tx = scratch.vault().join(".scheda").join("tx.log");
        let c = vault.collection("c").unwrap();
        let err = c.add("beta", empty_add(), None).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::IoPermission), "{err:?}");
        assert_eq!(fs::read_to_string(&tx).unwrap(), "c/ghost\n");
        let err = vault.begin().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::IoPermission), "{err:?}");
        assert_eq!(fs::read_to_string(&tx).unwrap(), "c/ghost\n");
        assert!(!vault.holds_write_lock(), "a failed begin keeps no lock");

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        c.add("beta", empty_add(), None).unwrap();
        assert!(!dir.join("ghost").exists());
        assert!(!tx.exists());
        assert_eq!(last_commit_files(&scratch.vault()), ["c/beta"]);
    }

    /// The background indexer, started by `open` on threads that ask for it.
    #[cfg(feature = "find")]
    mod background {
        use std::process::{Child, Command};
        use std::time::{Duration, Instant};

        use super::*;

        const WAIT: Duration = Duration::from_secs(20);
        const CHILD_ENV: &str = "SCHEDA_TEST_INDEXER_CHILD";

        fn poll_fast() -> OpenOptions {
            OpenOptions::new().with_index_poll_ms(20)
        }

        fn poll_never() -> OpenOptions {
            OpenOptions::new().with_index_poll_ms(3_600_000)
        }

        fn open_indexed(path: &Path, options: OpenOptions) -> Vault {
            Vault::open(path, options, None).unwrap()
        }

        /// A handle that can take both roles: with `search`, a projector and
        /// an embedder of model `m1`; without it, the lexical role only.
        fn open_both_roles(path: &Path, options: OpenOptions) -> Vault {
            #[cfg(feature = "search")]
            {
                let embedder: Arc<dyn Embedder> = CountingEmbedder::new("m1");
                Vault::open_with(path, options, Some(embedder), Some(tag_projector())).unwrap()
            }
            #[cfg(not(feature = "search"))]
            Vault::open(path, options, None).unwrap()
        }

        fn head_of(dir: &Path) -> String {
            git::peel_head_oid(dir).unwrap().unwrap()
        }

        /// Commit `body` as `rel` with the `git` CLI, outside Scheda.
        fn git_cli_commit(repo: &Path, rel: &str, body: &str) {
            let path = repo.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            for args in [
                vec!["add", "-A"],
                vec![
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "hand",
                ],
            ] {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args(&args)
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }

        fn sorted_ids(vault: &Vault, name: &str) -> Vec<String> {
            let mut ids = find_ids(vault, name);
            ids.sort();
            ids
        }

        fn task_path(vault: &Vault) -> String {
            let indexer = vault.indexer.as_ref().expect("indexer started");
            let deadline = Instant::now() + WAIT;
            loop {
                if let Some(path) = indexer.task_path() {
                    return path;
                }
                assert!(Instant::now() < deadline, "indexer thread never started");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// A joined thread's `/proc` entry may linger a moment after `join`.
        fn assert_task_gone(task: &str) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Path::new(task).exists() {
                assert!(
                    Instant::now() < deadline,
                    "thread {task} outlived the last handle"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn commit_moves_the_lexical_mark_and_a_replay_is_harmless() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_never());
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            let collection = vault.collection("c").unwrap();
            collection.add("alpha", empty_add(), None).unwrap();
            let before = head_of(vault.path());
            vault.wait_indexed(WAIT).unwrap();
            collection.add("beta", empty_add(), None).unwrap();
            let head = head_of(vault.path());
            let status = vault.wait_indexed(WAIT).unwrap();
            assert!(status.lexical_role);
            assert_eq!(status.lexical.as_deref(), Some(head.as_str()));
            // What a crash before the mark's rename leaves: the old mark.
            marks::write(
                &live_dir(&vault),
                marks::Leg::Lexical,
                marks::Repo::Main,
                &before,
            )
            .unwrap();
            let status = vault.wait_indexed(WAIT).unwrap();
            assert_eq!(status.lexical.as_deref(), Some(head.as_str()));
            let page = collection.find(&crate::FindQuery::all(), 0, 10).unwrap();
            assert_eq!(page.total, Some(2));
            assert_eq!(sorted_ids(&vault, "c"), ["alpha", "beta"]);
        }

        #[test]
        fn open_catches_up_with_a_commit_made_by_the_git_cli() {
            let scratch = Scratch::new();
            drop(seeded(&scratch));
            git_cli_commit(&scratch.vault(), "c/hand", "");
            let vault = open_indexed(&scratch.vault(), poll_never());
            let status = vault.wait_indexed(WAIT).unwrap();
            assert_eq!(status.lexical, Some(head_of(vault.path())));
            assert_eq!(sorted_ids(&vault, "c"), ["alpha", "hand"]);
        }

        #[test]
        fn poll_picks_up_a_commit_made_outside_the_handle() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_fast());
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            git_cli_commit(&scratch.vault(), "c/hand", "x");
            let head = head_of(vault.path());
            let deadline = Instant::now() + WAIT;
            // `index_status` does not wake the indexer: only the poll can.
            while vault.index_status().unwrap().lexical.as_deref() != Some(head.as_str()) {
                assert!(Instant::now() < deadline, "poll never applied {head}");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(find_ids(&vault, "c"), ["hand"]);
        }

        #[test]
        fn nested_repository_commit_is_indexed() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_never());
            let schema = Schema {
                isolated: Some(true),
                ..document_schema()
            };
            vault.create_collection("secret", schema, None).unwrap();
            vault
                .collection("secret")
                .unwrap()
                .add("alpha", empty_add(), None)
                .unwrap();
            git_cli_commit(&isolated::root(vault.path()), "secret/hand", "h");
            let status = vault.wait_indexed(WAIT).unwrap();
            assert!(status.isolated_head.is_some());
            assert_eq!(status.isolated_lexical, status.isolated_head);
            assert_eq!(sorted_ids(&vault, "secret"), ["alpha", "hand"]);
        }

        #[test]
        fn format_4_generation_is_rebuilt_in_the_background() {
            let scratch = Scratch::new();
            let plain = seeded(&scratch);
            let old = live_dir(&plain);
            // A generation an older release wrote: format 4, no marks.
            fs::write(old.join("format"), "4\n").unwrap();
            fs::remove_dir_all(old.join("marks")).unwrap();
            drop(plain);
            let vault = open_indexed(&scratch.vault(), poll_never());
            vault.wait_indexed(WAIT).unwrap();
            let new = live_dir(&vault);
            assert_ne!(new, old);
            assert_eq!(fs::read_to_string(new.join("format")).unwrap().trim(), "6");
            assert_eq!(find_ids(&vault, "c"), ["alpha"]);
        }

        #[test]
        fn format_5_generation_is_rebuilt_in_the_background() {
            use tantivy::schema::{FAST, INDEXED, STORED, STRING, TEXT};
            let scratch = Scratch::new();
            let plain = seeded(&scratch);
            let old = live_dir(&plain);
            drop(plain);
            // A generation a format 5 release wrote: no `num` field.
            fs::write(old.join("format"), "5\n").unwrap();
            let lexical = old.join("lexical");
            fs::remove_dir_all(&lexical).unwrap();
            fs::create_dir_all(&lexical).unwrap();
            let mut builder = tantivy::schema::Schema::builder();
            for name in ["key", "collection", "id", "path"] {
                builder.add_text_field(name, STRING | STORED);
            }
            builder.add_text_field("text", TEXT);
            for name in ["eq", "exists", "null"] {
                builder.add_text_field(name, STRING);
            }
            builder.add_text_field("tok", TEXT);
            builder.add_u64_field("ord", FAST | INDEXED | STORED);
            tantivy::Index::create_in_dir(&lexical, builder.build()).unwrap();
            let vault = open_indexed(&scratch.vault(), poll_never());
            let deadline = std::time::Instant::now() + WAIT;
            while live_dir(&vault) == old {
                assert!(
                    std::time::Instant::now() < deadline,
                    "no new generation published"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            vault.wait_indexed(WAIT).unwrap();
            let new = live_dir(&vault);
            assert_eq!(fs::read_to_string(new.join("format")).unwrap().trim(), "6");
            assert_eq!(find_ids(&vault, "c"), ["alpha"]);
        }

        #[test]
        fn dropping_the_last_handle_joins_the_thread() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_never());
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            let collection = vault.collection("c").unwrap();
            let task = task_path(&vault);
            assert!(Path::new(&task).exists());
            drop(vault);
            assert!(
                Path::new(&task).exists(),
                "a collection keeps the indexer alive"
            );
            drop(collection);
            assert_task_gone(&task);
        }

        #[test]
        fn two_handles_one_lexical_role_and_hand_over() {
            let scratch = Scratch::new();
            let first = open_indexed(&scratch.vault(), poll_fast());
            first
                .create_collection("c", document_schema(), None)
                .unwrap();
            first.wait_indexed(WAIT).unwrap();
            let second = open_indexed(&scratch.vault(), poll_fast());
            second.wait_indexed(WAIT).unwrap();
            assert!(first.index_status().unwrap().lexical_role);
            assert!(!second.index_status().unwrap().lexical_role);
            drop(first);
            git_cli_commit(&scratch.vault(), "c/after", "");
            let status = second.wait_indexed(WAIT).unwrap();
            assert!(status.lexical_role);
            assert_eq!(status.lexical, Some(head_of(second.path())));
            assert_eq!(find_ids(&second, "c"), ["after"]);
        }

        /// `exclude_vector_index` of `name` as `info` reports it.
        fn excludes_vectors(vault: &Vault, name: &str) -> Option<bool> {
            vault
                .info()
                .unwrap()
                .collections
                .iter()
                .find(|item| item.name.as_deref() == Some(name))
                .and_then(|item| item.schema.exclude_vector_index)
        }

        fn names_in_info(vault: &Vault) -> Vec<String> {
            vault
                .info()
                .unwrap()
                .collections
                .iter()
                .filter_map(|item| item.name.clone())
                .collect()
        }

        #[test]
        fn a_collection_another_handle_declares_is_found_without_reopen() {
            let scratch = Scratch::new();
            drop(open_indexed(&scratch.vault(), poll_never()));
            let by_name = open_indexed(&scratch.vault(), poll_never());
            let by_find = open_indexed(&scratch.vault(), poll_never());
            let writer = open_indexed(&scratch.vault(), poll_never());
            let missing = by_name.collection("x").unwrap_err();
            assert_eq!(missing.known_code(), Some(Code::NotFound));
            writer
                .create_collection("x", document_schema(), None)
                .unwrap()
                .add("alpha", empty_add(), None)
                .unwrap();
            assert_eq!(by_name.collection("x").unwrap().name(), "x");
            assert!(!names_in_info(&by_find).iter().any(|name| name == "x"));
            // Nobody indexed yet: an Index code is fine, `not_found` is not.
            let found = by_find.find(&["x"], &crate::FindQuery::all(), 0, 10);
            assert_resolved(found.map(|_| ()));
            assert!(names_in_info(&by_find).iter().any(|name| name == "x"));
        }

        /// The query got past name resolution.
        fn assert_resolved(result: Result<(), Error>) {
            if let Err(err) = result {
                assert_ne!(err.known_code(), Some(Code::NotFound), "{err:?}");
            }
        }

        /// A collection a Filter leaf qualifies counts as a name to look up.
        #[cfg(feature = "search")]
        #[test]
        fn a_filter_qualifier_declared_after_open_is_found() {
            let scratch = Scratch::new();
            drop(open_search(scratch.vault(), poll_never(), None));
            let reader = open_search(scratch.vault(), poll_never(), None);
            let writer = open_search(scratch.vault(), poll_never(), None);
            writer
                .create_collection("x", document_schema(), None)
                .unwrap()
                .add("alpha", rec_tag("red"), None)
                .unwrap();
            let filter: crate::Filter = serde_json::from_value(serde_json::json!(
                {"term": {"field": "tag", "collection": "x", "value": "red"}}
            ))
            .unwrap();
            let found = reader.find(&[], &crate::FindQuery::from(filter), 0, 10);
            assert_resolved(found.map(|_| ()));
            assert!(names_in_info(&reader).iter().any(|name| name == "x"));
        }

        /// Child of `a_collection_another_process_declares_is_found_without_reopen`:
        /// declares `x`, writes one Record, reports done.
        #[test]
        #[ignore = "run as the child process of a_collection_another_process_declares_is_found_without_reopen"]
        fn declaring_child_process() {
            let Some(root) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
                return;
            };
            crate::git::isolate_test_git_env();
            let vault = open_indexed(&root.join("vault"), poll_never());
            vault
                .create_collection("x", document_schema(), None)
                .unwrap()
                .add("alpha", empty_add(), None)
                .unwrap();
            fs::write(root.join("done"), b"").unwrap();
        }

        #[test]
        fn a_collection_another_process_declares_is_found_without_reopen() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_never());
            vault.wait_indexed(WAIT).unwrap();
            assert!(vault.collection("x").is_err());
            let mut child = spawn_child(
                &scratch.root,
                "vault::tests::background::declaring_child_process",
            );
            wait_for_child_file(&mut child, &scratch.root, "done");
            let found = vault.collection("x").unwrap();
            assert_eq!(found.get("alpha").unwrap().id.as_deref(), Some("alpha"));
        }

        /// A changed declaration of a collection the handle knows, read
        /// again once the Index passed the commit: by the handle that
        /// indexes and by one that only reads.
        #[test]
        fn a_changed_declaration_is_seen_after_wait_indexed() {
            let scratch = Scratch::new();
            let indexing = open_indexed(&scratch.vault(), poll_never());
            indexing
                .create_collection("c", document_schema(), None)
                .unwrap();
            indexing.wait_indexed(WAIT).unwrap();
            let reading = open_indexed(&scratch.vault(), poll_never());
            reading.wait_indexed(WAIT).unwrap();
            assert!(indexing.index_status().unwrap().lexical_role);
            assert!(!reading.index_status().unwrap().lexical_role);
            let writer = open_indexed(&scratch.vault(), poll_never());
            let changed = Schema {
                exclude_vector_index: Some(true),
                ..document_schema()
            };
            writer.create_collection("c", changed, None).unwrap();
            for vault in [&indexing, &reading] {
                assert_ne!(excludes_vectors(vault, "c"), Some(true));
            }
            for vault in [&indexing, &reading] {
                vault.wait_indexed(WAIT).unwrap();
                assert_eq!(excludes_vectors(vault, "c"), Some(true));
            }
        }

        /// A reload brought by the Index reads the work tree, so this
        /// handle's own uncommitted declaration stays.
        #[test]
        fn a_reload_in_an_open_transaction_keeps_its_own_declaration() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_never());
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            vault.begin().unwrap();
            vault
                .create_collection("y", document_schema(), None)
                .unwrap();
            // Another writer's commit, made while this handle holds the
            // transaction: only the git CLI can, and it commits `x` alone.
            let rel = "x/.collection.yaml";
            fs::create_dir_all(scratch.vault().join("x")).unwrap();
            fs::write(
                scratch.vault().join(rel),
                "version: 1\nname: x\nschema:\n  document: {}\n",
            )
            .unwrap();
            for args in [
                vec!["add", "--", rel],
                vec![
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "hand",
                    "--",
                    rel,
                ],
            ] {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(scratch.vault())
                    .args(&args)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{out:?}");
            }
            assert!(!names_in_info(&vault).iter().any(|name| name == "x"));
            vault.wait_indexed(WAIT).unwrap();
            let names = names_in_info(&vault);
            assert!(names.iter().any(|name| name == "x"), "{names:?}");
            assert!(names.iter().any(|name| name == "y"), "{names:?}");
            vault.commit("declare y").unwrap();
            assert_eq!(vault.collection("y").unwrap().name(), "y");
            assert!(last_commit_files(&scratch.vault()).contains(&"y/.collection.yaml".to_owned()));
        }

        /// A Vault with `c` holding `alpha` (tag `red`), indexed with the
        /// tag projector; the handle is dropped.
        #[cfg(feature = "search")]
        fn seeded_with_tags(scratch: &Scratch) {
            let vault = open_search(scratch.vault(), poll_never(), None);
            vault
                .create_collection("c", document_schema(), None)
                .unwrap()
                .add("alpha", rec_tag("red"), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
        }

        #[cfg(feature = "search")]
        fn search_c(vault: &Vault) -> Result<crate::SearchHits, Error> {
            vault.search(&["c"], &SearchQuery::default().with_text("red"), None)
        }

        /// A generation of another format: `index_version` that a repeat
        /// fixes, since the indexer rebuilds it.
        #[cfg(feature = "search")]
        #[test]
        fn another_format_is_index_version_a_repeat_fixes() {
            let scratch = Scratch::new();
            seeded_with_tags(&scratch);
            let root = scratch.vault().join(".scheda").join("index");
            let live = crate::index::current_dir(&root).unwrap().unwrap();
            fs::write(live.join("format"), "5\n").unwrap();
            // Held here, the rebuild waits: the first query sees format 5.
            let rebuild = crate::reindex::acquire_lock(&scratch.vault()).unwrap();
            let vault = open_search(scratch.vault(), poll_never(), None);
            let err = search_c(&vault).unwrap_err();
            assert_eq!(err.known_code(), Some(Code::IndexVersion), "{err:?}");
            assert_eq!(err.known_repeat(), Some(crate::Repeat::Allowed));
            drop(rebuild);
            vault.wait_indexed(WAIT).unwrap();
            let hits = search_c(&vault).unwrap();
            assert_eq!(hits.hits.len(), 1);
        }

        /// A handle without the generation's projector: `index_version`
        /// that no repeat fixes, from `search` with or without a Filter
        /// and from `find`.
        #[cfg(feature = "search")]
        #[test]
        fn another_projector_is_index_version_a_repeat_does_not_fix() {
            let scratch = Scratch::new();
            seeded_with_tags(&scratch);
            let bare = open_indexed(&scratch.vault(), poll_never());
            let forbidden = |err: Error| {
                assert_eq!(err.known_code(), Some(Code::IndexVersion), "{err:?}");
                assert_eq!(err.known_repeat(), Some(crate::Repeat::Forbidden));
                assert!(
                    err.message.as_deref().unwrap_or("").contains("reindex"),
                    "{err:?}"
                );
            };
            forbidden(search_c(&bare).unwrap_err());
            let term: crate::Filter = serde_json::from_value(serde_json::json!(
                {"term": {"field": "id", "value": "alpha"}}
            ))
            .unwrap();
            let filtered = SearchQuery {
                filter: term.into(),
                ..SearchQuery::default().with_text("red")
            };
            forbidden(bare.search(&["c"], &filtered, None).unwrap_err());
            forbidden(
                bare.find(&["c"], &crate::FindQuery::all(), 0, 10)
                    .unwrap_err(),
            );
            // Repeating changes nothing; `reindex` from this handle moves
            // the Index to its projector.
            forbidden(search_c(&bare).unwrap_err());
            bare.reindex().unwrap();
            search_c(&bare).unwrap();
        }

        /// Child of `indexer_role_passes_to_another_process`: takes the
        /// lexical role, reports ready, and holds it until killed (at most
        /// one minute). Never polls, so it ignores the parent's commits.
        #[test]
        #[ignore = "run as the child process of indexer_role_passes_to_another_process"]
        fn indexer_child_process() {
            let Some(root) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
                return;
            };
            crate::git::isolate_test_git_env();
            let vault = open_both_roles(&root.join("vault"), poll_never());
            vault.wait_indexed(WAIT).unwrap();
            // Marks may already be at HEAD: wait for this handle's roles.
            wait_for_roles(&vault);
            fs::write(root.join("ready"), b"").unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        struct KillOnDrop(Child);

        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        #[test]
        fn indexer_role_passes_to_another_process() {
            let scratch = Scratch::new();
            Vault::open(scratch.vault(), scratch.options(), None)
                .unwrap()
                .create_collection("c", document_schema(), None)
                .unwrap();
            let mut child = spawn_child(
                &scratch.root,
                "vault::tests::background::indexer_child_process",
            );
            wait_for_child_file(&mut child, &scratch.root, "ready");
            let vault = open_both_roles(&scratch.vault(), poll_fast());
            vault
                .collection("c")
                .unwrap()
                .add("alpha", br#"{"tag":"alpha"}"#, None)
                .unwrap();
            // The child holds both roles and never polls: nothing moves.
            let err = vault.wait_indexed(Duration::from_millis(300)).unwrap_err();
            assert_eq!(err.known_code(), Some(Code::IoTimedOut));
            let status = vault.index_status().unwrap();
            assert!(!status.lexical_role);
            assert!(!status.vector_role);
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            let status = match vault.wait_indexed(WAIT) {
                Ok(status) => status,
                Err(err) => panic!("{err:?}; status {:?}", vault.index_status()),
            };
            let head = head_of(vault.path());
            assert!(status.lexical_role);
            assert_eq!(status.lexical.as_deref(), Some(head.as_str()));
            if cfg!(feature = "search") {
                assert!(status.vector_role);
                assert_eq!(status.vector.as_deref(), Some(head.as_str()));
            }
            assert_eq!(find_ids(&vault, "c"), ["alpha"]);
        }

        #[cfg(feature = "search")]
        fn open_with_model(path: &Path, model: Option<&'static str>) -> Vault {
            let embedder = model.map(|model| CountingEmbedder::new(model) as Arc<dyn Embedder>);
            Vault::open_with(path, poll_fast(), embedder, Some(tag_projector())).unwrap()
        }

        #[cfg(feature = "search")]
        #[test]
        fn vector_role_goes_to_one_handle_with_a_matching_embedder() {
            let scratch = Scratch::new();
            {
                let plain = open_search(scratch.vault(), scratch.options(), None);
                plain
                    .create_collection("c", document_schema(), None)
                    .unwrap();
                plain
                    .collection("c")
                    .unwrap()
                    .add("r1", rec_tag("first"), None)
                    .unwrap();
            }
            let without = open_with_model(&scratch.vault(), None);
            let status = without.wait_indexed(WAIT).unwrap();
            assert!(status.lexical_role);
            assert!(!status.vector_role);
            assert_eq!(status.vector, None, "nobody can write vectors yet");
            assert_eq!(status.lexical_pending, Some(0));
            assert_eq!(status.vector_pending, Some(1), "the lag shows instead");
            assert!(!status.model_mismatch, "no embedder, nothing to mismatch");
            let first = open_with_model(&scratch.vault(), Some("m1"));
            let status = first.wait_indexed(WAIT).unwrap();
            assert!(status.vector_role);
            assert_eq!(status.vector, Some(head_of(first.path())));
            let second = open_with_model(&scratch.vault(), Some("m1"));
            let other_model = open_with_model(&scratch.vault(), Some("m2"));
            second.wait_indexed(WAIT).unwrap();
            other_model.wait_indexed(WAIT).unwrap();
            assert!(!second.index_status().unwrap().vector_role);
            assert!(!second.index_status().unwrap().model_mismatch);
            let status = other_model.index_status().unwrap();
            assert!(!status.vector_role);
            assert!(status.model_mismatch);
            assert_eq!(status.vector_pending, Some(0));
            drop(first);
            git_cli_commit(&scratch.vault(), "c/r2", r#"{"tag":"second"}"#);
            let status = second.wait_indexed(WAIT).unwrap();
            assert!(status.vector_role);
            assert_eq!(status.vector, Some(head_of(second.path())));
            assert!(!other_model.index_status().unwrap().vector_role);
        }

        /// Re-run this test binary as `test`, a child holding `CHILD_ENV`
        /// = `root`. Output goes to `root/child.log`.
        fn spawn_child(root: &Path, test: &str) -> KillOnDrop {
            let log = fs::File::create(root.join("child.log")).unwrap();
            KillOnDrop(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        test,
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(CHILD_ENV, root)
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
            )
        }

        /// Wait for the child to create `root/name`; fail if it exits first.
        fn wait_for_child_file(child: &mut KillOnDrop, root: &Path, name: &str) {
            let deadline = Instant::now() + WAIT;
            while !root.join(name).exists() {
                let exited = child.0.try_wait().unwrap();
                let log = fs::read_to_string(root.join("child.log")).unwrap_or_default();
                assert!(exited.is_none(), "child exited early: {log}");
                assert!(Instant::now() < deadline, "child never wrote {name}: {log}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// Wait for `vault`'s own indexer to hold the roles a handle of this
        /// build can take.
        fn wait_for_roles(vault: &Vault) {
            let deadline = Instant::now() + WAIT;
            loop {
                let status = vault.index_status().unwrap();
                if status.lexical_role && status.vector_role == cfg!(feature = "search") {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "handle never took its roles: {status:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[cfg(feature = "search")]
        fn vector_hits(vault: &Vault, name: &str, text: &str) -> Vec<String> {
            vault
                .collection(name)
                .unwrap()
                .search(
                    &SearchQuery::default()
                        .with_text(text)
                        .with_k(10)
                        .with_lexical_weight(0.0),
                    None,
                )
                .unwrap()
                .hits
                .into_iter()
                .filter_map(|hit| hit.id)
                .collect()
        }

        #[cfg(feature = "search")]
        fn open_reader(path: &Path, options: OpenOptions) -> Vault {
            let embedder: Arc<dyn Embedder> = CountingEmbedder::new("m1");
            Vault::open_with(path, options, Some(embedder), Some(tag_projector())).unwrap()
        }

        /// Child of `reader_in_another_process_follows_the_marks`: takes
        /// both roles and reports `ready`; on `go`, adds `r2` and waits for
        /// its own Index, reports `done`, and holds the roles until killed
        /// (at most one minute).
        #[cfg(feature = "search")]
        #[test]
        #[ignore = "run as the child process of reader_in_another_process_follows_the_marks"]
        fn index_writer_child_process() {
            let Some(root) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
                return;
            };
            crate::git::isolate_test_git_env();
            let vault = open_both_roles(&root.join("vault"), poll_fast());
            vault.wait_indexed(WAIT).unwrap();
            wait_for_roles(&vault);
            fs::write(root.join("ready"), b"").unwrap();
            let deadline = Instant::now() + WAIT;
            while !root.join("go").exists() {
                assert!(Instant::now() < deadline, "parent never said go");
                std::thread::sleep(Duration::from_millis(10));
            }
            vault
                .collection("c")
                .unwrap()
                .add("r2", rec_tag("beta"), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            fs::write(root.join("done"), b"").unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        #[cfg(feature = "search")]
        #[test]
        fn reader_in_another_process_follows_the_marks() {
            let scratch = Scratch::new();
            {
                let setup = open_search(scratch.vault(), scratch.options(), None);
                setup
                    .create_collection("c", document_schema(), None)
                    .unwrap();
                setup
                    .collection("c")
                    .unwrap()
                    .add("r1", rec_tag("alpha"), None)
                    .unwrap();
            }
            let mut child = spawn_child(
                &scratch.root,
                "vault::tests::background::index_writer_child_process",
            );
            wait_for_child_file(&mut child, &scratch.root, "ready");
            // Opened before the write. `lazy` never polls, so only a query
            // can refresh it; `polled` is never queried before its check.
            let lazy = open_reader(&scratch.vault(), poll_never());
            let polled = open_reader(&scratch.vault(), poll_fast());
            for reader in [&lazy, &polled] {
                reader.wait_indexed(WAIT).unwrap();
                let status = reader.index_status().unwrap();
                assert!(!status.lexical_role && !status.vector_role, "{status:?}");
            }
            assert_eq!(vector_hits(&lazy, "c", "beta"), ["r1"]);
            let loaded = lazy.search.held_vector_seq();
            fs::write(scratch.root.join("go"), b"").unwrap();
            wait_for_child_file(&mut child, &scratch.root, "done");

            let head = head_of(lazy.path());
            let status = lazy.wait_indexed(WAIT).unwrap();
            assert_eq!(status.lexical.as_deref(), Some(head.as_str()));
            assert_eq!(status.vector.as_deref(), Some(head.as_str()));
            assert_eq!(sorted_ids(&lazy, "c"), ["r1", "r2"]);
            assert_eq!(
                lazy.search.held_vector_seq(),
                loaded,
                "a handle that never polls must not have reloaded yet"
            );
            let mut hits = vector_hits(&lazy, "c", "beta");
            hits.sort();
            assert_eq!(hits, ["r1", "r2"], "the query reloads USearch");

            polled.wait_indexed(WAIT).unwrap();
            let deadline = Instant::now() + WAIT;
            while polled.search.held_vector_seq() != polled.search.disk_vector_seq() {
                assert!(Instant::now() < deadline, "the poll never reloaded USearch");
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut hits = vector_hits(&polled, "c", "beta");
            hits.sort();
            assert_eq!(hits, ["r1", "r2"]);
            assert_eq!(sorted_ids(&polled, "c"), ["r1", "r2"]);
        }

        /// Embeds two texts per call and counts the Records the projector
        /// is asked about.
        #[cfg(feature = "search")]
        struct SmallBatchEmbedder;

        #[cfg(feature = "search")]
        impl Embedder for SmallBatchEmbedder {
            fn model_id(&self) -> &str {
                "m1"
            }

            fn dimensions(&self) -> NonZeroUsize {
                NonZeroUsize::new(4).unwrap()
            }

            fn many_limit(&self) -> NonZeroUsize {
                NonZeroUsize::new(2).unwrap()
            }

            fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
                Ok(texts.iter().copied().map(counting_vector).collect())
            }
        }

        /// [`TagProjector`] that counts `project` calls.
        #[cfg(feature = "search")]
        struct CountingProjector {
            projects: AtomicU64,
        }

        #[cfg(feature = "search")]
        impl crate::Projector for CountingProjector {
            fn schema(&self, collection: &str) -> Result<crate::IndexSchema, Error> {
                TagProjector.schema(collection)
            }

            fn project(
                &self,
                collection: &str,
                id: &str,
                raw: &[u8],
            ) -> Result<crate::IndexDoc, Error> {
                self.projects.fetch_add(1, Ordering::Relaxed);
                TagProjector.project(collection, id, raw)
            }
        }

        /// A pass over many embedder batches saves the vector dump once, not
        /// once per batch, and each Record is projected once per leg.
        #[cfg(feature = "search")]
        #[test]
        fn a_pass_over_many_batches_saves_the_vector_dump_once() {
            const RECORDS: u64 = 20;
            let scratch = Scratch::new();
            let projector = Arc::new(CountingProjector {
                projects: AtomicU64::new(0),
            });
            let embedder: Arc<dyn Embedder> = Arc::new(SmallBatchEmbedder);
            let vault = Vault::open_with(
                scratch.vault(),
                poll_never(),
                Some(embedder),
                Some(Arc::clone(&projector) as Arc<dyn crate::Projector>),
            )
            .unwrap();
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            let saves = vault.search.vector_saves();
            let collection = vault.collection("c").unwrap();
            vault.begin().unwrap();
            for i in 0..RECORDS {
                collection
                    .add(&format!("row-{i:02}"), rec_tag(&format!("tag {i}")), None)
                    .unwrap();
            }
            let projected = projector.projects.load(Ordering::Relaxed);
            vault.commit("rows").unwrap();
            let status = vault.wait_indexed(WAIT).unwrap();
            assert_eq!(
                status.vector.as_deref(),
                Some(head_of(vault.path()).as_str())
            );
            assert_eq!(
                vault.search.vector_saves() - saves,
                1,
                "{RECORDS} Records in batches of two save the dump once"
            );
            assert_eq!(
                projector.projects.load(Ordering::Relaxed) - projected,
                2 * RECORDS,
                "one projection per Record for each of the two legs"
            );
            assert_eq!(vector_hits(&vault, "c", "tag 3").len(), 10);
        }

        /// The role passes to a handle opened before the last holder wrote:
        /// it must write on top of that holder's vectors and journal order,
        /// not over them with what it loaded at open.
        #[test]
        fn a_handle_taking_a_role_over_keeps_the_last_holders_work() {
            let scratch = Scratch::new();
            let first = open_both_roles(&scratch.vault(), poll_fast());
            first
                .create_collection("c", document_schema(), None)
                .unwrap();
            first
                .create_collection("j", journal_schema(), None)
                .unwrap();
            wait_for_roles(&first);
            let second = open_both_roles(&scratch.vault(), poll_fast());
            first
                .collection("c")
                .unwrap()
                .add("r1", tagged("alpha"), None)
                .unwrap();
            let journal = first.collection("j").unwrap();
            journal.add("l1", tagged("one"), None).unwrap();
            journal.add("l2", tagged("two"), None).unwrap();
            first.wait_indexed(WAIT).unwrap();
            drop(journal);
            first.close().unwrap();

            second
                .collection("c")
                .unwrap()
                .add("r2", tagged("beta"), None)
                .unwrap();
            second
                .collection("j")
                .unwrap()
                .add("l3", tagged("three"), None)
                .unwrap();
            second.wait_indexed(WAIT).unwrap();
            wait_for_roles(&second);
            second.wait_indexed(WAIT).unwrap();
            assert_eq!(find_ids(&second, "j"), ["l1", "l2", "l3"]);
            #[cfg(feature = "search")]
            {
                let mut hits = vector_hits(&second, "c", "alpha");
                hits.sort();
                assert_eq!(hits, ["r1", "r2"], "r1's vector survives the hand-over");
            }
        }

        /// Child of `split_roles_in_two_processes_keep_journal_order`: holds
        /// the lexical role only (no embedder, so never the vector role)
        /// and reports `ready` once it has it; held until killed (at most
        /// one minute).
        #[cfg(feature = "search")]
        #[test]
        #[ignore = "run as the child process of split_roles_in_two_processes_keep_journal_order"]
        fn lexical_only_child_process() {
            let Some(root) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
                return;
            };
            crate::git::isolate_test_git_env();
            let vault =
                Vault::open_with(root.join("vault"), poll_fast(), None, Some(tag_projector()))
                    .unwrap();
            let deadline = Instant::now() + WAIT;
            while !vault.index_status().unwrap().lexical_role {
                assert!(
                    Instant::now() < deadline,
                    "child never took the lexical role"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            fs::write(root.join("ready"), b"").unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        /// `index.lock` and `vector.lock` held by two processes: the vector
        /// process never writes `ord.json`, so journal lines keep their
        /// append order and unique `ord` values through several rounds.
        #[cfg(feature = "search")]
        #[test]
        fn split_roles_in_two_processes_keep_journal_order() {
            let scratch = Scratch::new();
            {
                let setup = open_search(scratch.vault(), scratch.options(), None);
                setup
                    .create_collection("j", journal_schema(), None)
                    .unwrap();
            }
            let mut child = spawn_child(
                &scratch.root,
                "vault::tests::background::lexical_only_child_process",
            );
            wait_for_child_file(&mut child, &scratch.root, "ready");
            let vector = open_reader(&scratch.vault(), poll_fast());
            let deadline = Instant::now() + WAIT;
            while !vector.index_status().unwrap().vector_role {
                assert!(Instant::now() < deadline, "vector role never taken");
                std::thread::sleep(Duration::from_millis(10));
            }
            let status = vector.index_status().unwrap();
            assert!(
                !status.lexical_role,
                "the child holds index.lock: {status:?}"
            );
            // Holds no role (the child and `vector` have them): only appends.
            let writer = open_search(scratch.vault(), poll_never(), None);

            let ord_file = live_dir(&vector).join("ord.json");
            let mut expected = Vec::new();
            for round in 0..3 {
                let author = if round == 1 { &writer } else { &vector };
                let journal = author.collection("j").unwrap();
                for n in 0..2 {
                    let id = format!("l{}", round * 2 + n + 1);
                    journal.add(&id, tagged(&id), None).unwrap();
                    expected.push(id);
                }
                vector.wait_indexed(WAIT).unwrap();
                assert_eq!(find_ids(&vector, "j"), expected, "round {round}");
                assert!(ord_file.is_file(), "ord.json vanished in round {round}");
            }
            let ord: std::collections::BTreeMap<String, u64> =
                serde_json::from_slice(&fs::read(&ord_file).unwrap()).unwrap();
            assert_eq!(
                ord,
                std::collections::BTreeMap::from([(String::from("j"), 6)]),
                "one counter step per line"
            );
            let mut hits = vector_hits(&vector, "j", "l1");
            hits.sort();
            assert_eq!(hits, ["l1", "l2", "l3", "l4", "l5", "l6"]);
            assert!(vector.index_status().unwrap().vector_role);
        }

        fn tagged(text: &str) -> Vec<u8> {
            format!(r#"{{"tag":"{text}"}}"#).into_bytes()
        }

        /// Fails its first `fails` batches, then embeds like
        /// [`CountingEmbedder`] once [`release`](Self::release) is called,
        /// so a test can see the error before the retry clears it. Counts
        /// every call.
        #[cfg(feature = "search")]
        struct FlakyEmbedder {
            fails: u64,
            calls: std::sync::Mutex<Vec<Instant>>,
            released: std::sync::Mutex<bool>,
            release: std::sync::Condvar,
        }

        #[cfg(feature = "search")]
        impl FlakyEmbedder {
            fn new(fails: u64) -> Arc<Self> {
                Arc::new(Self {
                    fails,
                    calls: std::sync::Mutex::new(Vec::new()),
                    released: std::sync::Mutex::new(false),
                    release: std::sync::Condvar::new(),
                })
            }

            fn release(&self) {
                *self.released.lock().unwrap() = true;
                self.release.notify_all();
            }

            fn calls(&self) -> Vec<Instant> {
                self.calls.lock().unwrap().clone()
            }
        }

        #[cfg(feature = "search")]
        impl Embedder for FlakyEmbedder {
            fn model_id(&self) -> &str {
                "m1"
            }

            fn dimensions(&self) -> NonZeroUsize {
                NonZeroUsize::new(4).unwrap()
            }

            fn many_limit(&self) -> NonZeroUsize {
                NonZeroUsize::new(32).unwrap()
            }

            fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
                let call = {
                    let mut calls = self.calls.lock().unwrap();
                    calls.push(Instant::now());
                    calls.len() as u64
                };
                if call <= self.fails {
                    return Err(Error::invalid("embedding service unavailable"));
                }
                let released = self.released.lock().unwrap();
                drop(
                    self.release
                        .wait_timeout_while(released, WAIT, |released| !*released)
                        .unwrap(),
                );
                Ok(texts.iter().copied().map(counting_vector).collect())
            }

            fn embed_query(&self, text: &str) -> Result<Vec<f32>, Error> {
                Ok(counting_vector(text))
            }
        }

        #[cfg(feature = "search")]
        #[test]
        fn embedder_fails_twice_then_vectors_land_and_the_error_showed_between() {
            let scratch = Scratch::new();
            let flaky = FlakyEmbedder::new(2);
            let vault = Vault::open_with(
                scratch.vault(),
                poll_never(),
                Some(Arc::clone(&flaky) as Arc<dyn Embedder>),
                Some(tag_projector()),
            )
            .unwrap();
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            vault
                .collection("c")
                .unwrap()
                .add("r1", rec_tag("secret-body"), None)
                .expect("a failing embedder must not fail the write");
            let head = head_of(vault.path());
            let deadline = Instant::now() + WAIT;
            let failed = loop {
                let status = vault.index_status().unwrap();
                if let Some(err) = status.last_error.clone() {
                    break (status, err);
                }
                assert!(Instant::now() < deadline, "no error reported: {status:?}");
                std::thread::sleep(Duration::from_millis(5));
            };
            let (status, err) = failed;
            assert_eq!(err.known_code(), Some(Code::Invalid));
            assert!(
                !format!("{err:?}").contains("secret-body"),
                "the error must not carry the Record: {err:?}"
            );
            assert_eq!(
                status.lexical.as_deref(),
                Some(head.as_str()),
                "the lexical leg does not wait for the embedder"
            );
            assert_ne!(status.vector.as_deref(), Some(head.as_str()));
            assert_eq!(
                find_ids(&vault, "c"),
                ["r1"],
                "find works while vectors fail"
            );

            flaky.release();
            let status = vault.wait_indexed(WAIT).unwrap();
            assert_eq!(status.vector.as_deref(), Some(head.as_str()));
            assert!(status.last_error.is_none(), "{status:?}");
            assert_eq!(status.vector_pending, Some(0));
            let calls = flaky.calls();
            assert_eq!(calls.len(), 3, "two failures and one success");
            // `wait_indexed` asks for a round every 50 ms; the retries still
            // keep to the backoff.
            let first = crate::indexer::BACKOFF_FIRST;
            assert!(calls[1] - calls[0] >= first, "{calls:?}");
            assert!(calls[2] - calls[1] >= first * 2, "{calls:?}");
            assert_eq!(vector_hits(&vault, "c", "secret-body"), ["r1"]);
        }

        #[cfg(feature = "search")]
        #[test]
        fn a_failing_embedder_holds_back_only_the_vector_leg() {
            let scratch = Scratch::new();
            let flaky = FlakyEmbedder::new(u64::MAX);
            let vault = Vault::open_with(
                scratch.vault(),
                poll_fast(),
                Some(Arc::clone(&flaky) as Arc<dyn Embedder>),
                Some(tag_projector()),
            )
            .unwrap();
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            vault
                .collection("c")
                .unwrap()
                .add("r1", rec_tag("one"), None)
                .unwrap();
            // Five failures: the vector leg now waits 1.6 s.
            let deadline = Instant::now() + WAIT;
            while flaky.calls().len() < 5 {
                assert!(Instant::now() < deadline, "the embedder was never retried");
                std::thread::sleep(Duration::from_millis(5));
            }
            let before = flaky.calls().len();
            git_cli_commit(&scratch.vault(), "c/r2", r#"{"tag":"two"}"#);
            // Polls every 20 ms; the vector hold has over a second to run.
            let deadline = Instant::now() + Duration::from_millis(600);
            while sorted_ids(&vault, "c") != ["r1", "r2"] {
                assert!(
                    Instant::now() < deadline,
                    "the lexical leg waited for the embedder's backoff"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                flaky.calls().len(),
                before,
                "the poll indexed r2 without retrying the embedder early"
            );
            // `find` sees r2 once Tantivy commits; the mark moves just after.
            let deadline = Instant::now() + WAIT;
            let status = loop {
                let status = vault.index_status().unwrap();
                if status.lexical == status.head {
                    break status;
                }
                assert!(Instant::now() < deadline, "lexical mark stuck: {status:?}");
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.vector_pending, Some(2));
            assert_eq!(
                status.last_error.and_then(|err| err.known_code()),
                Some(Code::Invalid),
                "rounds that skip the held vector leg keep reporting its error"
            );
        }

        fn live_of(vault: &Vault) -> PathBuf {
            crate::index::current_dir(vault.index_dir())
                .unwrap()
                .unwrap()
        }

        /// An embedder of another model changes nothing on
        /// its own, and two processes on two models do not take turns
        /// rebuilding; `reindex` from the new one moves the Index over.
        #[cfg(feature = "search")]
        #[test]
        fn model_change_waits_for_reindex_and_does_not_flip_flop() {
            let scratch = Scratch::new();
            let old = open_with_model(&scratch.vault(), Some("m1"));
            old.create_collection("c", document_schema(), None).unwrap();
            let collection = old.collection("c").unwrap();
            collection.add("r1", rec_tag("alpha"), None).unwrap();
            collection.add("r2", rec_tag("beta"), None).unwrap();
            old.wait_indexed(WAIT).unwrap();
            wait_for_roles(&old);
            let m2 = CountingEmbedder::new("m2");
            let new = Vault::open_with(
                scratch.vault(),
                poll_fast(),
                Some(Arc::clone(&m2) as Arc<dyn Embedder>),
                Some(tag_projector()),
            )
            .unwrap();
            let before = live_of(&new);
            let status = new.wait_indexed(WAIT).unwrap();
            assert!(status.model_mismatch);
            assert!(!status.vector_role);
            assert_eq!(
                vector_hits(&new, "c", "alpha"),
                Vec::<String>::new(),
                "no search across two vector spaces"
            );
            // Many polls of both handles: nothing moves.
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(live_of(&new), before);
            assert_eq!(m2.slots(), 0);
            let status = old.index_status().unwrap();
            assert!(status.vector_role && !status.model_mismatch);

            let counts = new.reindex().unwrap();
            assert_eq!(counts.records_read, Some(2));
            assert_eq!(m2.slots(), 2, "every slot is embedded with the new model");
            assert!(!new.index_status().unwrap().model_mismatch);
            let after = live_of(&new);
            assert_ne!(after, before);
            let deadline = Instant::now() + WAIT;
            while !new.index_status().unwrap().vector_role {
                assert!(
                    Instant::now() < deadline,
                    "the new model never took the role"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let status = old.index_status().unwrap();
            assert!(status.model_mismatch && !status.vector_role, "{status:?}");
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(live_of(&old), after, "the old model does not rebuild back");
            assert_eq!(m2.slots(), 2);
            let mut hits = vector_hits(&new, "c", "alpha");
            hits.sort();
            assert_eq!(hits, ["r1", "r2"]);
        }

        /// A generation whose Tantivy files no longer read is rebuilt by
        /// the lexical-role indexer; queries fail closed until then.
        #[test]
        fn damaged_lexical_index_is_rebuilt_in_the_background() {
            let scratch = Scratch::new();
            let vault = open_both_roles(&scratch.vault(), poll_fast());
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            vault
                .collection("c")
                .unwrap()
                .add("a", tagged("one"), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            let damaged = live_of(&vault);
            fs::write(damaged.join("lexical").join("meta.json"), b"not json").unwrap();
            let deadline = Instant::now() + WAIT;
            let mut failed = false;
            loop {
                match vault
                    .collection("c")
                    .unwrap()
                    .find(&crate::FindQuery::all(), 0, 10)
                {
                    Ok(page) if !page.records.is_empty() => break,
                    Ok(_) => {}
                    Err(err) => {
                        assert_eq!(err.known_code(), Some(Code::IndexLexical), "{err:?}");
                        failed = true;
                    }
                }
                assert!(Instant::now() < deadline, "never rebuilt");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(failed, "the damaged generation must fail closed first");
            assert_ne!(live_of(&vault), damaged);
            assert_eq!(find_ids(&vault, "c"), ["a"]);
            let status = vault.wait_indexed(WAIT).unwrap();
            assert_eq!(status.lexical, status.head);
        }

        /// A vector dump that no longer loads is found by a handle that
        /// reads it, and the lexical-role indexer rebuilds the generation.
        #[cfg(feature = "search")]
        #[test]
        fn damaged_vector_dump_is_rebuilt_in_the_background() {
            let scratch = Scratch::new();
            let holder = open_with_model(&scratch.vault(), Some("m1"));
            holder
                .create_collection("c", document_schema(), None)
                .unwrap();
            holder
                .collection("c")
                .unwrap()
                .add("r1", rec_tag("alpha"), None)
                .unwrap();
            holder.wait_indexed(WAIT).unwrap();
            wait_for_roles(&holder);
            let damaged = live_of(&holder);
            let vector = damaged.join("vector");
            let seq = fs::read_to_string(vector.join("current")).unwrap();
            let dump = vector.join(seq.trim()).join("index.usearch");
            assert!(dump.is_file(), "{dump:?}");
            fs::write(&dump, b"truncated").unwrap();
            // A handle opened now cannot load it: it flags the generation.
            let reader = open_with_model(&scratch.vault(), Some("m1"));
            let deadline = Instant::now() + WAIT;
            while live_of(&holder) == damaged {
                assert!(Instant::now() < deadline, "never rebuilt");
                std::thread::sleep(Duration::from_millis(10));
            }
            let status = reader.wait_indexed(WAIT).unwrap();
            assert_eq!(status.vector, status.head);
            let deadline = Instant::now() + WAIT;
            loop {
                let query = SearchQuery::default()
                    .with_text("alpha")
                    .with_k(10)
                    .with_lexical_weight(0.0);
                // Fails closed (`index_vector`) until the rebuild is live.
                let hits = reader.collection("c").unwrap().search(&query, None);
                if let Ok(hits) = hits
                    && hits.hits.iter().any(|hit| hit.id.as_deref() == Some("r1"))
                {
                    break;
                }
                assert!(Instant::now() < deadline, "vectors never came back");
                std::thread::sleep(Duration::from_millis(10));
            }
            // The reader's rounds failed while its generation was damaged;
            // its next round on the rebuilt one clears that error. Under
            // load that round may land after the search above succeeds.
            let deadline = Instant::now() + WAIT;
            loop {
                let status = reader.index_status().unwrap();
                if status.last_error.is_none() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "error never cleared: {:?}",
                    status.last_error
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// Embeds one text per batch, slowly, and reports progress in
        /// `{root}/embedded`.
        #[cfg(feature = "search")]
        struct ProgressEmbedder {
            root: PathBuf,
            calls: AtomicU64,
        }

        #[cfg(feature = "search")]
        impl Embedder for ProgressEmbedder {
            fn model_id(&self) -> &str {
                "m1"
            }

            fn dimensions(&self) -> NonZeroUsize {
                NonZeroUsize::new(4).unwrap()
            }

            fn many_limit(&self) -> NonZeroUsize {
                NonZeroUsize::new(1).unwrap()
            }

            fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
                std::thread::sleep(Duration::from_millis(30));
                let calls = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                fs::write(self.root.join("embedded"), calls.to_string()).unwrap();
                Ok(texts.iter().copied().map(counting_vector).collect())
            }
        }

        #[cfg(feature = "search")]
        const KILLED_RECORDS: usize = 30;

        /// Child of `killed_mid_indexing_the_next_handle_finishes`: indexes
        /// the vector leg one slow Record at a time until killed (at most
        /// one minute).
        #[cfg(feature = "search")]
        #[test]
        #[ignore = "run as the child process of killed_mid_indexing_the_next_handle_finishes"]
        fn slow_indexer_child_process() {
            let Some(root) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
                return;
            };
            crate::git::isolate_test_git_env();
            // Checkpoint after a few Records, not after hundreds: the parent
            // kills this process within a handful of them.
            crate::search::TEST_CHECKPOINT_FLOOR.store(4, Ordering::Relaxed);
            let embedder: Arc<dyn Embedder> = Arc::new(ProgressEmbedder {
                root: root.clone(),
                calls: AtomicU64::new(0),
            });
            let _vault = Vault::open_with(
                root.join("vault"),
                poll_fast(),
                Some(embedder),
                Some(tag_projector()),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        /// `kill -9` in the middle of a diff breaks nothing. The marks
        /// stay where the last durable write put them, and the next handle
        /// finishes from the saved vectors instead of from scratch.
        #[cfg(feature = "search")]
        #[test]
        fn killed_mid_indexing_the_next_handle_finishes() {
            let scratch = Scratch::new();
            {
                let setup = open_search(scratch.vault(), poll_fast(), None);
                setup
                    .create_collection("c", document_schema(), None)
                    .unwrap();
                setup.wait_indexed(WAIT).unwrap();
            }
            for n in 1..KILLED_RECORDS {
                let path = scratch.vault().join(format!("c/r{n:02}"));
                fs::write(path, format!(r#"{{"tag":"text {n}"}}"#)).unwrap();
            }
            git_cli_commit(
                &scratch.vault(),
                &format!("c/r{KILLED_RECORDS:02}"),
                r#"{"tag":"text last"}"#,
            );
            let mut child = spawn_child(
                &scratch.root,
                "vault::tests::background::slow_indexer_child_process",
            );
            let deadline = Instant::now() + WAIT;
            loop {
                let calls = fs::read_to_string(scratch.root.join("embedded"))
                    .ok()
                    .and_then(|text| text.parse::<usize>().ok())
                    .unwrap_or(0);
                if calls >= 5 {
                    break;
                }
                let exited = child.0.try_wait().unwrap();
                let log = fs::read_to_string(scratch.root.join("child.log")).unwrap_or_default();
                assert!(exited.is_none(), "child exited early: {log}");
                assert!(Instant::now() < deadline, "child never embedded: {log}");
                std::thread::sleep(Duration::from_millis(5));
            }
            child.0.kill().unwrap();
            child.0.wait().unwrap();

            let counting = CountingEmbedder::new("m1");
            let vault = Vault::open_with(
                scratch.vault(),
                poll_fast(),
                Some(Arc::clone(&counting) as Arc<dyn Embedder>),
                Some(tag_projector()),
            )
            .unwrap();
            let status = vault.wait_indexed(WAIT).unwrap();
            let head = head_of(vault.path());
            assert_eq!(status.lexical.as_deref(), Some(head.as_str()));
            assert_eq!(status.vector.as_deref(), Some(head.as_str()));
            assert_eq!(status.vector_pending, Some(0));
            assert!(status.last_error.is_none(), "{status:?}");
            let found = vault
                .collection("c")
                .unwrap()
                .find(&crate::FindQuery::all(), 0, 100)
                .unwrap();
            assert_eq!(found.records.len(), KILLED_RECORDS);
            let hits = vault
                .collection("c")
                .unwrap()
                .search(
                    &SearchQuery::default()
                        .with_text("text")
                        .with_k(100)
                        .with_lexical_weight(0.0),
                    None,
                )
                .unwrap()
                .hits;
            assert_eq!(hits.len(), KILLED_RECORDS, "every Record has its vector");
            let embedded = usize::try_from(counting.slots()).unwrap();
            assert!(
                embedded < KILLED_RECORDS,
                "vectors saved before the kill are kept: {embedded} embedded again"
            );
        }

        /// Holds the lexical role and never polls, so nothing indexes the
        /// commits another handle makes: "before the indexer ran".
        fn idle_holder(path: &Path) -> Vault {
            let holder = open_indexed(path, poll_never());
            let deadline = Instant::now() + WAIT;
            while !holder.index_status().unwrap().lexical_role {
                assert!(Instant::now() < deadline, "holder never took the role");
                std::thread::sleep(Duration::from_millis(5));
            }
            holder
        }

        #[test]
        fn journal_add_then_get_before_the_indexer_ran() {
            let scratch = Scratch::new();
            let holder = idle_holder(&scratch.vault());
            let writer = open_indexed(&scratch.vault(), poll_never());
            writer
                .create_collection("j", journal_schema(), None)
                .unwrap();
            let journal = writer.collection("j").unwrap();
            journal.add("line", b"payload", None).unwrap();
            assert_ne!(
                writer.index_status().unwrap().lexical,
                Some(head_of(writer.path())),
                "the indexer must not have run yet"
            );
            let got = journal.get("line").unwrap();
            assert_eq!(got.raw.as_deref(), Some(b"payload".as_slice()));
            drop(holder);
        }

        #[test]
        fn status_counts_the_records_each_leg_has_yet_to_apply() {
            let scratch = Scratch::new();
            let holder = idle_holder(&scratch.vault());
            let writer = open_indexed(&scratch.vault(), poll_never());
            writer
                .create_collection("c", document_schema(), None)
                .unwrap();
            let collection = writer.collection("c").unwrap();
            for id in ["a", "b", "c"] {
                collection.add(id, b"x", None).unwrap();
            }
            let status = writer.index_status().unwrap();
            assert_ne!(status.lexical, status.head, "nothing indexed yet");
            assert_eq!(status.lexical_pending, Some(3));
            let marked = holder.index_status().unwrap().lexical;
            collection.remove("a", None).unwrap();
            assert_eq!(
                writer.index_status().unwrap().lexical_pending,
                Some(2),
                "a Record added and removed since the mark is no work"
            );
            assert_eq!(holder.index_status().unwrap().lexical, marked);
            drop(holder);
            let status = writer.wait_indexed(WAIT).unwrap();
            assert_eq!(status.lexical, status.head);
            assert_eq!(status.lexical_pending, Some(0));
        }

        #[test]
        fn replace_and_remove_right_after_add_use_the_file() {
            let scratch = Scratch::new();
            let holder = idle_holder(&scratch.vault());
            let writer = open_indexed(&scratch.vault(), poll_never());
            writer
                .create_collection("c", document_schema(), None)
                .unwrap();
            let collection = writer.collection("c").unwrap();
            collection.add("a", b"one", None).unwrap();
            collection.replace("a", b"two", None).unwrap();
            assert_eq!(
                collection.get("a").unwrap().raw.as_deref(),
                Some(b"two".as_slice())
            );
            collection.remove("a", None).unwrap();
            let err = collection.replace("a", b"three", None).unwrap_err();
            assert_eq!(err.known_code(), Some(Code::NotFound));
            drop(holder);
        }

        #[test]
        fn journal_line_from_another_handle_or_the_git_cli_is_found() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_never());
            vault
                .create_collection("j", journal_schema(), None)
                .unwrap();
            vault
                .collection("j")
                .unwrap()
                .add("own", b"o", None)
                .unwrap();
            let other = Vault::open(scratch.vault(), poll_never(), None).unwrap();
            other
                .collection("j")
                .unwrap()
                .add("theirs", b"t", None)
                .unwrap();
            // The other handle appended to the shared `ids` file: no wait.
            assert!(vault.collection("j").unwrap().get("theirs").is_ok());
            let segment = fs::read_dir(scratch.vault().join("j"))
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .find(|name| name.ends_with(".jsonl"))
                .unwrap();
            let rel = format!("j/{segment}");
            let mut body = fs::read_to_string(scratch.vault().join(&rel)).unwrap();
            body.push_str("hand: h\n");
            git_cli_commit(&scratch.vault(), &rel, &body);
            vault.wait_indexed(WAIT).unwrap();
            let got = vault.collection("j").unwrap().get("hand").unwrap();
            assert_eq!(got.raw.as_deref(), Some(b"h".as_slice()));
            let status = vault.index_status().unwrap();
            assert_eq!(
                marks::read(&live_dir(&vault), marks::Leg::Ids, marks::Repo::Main).unwrap(),
                status.head
            );
        }

        #[test]
        fn close_joins_the_indexer_and_collections_keep_working() {
            let scratch = Scratch::new();
            let vault = open_indexed(&scratch.vault(), poll_never());
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            let collection = vault.collection("c").unwrap();
            let task = task_path(&vault);
            vault.close().unwrap();
            assert_task_gone(&task);
            collection.add("after", empty_add(), None).unwrap();
            assert_eq!(
                collection.get("after").unwrap().id.as_deref(),
                Some("after")
            );
            let reopened = open_indexed(&scratch.vault(), poll_never());
            reopened.wait_indexed(WAIT).unwrap();
            assert_eq!(find_ids(&reopened, "c"), ["after"]);
        }

        #[cfg(feature = "search")]
        struct Gate {
            open: std::sync::Mutex<bool>,
            turn: std::sync::Condvar,
        }

        /// Blocks every call until the test opens the gate, or panics.
        #[cfg(feature = "search")]
        struct StuckEmbedder {
            gate: Arc<Gate>,
            panics: bool,
        }

        #[cfg(feature = "search")]
        impl Embedder for StuckEmbedder {
            fn model_id(&self) -> &str {
                "stuck"
            }

            fn dimensions(&self) -> NonZeroUsize {
                NonZeroUsize::new(4).unwrap()
            }

            fn many_limit(&self) -> NonZeroUsize {
                NonZeroUsize::new(1).unwrap()
            }

            fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
                if self.panics {
                    panic!("embedder exploded");
                }
                let mut open = self.gate.open.lock().unwrap();
                while !*open {
                    open = self.gate.turn.wait(open).unwrap();
                }
                Ok(texts.iter().copied().map(counting_vector).collect())
            }
        }

        #[cfg(feature = "search")]
        #[test]
        fn a_blocking_or_panicking_embedder_neither_blocks_nor_fails_a_write() {
            for panics in [false, true] {
                let scratch = Scratch::new();
                let gate = Arc::new(Gate {
                    open: std::sync::Mutex::new(false),
                    turn: std::sync::Condvar::new(),
                });
                let embedder: Arc<dyn Embedder> = Arc::new(StuckEmbedder {
                    gate: Arc::clone(&gate),
                    panics,
                });
                let vault = Vault::open_with(
                    scratch.vault(),
                    poll_fast(),
                    Some(embedder),
                    Some(tag_projector()),
                )
                .unwrap();
                vault
                    .create_collection("c", document_schema(), None)
                    .unwrap();
                let collection = vault.collection("c").unwrap();
                let started = Instant::now();
                for i in 0..5 {
                    collection
                        .add(&format!("r{i}"), rec_tag(&format!("tag {i}")), None)
                        .unwrap();
                }
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "writes waited on the embedder: {:?}",
                    started.elapsed()
                );
                assert_eq!(collection.get("r4").unwrap().id.as_deref(), Some("r4"));
                if panics {
                    let deadline = Instant::now() + WAIT;
                    loop {
                        let status = vault.index_status().unwrap();
                        if let Some(err) = status.last_error {
                            assert_eq!(err.known_code(), Some(Code::IndexVector));
                            break;
                        }
                        assert!(Instant::now() < deadline, "the panic was not reported");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                *gate.open.lock().unwrap() = true;
                gate.turn.notify_all();
                drop(collection);
                vault.close().unwrap();
            }
        }

        #[cfg(feature = "search")]
        #[test]
        fn vector_dump_is_saved_only_when_a_slot_changes() {
            let scratch = Scratch::new();
            let embedder: Arc<dyn Embedder> = CountingEmbedder::new("m1");
            let vault = Vault::open_with(
                scratch.vault(),
                poll_never(),
                Some(embedder),
                Some(tag_projector()),
            )
            .unwrap();
            vault
                .create_collection("c", document_schema(), None)
                .unwrap();
            let lexical_only = Schema {
                exclude_vector_index: Some(true),
                ..document_schema()
            };
            vault
                .create_collection("words", lexical_only, None)
                .unwrap();
            vault
                .collection("c")
                .unwrap()
                .add("a", rec_tag("alpha"), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            let pointer = live_dir(&vault).join("vector").join("current");
            let seq = || fs::read_to_string(&pointer).unwrap();
            let saved = seq();
            assert!(
                live_dir(&vault)
                    .join("vector")
                    .join(saved.trim())
                    .join("keys.json")
                    .is_file()
            );
            vault
                .collection("words")
                .unwrap()
                .add("w", rec_tag("word"), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            vault
                .collection("c")
                .unwrap()
                .replace("a", rec_tag("alpha"), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            assert_eq!(seq(), saved, "no vector slot changed, so no save");
            vault
                .collection("c")
                .unwrap()
                .add("b", rec_tag("beta"), None)
                .unwrap();
            vault.wait_indexed(WAIT).unwrap();
            assert_ne!(seq(), saved, "a new slot is saved");
        }

        #[cfg(feature = "search")]
        struct SlowEmbedder {
            calls: AtomicU64,
        }

        #[cfg(feature = "search")]
        impl Embedder for SlowEmbedder {
            fn model_id(&self) -> &str {
                "slow"
            }

            fn dimensions(&self) -> NonZeroUsize {
                NonZeroUsize::new(4).unwrap()
            }

            fn many_limit(&self) -> NonZeroUsize {
                NonZeroUsize::new(1).unwrap()
            }

            fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(150));
                Ok(texts.iter().copied().map(counting_vector).collect())
            }
        }

        #[cfg(feature = "search")]
        #[test]
        fn drop_waits_for_one_batch_not_the_whole_diff() {
            const RECORDS: u64 = 40;
            let scratch = Scratch::new();
            {
                let plain = open_search(scratch.vault(), scratch.options(), None);
                plain
                    .create_collection("c", document_schema(), None)
                    .unwrap();
                plain.begin().unwrap();
                let collection = plain.collection("c").unwrap();
                for i in 0..RECORDS {
                    collection
                        .add(&format!("r{i:02}"), rec_tag(&format!("tag {i}")), None)
                        .unwrap();
                }
                plain.commit("many").unwrap();
            }
            let slow = Arc::new(SlowEmbedder {
                calls: AtomicU64::new(0),
            });
            let vault = {
                let embedder: Arc<dyn Embedder> = slow.clone();
                Vault::open_with(
                    scratch.vault(),
                    poll_never(),
                    Some(embedder),
                    Some(tag_projector()),
                )
                .unwrap()
            };
            let deadline = Instant::now() + WAIT;
            while slow.calls.load(Ordering::Relaxed) == 0 {
                assert!(Instant::now() < deadline, "the vector leg never started");
                std::thread::sleep(Duration::from_millis(5));
            }
            let task = task_path(&vault);
            let started = Instant::now();
            drop(vault);
            let took = started.elapsed();
            assert!(took < Duration::from_secs(2), "drop took {took:?}");
            assert!(
                slow.calls.load(Ordering::Relaxed) < RECORDS,
                "drop waited for the whole diff"
            );
            assert_task_gone(&task);
        }
    }
}
