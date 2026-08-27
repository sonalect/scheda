use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{Code, ContextAs, ContextIo, bail, io_code};
use crate::git;
use crate::index::{self, JournalIdMaps};
use crate::isolated;
use crate::journal;
use crate::query;
use crate::schema::{self, CollectionDecl, Kind, Schema as LoadedSchema};
use crate::search::{self, SearchEngine};
use crate::store;
use crate::{
    Embedder, Error, Filter, FindQuery, FindResult, History, OpenOptions, Record, RecordAdd,
    RecordReplace, ReindexResult, Reranker, Schema, SearchHits, SearchQuery, VaultInfo,
    search_hits,
};

const AUTO_COMMIT_MESSAGE: &str = "scheda";

/// A git working tree that holds collections of records.
pub struct Vault {
    path: PathBuf,
    index_dir: PathBuf,
    remotes: Vec<String>,
    auto_sync: bool,
    write_lock: Arc<Mutex<Option<File>>>,
    schema: Mutex<Arc<LoadedSchema>>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
}

/// A named set of records with one write mode (`document`, `journal`, or `singleton`).
pub struct Collection {
    vault: PathBuf,
    index_dir: PathBuf,
    write_lock: Arc<Mutex<Option<File>>>,
    auto_sync: bool,
    remotes: Vec<String>,
    decl: CollectionDecl,
    schema: Arc<LoadedSchema>,
    written: Arc<Mutex<BTreeSet<String>>>,
    in_txn: Arc<Mutex<bool>>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
}

