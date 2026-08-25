use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{ContextIo, bail, io_code};
use crate::git;
use crate::index::{self, JournalIdMaps};
use crate::journal;
use crate::query;
use crate::schema::{self, CollectionDecl, Kind, Multiplicity, Schema};
use crate::search::{self, Embedder, SearchEngine};
use crate::store;
use crate::{
    Catalog, Error, Filter, HistoryEntry, OpenOptions, Record, ReindexCounts, SearchHits,
    SearchQuery,
};

/// A git working tree that holds collections of records.
pub struct Vault {
    path: PathBuf,
    index_dir: PathBuf,
    remotes: Vec<String>,
    auto_sync: bool,
    write_lock: Option<File>,
    schema: Arc<Schema>,
    schema_versions: Vec<u32>,
    written: Arc<Mutex<BTreeSet<String>>>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
}

/// A named set of records with one write mode (`document`, `journal`, or `singleton`).
pub struct Collection {
    vault: PathBuf,
    index_dir: PathBuf,
    can_write: bool,
    decl: CollectionDecl,
    schema: Arc<Schema>,
    written: Arc<Mutex<BTreeSet<String>>>,
    journal_ids: Arc<Mutex<JournalIdMaps>>,
    search: Arc<SearchEngine>,
    embedder: Option<Arc<dyn Embedder>>,
}

/// Result of inserting a record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InsertOne {
    pub id: String,
}

/// Result of replacing a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplaceOne {
    pub id: String,
}

impl Vault {
    /// Create a Vault at `path` (`git init`, no Remote). LIB-2 / GIT-9.
    /// `remote_repo` set → [`clone`](Self::clone) (LIB-13).
    pub fn init(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        options.require_valid()?;
        if let Some(url) = remote_repo_url(&options) {
            let url = url.to_owned();
            return Self::clone(&url, path, without_remote_repo(options));
        }
        let path = path.as_ref();
        prepare_new_work_tree(path)?;
        git::init(path)?;
        Self::open(path, options)
    }

    /// Clone `url` into `path`. LIB-2 name `clone` (GIT-9).
    pub fn clone(url: &str, path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        options.require_valid()?;
        let url = url.trim();
        if url.is_empty() {
            bail!(Error::invalid("clone URL is empty"));
        }
        let path = path.as_ref();
        prepare_new_work_tree(path)?;
        git::clone(url, path)?;
        Self::open(path, without_remote_repo(options))
    }

    /// Open an existing Vault at `path`.
    pub fn open(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        options.require_valid()?;
        let path = path.as_ref();
        if let Some(url) = remote_repo_url(&options) {
            if git::is_work_tree(path) {
                bail!(Error::invalid(format!(
                    "remote_repo cannot be set on an existing repository: {}",
                    path.display()
                )));
            }
            let url = url.to_owned();
            return Self::clone(&url, path, without_remote_repo(options));
        }
        if !git::is_work_tree(path) {
            bail!(Error::invalid(format!(
                "path is not a git working tree: {}",
                path.display()
            )));
        }
        let write = options.write.unwrap_or(true);
        let auto_sync = options.auto_sync.unwrap_or(false);
        let path = fs::canonicalize(path).context("[Vault::open] canonicalize vault")?;
        let mut wrote_gitignore = false;
        if write {
            wrote_gitignore = ensure_gitignore(&path)?;
        }
        let index_dir = path.join(".scheda").join("index");
        fs::create_dir_all(&index_dir).context("[Vault::open] create index dir")?;
        let index_dir = fs::canonicalize(&index_dir).context("[Vault::open] canonicalize index")?;
        let remotes = git::remotes(&path)?;
        if auto_sync && !remotes.is_empty() && !write {
            bail!(Error::locked("auto_sync pull needs a write lock"));
        }
        let write_lock = if write {
            Some(acquire_write_lock(&index_dir)?)
        } else {
            None
        };
        if auto_sync && !remotes.is_empty() {
            git::pull(&path)?;
        }
        let schema = Arc::new(schema::load(&path)?);
        let schema_versions = schema.versions();
        let journal_ids = Arc::new(Mutex::new(index::load_journal_ids(&index_dir, &schema)?));
        let written = Arc::new(Mutex::new(BTreeSet::new()));
        let search = Arc::new(SearchEngine::open(&index_dir, write)?);
        let vault = Self {
            path,
            index_dir,
            remotes,
            auto_sync,
            write_lock,
            schema,
            schema_versions,
            written,
            journal_ids,
            search,
            embedder: None,
        };
        if wrote_gitignore {
            vault.note_write(String::from(".gitignore"));
        }
        Ok(vault)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    #[must_use]
    pub fn holds_write_lock(&self) -> bool {
        self.write_lock.is_some()
    }

    #[must_use]
    pub fn has_remote(&self) -> bool {
        !self.remotes.is_empty()
    }

    #[must_use]
    pub fn remote_names(&self) -> &[String] {
        &self.remotes
    }

    #[must_use]
    pub fn auto_sync(&self) -> bool {
        self.auto_sync
    }

    /// Schema `schema_version` values present in this Vault.
    #[must_use]
    pub fn schema_versions(&self) -> &[u32] {
        &self.schema_versions
    }

    /// Inject an embedder for the vector leg (IDX-8). No embedder → vector
    /// search is off; lexical search and writes still work.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    pub fn collection(&self, name: &str) -> Result<Collection, Error> {
        let Some(decl) = self.schema.get(name) else {
            bail!(Error::not_found(format!(
                "collection {name} is not in Schema"
            )));
        };
        if decl.isolated {
            bail!(Error::unsupported(format!(
                "isolated collection {name} is not on this Vault handle"
            )));
        }
        Ok(Collection {
            vault: self.path.clone(),
            index_dir: self.index_dir.clone(),
            can_write: self.write_lock.is_some(),
            decl: decl.clone(),
            schema: Arc::clone(&self.schema),
            written: Arc::clone(&self.written),
            journal_ids: Arc::clone(&self.journal_ids),
            search: Arc::clone(&self.search),
            embedder: self.embedder.clone(),
        })
    }

