use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(feature = "search")]
use crate::Reranker;
use crate::error::{Code, ContextAs, ContextIo, bail, io_code};
use crate::git;
use crate::index::{self, JournalIdMaps};
use crate::isolated;
use crate::journal;
#[cfg(feature = "find")]
use crate::query;
use crate::schema::{self, CollectionDecl, Kind, Schema as LoadedSchema};
#[cfg(feature = "find")]
use crate::search::{SearchEngine, marked_text};
use crate::store;
use crate::{
    Embedder, Encryption, Error, History, OpenOptions, Record, RecordAdd, RecordReplace,
    ReindexResult, Schema, VaultInfo,
};
#[cfg(feature = "find")]
use crate::{Filter, FindQuery, FindResult};
#[cfg(feature = "search")]
use crate::{SearchHits, SearchQuery, search_hits};

const AUTO_COMMIT_MESSAGE: &str = "scheda";

/// A git working tree that holds collections of records.
pub struct Vault {
    path: PathBuf,
    index: Arc<index::IndexHandle>,
    remotes: Vec<String>,
    auto_sync: bool,
    write_lock: Arc<Mutex<Option<File>>>,
    schema: Mutex<Arc<LoadedSchema>>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    #[cfg(feature = "find")]
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
    identity: Arc<git::CommitIdentity>,
    encryption: Option<Encryption>,
}

/// A named set of records with one write mode (`document`, `journal`, or `singleton`).
pub struct Collection {
    vault: PathBuf,
    index: Arc<index::IndexHandle>,
    write_lock: Arc<Mutex<Option<File>>>,
    auto_sync: bool,
    remotes: Vec<String>,
    decl: CollectionDecl,
    schema: Arc<LoadedSchema>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    #[cfg(feature = "find")]
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
    identity: Arc<git::CommitIdentity>,
    encryption: Option<Encryption>,
}

