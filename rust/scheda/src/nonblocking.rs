//! Async API on Tokio (crate feature `tokio`).
//!
//! [`AsyncVault`] and [`AsyncCollection`] wrap the sync [`Vault`] and
//! [`Collection`]. Every method that touches the disk, git, the Index, or a
//! caller callback (embedder, projector, reranker) runs its sync counterpart
//! on Tokio's blocking pool (`spawn_blocking`), so runtime worker threads
//! stay free. Arguments are owned, because the call outlives the borrow.
//! Cheap getters that only read the handle stay sync.
//!
//! There is no `begin` / `commit` / `discard`: the Vault write lock belongs
//! to the thread that took it, and two awaits may resume on two different
//! blocking threads. Batch writes go through [`AsyncVault::transaction`],
//! which runs the whole batch on one thread.
//!
//! A blocking task cancelled by a runtime that shuts down is
//! `io_interrupted`. A panic in a caller callback is `io_other`, with the
//! method in the stack. The sync API is unchanged and never starts a
//! runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Context as _;
use crate::{
    Collection, Embedder, Error, History, Kind, OpenOptions, Projector, Record, ReindexResult,
    Schema, Vault, VaultInfo,
};

/// Run `f` on Tokio's blocking pool and map a failed join to a Scheda
/// error. `frame` names the method for the error stack.
async fn blocking<T, F>(frame: &'static str, f: F) -> Result<T, Error>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Error> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(err) if err.is_cancelled() => {
            Err(Error::io_interrupted("the blocking task was cancelled").context(frame))
        }
        Err(_) => Err(Error::io_other("a caller callback panicked").context(frame)),
    }
}

/// Async handle on a Vault. Cheap to clone: clones share one [`Vault`].
///
/// Build it with [`AsyncVault::open`] / [`AsyncVault::open_with`], or wrap
/// a sync handle with `From<Vault>`.
#[derive(Clone)]
pub struct AsyncVault {
    vault: Arc<Vault>,
}

impl From<Vault> for AsyncVault {
    fn from(vault: Vault) -> Self {
        Self {
            vault: Arc::new(vault),
        }
    }
}

impl std::fmt::Debug for AsyncVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AsyncVault").field(&self.vault).finish()
    }
}