impl Vault {
    /// Open, create, or clone a Vault.
    ///
    /// Path exists → open (`remote_repo` ignored). Path missing + `remote_repo`
    /// → clone. Path missing → `git init`. Path exists but is not a Vault →
    /// `invalid`.
    pub fn open(
        path: impl AsRef<Path>,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, Error> {
        options.require_valid()?;
        let path = path.as_ref();
        if path.exists() {
            if !git::is_work_tree(path) {
                bail!(Error::invalid(format!(
                    "path exists but is not a Vault: {}",
                    path.display()
                )));
            }
            return Self::open_existing(path, options, embedder);
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
        commit_gitignore_if_needed(path)?;
        Self::open_existing(path, options, embedder)
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
        commit_gitignore_if_needed(path)?;
        Self::open_existing(path, without_remote_repo(options), embedder)
    }

    fn open_existing(
        path: &Path,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, Error> {
        let auto_sync = options.auto_sync.unwrap_or(false);
        let path = fs::canonicalize(path).context("[Vault::open] canonicalize vault")?;
        let gitignore_pending = gitignore_needs_write(&path)?;
        let index_dir = path.join(".scheda").join("index");
        fs::create_dir_all(&index_dir).context("[Vault::open] create index dir")?;
        let index_dir = fs::canonicalize(&index_dir).context("[Vault::open] canonicalize index")?;
        let remotes = git::remotes(&path)?;
        if auto_sync && !remotes.is_empty() {
            git::pull(&path)?;
        }
        let schema = Arc::new(schema::load(&path)?);
        let journal_ids = Arc::new(Mutex::new(index::load_all_journal_ids(
            &path, &index_dir, &schema,
        )?));
        let written = Arc::new(Mutex::new(load_written(&index_dir)?));
        let in_txn = Arc::new(Mutex::new(load_txn(&index_dir)?));
        let search = Arc::new(SearchEngine::open(&index_dir, false)?);
        let vault = Self {
            path,
            index_dir,
            remotes,
            auto_sync,
            write_lock: Arc::new(Mutex::new(None)),
            schema: Mutex::new(schema),
            written,
            in_txn,
            journal_ids,
            search,
            embedder,
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

    /// Index directory: `{vault}/.scheda/index`.
    #[must_use]
    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    /// Whether this handle currently holds `{vault}/.scheda/write.lock`.
    #[must_use]
    pub fn holds_write_lock(&self) -> bool {
        self.write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
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
        self.schema_arc().versions()
    }

    fn schema_arc(&self) -> Arc<LoadedSchema> {
        Arc::clone(&self.schema.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Handle for one collection.
    ///
    /// Unknown name → `not_found`. Isolated collections live under
    /// `{vault}/.isolated/{name}/` on this same handle.
    pub fn collection(&self, name: &str) -> Result<Collection, Error> {
        let schema = self.schema_arc();
        let Some(decl) = schema.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in Schema"
            )));
        };
        Ok(Collection {
            vault: self.path.clone(),
            index_dir: self.index_dir.clone(),
            write_lock: Arc::clone(&self.write_lock),
            auto_sync: self.auto_sync,
            remotes: self.remotes.clone(),
            decl: decl.clone(),
            schema,
            written: Arc::clone(&self.written),
            in_txn: Arc::clone(&self.in_txn),
            journal_ids: Arc::clone(&self.journal_ids),
            search: Arc::clone(&self.search),
            embedder: self.embedder.clone(),
        })
    }

    /// Run a Tantivy Query on the Index, then open matching Record files.
    /// Empty `collections` is every non-isolated collection on this handle.
    pub fn find(
        &self,
        collections: &[&str],
        query: &FindQuery,
        offset: u32,
        count: u32,
    ) -> Result<FindResult, Error> {
        let schema = self.schema_arc();
        QueryCtx {
            engine: &self.search,
            schema: &schema,
            vault: &self.path,
            journal_ids: &self.journal_ids,
        }
        .find(collections, query, offset, count)
    }

    /// Hybrid retrieval: BM25 and ANN when both are on, then RRF, then an
    /// optional reranker. Empty `collections` is every non-isolated collection
    /// on this handle.
    pub fn search(
        &self,
        collections: &[&str],
        query: &SearchQuery,
        reranker: Option<&dyn Reranker>,
    ) -> Result<SearchHits, Error> {
        let schema = self.schema_arc();
        QueryCtx {
            engine: &self.search,
            schema: &schema,
            vault: &self.path,
            journal_ids: &self.journal_ids,
        }
        .search(self.embedder.as_deref(), collections, query, reranker)
    }

    /// End the transaction and create one git commit per repository that has
    /// recorded paths (main tree and/or `{vault}/.isolated/`). `invalid` if
    /// none is open, the message is empty, or there is nothing to commit.
    pub fn commit(&self, message: &str) -> Result<(), Error> {
        self.require_write_lock("commit")?;
        if !flag(&self.in_txn) {
            bail!(Error::invalid("commit requires an open transaction"));
        }
        self.commit_recorded(message)?;
        set_flag(&self.index_dir, &self.in_txn, false)?;
        self.push_if_auto_sync()
    }

    /// Start a transaction. Discards leftover Scheda writes first.
    /// `invalid` if a transaction is already open.
    pub fn begin(&self) -> Result<(), Error> {
        self.require_write_lock("begin")?;
        if flag(&self.in_txn) {
            bail!(Error::invalid("already in a transaction"));
        }
        self.restore_recorded()?;
        set_flag(&self.index_dir, &self.in_txn, true)
    }

    /// Restore Scheda-recorded paths to HEAD in the repository that owns each
    /// path, end the transaction if any, reload Schema, and rebuild the Index
    /// (including `{vault}/.isolated/.scheda/` when that tree exists).
    pub fn discard(&self) -> Result<(), Error> {
        self.require_write_lock("discard")?;
        self.restore_recorded()?;
        set_flag(&self.index_dir, &self.in_txn, false)
    }

    fn commit_recorded(&self, message: &str) -> Result<(), Error> {
        let paths: Vec<String> = {
            let set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
            set.iter().cloned().collect()
        };
        isolated::commit_recorded(&self.path, message, &paths)?;
        let mut set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        set.clear();
        persist_written(&self.index_dir, &set)
    }

    fn restore_recorded(&self) -> Result<(), Error> {
        let paths: Vec<String> = {
            let mut set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
            let paths: Vec<String> = set.iter().cloned().collect();
            set.clear();
            persist_written(&self.index_dir, &set)?;
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
        *self
            .journal_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner) =
            index::load_all_journal_ids(&self.path, &self.index_dir, &loaded)?;
        self.search.close_for_rebuild();
        self.search
            .rebuild(&self.path, &loaded, self.embedder.as_deref())?;
        *self.schema.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(loaded);
        Ok(())
    }

    fn finish_mutation(&self) -> Result<(), Error> {
        if flag(&self.in_txn) {
            return Ok(());
        }
        match self.commit_recorded(AUTO_COMMIT_MESSAGE) {
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
        let mut set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        set.insert(rel);
        persist_written(&self.index_dir, &set)
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
                self.finish_mutation()?;
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
    /// Remote. Does not write Records.
    pub fn check(&self) -> Result<(), Error> {
        isolated::check_perimeter(&self.path)?;
        let schema = schema::load(&self.path)?;
        store::check_documents(&self.path, &schema)?;
        journal::check_journals(&self.path, &schema)
    }

    /// Proto snapshot (`api.v1.VaultInfo`): vault attrs plus the collection list.
    pub fn info(&self) -> Result<VaultInfo, Error> {
        let remote = self.remotes.first().cloned().unwrap_or_default();
        let info = schema::to_vault_info(
            &self.path,
            &self.index_dir,
            &remote,
            self.auto_sync,
            &self.schema_arc(),
        )?;
        Ok(info)
    }

    /// Write one collection declaration as `{name}/.schema.yaml`. Reloads Schema
    /// on this handle. `isolated: true` stores Records under `{vault}/.isolated/`
    /// and creates the nested git if needed.
    pub fn create_collection(&self, name: &str, schema: Schema) -> Result<Collection, Error> {
        schema.require_valid()?;
        self.require_write_lock("create_collection")?;
        if schema.isolated == Some(true)
            && let Some(rel) = isolated::ensure(&self.path)?
        {
            self.note_write(rel)?;
        }
        let rel = schema::put_collection(&self.path, name, &schema)?;
        self.note_write(rel)?;
        let loaded = schema::load(&self.path)?;
        *self
            .journal_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner) =
            index::load_all_journal_ids(&self.path, &self.index_dir, &loaded)?;
        *self.schema.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(loaded);
        self.finish_mutation()?;
        self.collection(name)
    }

    /// Rebuild the Index from Vault files. Writes format version `3` and the
    /// journal `id` map. Isolated collections rebuild `{vault}/.isolated/.scheda/`
    /// when that tree exists. Needs the write lock.
    pub fn reindex(&self) -> Result<ReindexResult, Error> {
        self.require_write_lock("reindex")?;
        self.search.close_for_rebuild();
        let schema = schema::load(&self.path)?;
        let (counts, maps) = index::rebuild(&self.path, &schema, &self.index_dir)?;
        *self
            .journal_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = maps;
        self.search
            .rebuild(&self.path, &schema, self.embedder.as_deref())?;
        Ok(counts)
    }
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("index_dir", &self.index_dir)
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
            index_dir: PathBuf::new(),
            write_lock: Arc::new(Mutex::new(None)),
            auto_sync: false,
            remotes: Vec::new(),
            decl: CollectionDecl {
                name: String::from("c"),
                kind: Kind::Document,
                version: 1,
                id_field: String::from("id"),
                id_pattern: None,
                filter_keys: std::collections::BTreeSet::new(),
                segment_max_bytes: None,
                segment_max_lines: None,
                segment_rotate: None,
                singleton_id: None,
                body: None,
                fields: Vec::new(),
                isolated: false,
            },
            schema: Arc::new(LoadedSchema::default()),
            written: Arc::new(Mutex::new(BTreeSet::new())),
            in_txn: Arc::new(Mutex::new(false)),
            journal_ids: Arc::new(Mutex::new(JournalIdMaps::default())),
            search: Arc::new(SearchEngine::empty()),
            embedder: None,
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

    /// Record identity field name. Default `id`.
    #[must_use]
    pub fn id_field(&self) -> &str {
        &self.decl.id_field
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

    fn require_write(&self) -> Result<(), Error> {
        let mut slot = self
            .write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(acquire_write_lock(&self.vault)?);
            self.search.enable_write()?;
        }
        Ok(())
    }

    fn note_write(&self, rel: String) -> Result<(), Error> {
        let mut set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        set.insert(rel);
        persist_written(&self.index_dir, &set)
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

    fn ids_index_dir(&self) -> PathBuf {
        isolated::ids_index_dir(&self.vault, &self.index_dir, self.decl.isolated)
    }

    fn finish_mutation(&self) -> Result<(), Error> {
        if flag(&self.in_txn) {
            return Ok(());
        }
        let paths: Vec<String> = {
            let set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
            set.iter().cloned().collect()
        };
        match isolated::commit_recorded(&self.vault, AUTO_COMMIT_MESSAGE, &paths) {
            Ok(()) => {
                let mut set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
                set.clear();
                persist_written(&self.index_dir, &set)?;
                if self.auto_sync && !self.remotes.is_empty() {
                    git::push(&self.vault)?;
                }
                Ok(())
            }
            Err(err) => {
                let paths: Vec<String> = {
                    let mut set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
                    let paths: Vec<String> = set.iter().cloned().collect();
                    set.clear();
                    persist_written(&self.index_dir, &set)?;
                    paths
                };
                let _ = isolated::discard_recorded(&self.vault, &paths);
                if !paths.is_empty() {
                    let _ = repair_index_after_discard(
                        &self.vault,
                        &self.index_dir,
                        &self.journal_ids,
                        &self.search,
                        self.embedder.as_deref(),
                    );
                }
                Err(err)
            }
        }
    }

    /// Create a Record. Same `id` already present → `duplicate_id`.
    pub fn add(&self, id: &str, record: RecordAdd) -> Result<(), Error> {
        crate::id::require_slug("id", id)?;
        record.require_valid()?;
        let mut record = record_from_add(id, &self.decl.name, record);
        self.require_write()?;
        self.prepare_isolated_write()?;
        search::check_strict_edges(&self.schema, &self.decl, &record, |collection, id| {
            self.lookup(collection, id)
        })?;
        match self.decl.kind {
            Kind::Journal => {
                let mut maps =
                    crate::error::mutex(&self.journal_ids, "[Collection::add] lock journal ids")?;
                let index_dir = self.ids_index_dir();
                let (id, rel, rollback) = journal::insert_journal(
                    &self.vault,
                    &self.decl,
                    record.clone(),
                    &index_dir,
                    &mut maps,
                )?;
                drop(maps);
                record.path = Some(rel.clone());
                record.valid = Some(true);
                self.note_write(rel.clone())?;
                if !self.decl.isolated
                    && let Err(err) =
                        self.search
                            .upsert(&self.decl, &record, self.embedder.as_deref())
                {
                    self.rollback_journal_insert(&id, &rel, &rollback);
                    return Err(err);
                }
                self.finish_mutation()?;
                Ok(())
            }
            Kind::Document | Kind::Singleton => {
                let (_id, rel) = store::insert_document(&self.vault, &self.decl, record.clone())?;
                record.path = Some(rel.clone());
                record.valid = Some(true);
                self.note_write(rel.clone())?;
                if !self.decl.isolated
                    && let Err(err) =
                        self.search
                            .upsert(&self.decl, &record, self.embedder.as_deref())
                {
                    let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
                    let _ = self.unnote_write(&rel);
                    return Err(err);
                }
                self.finish_mutation()?;
                Ok(())
            }
        }
    }

    /// Read one Record by `id`. Missing → `not_found`.
    pub fn get(&self, id: &str) -> Result<Record, Error> {
        crate::id::require_slug("id", id)?;
        match self.decl.kind {
            Kind::Journal => {
                let maps = self
                    .journal_ids
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                journal::find_journal(&self.vault, &self.decl, id, &maps)
            }
            Kind::Document | Kind::Singleton => store::find_document(&self.vault, &self.decl, id),
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

    /// Replace a whole `document` or `singleton`. `journal` → `unsupported`.
    pub fn replace(&self, id: &str, record: RecordReplace) -> Result<(), Error> {
        crate::id::require_slug("id", id)?;
        record.require_valid()?;
        if self.decl.kind == Kind::Journal {
            bail!(Error::unsupported("replace is not offered for journal"));
        }
        let known = if self.decl.isolated {
            store::locate_file(&self.vault, &self.decl, id)?.is_some()
        } else {
            self.search.has_id(&self.decl.name, id)?
        };
        if !known {
            bail!(Error::not_found(format!(
                "record {id} is not in the Index of {}",
                self.decl.name
            )));
        }
        let mut record = record_from_replace(id, &self.decl.name, record);
        self.require_write()?;
        self.prepare_isolated_write()?;
        search::check_strict_edges(&self.schema, &self.decl, &record, |collection, id| {
            self.lookup(collection, id)
        })?;
        let (_id, rel) = store::replace_document(&self.vault, &self.decl, id, record.clone())?;
        record.path = Some(rel.clone());
        record.valid = Some(true);
        self.note_write(rel.clone())?;
        if !self.decl.isolated
            && let Err(err) = self
                .search
                .upsert(&self.decl, &record, self.embedder.as_deref())
        {
            let _ = isolated::discard_recorded(&self.vault, std::slice::from_ref(&rel));
            let _ = self.unnote_write(&rel);
            return Err(err);
        }
        self.finish_mutation()?;
        Ok(())
    }

    /// Run a Tantivy Query on this collection's Index, then open matching files.
    pub fn find(&self, query: &FindQuery, offset: u32, count: u32) -> Result<FindResult, Error> {
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
    pub fn search(
        &self,
        query: &SearchQuery,
        reranker: Option<&dyn Reranker>,
    ) -> Result<SearchHits, Error> {
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
    /// line-level blame. `count` `0` → `invalid`. Missing file → `not_found`.
    /// A missing Index directory does not stop this call.
    pub fn history(&self, id: &str, offset: u32, count: u32) -> Result<History, Error> {
        crate::id::require_slug("id", id)?;
        if count == 0 {
            bail!(Error::invalid("history count must be > 0"));
        }
        let record = self.get(id)?;
        let path = record.path.as_deref().unwrap_or("");
        let (repo, git_path) = isolated::git_addr(&self.vault, path);
        if self.decl.isolated && !git::is_work_tree(&repo) {
            return Ok(History {
                entries: Vec::new(),
                ..History::default()
            });
        }
        let entries = git::history(&repo, &git_path, offset, count)?;
        Ok(History {
            entries,
            ..History::default()
        })
    }

    fn unnote_write(&self, rel: &str) -> Result<(), Error> {
        let mut set = crate::error::mutex(&self.written, "[Collection::unnote_write] lock")?;
        set.remove(rel);
        persist_written(&self.index_dir, &set)
    }

    fn rollback_journal_insert(&self, id: &str, rel: &str, rollback: &journal::JournalRollback) {
        let _ = journal::rollback_insert(rollback);
        if let Ok(mut maps) =
            crate::error::mutex(&self.journal_ids, "[Collection::add] rollback maps")
        {
            let _ = index::forget_remembered(&self.ids_index_dir(), &mut maps, &self.decl.name, id);
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
                let maps = self
                    .journal_ids
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                journal::find_journal(&self.vault, decl, id, &maps)
            }
            Kind::Document | Kind::Singleton => store::find_document(&self.vault, decl, id),
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

fn collection_from_record_path(path: &str) -> &str {
    path.split('/').next().unwrap_or("")
}

fn resolve_collections(
    schema: &LoadedSchema,
    names: &[&str],
) -> Result<Vec<schema::CollectionDecl>, Error> {
    if names.is_empty() {
        return Ok(schema
            .collections()
            .filter(|decl| !decl.isolated)
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
        if decl.isolated {
            bail!(Error::unsupported(format!(
                "isolated collection {name} is not on this Vault handle"
            )));
        }
        out.push(decl.clone());
    }
    Ok(out)
}

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
            let maps = journal_ids.lock().unwrap_or_else(PoisonError::into_inner);
            journal::find_journal(vault, decl, id, &maps)
        }
        Kind::Document | Kind::Singleton => store::find_document(vault, decl, id),
    }
}

struct QueryCtx<'a> {
    engine: &'a SearchEngine,
    schema: &'a LoadedSchema,
    vault: &'a Path,
    journal_ids: &'a Mutex<JournalIdMaps>,
}

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
        let page = self
            .engine
            .find(&collections, journal_ord, &decls, filter, offset, count)?;
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
            Some(self.engine.find_ids(&collections, &decls, filter)?)
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
        let scores = reranker.score_many(text, &records)?;
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

fn without_raw(mut record: Record) -> Record {
    record.raw = None;
    record
}

fn hit_collection(hit: &search_hits::Hit) -> &str {
    hit.collection
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| collection_from_record_path(hit.path.as_deref().unwrap_or("")))
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
/// `write.lock`. A later `begin` is then the first `try_lock`.
fn commit_gitignore_if_needed(vault: &Path) -> Result<(), Error> {
    if ensure_gitignore(vault)? {
        git::commit(vault, AUTO_COMMIT_MESSAGE, &[String::from(".gitignore")])?;
    }
    Ok(())
}

/// Uncommitted write paths for the next `commit`. Lives under `.scheda/index`
/// (gitignored) so a process-per-command CLI can write then `commit`.
fn written_log(index_dir: &Path) -> PathBuf {
    index_dir.join("written")
}

fn txn_log(index_dir: &Path) -> PathBuf {
    index_dir.join("txn")
}

fn flag(slot: &Mutex<bool>) -> bool {
    *slot.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set_flag(index_dir: &Path, slot: &Mutex<bool>, on: bool) -> Result<(), Error> {
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = on;
    persist_txn(index_dir, on)
}

fn load_txn(index_dir: &Path) -> Result<bool, Error> {
    match fs::read(txn_log(index_dir)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).context("[Vault::open] read txn flag"),
        Ok(_) => Ok(true),
    }
}

fn persist_txn(index_dir: &Path, on: bool) -> Result<(), Error> {
    let path = txn_log(index_dir);
    if on {
        fs::write(&path, b"1\n").context("[Vault] write txn flag")
    } else {
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context("[Vault] remove txn flag"),
        }
    }
}

fn repair_index_after_discard(
    vault: &Path,
    index_dir: &Path,
    journal_ids: &Mutex<JournalIdMaps>,
    search: &SearchEngine,
    embedder: Option<&dyn Embedder>,
) -> Result<(), Error> {
    let loaded = schema::load(vault)?;
    *journal_ids.lock().unwrap_or_else(PoisonError::into_inner) =
        index::load_all_journal_ids(vault, index_dir, &loaded)?;
    search.close_for_rebuild();
    search.rebuild(vault, &loaded, embedder)
}

fn load_written(index_dir: &Path) -> Result<BTreeSet<String>, Error> {
    match fs::read(written_log(index_dir)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(err) => Err(err).context("[Vault::open] read written paths"),
        Ok(bytes) if bytes.is_empty() => Ok(BTreeSet::new()),
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)
                .context(Code::NotUtf8, "[Vault::open] decode written paths")?;
            Ok(text
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect())
        }
    }
}

fn persist_written(index_dir: &Path, set: &BTreeSet<String>) -> Result<(), Error> {
    let path = written_log(index_dir);
    if set.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context("[Vault] remove written paths"),
        }
    } else {
        let mut bytes = Vec::new();
        for rel in set {
            bytes.extend_from_slice(rel.as_bytes());
            bytes.push(b'\n');
        }
        store::atomic_write(&path, &bytes)
    }
}

fn acquire_write_lock(vault: &Path) -> Result<File, Error> {
    let scheda = vault.join(".scheda");
    fs::create_dir_all(&scheda).context("[Vault] create .scheda for write lock")?;
    drain_legacy_index_lock(&scheda)?;
    let lock_path = scheda.join("write.lock");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .context("[Vault] open write lock")?;
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
    use crate::{Code, Error, RecordAdd, RecordReplace, Schema, VaultInfo};

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
    fn empty_remote_repo_option_is_local_init() {
        let scratch = Scratch::new();
        let vault = Vault::open(scratch.vault(), OpenOptions::new().remote_repo(""), None).unwrap();
        assert!(!vault.has_remote());
    }

    #[test]
    fn insert_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .add("", RecordAdd::default())
            .expect_err("empty id must fail slug rules");
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn replace_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .replace("", RecordReplace::default())
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
        let _: fn(&Vault, &str, Schema) -> Result<Collection, Error> = Vault::create_collection;
        let _: fn(&Vault) -> Result<(), Error> = Vault::begin;
        let _: fn(&Vault) -> Result<(), Error> = Vault::discard;
    }
}