    /// Create one git commit for paths this handle wrote (GIT-2, LIB-12).
    pub fn commit(&self, message: &str) -> Result<(), Error> {
        if self.write_lock.is_none() {
            bail!(Error::locked("commit needs a write lock"));
        }
        let paths: Vec<String> = {
            let set = self.written.lock().unwrap_or_else(PoisonError::into_inner);
            set.iter().cloned().collect()
        };
        git::commit(&self.path, message, &paths)?;
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        if self.auto_sync && !self.remotes.is_empty() {
            self.push_remote()?;
        }
        Ok(())
    }

    /// Git commits that changed `path` (GIT-3, LIB-17). Newest first.
    pub fn history(&self, path: &str) -> Result<Vec<HistoryEntry>, Error> {
        git::history(&self.path, path)
    }

    fn note_write(&self, rel: String) {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(rel);
    }

    pub fn push_remote(&self) -> Result<(), Error> {
        self.require_write_lock("push_remote")?;
        git::push(&self.path)
    }

    pub fn pull_remote(&self) -> Result<(), Error> {
        self.require_write_lock("pull_remote")?;
        git::pull(&self.path)
    }

    pub fn sync_remote(&self) -> Result<(), Error> {
        self.require_write_lock("sync_remote")?;
        git::pull(&self.path)?;
        git::push(&self.path)
    }

    fn require_write_lock(&self, op: &str) -> Result<(), Error> {
        if self.write_lock.is_some() {
            Ok(())
        } else {
            Err(Error::locked(format!("{op} needs a write lock")))
        }
    }

    pub fn check(&self) -> Result<(), Error> {
        let schema = schema::load(&self.path)?;
        store::check_documents(&self.path, &schema)?;
        journal::check_journals(&self.path, &schema)
    }

    /// Schema as `api/v1` `Catalog` (LIB-20, AGT-1). Isolated collections are
    /// listed without `fields` (ISO-3).
    pub fn catalog(&self) -> Result<Catalog, Error> {
        let catalog = schema::to_catalog(&self.schema)?;
        catalog.require_valid()?;
        Ok(catalog)
    }