impl AsyncVault {
    /// Async [`Vault::open`]: open, create, or clone a Vault.
    pub async fn open(
        path: impl Into<PathBuf>,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Self, Error> {
        Self::open_with(path, options, embedder, None).await
    }

    /// Async [`Vault::open_with`]: [`open`](Self::open) with an optional
    /// caller [`Projector`].
    pub async fn open_with(
        path: impl Into<PathBuf>,
        options: OpenOptions,
        embedder: Option<Arc<dyn Embedder>>,
        projector: Option<Arc<dyn Projector>>,
    ) -> Result<Self, Error> {
        let path = path.into();
        blocking("[AsyncVault::open]", move || {
            Vault::open_with(path, options, embedder, projector)
        })
        .await
        .map(Self::from)
    }

    /// The sync handle this one wraps, for a call already on a blocking
    /// thread.
    #[must_use]
    pub fn as_vault(&self) -> &Vault {
        &self.vault
    }

    /// [`Vault::path`].
    #[must_use]
    pub fn path(&self) -> &Path {
        self.vault.path()
    }

    /// [`Vault::index_dir`].
    #[must_use]
    pub fn index_dir(&self) -> &Path {
        self.vault.index_dir()
    }

    /// [`Vault::writer`].
    #[must_use]
    pub fn writer(&self) -> &str {
        self.vault.writer()
    }

    /// [`Vault::has_remote`].
    #[must_use]
    pub fn has_remote(&self) -> bool {
        self.vault.has_remote()
    }

    /// [`Vault::remote_names`].
    #[must_use]
    pub fn remote_names(&self) -> &[String] {
        self.vault.remote_names()
    }

    /// [`Vault::auto_sync`].
    #[must_use]
    pub fn auto_sync(&self) -> bool {
        self.vault.auto_sync()
    }

    /// [`Vault::versions`].
    pub fn versions(&self) -> Result<Vec<u32>, Error> {
        self.vault.versions()
    }

    /// [`Vault::set_embedder`]: swaps the handle's embedder and wakes the
    /// background indexer; no I/O.
    pub fn set_embedder(
        &self,
        embedder: Option<Arc<dyn Embedder>>,
    ) -> Result<Option<Arc<dyn Embedder>>, Error> {
        self.vault.set_embedder(embedder)
    }

    /// [`Vault::collection`]: an async handle for one collection.
    pub fn collection(&self, name: &str) -> Result<AsyncCollection, Error> {
        self.vault.collection(name).map(AsyncCollection::from)
    }

    /// Async [`Vault::info`].
    pub async fn info(&self) -> Result<VaultInfo, Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::info]", move || vault.info()).await
    }

    /// Async [`Vault::create_collection`].
    pub async fn create_collection(
        &self,
        name: String,
        schema: Schema,
        message: Option<String>,
    ) -> Result<AsyncCollection, Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::create_collection]", move || {
            vault.create_collection(&name, schema, message.as_deref())
        })
        .await
        .map(AsyncCollection::from)
    }

    /// Run `f` as one transaction: [`Vault::begin`], `f`, then
    /// [`Vault::commit`] with `message`, all on one blocking thread, so the
    /// write lock never changes threads mid-transaction. Every write `f`
    /// makes through the [`Vault`] it is handed, or through a collection
    /// from it, joins the transaction and lands in one git commit.
    ///
    /// When `f` returns an error, or `commit` fails, the transaction is
    /// discarded and that error is returned. When `f` panics, the
    /// transaction is discarded and the result is `io_other`. While this
    /// `Vault` holds a transaction on another thread (another `transaction`
    /// call, or a sync [`Vault::begin`]), the result is `locked` at once and
    /// `f` does not run.
    pub async fn transaction<T, F>(&self, message: impl Into<String>, f: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&Vault) -> Result<T, Error> + Send + 'static,
    {
        const FRAME: &str = "[AsyncVault::transaction]";
        let vault = Arc::clone(&self.vault);
        let message = message.into();
        blocking(FRAME, move || {
            vault.begin().with_context(|| FRAME)?;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&vault)));
            let result = match outcome {
                Ok(Ok(value)) => vault.commit(&message).map(|()| value),
                Ok(Err(err)) => Err(err),
                Err(_panic) => Err(Error::io_other("the transaction closure panicked")),
            };
            if result.is_err() {
                // The error the caller sees is the one that ended the
                // transaction; a failed discard leaves leftovers that the
                // next `begin` or mutating call restores.
                let _ = vault.discard();
            }
            result.with_context(|| FRAME)
        })
        .await
    }

    /// Async [`Vault::check`].
    pub async fn check(&self) -> Result<(), Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::check]", move || vault.check()).await
    }

    /// Async [`Vault::reindex`].
    pub async fn reindex(&self) -> Result<ReindexResult, Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::reindex]", move || vault.reindex()).await
    }

    /// Async [`Vault::push`].
    pub async fn push(&self) -> Result<(), Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::push]", move || vault.push()).await
    }

    /// Async [`Vault::pull`].
    pub async fn pull(&self) -> Result<(), Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::pull]", move || vault.pull()).await
    }

    /// Async [`Vault::sync`]: `pull`, then `push`.
    pub async fn sync(&self) -> Result<(), Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::sync]", move || vault.sync()).await
    }

    /// Async [`Vault::push_isolated`].
    pub async fn push_isolated(&self) -> Result<(), Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::push_isolated]", move || vault.push_isolated()).await
    }

    /// Async [`Vault::pull_isolated`].
    pub async fn pull_isolated(&self) -> Result<(), Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::pull_isolated]", move || vault.pull_isolated()).await
    }

    /// Async [`Vault::sync_isolated`].
    pub async fn sync_isolated(&self) -> Result<(), Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::sync_isolated]", move || vault.sync_isolated()).await
    }

    /// Async [`Vault::find`].
    #[cfg(feature = "find")]
    pub async fn find(
        &self,
        collections: Vec<String>,
        query: crate::FindQuery,
        offset: u32,
        count: u32,
    ) -> Result<crate::FindResult, Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::find]", move || {
            let names: Vec<&str> = collections.iter().map(String::as_str).collect();
            vault.find(&names, &query, offset, count)
        })
        .await
    }

    /// Async [`Vault::search`]. The query embedding and the reranker run on
    /// the same blocking thread as the search.
    #[cfg(feature = "search")]
    pub async fn search(
        &self,
        collections: Vec<String>,
        query: crate::SearchQuery,
        reranker: Option<Arc<dyn crate::Reranker>>,
    ) -> Result<crate::SearchHits, Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::search]", move || {
            let names: Vec<&str> = collections.iter().map(String::as_str).collect();
            vault.search(&names, &query, reranker.as_deref())
        })
        .await
    }

    /// Async [`Vault::index_status`].
    #[cfg(feature = "find")]
    pub async fn index_status(&self) -> Result<crate::IndexStatus, Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::index_status]", move || vault.index_status()).await
    }

    /// Async [`Vault::wait_indexed`].
    #[cfg(feature = "find")]
    pub async fn wait_indexed(
        &self,
        timeout: std::time::Duration,
    ) -> Result<crate::IndexStatus, Error> {
        let vault = Arc::clone(&self.vault);
        blocking("[AsyncVault::wait_indexed]", move || {
            vault.wait_indexed(timeout)
        })
        .await
    }

    /// Async [`Vault::close`]. With the last reference to the Vault, stops
    /// and joins the background indexer on a blocking thread. Otherwise it
    /// only releases this reference: the last clone (or [`Vault`] drop)
    /// stops the indexer, as `Drop` does.
    pub async fn close(self) -> Result<(), Error> {
        match Arc::try_unwrap(self.vault) {
            Ok(vault) => blocking("[AsyncVault::close]", move || vault.close()).await,
            Err(_shared) => Ok(()),
        }
    }
}

