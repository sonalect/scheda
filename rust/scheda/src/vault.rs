use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};

use crate::git;
use crate::schema::{self, CollectionDecl, Kind, Schema};
use crate::{Error, OpenOptions, Record};

/// A git working tree that holds collections of records.
#[derive(Debug)]
pub struct Vault {
    path: PathBuf,
    index_dir: PathBuf,
    remotes: Vec<String>,
    auto_sync: bool,
    write_lock: Option<File>,
    schema: Schema,
    schema_versions: Vec<u32>,
}

/// A named set of records with one write mode (`document`, `journal`, or `singleton`).
#[derive(Debug)]
pub struct Collection {
    name: String,
    kind: Kind,
    schema_version: u32,
    id_field: String,
    layout: Vec<String>,
    filter_keys: std::collections::BTreeSet<String>,
    segment_max_bytes: Option<u64>,
    singleton_id: Option<String>,
    fields: Vec<crate::schema::FieldDecl>,
}

/// Field filter for `find`. Shape is not modelled yet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Filter;

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

/// Hybrid search hits. Ranking is not modelled yet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchHits;

impl Vault {
    /// Create a Vault at `path` (`git init`, no Remote). LIB-2 / GIT-9.
    pub fn init(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        options.require_valid()?;
        let path = path.as_ref();
        prepare_new_work_tree(path)?;
        git::init(path)?;
        Self::open(path, options)
    }

    /// Clone `url` into `path`. LIB-2 name `clone` (GIT-9).
    pub fn clone(url: &str, path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        options.require_valid()?;
        let _url = url;
        let _path = path.as_ref();
        todo!("Vault::clone is not implemented (C7)")
    }

    /// Open an existing Vault at `path`.
    pub fn open(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        options.require_valid()?;
        let path = path.as_ref();
        if !git::is_work_tree(path) {
            return Err(Error::invalid(format!(
                "path is not a git working tree: {}",
                path.display()
            )));
        }
        let Some(index_dir) = options.index_dir.as_deref() else {
            unreachable!("OpenOptions validate requires index_dir");
        };
        let write = options.write.unwrap_or(true);
        let auto_sync = options.auto_sync.unwrap_or(false);
        let path = fs::canonicalize(path)
            .map_err(|err| Error::io(format!("canonicalize vault {}: {err}", path.display())))?;
        fs::create_dir_all(index_dir)
            .map_err(|err| Error::io(format!("create index directory {index_dir}: {err}")))?;
        let index_dir = fs::canonicalize(index_dir)
            .map_err(|err| Error::io(format!("canonicalize index {index_dir}: {err}")))?;
        reject_index_inside_vault(&path, &index_dir)?;
        let remotes = git::remotes(&path)?;
        if auto_sync && !remotes.is_empty() {
            return Err(Error::unsupported(
                "auto_sync with a Remote is not implemented until C7",
            ));
        }
        let write_lock = if write {
            Some(acquire_write_lock(&index_dir)?)
        } else {
            None
        };
        let schema = schema::load(&path)?;
        let schema_versions = schema.versions();
        Ok(Self {
            path,
            index_dir,
            remotes,
            auto_sync,
            write_lock,
            schema,
            schema_versions,
        })
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

    pub fn collection(&self, name: &str) -> Result<Collection, Error> {
        let Some(decl) = self.schema.get(name) else {
            return Err(Error::not_found(format!(
                "collection {name} is not in Schema"
            )));
        };
        Ok(Collection::from_decl(decl))
    }

    /// Create one git commit for changes already written to the working tree.
    pub fn commit(&self, message: &str) -> Result<(), Error> {
        let _message = message;
        todo!("Vault::commit is not implemented (C6)")
    }

    pub fn push_remote(&self) -> Result<(), Error> {
        todo!("Vault::push_remote is not implemented (C7)")
    }

    pub fn pull_remote(&self) -> Result<(), Error> {
        todo!("Vault::pull_remote is not implemented (C7)")
    }

    pub fn sync_remote(&self) -> Result<(), Error> {
        todo!("Vault::sync_remote is not implemented (C7)")
    }

    pub fn check(&self) -> Result<(), Error> {
        schema::load(&self.path).map(|_| ())
    }

    pub fn reindex(&self) -> Result<(), Error> {
        todo!("Vault::reindex is not implemented (C5)")
    }
}

impl Collection {
    fn from_decl(decl: &CollectionDecl) -> Self {
        Self {
            name: decl.name.clone(),
            kind: decl.kind,
            schema_version: decl.schema_version,
            id_field: decl.id_field.clone(),
            layout: decl.layout.clone(),
            filter_keys: decl.filter_keys.clone(),
            segment_max_bytes: decl.segment_max_bytes,
            singleton_id: decl.singleton_id.clone(),
            fields: decl.fields.clone(),
        }
    }