    pub fn reindex(&self) -> Result<ReindexCounts, Error> {
        if self.write_lock.is_none() {
            bail!(Error::locked("reindex needs a write lock"));
        }
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
            can_write: true,
            decl: CollectionDecl {
                name: String::from("c"),
                kind: Kind::Document,
                schema_version: 1,
                layout: vec![String::from("c/{id}.md")],
                id_field: String::from("id"),
                id_pattern: None,
                id_pattern_src: None,
                filter_keys: std::collections::BTreeSet::new(),
                segment_max_bytes: None,
                segment_max_lines: None,
                segment_rotate: None,
                singleton_id: None,
                body: None,
                fields: Vec::new(),
                isolated: false,
            },
            schema: Arc::new(Schema::default()),
            written: Arc::new(Mutex::new(BTreeSet::new())),
            journal_ids: Arc::new(Mutex::new(JournalIdMaps::default())),
            search: Arc::new(SearchEngine::empty()),
            embedder: None,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.decl.name
    }

    #[must_use]
    pub fn kind(&self) -> Kind {
        self.decl.kind
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.decl.schema_version
    }

    #[must_use]
    pub fn id_field(&self) -> &str {
        &self.decl.id_field
    }

    #[must_use]
    pub fn is_filter_key(&self, field: &str) -> bool {
        self.decl.filter_keys.contains(field)
    }

    #[must_use]
    pub fn segment_max_bytes(&self) -> Option<u64> {
        self.decl.segment_max_bytes
    }

    #[must_use]
    pub fn layout(&self) -> &[String] {
        &self.decl.layout
    }

    #[must_use]
    pub fn singleton_id(&self) -> Option<&str> {
        self.decl.singleton_id.as_deref()
    }