impl Vault {
    /// Open, create, or clone a Vault.
    ///
    /// Path exists → open (`remote_repo` ignored). Path missing + `remote_repo`
    /// → clone. Path missing → `git init`. Path exists but is not a Vault →
    /// `invalid`.
    /// Optional caller embedder. Turns on the vector leg of `search`.
    /// Without the `search` crate feature, a `Some` value fails
    /// (`index_vector`): that leg is absent. The signature is the same in
    /// every build.
    ///
    /// `OpenOptions.author` is the git author and committer for every commit
    /// on this handle (main tree and `{vault}/.isolated/`). Unset `name` or
    /// `email` on a set Author, or characters git cannot carry, fail here
    /// before the first commit. Unset Author → local git config, then
    /// `Scheda <scheda@invalid>`.
    ///
    /// `OpenOptions.encryption` is stored on this handle for I/O of collections
    /// whose Schema marks `encrypted`. A build without the `encrypt` crate
    /// feature refuses a set value (`unsupported_encryption`) before any
    /// directory is created. Unset `encryption` is unchanged: `open` of the
    /// Vault still succeeds.
    pub fn open(
        path: impl AsRef<Path>,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
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
            let identity = git::identity_from_open(&options, path)?;
            return Self::open_existing(path, options, embedder, identity);
        }
        let remote = remote_repo_url(&options).map(str::to_owned);
        if let Some(url) = remote {
            return Self::clone_into(&url, path, options, embedder);
        }
        Self::init_at(path, options, embedder)
    }

    fn init_at(
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, Error> {
        prepare_new_work_tree(path)?;
        git::init(path)?;
        let identity = git::identity_from_open(&options, path)?;
        commit_gitignore_if_needed(path, &identity)?;
        Self::open_existing(path, options, embedder, identity)
    }

    fn clone_into(
        url: &str,
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, Error> {
        let url = url.trim();
        if url.is_empty() {
            bail!(Error::invalid("clone URL is empty"));
        }
        prepare_new_work_tree(path)?;
        git::clone(url, path)?;
        let identity = git::identity_from_open(&options, path)?;
        commit_gitignore_if_needed(path, &identity)?;
        Self::open_existing(path, without_remote_repo(options), embedder, identity)
    }

    fn open_existing(
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
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
        let _ = fs::remove_file(path.join(".scheda").join("index").join("txn"));
        #[cfg(feature = "find")]
        let search = Arc::new(SearchEngine::open(&generation, false)?);
        let encryption = options.encryption.as_option().cloned();
        let vault = Self {
            path,
            index,
            remotes,
            auto_sync,
            write_lock: Arc::new(Mutex::new(None)),
            schema: Mutex::new(schema),
            written,
            in_txn,
            journal_ids,
            #[cfg(feature = "find")]
            search,
            embedder,
            identity: Arc::new(identity),
            encryption,
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
    #[must_use]
    pub fn holds_write_lock(&self) -> bool {
        match self.write_lock.lock() {
            Ok(slot) => slot.is_some(),
            Err(_) => true,
        }
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

    /// Schema `version` values present in this Vault.
    #[must_use]
    pub fn versions(&self) -> Vec<u32> {
        match self.schema_arc() {
            Ok(schema) => schema.versions(),
            Err(_) => Vec::new(),
        }
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
            journal_ids: Arc::clone(&self.journal_ids),
            #[cfg(feature = "find")]
            search: Arc::clone(&self.search),
            embedder: self.embedder.clone(),
            identity: Arc::clone(&self.identity),
            encryption: self.encryption.clone(),
        })
    }

    /// Run a Tantivy Query on the Index, then open matching Record files.
    /// Empty `collections` is every non-isolated, non-encrypted collection
    /// on this handle. Naming an isolated collection is an ordinary query.
    /// Naming an encrypted collection is `unsupported`.
    ///
    /// Page order is score descending. Equal scores break by the Index `ord`
    /// fast field, ascending (earlier indexed first). `ord` is unique within
    /// a collection; two Records in different collections that share both
    /// score and `ord` stay unordered relative to each other. One `journal`
    /// collection uses append order instead.
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
        }
        .find(collections, query, offset, count)
    }

    /// Hybrid retrieval: BM25 and ANN when both are on, then RRF, then an
    /// optional reranker. Empty `collections` is every non-isolated,
    /// non-encrypted collection on this handle. Naming an isolated
    /// collection is an ordinary query. Naming an encrypted collection
    /// is `unsupported`.
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
    pub fn commit(&self, message: &str) -> Result<(), Error> {
        self.require_write_lock("commit")?;
        if !flag(&self.in_txn)? {
            bail!(Error::invalid("commit requires an open transaction"));
        }
        self.commit_recorded(message)?;
        set_in_txn(&self.in_txn, false)?;
        self.push_if_auto_sync()
    }

    /// Start a transaction on this handle. Later writes on this handle do
    /// not autocommit until `commit` or `discard`. Dropping the handle ends
    /// the transaction without a git commit; recorded paths stay in
    /// `{vault}/.scheda/tx.log` until `begin`, `discard`, or a mutating
    /// call outside a transaction restores them.
    ///
    /// Discards leftover Scheda writes first. `invalid` if a transaction is
    /// already open on this handle.
    pub fn begin(&self) -> Result<(), Error> {
        self.require_write_lock("begin")?;
        if flag(&self.in_txn)? {
            bail!(Error::invalid("already in a transaction"));
        }
        self.restore_recorded()?;
        set_in_txn(&self.in_txn, true)?;
        Ok(())
    }

    /// Restore Scheda-recorded paths to HEAD in the repository that owns
    /// each path, end the transaction if any on this handle, reload Schema,
    /// and rebuild the Index. Allowed without an open transaction so a later
    /// process can clear crash leftovers.
    pub fn discard(&self) -> Result<(), Error> {
        self.require_write_lock("discard")?;
        self.restore_recorded()?;
        set_in_txn(&self.in_txn, false)?;
        Ok(())
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
        isolated::discard_recorded(&self.path, &paths)?;
        self.reload_after_restore()
    }

    fn reload_after_restore(&self) -> Result<(), Error> {
        let loaded = schema::load(&self.path)?;
        let dir = self.generation_dir()?;
        *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? =
            index::load_journal_ids(&dir, &loaded)?;
        #[cfg(feature = "find")]
        {
            self.search.close_for_rebuild();
            self.search
                .rebuild(&self.path, &loaded, self.embedder.as_deref())?;
        }
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

    /// `git push` to `origin`. Needs the write lock.
    pub fn push(&self) -> Result<(), Error> {
        self.require_write_lock("push")?;
        git::push(&self.path)
    }

    /// `git pull` from `origin`, then reload Schema and rebuild the Index.
    pub fn pull(&self) -> Result<(), Error> {
        self.require_write_lock("pull")?;
        git::pull(&self.path)?;
        self.reload_after_restore()
    }

    /// `pull` then `push`. Needs the write lock.
    pub fn sync(&self) -> Result<(), Error> {
        self.require_write_lock("sync")?;
        git::pull(&self.path)?;
        self.reload_after_restore()?;
        git::push(&self.path)
    }

    fn require_write_lock(&self, op: &str) -> Result<(), Error> {
        let mut slot = crate::error::mutex(&self.write_lock, "[Vault] lock write lock")?;
        if slot.is_none() {
            *slot = Some(acquire_write_lock(&self.path)?);
            #[cfg(feature = "find")]
            self.search.enable_write()?;
        }
        let _ = op;
        Ok(())
    }

    /// Patch `.gitignore` on an existing Vault. Takes `write.lock` for that
    /// write, then `unlock`s so `open` does not keep the writer lock.
    fn write_gitignore_under_lock(&self) -> Result<(), Error> {
        {
            let mut slot = crate::error::mutex(&self.write_lock, "[Vault::open] lock write lock")?;
            if slot.is_none() {
                *slot = Some(acquire_write_lock(&self.path)?);
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
            if let Some(file) = slot.take()
                && let Err(err) = file.unlock()
                && result.is_ok()
            {
                return Err(err).context("[Vault::open] unlock write lock");
            }
        }
        result
    }

    /// Validate Schema, encodings, journal lines, `id` uniqueness, and conflict
    /// markers. Isolated Record files are scanned when `{vault}/.isolated/`
    /// exists. Parent git must not track `.isolated/`; nested git must have no
    /// Remote. On Unix, `{vault}/.scheda/` must be owner-only (`0700`
    /// directories, `0600` files); a wider mode is reported with that path.
    /// Does not write Records. Lexical Index freshness is validated
    /// only when the crate is built with `find`; a build without `find` has no
    /// lexical segments and no way to rebuild them, so it ignores the stale
    /// marker even though its own writes keep setting it. An encrypted
    /// collection whose secret is absent is not checked (`crypto`); a build
    /// without `encrypt` reports `unsupported_encryption`. With the secret,
    /// encrypted collections are validated like any other.
    pub fn check(&self) -> Result<(), Error> {
        isolated::check_perimeter(&self.path)?;
        #[cfg(feature = "find")]
        {
            let dir = self.generation_dir()?;
            index::require_fresh(&dir)?;
        }
        store::check_scheda_owner_only(&self.path)?;
        let schema = schema::load(&self.path)?;
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

    /// Write one collection declaration as `{name}/.schema.yaml`. Reloads Schema
    /// on this handle. `isolated: true` stores Records under `{vault}/.isolated/`
    /// and creates the nested git if needed. Setting or clearing `encrypted`
    /// on a collection that already holds Record files is `invalid`; existing
    /// files are left untouched. A build without the `encrypt` crate feature
    /// refuses a Schema that marks `encrypted` (`unsupported_encryption`) and
    /// writes nothing.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject.
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
        self.drain_abandoned()?;
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
        *crate::error::mutex(&self.schema, "[Vault] lock schema")? = Arc::new(loaded);
        self.finish_mutation(message)?;
        self.collection(name)
    }

    /// Rebuild the Index from Vault files into a new generation directory,
    /// then point `{vault}/.scheda/index/current` at it with one atomic write.
    /// Writes format version `4` and the journal `id` map for every
    /// non-encrypted collection, isolated included. Incremental writes keep
    /// going into the live generation; only this method swaps. A reader that
    /// opened before the swap keeps its generation. A reader older than one
    /// `reindex` cycle may lose those files (`index_missing` or
    /// `index_lexical`). Removes a leftover `{vault}/.isolated/.scheda/`
    /// directory when one is present. Needs the write lock, so there is no
    /// concurrent writer during the swap.
    pub fn reindex(&self) -> Result<ReindexResult, Error> {
        self.require_write_lock("reindex")?;
        #[cfg(feature = "find")]
        self.search.close_for_rebuild();
        let schema = schema::load(&self.path)?;
        #[cfg(feature = "find")]
        let previous = self.generation_dir()?;
        let (counts, maps, new_dir) =
            index::prepare_generation(&self.path, &schema, &self.index.root)?;
        #[cfg(feature = "find")]
        if let Err(err) =
            self.search
                .rebuild_into(&new_dir, &self.path, &schema, self.embedder.as_deref())
        {
            let _ = self.search.set_index_dir(&previous);
            return Err(err);
        }
        #[cfg(not(feature = "find"))]
        index::mark_stale(&new_dir)?;
        #[cfg(feature = "find")]
        if let Err(err) = index::publish_generation(&self.index.root, &new_dir) {
            let _ = self.search.set_index_dir(&previous);
            return Err(err);
        }
        #[cfg(not(feature = "find"))]
        index::publish_generation(&self.index.root, &new_dir)?;
        self.index.set_dir(new_dir)?;
        *crate::error::mutex(&self.journal_ids, "[Vault] lock journal ids")? = maps;
        Ok(counts)
    }

    /// Fill a new generation without swapping `current`. Test seam for a
    /// crash between rebuild and the pointer write.
    #[cfg(all(test, feature = "find"))]
    pub(crate) fn reindex_without_publish(&self) -> Result<PathBuf, Error> {
        self.require_write_lock("reindex")?;
        self.search.close_for_rebuild();
        let schema = schema::load(&self.path)?;
        let previous = self.generation_dir()?;
        let (_, _, new_dir) = index::prepare_generation(&self.path, &schema, &self.index.root)?;
        if let Err(err) =
            self.search
                .rebuild_into(&new_dir, &self.path, &schema, self.embedder.as_deref())
        {
            let _ = self.search.set_index_dir(&previous);
            return Err(err);
        }
        Ok(new_dir)
    }
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("index_dir", &self.index.root)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collection")
            .field("name", &self.decl.name)
            .finish_non_exhaustive()
    }
}

impl Collection {
    #[cfg(test)]
    fn stub() -> Self {
        Self {
            vault: PathBuf::new(),
            index: Arc::new(index::IndexHandle::empty()),
            write_lock: Arc::new(Mutex::new(None)),
            auto_sync: false,
            remotes: Vec::new(),
            decl: CollectionDecl {
                name: String::from("c"),
                kind: Kind::Document,
                version: 1,
                filter_keys: std::collections::BTreeSet::new(),
                segment_max_bytes: None,
                segment_max_lines: None,
                segment_rotate: None,
                singleton_id: None,
                body: None,
                fields: Vec::new(),
                isolated: false,
                encrypted: false,
            },
            schema: Arc::new(LoadedSchema::default()),
            written: Arc::new(Mutex::new(BTreeSet::new())),
            in_txn: Arc::new(Mutex::new(false)),
            journal_ids: Arc::new(Mutex::new(JournalIdMaps::default())),
            #[cfg(feature = "find")]
            search: Arc::new(SearchEngine::empty()),
            embedder: None,
            identity: Arc::new(git::CommitIdentity::fallback()),
            encryption: None,
        }
    }

    /// Collection directory name (same as Schema `name`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.decl.name
    }

    /// This collection's Schema as `api.v1.Schema`. Not a Vault listing (`info`).
    pub fn schema(&self) -> Result<Schema, Error> {
        schema::to_proto_schema(&self.decl, false)
    }

    /// Write mode: `document`, `journal`, or `singleton`.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.decl.kind
    }

    /// Schema `version` of this collection (≥ 1).
    #[must_use]
    pub fn version(&self) -> u32 {
        self.decl.version
    }

    /// Whether `field` is a Schema filter key (`find` / `search` Filter).
    #[must_use]
    pub fn is_filter_key(&self, field: &str) -> bool {
        self.decl.filter_keys.contains(field)
    }

    /// Journal segment size cap in bytes. `None` on `document` / `singleton`.
    #[must_use]
    pub fn segment_max_bytes(&self) -> Option<u64> {
        self.decl.segment_max_bytes
    }

    /// Fixed `id` of a `singleton` collection. `None` otherwise.
    #[must_use]
    pub fn singleton_id(&self) -> Option<&str> {
        self.decl.singleton_id.as_deref()
    }

    /// Schema type name of `name` (`string`, `integer`, …). Nested fields included.
    #[must_use]
    pub fn field_type(&self, name: &str) -> Option<&'static str> {
        self.field(name).map(|field| field.type_.as_str())
    }

    /// Whether Schema marks `name` required. `None` if the field is unknown.
    #[must_use]
    pub fn field_is_required(&self, name: &str) -> Option<bool> {
        self.field(name).map(|field| field.required)
    }

    /// Whether Schema marks `name` nullable. `None` if the field is unknown.
    #[must_use]
    pub fn field_is_nullable(&self, name: &str) -> Option<bool> {
        self.field(name).map(|field| field.nullable)
    }

    /// Whether Schema sets a default for `name`. `None` if the field is unknown.
    #[must_use]
    pub fn field_has_default(&self, name: &str) -> Option<bool> {
        self.field(name).map(|field| field.default.is_some())
    }

    fn field(&self, name: &str) -> Option<&crate::schema::FieldDecl> {
        fn walk<'a>(
            fields: &'a [crate::schema::FieldDecl],
            name: &str,
        ) -> Option<&'a crate::schema::FieldDecl> {
            for field in fields {
                if field.name == name {
                    return Some(field);
                }
                if let Some(found) = walk(&field.fields, name) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.decl.fields, name)
    }

    fn generation_dir(&self) -> Result<PathBuf, Error> {
        self.index.dir()
    }

    fn upsert_lexical(&self, record: &Record) -> Result<(), Error> {
        if self.decl.encrypted {
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
        if self.decl.encrypted {
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

    fn record_exists(&self, id: &str) -> Result<bool, Error> {
        if self.decl.encrypted {
            return Ok(store::locate_file(&self.vault, &self.decl, id)?.is_some());
        }
        #[cfg(feature = "find")]
        {
            self.search.has_id(&self.decl.name, id)
        }
        #[cfg(not(feature = "find"))]
        Ok(store::locate_file(&self.vault, &self.decl, id)?.is_some())
    }

    fn require_write(&self) -> Result<(), Error> {
        let mut slot = crate::error::mutex(&self.write_lock, "[Collection] lock write lock")?;
        if slot.is_none() {
            *slot = Some(acquire_write_lock(&self.vault)?);
            #[cfg(feature = "find")]
            self.search.enable_write()?;
        }
        Ok(())
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
        isolated::discard_recorded(&self.vault, &paths)?;
        let dir = self.generation_dir()?;
        repair_index_after_discard(
            &self.vault,
            &dir,
            &self.journal_ids,
            #[cfg(feature = "find")]
            &self.search,
            self.embedder.as_deref(),
        )
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
    /// Encrypted collections write age armor after the canonical plaintext
    /// is built. A missing secret is `crypto`. The temp file never holds
    /// plaintext.
    ///
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject.
    pub fn add(&self, id: &str, record: RecordAdd, message: Option<&str>) -> Result<(), Error> {
        crate::id::require_slug("id", id)?;
        record.require_valid()?;
        self.require_encryption_secret()?;
        let message = require_commit_message(message)?;
        let mut record = record_from_add(id, &self.decl.name, record);
        self.require_write()?;
        self.drain_abandoned()?;
        self.prepare_isolated_write()?;
        schema::check_strict_edges(&self.schema, &self.decl, &record, |collection, id| {
            self.lookup(collection, id)
        })?;
        match self.decl.kind {
            Kind::Journal => {
                let mut maps =
                    crate::error::mutex(&self.journal_ids, "[Collection::add] lock journal ids")?;
                let dir = self.generation_dir()?;
                let (id, rel, rollback) = journal::insert_journal(
                    &self.vault,
                    &self.decl,
                    record.clone(),
                    &dir,
                    &mut maps,
                )?;
                drop(maps);
                record.path = Some(rel.clone());
                record.valid = Some(true);
                self.note_write(rel.clone())?;
                if let Err(err) = self.upsert_lexical(&record) {
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
                    record.clone(),
                    self.encryption.as_ref(),
                )?;
                record.path = Some(rel.clone());
                record.valid = Some(true);
                self.note_write(rel.clone())?;
                if let Err(err) = self.upsert_lexical(&record) {
                    let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
                    let _ = self.unnote_write(&rel);
                    return Err(err);
                }
                self.finish_mutation(message)?;
                Ok(())
            }
        }
    }

    /// Read one Record by `id`. Missing → `not_found`.
    ///
    /// Encrypted collections decrypt the file. A missing or wrong secret,
    /// or bytes that will not decrypt, is `crypto`. After a successful
    /// decrypt, a file that does not parse is a Record with `valid: false`.
    pub fn get(&self, id: &str) -> Result<Record, Error> {
        crate::id::require_slug("id", id)?;
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
    /// Files the library names, in the byte order of the file name (`{id}.md`).
    /// A `singleton` yields its one Record, or nothing before the first write.
    /// Each item is what `get` returns for that `id`, `raw` included: a file
    /// that does not parse and a Record that fails current Schema are both
    /// yielded with `valid: false`, and the walk continues. Unlike `lines()`,
    /// a bad file does not hide the next — each Record is its own file.
    /// Names that are not Record files (dotfiles, not `*.md`, not a regular
    /// file) and subdirectories are skipped. `journal` → `unsupported`. A
    /// missing Index directory does not stop the walk. Encrypted collections
    /// decrypt each file; a file that decrypts but does not parse is
    /// `valid: false`. A file that will not decrypt is `crypto`.
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
    /// `message` is the git commit subject outside a transaction. Omitted →
    /// `scheda`. Present but empty → `invalid` before any file is written.
    /// Inside a transaction the argument is ignored; `commit` supplies the
    /// subject.
    pub fn replace(
        &self,
        id: &str,
        record: RecordReplace,
        message: Option<&str>,
    ) -> Result<(), Error> {
        crate::id::require_slug("id", id)?;
        record.require_valid()?;
        self.require_encryption_secret()?;
        let message = require_commit_message(message)?;
        if self.decl.kind == Kind::Journal {
            bail!(Error::unsupported("replace is not offered for journal"));
        }
        let mut record = record_from_replace(id, &self.decl.name, record);
        self.require_write()?;
        self.drain_abandoned()?;
        let known = self.record_exists(id)?;
        if !known {
            bail!(Error::not_found(format!(
                "record {id} is not in {}",
                self.decl.name
            )));
        }
        self.prepare_isolated_write()?;
        schema::check_strict_edges(&self.schema, &self.decl, &record, |collection, id| {
            self.lookup(collection, id)
        })?;
        let (_id, rel) = store::replace_document(
            &self.vault,
            &self.decl,
            id,
            record.clone(),
            self.encryption.as_ref(),
        )?;
        record.path = Some(rel.clone());
        record.valid = Some(true);
        self.note_write(rel.clone())?;
        if let Err(err) = self.upsert_lexical(&record) {
            let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
            let _ = self.unnote_write(&rel);
            return Err(err);
        }
        self.finish_mutation(message)?;
        Ok(())
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
    /// subject.
    pub fn remove(&self, id: &str, message: Option<&str>) -> Result<(), Error> {
        crate::id::require_slug("id", id)?;
        self.require_encryption_secret()?;
        let message = require_commit_message(message)?;
        if self.decl.kind == Kind::Journal {
            bail!(Error::unsupported("remove is not offered for journal"));
        }
        self.require_write()?;
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
        self.finish_mutation(message)?;
        Ok(())
    }

    /// Run a Tantivy Query on this collection's Index, then open matching files.
    ///
    /// Isolated collections are in the Vault Index. An encrypted collection is
    /// `unsupported`: no Index holds its Records.
    ///
    /// Page order is score descending. Equal scores break by the Index `ord`
    /// fast field, ascending (earlier indexed first). `ord` is unique within
    /// a collection; two Records in different collections that share both
    /// score and `ord` stay unordered relative to each other. One `journal`
    /// collection uses append order instead.
    #[cfg(feature = "find")]
    pub fn find(&self, query: &FindQuery, offset: u32, count: u32) -> Result<FindResult, Error> {
        if self.decl.encrypted {
            bail!(Error::unsupported(format!(
                "encrypted collection {} cannot be found",
                self.decl.name
            )));
        }
        let name = self.decl.name.as_str();
        QueryCtx {
            engine: &self.search,
            schema: &self.schema,
            vault: &self.vault,
            journal_ids: &self.journal_ids,
        }
        .find(&[name], query, offset, count)
    }

    /// Hybrid retrieval on this collection: BM25 and ANN when both are on,
    /// then RRF, then an optional reranker.
    ///
    /// Isolated collections are in the Vault Index. An encrypted collection is
    /// `unsupported`: no Index holds its Records.
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
        let name = self.decl.name.as_str();
        QueryCtx {
            engine: &self.search,
            schema: &self.schema,
            vault: &self.vault,
            journal_ids: &self.journal_ids,
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
        crate::id::require_slug("id", id)?;
        self.require_encryption_secret()?;
        if count == 0 {
            bail!(Error::invalid("history count must be > 0"));
        }
        let rel = match self.decl.kind {
            Kind::Journal => {
                let record = self.get(id)?;
                record.path.unwrap_or_default()
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

    fn lookup(&self, collection: &str, id: &str) -> Result<Record, Error> {
        let Some(decl) = self.schema.get(collection) else {
            bail!(Error::not_found(format!(
                "collection {collection} is not in Schema"
            )));
        };
        self.lookup_decl(decl, id)
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

fn record_from_add(id: &str, collection: &str, add: RecordAdd) -> Record {
    let mut record = Record::default().with_id(id).with_collection(collection);
    record.fields = add.fields;
    record.body = add.body;
    record
}

fn record_from_replace(id: &str, collection: &str, replace: RecordReplace) -> Record {
    let mut record = Record::default().with_id(id).with_collection(collection);
    record.fields = replace.fields;
    record.body = replace.body;
    record
}

#[cfg(feature = "search")]
fn collection_from_record_path(path: &str) -> &str {
    path.split('/').next().unwrap_or("")
}

#[cfg(feature = "find")]
fn resolve_collections(
    schema: &LoadedSchema,
    names: &[&str],
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if names.is_empty() {
        return Ok(schema
            .collections()
            .filter(|decl| !decl.isolated && !decl.encrypted)
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
        out.push(decl.clone());
    }
    Ok(out)
}

#[cfg(feature = "find")]
fn apply_filter_schema(
    decls: Vec<schema::CollectionDecl>,
    filter: &Filter,
    explicit: bool,
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if explicit {
        for decl in &decls {
            query::validate(decl, filter)?;
        }
        return Ok(decls);
    }
    Ok(decls
        .into_iter()
        .filter(|decl| query::validate(decl, filter).is_ok())
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
/// Missing `id` is `not_found`. An encrypted collection never reaches `find`
/// (`unsupported` at the collection set); if this path still sees one, it
/// returns `unsupported` rather than `crypto`.
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
    let (fts, _) = marked_text(decl, &record)?;
    Ok(fts)
}

#[cfg(feature = "find")]
struct QueryCtx<'a> {
    engine: &'a SearchEngine,
    schema: &'a LoadedSchema,
    vault: &'a Path,
    journal_ids: &'a Mutex<JournalIdMaps>,
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
        let decls =
            apply_filter_schema(resolve_collections(self.schema, names)?, filter, explicit)?;
        if decls.is_empty() {
            return Ok(FindResult {
                total: Some(0),
                ..FindResult::default()
            });
        }
        let journal_ord = decls.len() == 1 && decls[0].kind == Kind::Journal;
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
        let mut decls = resolve_collections(self.schema, names)?;
        if filter.kind.is_some() {
            decls = apply_filter_schema(decls, filter, explicit)?;
        }
        if decls.is_empty() {
            return Ok(SearchHits::default());
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
        let mut hits = self
            .engine
            .search(&collections, query, embedder, allowed.as_ref())?;
        let k = match query.k {
            None | Some(0) => 10,
            Some(n) => n as usize,
        };
        let include_raw = query.include_raw == Some(true);
        let Some(reranker) = reranker else {
            hits.hits.truncate(k);
            if include_raw {
                self.fill_hit_raw(&mut hits)?;
            }
            return Ok(hits);
        };
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
        if scores.len() != records.len() || scores.len() != hits.hits.len() {
            bail!(Error::invalid(
                "reranker score_many must return one score per record"
            ));
        }
        let mut scored: Vec<_> = scores
            .into_iter()
            .zip(hits.hits)
            .zip(records)
            .map(|((score, mut hit), record)| {
                if include_raw {
                    hit.raw.clone_from(&record.raw);
                }
                (score, hit)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.id.cmp(&b.1.id))
                .then_with(|| a.1.path.cmp(&b.1.path))
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
    hit.collection
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| collection_from_record_path(hit.path.as_deref().unwrap_or("")))
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
    *crate::error::mutex(journal_ids, "[Vault] lock journal ids")? =
        index::load_journal_ids(index_dir, &loaded)?;
    #[cfg(feature = "find")]
    {
        search.close_for_rebuild();
        search.rebuild(vault, &loaded, embedder)?;
    }
    #[cfg(not(feature = "find"))]
    let _ = embedder;
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

/// Prune orphan generations when no writer holds the lock. Skip if a writer
/// is in the middle of `reindex` (its new generation is not yet `current`).
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
            let result = index::prune_generations(root);
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

fn acquire_write_lock(vault: &Path) -> Result<File, Error> {
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
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            Err(Error::locked("vault write lock is held").context("[Vault] acquire write lock"))
        }
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
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Collection, OpenOptions, Vault};
    use crate::git;
    use crate::{Code, Encryption, Error, RecordAdd, RecordReplace, Schema, VaultInfo};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("scheda-c1-{}-{n}", process::id()));
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
        assert!(vault.index_dir().starts_with(vault.path()));
        assert!(vault.index_dir().ends_with("index"));
        fs::remove_dir_all(vault.index_dir()).unwrap();
        assert!(scratch.vault().join(".git").exists());
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
        use crate::Body;
        let mut schema = Schema::default().with_version(1).with_encrypted(true);
        schema.kind = Body::default().into();
        schema
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
        let add: RecordAdd = serde_json::from_value(serde_json::json!({"fields": {}})).unwrap();
        vault
            .collection("c")
            .unwrap()
            .add("alpha", add, None)
            .unwrap();
        let got = vault.collection("c").unwrap().get("alpha").unwrap();
        assert_eq!(got.id.as_deref(), Some("alpha"));
        assert_eq!(got.valid, Some(true));
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
        assert_eq!(records[0].valid, Some(false));
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
            .add("", RecordAdd::default(), None)
            .expect_err("empty id must fail slug rules");
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn replace_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .replace("", RecordReplace::default(), None)
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
    fn open_on_existing_vault_ignores_remote_repo() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), scratch.options(), None).unwrap();
        git::add_remote(vault.path(), "origin", "https://example.invalid/scheda.git").unwrap();
        drop(vault);
        let opened = Vault::open(
            scratch.vault(),
            OpenOptions::new().remote_repo("https://example.invalid/other.git"),
            None,
        )
        .unwrap();
        assert_eq!(opened.remote_names(), &["origin".to_owned()]);
        assert_eq!(
            git::remotes(opened.path()).unwrap(),
            vec!["origin".to_owned()]
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
    fn git2_works_without_an_async_runtime() {
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
        use crate::{FindQuery, RecordAdd};

        let scratch = Scratch::new();
        let writer = Vault::open(scratch.vault(), OpenOptions::new(), None).unwrap();
        writer.begin().unwrap();
        let mut schema = Schema::default().with_version(1);
        schema.kind = crate::Body::default().into();
        writer.create_collection("c", schema, None).unwrap();
        writer.commit("schema").unwrap();
        let add: RecordAdd = serde_json::from_value(serde_json::json!({"fields": {}})).unwrap();
        writer
            .collection("c")
            .unwrap()
            .add("alpha", add, None)
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
}