    #[cfg(test)]
    fn stub() -> Self {
        Self {
            name: String::from("c"),
            kind: Kind::Document,
            schema_version: 1,
            id_field: String::from("id"),
            layout: vec![String::from("c/{id}.md")],
            filter_keys: std::collections::BTreeSet::new(),
            segment_max_bytes: None,
            singleton_id: None,
            fields: Vec::new(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub fn id_field(&self) -> &str {
        &self.id_field
    }

    #[must_use]
    pub fn is_filter_key(&self, field: &str) -> bool {
        self.filter_keys.contains(field)
    }

    #[must_use]
    pub fn segment_max_bytes(&self) -> Option<u64> {
        self.segment_max_bytes
    }

    #[must_use]
    pub fn layout(&self) -> &[String] {
        &self.layout
    }

    #[must_use]
    pub fn singleton_id(&self) -> Option<&str> {
        self.singleton_id.as_deref()
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
        self.field(name).map(|field| field.has_default)
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
        walk(&self.fields, name)
    }

    pub fn insert_one(&self, record: Record) -> Result<InsertOne, Error> {
        record.require_valid()?;
        todo!("Collection::insert_one is not implemented (C3)")
    }

    pub fn find_one(&self, id: &str) -> Result<Option<Record>, Error> {
        let _id = id;
        todo!("Collection::find_one is not implemented (C3)")
    }

    /// Replace a whole `document`. Errors on `journal`.
    pub fn replace_one(&self, _id: &str, record: Record) -> Result<ReplaceOne, Error> {
        record.require_valid()?;
        todo!("Collection::replace_one is not implemented (C3)")
    }

    pub fn find(&self, filter: &Filter) -> Result<Vec<Record>, Error> {
        let _filter = filter;
        todo!("Collection::find is not implemented (C5)")
    }

    /// `find` with an empty filter (LIB-10).
    pub fn scan(&self) -> Result<Vec<Record>, Error> {
        self.find(&Filter)
    }

    /// Hybrid retrieval. Not in **core** (LIB-11).
    pub fn search(&self, query: &str) -> Result<SearchHits, Error> {
        let _query = query;
        Err(Error::unsupported(
            "search is not implemented until the search phase",
        ))
    }
}

fn prepare_new_work_tree(path: &Path) -> Result<(), Error> {
    if git::is_work_tree(path) {
        return Err(Error::invalid(format!(
            "path is already a git working tree: {}",
            path.display()
        )));
    }
    if path.exists() {
        if !path.is_dir() {
            return Err(Error::invalid(format!(
                "init path is not a directory: {}",
                path.display()
            )));
        }
        let mut entries = fs::read_dir(path)
            .map_err(|err| Error::io(format!("read init directory {}: {err}", path.display())))?;
        if entries
            .next()
            .transpose()
            .map_err(|err| Error::io(format!("read init directory {}: {err}", path.display())))?
            .is_some()
        {
            return Err(Error::invalid(format!(
                "init path is not empty: {}",
                path.display()
            )));
        }
    } else {
        fs::create_dir_all(path)
            .map_err(|err| Error::io(format!("create vault {}: {err}", path.display())))?;
    }
    Ok(())
}

fn reject_index_inside_vault(vault: &Path, index: &Path) -> Result<(), Error> {
    if index == vault || index.starts_with(vault) {
        return Err(Error::invalid(format!(
            "index directory {} must be outside the Vault {}",
            index.display(),
            vault.display()
        )));
    }
    Ok(())
}

fn acquire_write_lock(index_dir: &Path) -> Result<File, Error> {
    let lock_path = index_dir.join("write.lock");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| Error::io(format!("open write lock {}: {err}", lock_path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::locked(format!(
            "vault write lock is held: {}",
            lock_path.display()
        ))),
        Err(TryLockError::Error(err)) => Err(Error::io(format!(
            "lock write lock {}: {err}",
            lock_path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Collection, OpenOptions, Vault};
    use crate::git;
    use crate::{Code, Error, Record};

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

        fn index(&self) -> PathBuf {
            self.root.join("index")
        }

        fn options(&self) -> OpenOptions {
            OpenOptions::new(self.index())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn init_rejects_empty_index_dir_before_creating_tree() {
        let scratch = Scratch::new();
        let err = Vault::init(scratch.vault(), OpenOptions::new("")).unwrap_err();
        assert_eq!(Code::of(&err), Some(Code::Invalid));
        assert!(!git::is_work_tree(&scratch.vault()));
    }

    #[test]
    fn open_rejects_missing_index_dir() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options().write(false)).unwrap();
        drop(vault);
        let err = Vault::open(scratch.vault(), OpenOptions::default()).unwrap_err();
        assert_eq!(Code::of(&err), Some(Code::Invalid));
    }

    #[test]
    fn clone_rejects_invalid_options_before_c7() {
        let scratch = Scratch::new();
        let err = Vault::clone(
            "https://example.invalid/scheda.git",
            scratch.vault(),
            OpenOptions::default(),
        )
        .unwrap_err();
        assert_eq!(Code::of(&err), Some(Code::Invalid));
    }

    #[test]
    fn insert_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .insert_one(Record::default())
            .expect_err("empty Record must fail envelope rules");
        assert_eq!(Code::of(&err), Some(Code::Invalid));
    }

    #[test]
    fn replace_one_rejects_empty_record_envelope() {
        let collection = Collection::stub();
        let err = collection
            .replace_one("alpha", Record::default())
            .expect_err("empty Record must fail envelope rules");
        assert_eq!(Code::of(&err), Some(Code::Invalid));
    }

    #[test]
    fn search_is_unsupported_and_does_not_write() {
        let collection = Collection::stub();
        let err = collection
            .search("anything")
            .expect_err("search must fail in core");
        assert_eq!(Code::of(&err), Some(Code::Unsupported));
        assert_eq!(err.code.as_deref(), Some("unsupported"));
    }

    #[test]
    fn unsupported_code_is_not_only_the_message() {
        let err = Error::unsupported("replace_one on journal");
        assert_eq!(Code::of(&err), Some(Code::Unsupported));
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
    fn open_uses_index_outside_the_tree() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options()).unwrap();
        assert_eq!(
            vault.index_dir(),
            scratch.index().canonicalize().unwrap().as_path()
        );
        assert!(!vault.index_dir().starts_with(vault.path()));
        assert!(
            git::worktree_is_clean(vault.path()).unwrap(),
            "Index must not appear in the Vault working tree"
        );
        assert!(scratch.index().join("write.lock").exists());
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
        assert_eq!(Code::of(&err), Some(Code::Locked));
        assert_eq!(err.code.as_deref(), Some("locked"));
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
    fn index_inside_vault_is_invalid() {
        let scratch = Scratch::new();
        let vault_path = scratch.vault();
        let err = Vault::init(
            &vault_path,
            OpenOptions::new(vault_path.join("nested-index")),
        )
        .unwrap_err();
        assert_eq!(Code::of(&err), Some(Code::Invalid));
    }

    #[test]
    fn auto_sync_with_remote_is_unsupported_until_c7() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options().write(false)).unwrap();
        git::add_remote(vault.path(), "origin", "https://example.invalid/x.git").unwrap();
        drop(vault);
        let err = Vault::open(
            scratch.vault(),
            scratch.options().write(false).auto_sync(true),
        )
        .unwrap_err();
        assert_eq!(Code::of(&err), Some(Code::Unsupported));
    }

    #[test]
    fn git2_works_without_an_async_runtime() {
        let scratch = Scratch::new();
        let vault = Vault::init(scratch.vault(), scratch.options().write(false)).unwrap();
        assert!(git::is_work_tree(vault.path()));
    }
}