    #[must_use]
    pub fn field_type(&self, name: &str) -> Option<&'static str> {
        self.field(name).map(|field| field.type_.as_str())
    }

    #[must_use]
    pub fn field_is_required(&self, name: &str) -> Option<bool> {
        self.field(name).map(|field| field.required)
    }

    #[must_use]
    pub fn field_is_nullable(&self, name: &str) -> Option<bool> {
        self.field(name).map(|field| field.nullable)
    }

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
        if self.can_write {
            Ok(())
        } else {
            Err(Error::invalid("vault is open read-only"))
        }
    }

    fn note_write(&self, rel: String) {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(rel);
    }

    /// Create a `document` file, append a `journal` line, or the first write of a `singleton`.
    pub fn insert_one(&self, record: Record) -> Result<InsertOne, Error> {
        record.require_valid()?;
        self.require_write()?;
        search::check_strict_edges(&self.schema, &self.decl, &record, |collection, id| {
            self.lookup(collection, id)
        })?;
        match self.decl.kind {
            Kind::Journal => {
                let mut maps = self
                    .journal_ids
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let (id, rel) = journal::insert_journal(
                    &self.vault,
                    &self.decl,
                    record,
                    &self.index_dir,
                    &mut maps,
                )?;
                drop(maps);
                self.note_write(rel);
                self.index_id(&id)?;
                Ok(InsertOne { id })
            }
            Kind::Document | Kind::Singleton => {
                let (id, rel) = store::insert_document(&self.vault, &self.decl, record)?;
                self.note_write(rel);
                self.index_id(&id)?;
                Ok(InsertOne { id })
            }
        }
    }

    /// Read one Record by `id`. Missing → `not_found`.
    pub fn find_one(&self, id: &str) -> Result<Record, Error> {
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

    /// Replace a whole `document` or `singleton`. Errors on `journal`.
    pub fn replace_one(&self, id: &str, record: Record) -> Result<ReplaceOne, Error> {
        record.require_valid()?;
        if self.decl.kind == Kind::Journal {
            bail!(Error::unsupported("replace_one is not offered for journal"));
        }
        self.require_write()?;
        search::check_strict_edges(&self.schema, &self.decl, &record, |collection, id| {
            self.lookup(collection, id)
        })?;
        let (id, rel) = store::replace_document(&self.vault, &self.decl, id, record)?;
        self.note_write(rel);
        self.index_id(&id)?;
        Ok(ReplaceOne { id })
    }

    /// SCH-8 filters. An empty filter (or [`scan`](Self::scan)) returns every Record.
    pub fn find(&self, filter: &Filter) -> Result<Vec<Record>, Error> {
        filter.require_valid()?;
        query::validate(&self.decl, filter)?;
        let records = match self.decl.kind {
            Kind::Journal => journal::scan_journal(&self.vault, &self.decl)?,
            Kind::Document | Kind::Singleton => store::scan_documents(&self.vault, &self.decl)?,
        };
        let mut out = Vec::new();
        for record in records {
            if query::matches(&self.decl, &record, filter)? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// `find` with an empty filter (LIB-10).
    pub fn scan(&self) -> Result<Vec<Record>, Error> {
        self.find(&Filter::default())
    }

    /// Hybrid retrieval (LIB-11, QRY-3–6).
    pub fn search(&self, query: &SearchQuery) -> Result<SearchHits, Error> {
        query.require_valid()?;
        let filter = search_filter(query);
        query::validate(&self.decl, &filter)?;
        let allowed = if query::is_unconstrained(&filter) {
            None
        } else {
            Some(
                self.find(&filter)?
                    .into_iter()
                    .filter_map(|record| record.id)
                    .collect(),
            )
        };
        self.search.search(
            &self.decl.name,
            query,
            self.embedder.as_deref(),
            allowed.as_ref(),
        )
    }

    /// One-step walk along an SCH-11 edge (REL-2).
    pub fn walk(&self, field: &str, filter: &Filter) -> Result<Vec<Record>, Error> {
        let edge = search::require_edge(&self.decl, field)?;
        let sources = self.find(filter)?;
        let Some(target) = self.schema.get(&edge.collection) else {
            bail!(Error::invalid(format!(
                "edge {field} targets unknown collection {}",
                edge.collection
            )));
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for source in sources {
            let fields = store::fields_map(&source)?;
            let mut ids = search::edge_target_ids(fields.get(field));
            if edge.multiplicity == Multiplicity::One {
                ids.truncate(1);
            }
            for id in ids {
                if !seen.insert(id.clone()) {
                    continue;
                }
                match self.lookup_decl(target, &id) {
                    Ok(record) => out.push(record),
                    Err(err) if err.known_code() == Some(crate::Code::NotFound) => {}
                    Err(err) => bail!(err),
                }
            }
        }
        Ok(out)
    }

    /// Inverse walk: source records that point at `id` (REL-3).
    pub fn walk_inverse(
        &self,
        source_collection: &str,
        field: &str,
        id: &str,
    ) -> Result<Vec<Record>, Error> {
        let Some(source) = self.schema.get(source_collection) else {
            bail!(Error::not_found(format!(
                "collection {source_collection} is not in Schema"
            )));
        };
        let edge = search::require_edge(source, field)?;
        if edge.collection != self.decl.name {
            bail!(Error::invalid(format!(
                "edge {source_collection}.{field} does not target {}",
                self.decl.name
            )));
        }
        let source_ids = self.search.inverse(source_collection, field, id)?;
        let mut out = Vec::new();
        for source_id in source_ids {
            match self.lookup_decl(source, &source_id) {
                Ok(record) => out.push(record),
                Err(err) if err.known_code() == Some(crate::Code::NotFound) => {}
                Err(err) => bail!(err),
            }
        }
        Ok(out)
    }

    fn index_id(&self, id: &str) -> Result<(), Error> {
        let record = self.find_one(id)?;
        self.search
            .upsert(&self.decl, &record, self.embedder.as_deref())
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

fn search_filter(query: &SearchQuery) -> Filter {
    (*query.filter).clone()
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

fn ensure_gitignore(vault: &Path) -> Result<bool, Error> {
    let path = vault.join(".gitignore");
    let mut text = if path.exists() {
        fs::read_to_string(&path).context("[Vault::open] read gitignore")?
    } else {
        String::new()
    };
    let already = text.lines().any(|line| {
        let line = line.trim();
        line == ".scheda/" || line == ".scheda"
    });
    if already {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(".scheda/\n");
    fs::write(&path, text).context("[Vault::open] write gitignore")?;
    Ok(true)
}

fn acquire_write_lock(index_dir: &Path) -> Result<File, Error> {
    let lock_path = index_dir.join("write.lock");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .context("[Vault::open] open write lock")?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            Err(Error::locked("vault write lock is held")
                .context("[Vault::open] acquire write lock"))
        }
        Err(TryLockError::Error(err)) => {
            Err(Error::new(io_code(&err), err.to_string()).context("[Vault::open] lock write lock"))
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
    use crate::{Catalog, Code, Error, Record};

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
        let vault = Vault::init(scratch.vault(), OpenOptions::new()).unwrap();
        let gitignore = fs::read_to_string(scratch.vault().join(".gitignore")).unwrap();
        assert!(gitignore.lines().any(|line| line.trim() == ".scheda/"));
        assert!(git::is_ignored(vault.path(), Path::new(".scheda/index")).unwrap());
        assert!(vault.index_dir().starts_with(vault.path()));
        assert!(vault.index_dir().ends_with("index"));
        assert!(vault.index_dir().join("write.lock").exists());
        fs::remove_dir_all(vault.index_dir()).unwrap();
        assert!(scratch.vault().join(".git").exists());
    }

    #[test]
    fn empty_remote_repo_option_is_local_init() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), OpenOptions::new().remote_repo("")).unwrap();
        assert!(!vault.has_remote());
    }

    #[test]
    fn insert_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .insert_one(Record::default())
            .expect_err("empty Record must fail envelope rules");
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn replace_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .replace_one("alpha", Record::default())
            .expect_err("empty Record must fail envelope rules");
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
        let vault = Vault::init(scratch.vault(), scratch.options()).unwrap();
        assert!(scratch.vault().join(".git").is_dir());
        assert!(!vault.has_remote());
        assert!(vault.remote_names().is_empty());
        assert!(vault.holds_write_lock());
        assert_eq!(git::remotes(vault.path()).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn open_keeps_index_inside_tree_and_gitignored() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options()).unwrap();
        assert!(vault.index_dir().starts_with(vault.path()));
        assert!(git::is_ignored(vault.path(), Path::new(".scheda")).unwrap());
        assert!(vault.index_dir().join("write.lock").exists());
        fs::remove_dir_all(vault.index_dir()).unwrap();
        assert!(scratch.vault().join(".git").exists());
    }

    #[test]
    fn two_opens_on_distinct_roots() {
        let a = Scratch::new();
        let b = Scratch::new();
        let vault_a = Vault::init(a.vault(), a.options()).unwrap();
        let vault_b = Vault::init(b.vault(), b.options()).unwrap();
        assert_ne!(vault_a.path(), vault_b.path());
        assert!(vault_a.holds_write_lock());
        assert!(vault_b.holds_write_lock());
    }

    #[test]
    fn second_write_open_is_locked() {
        let scratch = Scratch::new();
        let _writer = Vault::init(scratch.vault(), scratch.options()).unwrap();
        let err = Vault::open(scratch.vault(), scratch.options()).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert_eq!(err.known_code(), Some(Code::Locked));
    }

    #[test]
    fn reader_open_does_not_take_write_lock() {
        let scratch = Scratch::new();
        let writer = Vault::init(scratch.vault(), scratch.options()).unwrap();
        let reader = Vault::open(scratch.vault(), scratch.options().write(false)).unwrap();
        assert!(writer.holds_write_lock());
        assert!(!reader.holds_write_lock());
        assert_eq!(reader.path(), writer.path());
    }

    #[test]
    fn open_reports_origin_without_network() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options().write(false)).unwrap();
        git::add_remote(vault.path(), "origin", "https://example.invalid/scheda.git").unwrap();
        drop(vault);
        let opened = Vault::open(scratch.vault(), scratch.options().write(false)).unwrap();
        assert!(opened.has_remote());
        assert_eq!(opened.remote_names(), &["origin".to_owned()]);
        assert!(!opened.auto_sync());
    }

    #[test]
    fn init_does_not_configure_lfs() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options().write(false)).unwrap();
        assert!(!git::local_config_mentions_lfs(vault.path()).unwrap());
    }

    #[test]
    fn reader_auto_sync_with_remote_is_locked() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options().write(false)).unwrap();
        git::add_remote(vault.path(), "origin", "https://example.invalid/x.git").unwrap();
        drop(vault);
        let err = Vault::open(
            scratch.vault(),
            scratch.options().write(false).auto_sync(true),
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Locked));
        assert_eq!(err.known_code(), Some(Code::Locked));
    }

    #[test]
    fn git2_works_without_an_async_runtime() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options().write(false)).unwrap();
        assert!(git::is_work_tree(vault.path()));
    }

    #[test]
    fn catalog_is_on_the_vault_surface() {
        let _: fn(&Vault) -> Result<Catalog, Error> = Vault::catalog;
    }
}
