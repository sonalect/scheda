use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
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
#[cfg(feature = "find")]
use crate::query;
use crate::schema::{self, CollectionDecl, Kind, Schema as LoadedSchema};
#[cfg(feature = "find")]
use crate::search::SearchEngine;
use crate::store;
use crate::{
    Declaration, Embedder, Encryption, Error, History, OpenOptions, Projector, Record,
    ReindexResult, VaultInfo,
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
    schema: Mutex<Arc<LoadedSchema>>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    writer: Arc<str>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    #[cfg(feature = "find")]
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
    projector: Option<Arc<dyn Projector>>,
    identity: Arc<git::CommitIdentity>,
    encryption: Option<Encryption>,
    #[cfg(all(test, feature = "find"))]
    walk_hook: Mutex<Option<ReindexWalkHook>>,
}

/// A named set of records with one write mode (`document`, `journal`, or `singleton`).
pub struct Collection {
    vault: PathBuf,
    index: Arc<index::IndexHandle>,
    write_lock: Arc<Mutex<WriteLock>>,
    auto_sync: bool,
    remotes: Vec<String>,
    decl: CollectionDecl,
    schema: Arc<LoadedSchema>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    writer: Arc<str>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    #[cfg(feature = "find")]
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
    projector: Option<Arc<dyn Projector>>,
    identity: Arc<git::CommitIdentity>,
    encryption: Option<Encryption>,
}

impl Vault {
    /// Open, create, or clone a Vault.
    ///
    /// Path exists → open. A set `remote_repo` must match the fetch URL of
    /// `origin` (`invalid` if there is no `origin` or the URL differs); `origin`
    /// is not added or rewritten. Unset `remote_repo` attaches and does not
    /// change `origin`. Path missing + `remote_repo` → clone. Path missing →
    /// `git init`. Path exists but is not a Vault → `invalid`.
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
        if path.exists() {
            if !git::is_work_tree(path) {
                bail!(Error::invalid(format!(
                    "path exists but is not a Vault: {}",
                    path.display()
                )));
            }
            require_matching_origin(path, &options)?;
            let identity = git::identity_from_open(&options, path)?;
            return Self::open_existing(path, options, embedder, projector, identity);
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
        commit_gitignore_if_needed(path, &identity)?;
        Self::open_existing(path, options, embedder, projector, identity)
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
        commit_gitignore_if_needed(path, &identity)?;
        Self::open_existing(
            path,
            without_remote_repo(options),
            embedder,
            projector,
            identity,
        )
    }