/// Async handle on one collection. Cheap to clone.
#[derive(Clone)]
pub struct AsyncCollection {
    collection: Arc<Collection>,
}

impl From<Collection> for AsyncCollection {
    fn from(collection: Collection) -> Self {
        Self {
            collection: Arc::new(collection),
        }
    }
}

impl std::fmt::Debug for AsyncCollection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AsyncCollection")
            .field(&self.collection)
            .finish()
    }
}

impl AsyncCollection {
    /// The sync handle this one wraps.
    #[must_use]
    pub fn as_collection(&self) -> &Collection {
        &self.collection
    }

    /// [`Collection::name`].
    #[must_use]
    pub fn name(&self) -> &str {
        self.collection.name()
    }

    /// [`Collection::schema`].
    pub fn schema(&self) -> Result<Schema, Error> {
        self.collection.schema()
    }

    /// [`Collection::kind`].
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.collection.kind()
    }

    /// [`Collection::version`].
    #[must_use]
    pub fn version(&self) -> u32 {
        self.collection.version()
    }

    /// [`Collection::segment_max_bytes`].
    #[must_use]
    pub fn segment_max_bytes(&self) -> Option<u64> {
        self.collection.segment_max_bytes()
    }

    /// [`Collection::singleton_id`].
    #[must_use]
    pub fn singleton_id(&self) -> Option<&str> {
        self.collection.singleton_id()
    }

    /// Async [`Collection::add`].
    pub async fn add(
        &self,
        id: String,
        raw: Vec<u8>,
        message: Option<String>,
    ) -> Result<(), Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::add]", move || {
            collection.add(&id, raw, message.as_deref())
        })
        .await
    }

    /// Async [`Collection::get`].
    pub async fn get(&self, id: String) -> Result<Record, Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::get]", move || collection.get(&id)).await
    }

    /// Async [`Collection::replace`].
    pub async fn replace(
        &self,
        id: String,
        raw: Vec<u8>,
        message: Option<String>,
    ) -> Result<(), Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::replace]", move || {
            collection.replace(&id, raw, message.as_deref())
        })
        .await
    }

    /// Async [`Collection::remove`].
    pub async fn remove(&self, id: String, message: Option<String>) -> Result<(), Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::remove]", move || {
            collection.remove(&id, message.as_deref())
        })
        .await
    }

    /// Every line of a `journal` in append order, as
    /// [`Collection::lines`] yields them, collected on one blocking thread.
    /// The whole journal is held in memory; walk [`Collection::lines`] on a
    /// blocking thread of your own to stream it.
    pub async fn lines(&self) -> Result<Vec<Record>, Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::lines]", move || {
            collection.lines()?.collect()
        })
        .await
    }

    /// Every Record of a `document` / `singleton` collection, as
    /// [`Collection::documents`] yields them, collected on one blocking
    /// thread. The whole collection is held in memory; walk
    /// [`Collection::documents`] on a blocking thread of your own to stream
    /// it.
    pub async fn documents(&self) -> Result<Vec<Record>, Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::documents]", move || {
            collection.documents()?.collect()
        })
        .await
    }

    /// Async [`Collection::ids`].
    pub async fn ids(&self) -> Result<Vec<String>, Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::ids]", move || collection.ids()).await
    }

    /// Async [`Collection::history`].
    pub async fn history(&self, id: String, offset: u32, count: u32) -> Result<History, Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::history]", move || {
            collection.history(&id, offset, count)
        })
        .await
    }

    /// Async [`Collection::find`].
    #[cfg(feature = "find")]
    pub async fn find(
        &self,
        query: crate::FindQuery,
        offset: u32,
        count: u32,
    ) -> Result<crate::FindResult, Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::find]", move || {
            collection.find(&query, offset, count)
        })
        .await
    }

    /// Async [`Collection::search`]. The query embedding and the reranker
    /// run on the same blocking thread as the search.
    #[cfg(feature = "search")]
    pub async fn search(
        &self,
        query: crate::SearchQuery,
        reranker: Option<Arc<dyn crate::Reranker>>,
    ) -> Result<crate::SearchHits, Error> {
        let collection = Arc::clone(&self.collection);
        blocking("[AsyncCollection::search]", move || {
            collection.search(&query, reranker.as_deref())
        })
        .await
    }
}