    fn open_existing(
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
        projector: Option<Arc<dyn Projector>>,
        identity: git::CommitIdentity,
    ) -> Result<Self, Error> {
        #[cfg(not(feature = "search"))]
        if embedder.is_some() {
            bail!(
                Error::index_vector("vector index is not available")
                    .context("[Vault::open] embedder")
            );
        }
        let auto_sync = options.auto_sync.unwrap_or(false);
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
        let schema = Arc::new(schema::load(&path)?);
        let journal_ids = Arc::new(Mutex::new(index::load_journal_ids(&generation, &schema)?));
        let written = Arc::new(Mutex::new(load_written(&path)?));
        let in_txn = Arc::new(Mutex::new(false));
        let writer = Arc::<str>::from(crate::writer::load_or_create(
            &path,
            options.writer.as_deref(),
        )?);
        let _ = fs::remove_file(path.join(".scheda").join("index").join("txn"));
        #[cfg(feature = "find")]
        let search = Arc::new(SearchEngine::open(&generation, false, projector.clone())?);
        let encryption = options.encryption.as_option().cloned();
        let vault = Self {
            path,
            index,
            remotes,
            auto_sync,
            write_lock: Arc::new(Mutex::new(WriteLock::new())),
            schema: Mutex::new(schema),
            written,
            in_txn,
            writer,
            journal_ids,
            #[cfg(feature = "find")]
            search,
            embedder,
            projector,
            identity: Arc::new(identity),
            encryption,
            #[cfg(all(test, feature = "find"))]
            walk_hook: Mutex::new(None),
        };
        if gitignore_pending {
            vault.write_gitignore_under_lock()?;
        }
        Ok(vault)
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
    /// before Index teardown waits on merge threads. The lexical Index
    /// writer is released with the lock, so another handle can write
    /// without waiting on Tantivy's directory lock.
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
    /// A poisoned schema lock is `io_other`, same as the other schema reads.
    pub fn versions(&self) -> Result<Vec<u32>, Error> {
        Ok(self.schema_arc()?.versions())
    }

    fn schema_arc(&self) -> Result<Arc<LoadedSchema>, Error> {
        Ok(Arc::clone(&*crate::error::mutex(
            &self.schema,
            "[Vault] lock schema",
        )?))
    }

    /// Handle for one collection.
    ///
    /// Unknown name → `not_found`. Isolated collections live under
    /// `{vault}/.isolated/{name}/` on this same handle. A build without the
    /// `encrypt` crate feature refuses an encrypted collection
    /// (`unsupported_encryption`); `open` of the Vault still succeeds.
    pub fn collection(&self, name: &str) -> Result<Collection, Error> {
        let schema = self.schema_arc()?;
        let Some(decl) = schema.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in Schema"
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
            schema,
            written: Arc::clone(&self.written),
            in_txn: Arc::clone(&self.in_txn),
            writer: Arc::clone(&self.writer),
            journal_ids: Arc::clone(&self.journal_ids),
            #[cfg(feature = "find")]
            search: Arc::clone(&self.search),
            embedder: self.embedder.clone(),
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
    /// Without a projector, only `all` and Filter on `id` are valid.
    #[cfg(feature = "find")]
    pub fn find(
        &self,
        collections: &[&str],
        query: &FindQuery,
        offset: u32,
        count: u32,
    ) -> Result<FindResult, Error> {
        let schema = self.schema_arc()?;
        QueryCtx {
            engine: &self.search,
            schema: &schema,
            vault: &self.path,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
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
    #[cfg(feature = "search")]
    pub fn search(
        &self,
        collections: &[&str],
        query: &SearchQuery,
        reranker: Option<&dyn Reranker>,
    ) -> Result<SearchHits, Error> {
        let schema = self.schema_arc()?;
        QueryCtx {
            engine: &self.search,
            schema: &schema,
            vault: &self.path,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
        }
        .search(self.embedder.as_deref(), collections, query, reranker)
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
    /// and the lexical Index writer before returning. A later `open` on
    /// this path can write immediately, including from a second handle
    /// that is still concurrent with this one.
    ///
    /// Opening a Tantivy writer allocates a 16 MiB heap and starts a merge
    /// thread. This handle keeps one writer for the whole transaction and
    /// drops it here, not per Record. Autocommit does the same per call;
    /// that cost sits next to a git commit. The vector Index is an
    /// in-memory USearch dump with no directory lock, so it is not
    /// released here.
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
    /// The lexical Index writer is opened on the first indexed write and
    /// kept until that point. Discards leftover Scheda writes first.
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
    /// each path, end the transaction if any on this handle, reload Schema,
    /// and rebuild the Index. Allowed without an open transaction so a later
    /// process can clear crash leftovers.
    ///
    /// Releases the write lock and the lexical Index writer before returning.
    pub fn discard(&self) -> Result<(), Error> {
        self.require_write_lock("discard")?;
        self.conclude_write((|| {
            self.reject_if_reindex_active("discard")?;
            self.restore_recorded()?;
            set_in_txn(&self.in_txn, false)?;
            Ok(())
        })())
    }

    fn commit_recorded(&self, message: &str) -> Result<(), Error> {
        let paths: Vec<String> = {
            let set = crate::error::mutex(&self.written, "[Vault] lock written")?;
            set.iter().cloned().collect()
        };
        isolated::commit_recorded(&self.path, message, &paths, &self.identity)?;
        let mut set = crate::error::mutex(&self.written, "[Vault] lock written")?;
        set.clear();
        persist_written(&self.path, &set)?;
        #[cfg(feature = "find")]
        self.search.flush()?;
        Ok(())
    }

    fn restore_recorded(&self) -> Result<(), Error> {
        let paths: Vec<String> = {
            let mut set = crate::error::mutex(&self.written, "[Vault] lock written")?;
            let paths: Vec<String> = set.iter().cloned().collect();
            set.clear();
            persist_written(&self.path, &set)?;
            paths
        };
        if paths.is_empty() {
            return Ok(());
        }
        let schema = self.schema_arc()?;
        let snapshot = crate::reindex::snapshot_restored(&self.path, &schema, &paths)?;
        isolated::discard_recorded(&self.path, &paths)?;
        self.reload_after_restore()?;
        let schema = self.schema_arc()?;
        crate::reindex::enqueue_restored(&self.path, &schema, &paths, &snapshot)?;
        Ok(())
    }

    fn reload_after_restore(&self) -> Result<(), Error> {
        let loaded = schema::load(&self.path)?;
        let dir = self.generation_dir()?;
        #[cfg(feature = "find")]
        {
            self.search.close_for_rebuild();
            self.search
                .rebuild(&self.path, &loaded, self.embedder.as_deref())?;
        }
        *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? =
            index::load_journal_ids(&dir, &loaded)?;
        *crate::error::mutex(&self.schema, "[Vault] lock schema")? = Arc::new(loaded);
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

    /// `git pull` from `origin`, then reload Schema and rebuild the Index.
    /// Takes the write lock for this call. Outside a transaction the lock
    /// is released before return.
    pub fn pull(&self) -> Result<(), Error> {
        self.require_write_lock("pull")?;
        self.conclude_write((|| {
            self.reject_if_reindex_active("pull")?;
            git::pull(&self.path)?;
            self.reload_after_restore()
        })())
    }

    /// `pull` then `push`. Takes the write lock for this call. Outside a
    /// transaction the lock is released before return.
    pub fn sync(&self) -> Result<(), Error> {
        self.require_write_lock("sync")?;
        self.conclude_write((|| {
            self.reject_if_reindex_active("sync")?;
            git::pull(&self.path)?;
            self.reload_after_restore()?;
            git::push(&self.path)
        })())
    }

    /// `git push` of `{vault}/.isolated/` to its own `origin`.
    ///
    /// Vault `push` never reaches this nested repository. Missing nested
    /// work tree → `invalid`. No Remote → `invalid`. Takes the write lock
    /// for this call. Not `locked` while `reindex` is in flight: no Record
    /// file is written.
    pub fn push_isolated(&self) -> Result<(), Error> {
        self.require_write_lock("push_isolated")?;
        self.conclude_write(git::push(&self.isolated_work_tree()?))
    }

    /// `git pull` of `{vault}/.isolated/` from its own `origin`, then reload
    /// Schema and rebuild the Index.
    ///
    /// Isolated Records live in the one Vault Index, so this call is
    /// `locked` while `reindex` is in flight. Missing nested work tree →
    /// `invalid`.
    pub fn pull_isolated(&self) -> Result<(), Error> {
        self.require_write_lock("pull_isolated")?;
        self.conclude_write((|| {
            self.reject_if_reindex_active("pull_isolated")?;
            git::pull(&self.isolated_work_tree()?)?;
            self.reload_after_restore()
        })())
    }

    /// Isolated `pull` then `push`. Takes the write lock for this call.
    pub fn sync_isolated(&self) -> Result<(), Error> {
        self.require_write_lock("sync_isolated")?;
        self.conclude_write((|| {
            self.reject_if_reindex_active("sync_isolated")?;
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
        take_write_slot(
            &mut slot,
            &self.path,
            #[cfg(feature = "find")]
            &self.search,
            "Vault",
            op,
        )
    }

    fn conclude_write<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        release_lock_unless_in_transaction(
            &self.write_lock,
            &self.in_txn,
            #[cfg(feature = "find")]
            &self.search,
            result,
        )
    }

    /// Patch `.gitignore` on an existing Vault. Takes `write.lock` for that
    /// write, then `unlock`s so `open` does not keep the writer lock.
    fn write_gitignore_under_lock(&self) -> Result<(), Error> {
        {
            let mut slot = crate::error::mutex(&self.write_lock, "[Vault::open] lock write lock")?;
            if !slot.is_held() {
                take_write_slot(
                    &mut slot,
                    &self.path,
                    #[cfg(feature = "find")]
                    &self.search,
                    "Vault",
                    "open",
                )?;
            }
        }
        let result = (|| {
            if ensure_gitignore(&self.path)? {
                self.note_write(String::from(".gitignore"))?;
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
    /// `index_corrupt`. Does not write Records. Lexical Index freshness is
    /// validated only when the crate is built with `find`; a build without
    /// `find` has no lexical segments and no way to rebuild them, so it ignores
    /// the stale marker even though its own writes keep setting it. An
    /// encrypted collection whose secret is absent is not checked
    /// (`crypto`); a build without `encrypt` reports `unsupported_encryption`.
    /// With the secret, encrypted collections are validated like any other.
    pub fn check(&self) -> Result<(), Error> {
        isolated::check_perimeter(&self.path)?;
        #[cfg(feature = "find")]
        {
            let dir = self.generation_dir()?;
            index::require_fresh(&dir)?;
        }
        store::check_scheda_owner_only(&self.path)?;
        let schema = schema::load(&self.path)?;
        index::check_journal_id_maps(&self.generation_dir()?, &schema)?;
        store::check_documents(&self.path, &schema, self.encryption.as_ref())?;
        journal::check_journals(&self.path, &schema)
    }

    /// Proto snapshot (`api.v1.VaultInfo`): vault attrs plus the collection list.
    pub fn info(&self) -> Result<VaultInfo, Error> {
        let remote = self.remotes.first().cloned().unwrap_or_default();
        let info = schema::to_vault_info(
            &self.path,
            &self.index.root,
            &remote,
            self.auto_sync,
            &*self.schema_arc()?,
        )?;
        Ok(info)
    }

    /// Write one collection declaration (`api.v1.Declaration`) as
    /// `{name}/.collection.yaml`. Reloads declarations on this handle.
    /// `isolated: true` stores Records under
    /// `{vault}/.isolated/` and creates the nested git if needed. Setting or
    /// clearing `encrypted` on a collection that already holds Record files is
    /// `invalid`; existing files are left untouched. A build without the
    /// `encrypt` crate feature refuses a declaration that marks `encrypted`
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
        decl: Declaration,
        message: Option<&str>,
    ) -> Result<Collection, Error> {
        decl.require_valid()?;
        #[cfg(not(feature = "encrypt"))]
        if decl.encrypted == Some(true) {
            bail!(
                Error::unsupported_encryption("this build cannot create an encrypted collection")
                    .context("[Vault::create_collection] encryption")
            );
        }
        let message = require_commit_message(message)?;
        self.require_write_lock("create_collection")?;
        self.conclude_write((|| {
            self.reject_if_reindex_active("create_collection")?;
            self.drain_abandoned()?;
            let result = (|| {
                if decl.isolated == Some(true)
                    && let Some(rel) = isolated::ensure(&self.path)?
                {
                    self.note_write(rel)?;
                }
                let rel = schema::put_collection(&self.path, name, &decl)?;
                self.note_write(rel)?;
                let loaded = schema::load(&self.path)?;
                let dir = self.generation_dir()?;
                *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? =
                    index::load_journal_ids(&dir, &loaded)?;
                *crate::error::mutex(&self.schema, "[Vault] lock schema")? = Arc::new(loaded);
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

    /// Rebuild the Index from Vault files into a new generation directory,
    /// then point `{vault}/.scheda/index/current` at it with one atomic write.
    /// Writes format version `4` and the journal `id` map for every
    /// non-encrypted `journal`, isolated included. Encrypted collections
    /// are skipped. A collection with both Index legs excluded is not
    /// walked for Tantivy or vectors; its journal id map is still rebuilt.
    /// Incremental writes keep going into the live generation; only this
    /// method swaps. Vector slots whose embedder text is unchanged are
    /// copied into the new generation; the lexical Index is rebuilt in
    /// full. A different `model_id` or dimensions starts the vector table
    /// empty and re-embeds every slot. A reader that opened before the
    /// swap keeps its generation. A reader older than one `reindex` cycle
    /// may lose those files (`index_missing` or `index_lexical`). Removes
    /// a leftover `{vault}/.isolated/.scheda/` directory when one is
    /// present.
    ///
    /// Takes `write.lock` only for start and publish. The file walk does
    /// not hold it, so `add` / `replace` / `remove` stay allowed. Holds
    /// `{vault}/.scheda/reindex.lock` for the whole call. A second `reindex`
    /// is `locked`. Inside an open transaction the write lock is kept for
    /// the whole call and no marker or queue is written.
    pub fn reindex(&self) -> Result<ReindexResult, Error> {
        let in_txn = flag(&self.in_txn)?;
        self.require_write_lock("reindex")?;
        let reindex_lock = match crate::reindex::acquire_lock(&self.path) {
            Ok(file) => file,
            Err(err) => return self.conclude_write(Err(err)),
        };
        let result = match self.reindex_body(in_txn) {
            Ok(counts) => Ok(counts),
            Err(err) => {
                let _ = crate::reindex::clear_active(&self.path);
                Err(err)
            }
        };
        drop(reindex_lock);
        if in_txn {
            result
        } else {
            self.conclude_write(result)
        }
    }

    fn reindex_body(&self, in_txn: bool) -> Result<ReindexResult, Error> {
        let schema = schema::load(&self.path)?;
        #[cfg(feature = "search")]
        let previous = self.generation_dir()?;
        let new_dir = index::start_generation(&self.index.root)?;
        let generation = index::generation_number(&new_dir)?;
        if !in_txn {
            crate::reindex::write_active(&self.path, generation)?;
        }
        #[cfg(feature = "search")]
        {
            self.search.flush()?;
            // Must precede `index::rebuild`: that walk clears the generation
            // except an inherited `vector/` dump already in place.
            crate::search::copy_vector_dump(&previous, &new_dir, self.embedder.as_deref())?;
        }
        if !in_txn {
            #[cfg(feature = "find")]
            self.search.release_writer()?;
            release_held_write_lock(&self.write_lock)?;
        }
        let (counts, mut maps) = index::rebuild(&self.path, &schema, &new_dir)?;
        #[cfg(feature = "find")]
        let counts = {
            let hook = self.walk_hook();
            let hook_ref = hook.as_deref();
            let rebuilder = SearchEngine::open(&new_dir, true, self.projector.clone())?;
            let walk = rebuilder.rebuild_online(
                &self.path,
                &schema,
                self.embedder.as_deref(),
                hook_ref.map(|h| h as &dyn Fn(&str)),
            )?;
            let counts = merge_reindex_counts(counts, &walk);
            maps.absorb(walk.journal_maps);
            if in_txn {
                self.publish_reindex(&new_dir, maps, rebuilder)?;
            } else {
                self.catch_up_and_publish(
                    &schema,
                    &new_dir,
                    &mut maps,
                    rebuilder,
                    #[cfg(feature = "search")]
                    &previous,
                )?;
            }
            counts
        };
        #[cfg(not(feature = "find"))]
        {
            index::mark_stale(&new_dir)?;
            if in_txn {
                index::publish_generation(&self.index.root, &new_dir)?;
                self.index.set_dir(new_dir)?;
                *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? = maps;
            } else {
                self.catch_up_and_publish_thin(&schema, &new_dir, &mut maps)?;
            }
        }
        Ok(counts)
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

    fn reject_if_reindex_active(&self, op: &str) -> Result<(), Error> {
        if crate::reindex::active_exists(&self.path) {
            return Err(Error::locked("reindex is in progress")
                .with_context(|| format!("[Vault::{op}] reindex")));
        }
        Ok(())
    }

    #[cfg(feature = "find")]
    fn catch_up_and_publish(
        &self,
        schema: &LoadedSchema,
        new_dir: &Path,
        maps: &mut JournalIdMaps,
        rebuilder: SearchEngine,
        #[cfg(feature = "search")] previous: &Path,
    ) -> Result<(), Error> {
        #[cfg(feature = "search")]
        let models_differ = self.vector_models_differ();
        #[cfg(not(feature = "search"))]
        let models_differ = false;
        let copy_vectors = self.embedder.is_some() && !models_differ;
        let deadline = Instant::now() + crate::reindex::publish_window();
        loop {
            self.apply_queue(
                schema,
                new_dir,
                maps,
                &rebuilder,
                #[cfg(feature = "search")]
                previous,
                copy_vectors,
            )?;
            if self.try_take_write_lock()? {
                #[cfg(feature = "search")]
                if !crate::reindex::list_queue(&self.path)?.is_empty() && models_differ {
                    let _ = self.search.release_writer();
                    release_held_write_lock(&self.write_lock)?;
                    if Instant::now() >= deadline {
                        return Err(Error::locked("vault write lock is held")
                            .context("[Vault::reindex] publish write lock"));
                    }
                    thread::sleep(crate::reindex::PUBLISH_RETRY);
                    continue;
                }
                self.apply_queue(
                    schema,
                    new_dir,
                    maps,
                    &rebuilder,
                    #[cfg(feature = "search")]
                    previous,
                    copy_vectors,
                )?;
                self.publish_reindex(new_dir, std::mem::take(maps), rebuilder)?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::locked("vault write lock is held")
                    .context("[Vault::reindex] publish write lock"));
            }
            thread::sleep(crate::reindex::PUBLISH_RETRY);
        }
    }

    #[cfg(not(feature = "find"))]
    fn catch_up_and_publish_thin(
        &self,
        schema: &LoadedSchema,
        new_dir: &Path,
        maps: &mut JournalIdMaps,
    ) -> Result<(), Error> {
        let deadline = Instant::now() + crate::reindex::publish_window();
        loop {
            self.apply_queue_thin(schema, new_dir, maps)?;
            if self.try_take_write_lock()? {
                self.apply_queue_thin(schema, new_dir, maps)?;
                index::publish_generation(&self.index.root, new_dir)?;
                self.index.set_dir(new_dir.to_path_buf())?;
                *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? =
                    std::mem::take(maps);
                crate::reindex::clear_active(&self.path)?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::locked("vault write lock is held")
                    .context("[Vault::reindex] publish write lock"));
            }
            thread::sleep(crate::reindex::PUBLISH_RETRY);
        }
    }

    #[cfg(feature = "find")]
    fn apply_queue(
        &self,
        schema: &LoadedSchema,
        new_dir: &Path,
        maps: &mut JournalIdMaps,
        rebuilder: &SearchEngine,
        #[cfg(feature = "search")] previous: &Path,
        copy_vectors: bool,
    ) -> Result<(), Error> {
        let files = crate::reindex::list_queue(&self.path)?;
        if files.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "search")]
        let vector_source = if copy_vectors {
            Some(crate::search::load_vector_source(previous)?)
        } else {
            None
        };
        #[cfg(not(feature = "search"))]
        let _ = copy_vectors;
        for path in files {
            let Some((collection, id)) = crate::reindex::take_queue_file(&path)? else {
                continue;
            };
            let Some(decl) = schema.get(&collection) else {
                continue;
            };
            if decl.encrypted {
                continue;
            }
            match self.read_queued_record(decl, &id)? {
                Some((record, journal_rel)) if store::record_indexable(decl, &record) => {
                    if decl.kind == Kind::Journal
                        && let Some(rel) = journal_rel.as_deref()
                    {
                        index::remember_id(new_dir, maps, &decl.name, &id, rel)?;
                    }
                    #[cfg(feature = "search")]
                    if decl.vector_indexed()
                        && let Some(source) = vector_source.as_ref()
                    {
                        source.copy_record(rebuilder, &decl.name, &id)?;
                    }
                    if decl.any_indexed() {
                        rebuilder.upsert(decl, &record, self.embedder.as_deref())?;
                    }
                }
                _ => {
                    if decl.kind == Kind::Journal {
                        index::forget_remembered(new_dir, maps, &decl.name, &id)?;
                    }
                    if decl.any_indexed() {
                        rebuilder.forget(decl, &id)?;
                    }
                }
            }
        }
        rebuilder.flush()
    }

    #[cfg(not(feature = "find"))]
    fn apply_queue_thin(
        &self,
        schema: &LoadedSchema,
        new_dir: &Path,
        maps: &mut JournalIdMaps,
    ) -> Result<(), Error> {
        for path in crate::reindex::list_queue(&self.path)? {
            let Some((collection, id)) = crate::reindex::take_queue_file(&path)? else {
                continue;
            };
            let Some(decl) = schema.get(&collection) else {
                continue;
            };
            if decl.encrypted {
                continue;
            }
            match self.read_queued_record(decl, &id)? {
                Some((record, journal_rel)) if store::record_indexable(decl, &record) => {
                    if decl.kind == Kind::Journal
                        && let Some(rel) = journal_rel.as_deref()
                    {
                        index::remember_id(new_dir, maps, &decl.name, &id, rel)?;
                    }
                }
                _ => {
                    if decl.kind == Kind::Journal {
                        index::forget_remembered(new_dir, maps, &decl.name, &id)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn read_queued_record(
        &self,
        decl: &CollectionDecl,
        id: &str,
    ) -> Result<Option<(Record, Option<String>)>, Error> {
        match decl.kind {
            Kind::Journal => Ok(self
                .read_queued_journal(decl, id)?
                .map(|(record, rel)| (record, Some(rel)))),
            Kind::Document | Kind::Singleton => {
                match store::find_document(&self.path, decl, id, self.encryption.as_ref()) {
                    Ok(record) => Ok(Some((record, None))),
                    Err(err) if crate::reindex::is_absent(&err) => Ok(None),
                    Err(err) => Err(err),
                }
            }
        }
    }

    fn read_queued_journal(
        &self,
        decl: &CollectionDecl,
        id: &str,
    ) -> Result<Option<(Record, String)>, Error> {
        let live = self.generation_dir()?;
        match crate::index::lookup_on_disk(&live, &decl.name, id)? {
            crate::index::OwnedLookup::Path(rel) => {
                match journal::find_in_segment(&self.path, decl, &rel, id) {
                    Ok(record) => Ok(Some((record, rel))),
                    Err(err) if crate::reindex::is_absent(&err) => Ok(None),
                    Err(_) => match journal::find_journal_scan_with_rel(&self.path, decl, id) {
                        Ok(pair) => Ok(Some(pair)),
                        Err(err) if crate::reindex::is_absent(&err) => Ok(None),
                        Err(err) => Err(err),
                    },
                }
            }
            crate::index::OwnedLookup::UnknownId => Ok(None),
            crate::index::OwnedLookup::MissingMap => {
                match journal::find_journal_scan_with_rel(&self.path, decl, id) {
                    Ok(pair) => Ok(Some(pair)),
                    Err(err) if crate::reindex::is_absent(&err) => Ok(None),
                    Err(err) => Err(err),
                }
            }
        }
    }

    #[cfg(feature = "find")]
    fn publish_reindex(
        &self,
        new_dir: &Path,
        maps: JournalIdMaps,
        rebuilder: SearchEngine,
    ) -> Result<(), Error> {
        index::publish_generation(&self.index.root, new_dir)?;
        self.search.release_writer()?;
        self.search.adopt(rebuilder)?;
        self.index.set_dir(new_dir.to_path_buf())?;
        *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? = maps;
        crate::reindex::clear_active(&self.path)
    }

    #[cfg(feature = "search")]
    fn vector_models_differ(&self) -> bool {
        let Some(embedder) = self.embedder.as_deref() else {
            return false;
        };
        let Ok(dir) = self.generation_dir() else {
            return false;
        };
        let meta_path = dir.join("vector").join("meta.json");
        let Ok(raw) = fs::read_to_string(&meta_path) else {
            return false;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return false;
        };
        let model = meta.get("model_id").and_then(|v| v.as_str()).unwrap_or("");
        let dims = meta
            .get("dimensions")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        model != embedder.model_id() || dims != embedder.dimensions().get() as u64
    }

    fn try_take_write_lock(&self) -> Result<bool, Error> {
        let mut slot = crate::error::mutex(&self.write_lock, "[Vault] lock write lock")
            .with_context(|| "[Vault::reindex] take write lock")?;
        if slot.is_held() {
            return Ok(slot.owner == Some(thread::current().id()));
        }
        match try_acquire_write_lock(&self.path)? {
            Some(file) => {
                #[cfg(feature = "find")]
                if let Err(err) = self.search.enable_write() {
                    let _ = file.unlock();
                    return Err(err).with_context(|| "[Vault::reindex] take write lock");
                }
                slot.file = Some(file);
                slot.owner = Some(thread::current().id());
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Fill a new generation without swapping `current`. Test seam for a
    /// crash between rebuild and the pointer write.
    #[cfg(all(test, feature = "find"))]
    pub(crate) fn reindex_without_publish(&self) -> Result<PathBuf, Error> {
        self.require_write_lock("reindex")?;
        self.conclude_write((|| {
            let schema = schema::load(&self.path)?;
            #[cfg(feature = "search")]
            let previous = self.generation_dir()?;
            let new_dir = index::start_generation(&self.index.root)?;
            #[cfg(feature = "search")]
            {
                self.search.flush()?;
                // Must precede `index::rebuild`: that walk clears the generation
                // except an inherited `vector/` dump already in place.
                crate::search::copy_vector_dump(&previous, &new_dir, self.embedder.as_deref())?;
            }
            index::rebuild(&self.path, &schema, &new_dir)?;
            let rebuilder = SearchEngine::open(&new_dir, true, self.projector.clone())?;
            rebuilder.rebuild_online(&self.path, &schema, self.embedder.as_deref(), None)?;
            drop(rebuilder);
            Ok(new_dir)
        })())
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

#[cfg(feature = "find")]
fn merge_reindex_counts(base: ReindexResult, walk: &crate::search::RebuildStats) -> ReindexResult {
    ReindexResult::default()
        .with_records_read(base.records_read.unwrap_or(0) + walk.records_read)
        .with_skipped_invalid(base.skipped_invalid.unwrap_or(0) + walk.skipped_invalid)
        .with_segments_seen(base.segments_seen.unwrap_or(0) + walk.segments_seen)
}

/// Releases `{vault}/.scheda/write.lock` when this is the last handle that
/// shares it, before Index teardown waits on merge threads. An open
/// transaction ends without a git commit.
impl Drop for Vault {
    fn drop(&mut self) {
        if Arc::strong_count(&self.write_lock) > 1 {
            return;
        }
        release_write_lock_on_drop(&self.write_lock);
    }
}

/// Releases `{vault}/.scheda/write.lock` when this is the last handle that
/// shares it, before Index teardown waits on merge threads.
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
            schema: Arc::new(LoadedSchema::default()),
            written: Arc::new(Mutex::new(BTreeSet::new())),
            in_txn: Arc::new(Mutex::new(false)),
            writer: Arc::from("stub"),
            journal_ids: Arc::new(Mutex::new(JournalIdMaps::default())),
            #[cfg(feature = "find")]
            search: Arc::new(SearchEngine::empty()),
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

    /// This collection's declaration as `api.v1.Declaration`. Not a Vault listing (`info`).
    pub fn decl(&self) -> Result<Declaration, Error> {
        schema::to_proto_declaration(&self.decl)
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
    /// Rust helper for declaration inspection. The collection catalog is
    /// [`decl`](Self::decl).
    #[must_use]
    pub fn segment_max_bytes(&self) -> Option<u64> {
        self.decl.segment_max_bytes
    }

    /// Fixed `id` of a `singleton` collection. `None` otherwise.
    ///
    /// Rust helper for declaration inspection. The collection catalog is
    /// [`decl`](Self::decl).
    #[must_use]
    pub fn singleton_id(&self) -> Option<&str> {
        self.decl.singleton_id.as_deref()
    }

    fn generation_dir(&self) -> Result<PathBuf, Error> {
        self.index.dir()
    }

    fn upsert_lexical(&self, record: &Record) -> Result<(), Error> {
        if !self.decl.any_indexed() {
            return Ok(());
        }
        #[cfg(feature = "find")]
        {
            self.search
                .upsert(&self.decl, record, self.embedder.as_deref())
        }
        #[cfg(not(feature = "find"))]
        {
            let _ = record;
            let dir = self.generation_dir()?;
            index::mark_stale(&dir)
        }
    }

    fn forget_lexical(&self, id: &str) -> Result<(), Error> {
        if !self.decl.any_indexed() {
            return Ok(());
        }
        #[cfg(feature = "find")]
        {
            self.search.forget(&self.decl, id)
        }
        #[cfg(not(feature = "find"))]
        {
            let _ = id;
            let dir = self.generation_dir()?;
            index::mark_stale(&dir)
        }
    }

    fn rollback_file_and_index(&self, rel: &str, id: &str) {
        let path = rel.to_owned();
        let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&path));
        let _ = self.unnote_write(rel);
        let _ = self.restore_indexed_from_disk(id);
    }

    fn restore_indexed_from_disk(&self, id: &str) -> Result<(), Error> {
        match self.lookup_decl(&self.decl, id) {
            Ok(record) if store::record_indexable(&self.decl, &record) => {
                self.upsert_lexical(&record)
            }
            Ok(_) => self.forget_lexical(id),
            Err(err) if crate::reindex::is_absent(&err) => self.forget_lexical(id),
            Err(err) => Err(err),
        }
    }

    fn record_exists(&self, id: &str) -> Result<bool, Error> {
        if !self.decl.lexical_indexed() {
            return Ok(store::locate_file(&self.vault, &self.decl, id)?.is_some());
        }
        #[cfg(feature = "find")]
        {
            self.search.has_id(&self.decl.name, id)
        }
        #[cfg(not(feature = "find"))]
        Ok(store::locate_file(&self.vault, &self.decl, id)?.is_some())
    }

    fn require_write(&self, op: &str) -> Result<(), Error> {
        let mut slot = crate::error::mutex(&self.write_lock, "[Collection] lock write lock")
            .with_context(|| format!("[Collection::{op}] take write lock"))?;
        take_write_slot(
            &mut slot,
            &self.vault,
            #[cfg(feature = "find")]
            &self.search,
            "Collection",
            op,
        )
    }

    fn conclude_write<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        release_lock_unless_in_transaction(
            &self.write_lock,
            &self.in_txn,
            #[cfg(feature = "find")]
            &self.search,
            result,
        )
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
        let paths: Vec<String> = {
            let mut set = crate::error::mutex(&self.written, "[Collection] lock written")?;
            let paths: Vec<String> = set.iter().cloned().collect();
            set.clear();
            persist_written(&self.vault, &set)?;
            paths
        };
        if paths.is_empty() {
            return Ok(());
        }
        let snapshot = crate::reindex::snapshot_restored(&self.vault, &self.schema, &paths)?;
        isolated::discard_recorded(&self.vault, &paths)?;
        let dir = self.generation_dir()?;
        repair_index_after_discard(
            &self.vault,
            &dir,
            &self.journal_ids,
            #[cfg(feature = "find")]
            &self.search,
            self.embedder.as_deref(),
        )?;
        crate::reindex::enqueue_restored(&self.vault, &self.schema, &paths, &snapshot)
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
        let paths: Vec<String> = {
            let set = crate::error::mutex(&self.written, "[Collection] lock written")?;
            set.iter().cloned().collect()
        };
        match isolated::commit_recorded(
            &self.vault,
            autocommit_subject(message),
            &paths,
            &self.identity,
        ) {
            Ok(()) => {
                let mut set = crate::error::mutex(&self.written, "[Collection] lock written")?;
                set.clear();
                persist_written(&self.vault, &set)?;
                #[cfg(feature = "find")]
                self.search.flush()?;
                if self.auto_sync && !self.remotes.is_empty() {
                    git::push(&self.vault)?;
                }
                Ok(())
            }
            Err(err) => {
                let paths: Vec<String> = {
                    let mut set = crate::error::mutex(&self.written, "[Collection] lock written")?;
                    let paths: Vec<String> = set.iter().cloned().collect();
                    set.clear();
                    persist_written(&self.vault, &set)?;
                    paths
                };
                let _ = isolated::discard_recorded(&self.vault, &paths);
                if !paths.is_empty() {
                    let dir = self.generation_dir()?;
                    let _ = repair_index_after_discard(
                        &self.vault,
                        &dir,
                        &self.journal_ids,
                        #[cfg(feature = "find")]
                        &self.search,
                        self.embedder.as_deref(),
                    );
                }
                Err(err)
            }
        }
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
    /// plaintext. `exclude_lexical_index` skips the Tantivy write;
    /// `exclude_vector_index` skips the embedder and the vector Index.
    ///
    /// `raw` is the UTF-8 file / payload bytes. Empty is allowed. Non-UTF-8
    /// is refused and writes no file.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject. Outside a transaction the write lock is held only for this
    /// call, and the lexical Index writer is released with it.
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
                    let (id, rel, rollback) = journal::insert_journal(
                        &self.vault,
                        &self.decl,
                        &record,
                        &dir,
                        &mut maps,
                        &self.writer,
                    )?;
                    drop(maps);
                    self.note_write(rel.clone())?;
                    if let Err(err) = self.upsert_lexical(&record) {
                        self.rollback_journal_insert(&id, &rel, &rollback);
                        return Err(err);
                    }
                    if let Err(err) = crate::reindex::enqueue(&self.vault, &self.decl, &id) {
                        let _ = self.forget_lexical(&id);
                        self.rollback_journal_insert(&id, &rel, &rollback);
                        return Err(err);
                    }
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
                    if let Err(err) = self.upsert_lexical(&record) {
                        let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
                        let _ = self.unnote_write(&rel);
                        return Err(err);
                    }
                    if let Err(err) = crate::reindex::enqueue(&self.vault, &self.decl, id) {
                        let _ = self.forget_lexical(id);
                        let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
                        let _ = self.unnote_write(&rel);
                        return Err(err);
                    }
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
                let maps =
                    crate::error::mutex(&self.journal_ids, "[Collection::get] lock journal ids")?;
                journal::find_journal(&self.vault, &self.decl, id, &maps)
            }
            Kind::Document | Kind::Singleton => {
                store::find_document(&self.vault, &self.decl, id, self.encryption.as_ref())
            }
        }
    }

    /// Walk this `journal` in append order without opening the Index.
    ///
    /// Closed segments in Schema sequence, each file from first line to last,
    /// then the open segment. Rotation does not reorder earlier lines. Each
    /// item is one Record; `raw` is the line bytes including the trailing
    /// newline. `document` / `singleton` → `unsupported`. A missing Index
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
    /// one id equal to the Schema `singleton_id`. A missing collection
    /// directory is an empty list. Encrypted collections do not need the
    /// handle secret.
    pub fn ids(&self) -> Result<Vec<String>, Error> {
        store::collection_ids(&self.vault, &self.decl)
    }

    /// Replace a whole `document` or `singleton`. `journal` → `unsupported`.
    ///
    /// `exclude_lexical_index` skips the Tantivy write; `exclude_vector_index`
    /// skips the embedder and the vector Index.
    ///
    /// `raw` is the UTF-8 file bytes. Empty is allowed. Non-UTF-8 is
    /// refused and writes no file.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject. Outside a transaction the write lock is held only for this
    /// call, and the lexical Index writer is released with it.
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
            if let Err(err) = self.upsert_lexical(&record) {
                let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
                let _ = self.unnote_write(&rel);
                return Err(err);
            }
            if let Err(err) = crate::reindex::enqueue(&self.vault, &self.decl, id) {
                self.rollback_file_and_index(&rel, id);
                return Err(err);
            }
            self.finish_mutation(message)?;
            Ok(())
        })())
    }

    /// Delete a `document` or `singleton` Record. `journal` → `unsupported`.
    ///
    /// Unknown `id` → `not_found`. An illegal slug → `invalid` before any
    /// file is touched. The file is unlinked as a whole and staged like
    /// `add` / `replace`: autocommit outside a transaction, part of the
    /// batch inside one. Isolated collections use the nested git. When
    /// `find` is on, the Record leaves the Index; when `find` is off, the
    /// Index is marked stale.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject. Outside a transaction the write lock is held only for this
    /// call, and the lexical Index writer is released with it.
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
            self.note_write(rel.clone())?;
            store::remove_document(&self.vault, &self.decl, id)?;
            if let Err(err) = self.forget_lexical(id) {
                let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
                let _ = self.unnote_write(&rel);
                return Err(err);
            }
            if let Err(err) = crate::reindex::enqueue(&self.vault, &self.decl, id) {
                self.rollback_file_and_index(&rel, id);
                return Err(err);
            }
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
    /// Without a projector, only `all` and Filter on `id` are valid.
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
            schema: &self.schema,
            vault: &self.vault,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
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
            schema: &self.schema,
            vault: &self.vault,
            journal_ids: &self.journal_ids,
            projector: self.projector.as_deref(),
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
                let maps = crate::error::mutex(
                    &self.journal_ids,
                    "[Collection::history] lock journal ids",
                )?;
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

    fn unnote_write(&self, rel: &str) -> Result<(), Error> {
        let mut set = crate::error::mutex(&self.written, "[Collection::unnote_write] lock")?;
        set.remove(rel);
        persist_written(&self.vault, &set)
    }

    fn rollback_journal_insert(&self, id: &str, rel: &str, rollback: &journal::JournalRollback) {
        let _ = journal::rollback_insert(rollback);
        if let Ok(mut maps) =
            crate::error::mutex(&self.journal_ids, "[Collection::add] rollback maps")
        {
            if let Ok(dir) = self.generation_dir() {
                let _ = index::forget_remembered(&dir, &mut maps, &self.decl.name, id);
            }
            maps.forget_segment_line(&rollback.path);
        }
        if rollback.previous_len == 0 {
            let _ = self.unnote_write(rel);
        }
    }

    fn lookup_decl(&self, decl: &CollectionDecl, id: &str) -> Result<Record, Error> {
        match decl.kind {
            Kind::Journal => {
                let maps = crate::error::mutex(&self.journal_ids, "[Collection] lock journal ids")?;
                journal::find_journal(&self.vault, decl, id, &maps)
            }
            Kind::Document | Kind::Singleton => {
                store::find_document(&self.vault, decl, id, self.encryption.as_ref())
            }
        }
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
    schema: &LoadedSchema,
    names: &[&str],
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if names.is_empty() {
        return Ok(schema
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
        let Some(decl) = schema.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in Schema"
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
    schema: &LoadedSchema,
    names: &[&str],
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if names.is_empty() {
        return Ok(schema
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
        let Some(decl) = schema.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in Schema"
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
fn apply_filter_schema(
    decls: Vec<schema::CollectionDecl>,
    filter: &Filter,
    explicit: bool,
    projector: Option<&dyn crate::Projector>,
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if explicit {
        for decl in &decls {
            query::validate(decl, filter, projector)?;
        }
        return Ok(decls);
    }
    Ok(decls
        .into_iter()
        .filter(|decl| query::validate(decl, filter, projector).is_ok())
        .collect())
}

#[cfg(feature = "find")]
fn read_record(
    vault: &Path,
    schema: &LoadedSchema,
    journal_ids: &Mutex<JournalIdMaps>,
    collection: &str,
    id: &str,
) -> Result<Record, Error> {
    let Some(decl) = schema.get(collection) else {
        bail!(Error::not_found(format!(
            "collection {collection} is not in Schema"
        )));
    };
    match decl.kind {
        Kind::Journal => {
            let maps = crate::error::mutex(journal_ids, "[Vault] lock journal ids")?;
            journal::find_journal(vault, decl, id, &maps)
        }
        Kind::Document | Kind::Singleton => store::find_document(vault, decl, id, None),
    }
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
    let record = match read_record(ctx.vault, ctx.schema, ctx.journal_ids, &decl.name, id) {
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
    let schema = match projector.schema(&decl.name) {
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
    Ok(crate::projector::fts_blob(&schema, &doc))
}

#[cfg(feature = "find")]
struct QueryCtx<'a> {
    engine: &'a SearchEngine,
    schema: &'a LoadedSchema,
    vault: &'a Path,
    journal_ids: &'a Mutex<JournalIdMaps>,
    projector: Option<&'a dyn crate::Projector>,
}

#[cfg(feature = "find")]
impl QueryCtx<'_> {
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
        let include_raw = query.include_raw == Some(true);
        let explicit = !names.is_empty();
        let decls = apply_filter_schema(
            resolve_collections(self.schema, names)?,
            filter,
            explicit,
            self.projector,
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
            match read_record(self.vault, self.schema, self.journal_ids, &collection, &id) {
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
        let mut decls = resolve_search_collections(self.schema, names)?;
        if filter.kind.is_some() {
            decls = apply_filter_schema(decls, filter, explicit, self.projector)?;
        }
        if decls.is_empty() {
            return Ok(SearchHits::default());
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
                return Ok(SearchHits::default());
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
        let mut hits = self.engine.search(
            &decls,
            query,
            embedder,
            allowed.as_ref(),
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
        let mut records = Vec::with_capacity(hits.hits.len());
        for hit in &hits.hits {
            records.push(read_record(
                self.vault,
                self.schema,
                self.journal_ids,
                hit_collection(hit),
                hit.id.as_deref().unwrap_or(""),
            )?);
        }
        let refs: Vec<&Record> = records.iter().collect();
        let scores = crate::embed::score_all(reranker, text, &refs)?;
        let mut scored: Vec<_> = scores
            .into_iter()
            .zip(hits.hits)
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
        let mut out = SearchHits::default();
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
            match read_record(
                self.vault,
                self.schema,
                self.journal_ids,
                collection,
                hit.id.as_deref().unwrap_or(""),
            ) {
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

fn gitignore_needs_write(vault: &Path) -> Result<bool, Error> {
    let path = vault.join(".gitignore");
    if !path.exists() {
        return Ok(true);
    }
    let text = fs::read_to_string(&path).context("[Vault::open] read gitignore")?;
    let already_scheda = text.lines().any(|line| {
        let line = line.trim();
        line == ".scheda/" || line == ".scheda"
    });
    let already_isolated = text.lines().any(|line| {
        let line = line.trim();
        line == ".isolated/" || line == ".isolated"
    });
    Ok(!(already_scheda && already_isolated))
}

fn ensure_gitignore(vault: &Path) -> Result<bool, Error> {
    let path = vault.join(".gitignore");
    let mut text = if path.exists() {
        fs::read_to_string(&path).context("[Vault::open] read gitignore")?
    } else {
        String::new()
    };
    let already_scheda = text.lines().any(|line| {
        let line = line.trim();
        line == ".scheda/" || line == ".scheda"
    });
    let already_isolated = text.lines().any(|line| {
        let line = line.trim();
        line == ".isolated/" || line == ".isolated"
    });
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

/// Write `.scheda/` and `.isolated/` into `.gitignore` and commit when this
/// tree is new (init or clone). No writer exists yet, so this does not take
/// `write.lock`. A later mutating call is then the first `try_lock`.
fn commit_gitignore_if_needed(vault: &Path, identity: &git::CommitIdentity) -> Result<(), Error> {
    if ensure_gitignore(vault)? {
        git::commit(
            vault,
            AUTO_COMMIT_MESSAGE,
            &[String::from(".gitignore")],
            identity,
        )?;
    }
    Ok(())
}

/// Uncommitted write paths after a crash. Next to `write.lock`, not under
/// `index/`, so `reindex` does not delete the journal.
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

fn repair_index_after_discard(
    vault: &Path,
    index_dir: &Path,
    journal_ids: &Mutex<JournalIdMaps>,
    #[cfg(feature = "find")] search: &SearchEngine,
    embedder: Option<&dyn Embedder>,
) -> Result<(), Error> {
    let loaded = schema::load(vault)?;
    #[cfg(feature = "find")]
    {
        search.close_for_rebuild();
        search.rebuild(vault, &loaded, embedder)?;
    }
    #[cfg(not(feature = "find"))]
    let _ = embedder;
    *crate::error::mutex(journal_ids, "[Vault] lock journal ids")? =
        index::load_journal_ids(index_dir, &loaded)?;
    Ok(())
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
                    let result = crate::reindex::clear_stale(vault)
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

fn take_write_slot(
    slot: &mut WriteLock,
    vault: &Path,
    #[cfg(feature = "find")] search: &SearchEngine,
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
    #[cfg(feature = "find")]
    if let Err(err) = search.enable_write() {
        let _ = file.unlock();
        return Err(err).with_context(|| format!("[{kind}::{op}] take write lock"));
    }
    slot.file = Some(file);
    slot.owner = Some(me);
    Ok(())
}

fn release_lock_unless_in_transaction<T>(
    write_lock: &Mutex<WriteLock>,
    in_txn: &Mutex<bool>,
    #[cfg(feature = "find")] search: &SearchEngine,
    result: Result<T, Error>,
) -> Result<T, Error> {
    let in_txn = match flag(in_txn) {
        Ok(v) => v,
        Err(err) => {
            #[cfg(feature = "find")]
            let _ = search.release_writer();
            let _ = release_held_write_lock(write_lock);
            return result.and(Err(err));
        }
    };
    if in_txn {
        return result;
    }
    #[cfg(feature = "find")]
    let result = match search.release_writer() {
        Ok(()) => result,
        Err(err) => result.and(Err(err)),
    };
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
    use crate::git;
    use crate::{Code, Declaration, Encryption, Error, VaultInfo};
    #[cfg(feature = "search")]
    use crate::{Embedder, SearchQuery};

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
    fn encrypted_document_schema() -> Declaration {
        Declaration {
            kind: crate::Unit::default().into(),
            ..Declaration::default()
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
        fs::remove_dir_all(vault.index_dir()).unwrap();
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

    fn document_schema() -> Declaration {
        Declaration {
            kind: crate::Unit::default().into(),
            ..Declaration::default()
        }
    }

    fn journal_schema() -> Declaration {
        serde_json::from_value(serde_json::json!({
            "journal": {}
        }))
        .unwrap()
    }

    #[cfg(feature = "search")]
    fn journal_vector_schema() -> Declaration {
        serde_json::from_value(serde_json::json!({
            "journal": {}
        }))
        .unwrap()
    }

    fn empty_add() -> &'static [u8] {
        b""
    }

    #[cfg(feature = "find")]
    fn empty_replace() -> &'static [u8] {
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
    fn batch_inside_transaction_opens_lexical_writer_once() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        vault.begin().unwrap();
        vault
            .create_collection("c", document_schema(), None)
            .unwrap();
        vault.commit("schema").unwrap();

        vault.begin().unwrap();
        let before = vault.lexical_writer_opens();
        let collection = vault.collection("c").unwrap();
        for i in 0..8 {
            collection
                .add(&format!("row-{i}"), empty_add(), None)
                .unwrap();
        }
        vault.commit("batch").unwrap();
        assert_eq!(vault.lexical_writer_opens().saturating_sub(before), 1);
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
        let err = reader.reindex().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert!(
            err.stack
                .iter()
                .any(|frame| frame.contains("[Vault::reindex]")),
            "operation missing from stack: {:?}",
            err.stack
        );
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
        let _: fn(&Vault, &str, Declaration, Option<&str>) -> Result<Collection, Error> =
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
        let schema = Declaration {
            kind: crate::Unit::default().into(),
            ..Declaration::default()
        };
        writer.create_collection("c", schema, None).unwrap();
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
        vault
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

    #[cfg(feature = "find")]
    #[test]
    fn reindex_releases_write_lock_during_walk() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        let saw_free = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        vault
            .set_reindex_walk_hook({
                let start = std::sync::Arc::clone(&start);
                let done = std::sync::Arc::clone(&done);
                let saw_free = std::sync::Arc::clone(&saw_free);
                std::sync::Arc::new(move |_| {
                    start.wait();
                    done.wait();
                    saw_free.store(true, std::sync::atomic::Ordering::Relaxed);
                })
            })
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                assert!(
                    !vault.holds_write_lock(),
                    "write.lock must be free during the walk"
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
        assert!(saw_free.load(std::sync::atomic::Ordering::Relaxed));
        let mut ids = find_ids(&vault, "c");
        ids.sort();
        assert_eq!(ids, ["alpha", "beta"]);
        assert!(
            !scratch
                .vault()
                .join(".scheda")
                .join("reindex")
                .join("active")
                .exists()
        );
        vault
            .collection("c")
            .unwrap()
            .add("gamma", empty_add(), None)
            .unwrap();
        let queue = scratch
            .vault()
            .join(".scheda")
            .join("reindex")
            .join("queue");
        let leftover = queue.is_dir()
            && fs::read_dir(&queue)
                .unwrap()
                .any(|entry| entry.unwrap().path().is_file());
        assert!(!leftover, "add after publish must not enqueue");
    }

    #[cfg(feature = "find")]
    #[test]
    fn publish_gives_up_when_transaction_holds_write_lock() {
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
        let window = std::time::Duration::from_millis(250);
        let _guard = crate::reindex::set_publish_window(window);
        std::thread::scope(|s| {
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            s.spawn(move || {
                start.wait();
                holder.begin().unwrap();
                done.wait();
                release_rx.recv().unwrap();
                holder.discard().unwrap();
            });
            let started = std::time::Instant::now();
            let err = vault.reindex().unwrap_err();
            assert!(
                started.elapsed() >= window,
                "publish must retry for the shortened window"
            );
            assert_locked_reindex(&err);
            assert_eq!(live_generation(&vault), live);
            release_tx.send(()).unwrap();
        });
    }

    #[cfg(feature = "search")]
    #[test]
    fn same_model_catch_up_copies_queued_vectors() {
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
        assert_eq!(
            embedder.slots(),
            after_add + 1,
            "catch-up embeds a new id once; same-model copy has no previous slot"
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

    #[cfg(feature = "search")]
    #[test]
    fn catch_up_without_embedder_does_not_create_vector_table() {
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
        drop(seeded);
        let vault = open_search(scratch.vault(), scratch.options(), None);
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
                    .add("queued", rec_tag("queued mineral"), None)
                    .unwrap();
                done.wait();
            });
            vault.reindex().unwrap();
        });
        let generation = live_generation(&vault);
        assert!(
            !vault
                .index_dir()
                .join(generation.to_string())
                .join("vector")
                .exists(),
            "catch-up without an embedder must not create a vector table"
        );
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
    fn remove_during_walk_is_skipped_not_not_found() {
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

    #[cfg(feature = "find")]
    #[test]
    fn admin_ops_are_locked_during_reindex_but_push_is_not() {
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
                assert_locked_reindex(
                    &other
                        .create_collection("x", document_schema(), None)
                        .unwrap_err(),
                );
                assert_locked_reindex(&other.discard().unwrap_err());
                assert_locked_reindex(&other.pull().unwrap_err());
                assert_locked_reindex(&other.sync().unwrap_err());
                let push = other.push().unwrap_err();
                assert_ne!(
                    push.known_code(),
                    Some(Code::Locked),
                    "push must not be locked by reindex: {:?}",
                    push.stack
                );
                done.wait();
            });
            vault.reindex().unwrap();
        });
    }

    #[cfg(feature = "find")]
    #[test]
    fn reindex_inside_transaction_writes_no_marker() {
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        vault.begin().unwrap();
        vault.reindex().unwrap();
        assert!(vault.holds_write_lock());
        assert!(
            !scratch
                .vault()
                .join(".scheda")
                .join("reindex")
                .join("active")
                .exists()
        );
        vault.discard().unwrap();
        assert_eq!(find_ids(&vault, "c"), ["alpha"]);
    }

    #[cfg(feature = "find")]
    #[test]
    fn journal_add_during_walk_is_in_map_after_publish() {
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

    #[cfg(feature = "find")]
    #[test]
    fn replace_enqueue_failure_keeps_file_and_index() {
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
        let scratch = Scratch::new();
        let vault = seeded(&scratch);
        let other = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        let failed = std::sync::Arc::new(AtomicBool::new(false));
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let done = std::sync::Arc::new(std::sync::Barrier::new(2));
        let queue = scratch
            .vault()
            .join(".scheda")
            .join("reindex")
            .join("queue");
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
                if queue.is_dir() {
                    let _ = fs::remove_dir_all(&queue);
                }
                fs::write(&queue, b"not-a-directory").unwrap();
                let err = other
                    .collection("c")
                    .unwrap()
                    .replace("alpha", empty_replace(), None);
                failed.store(err.is_err(), AtomicOrdering::Relaxed);
                let _ = fs::remove_file(&queue);
                let _ = fs::create_dir_all(&queue);
                done.wait();
            });
            vault.reindex().unwrap();
        });
        assert!(
            failed.load(AtomicOrdering::Relaxed),
            "replace must fail when the catch-up queue cannot be written"
        );
        let got = vault.collection("c").unwrap().get("alpha").unwrap();
        assert_eq!(got.id.as_deref(), Some("alpha"));
        assert_eq!(find_ids(&vault, "c"), ["alpha"]);
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
    fn stale_active_without_lock_is_cleared_on_open() {
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
        assert!(!reindex.join("active").exists());
        assert!(
            !reindex.join("queue").join("deadbeef").exists(),
            "stale queue files must be dropped"
        );
        assert_eq!(
            fs::read_to_string(opened.index_dir().join("current")).unwrap(),
            live
        );
        opened
            .collection("c")
            .unwrap()
            .add("gamma", empty_add(), None)
            .unwrap();
        assert!(
            fs::read_dir(reindex.join("queue"))
                .map(|entries| !entries.filter_map(Result::ok).any(|e| e.path().is_file()))
                .unwrap_or(true),
            "add after stale clear must not enqueue"
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
    fn restore_of_uncommitted_journal_segment_enqueues_deleted_ids() {
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
        vault.reindex().unwrap();
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

        let _reindex_lock = crate::reindex::acquire_lock(&scratch.vault()).unwrap();
        crate::reindex::write_active(&scratch.vault(), 2).unwrap();

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
        let files = crate::reindex::list_queue(&scratch.vault()).unwrap();
        let mut queued = Vec::new();
        for path in files {
            if let Some(entry) = crate::reindex::take_queue_file(&path).unwrap() {
                queued.push(entry);
            }
        }
        assert!(
            queued.iter().any(|(c, id)| c == "j" && id == "lost"),
            "deleted journal line must be queued, got {queued:?}"
        );
    }
}
