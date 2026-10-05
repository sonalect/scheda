//! Git-backed document store.
//!
//! Records live as text files in a **Vault** (a git working tree). The search
//! index is derived and lives under `{vault}/.scheda/`, which `open` gitignores.
//! Scheda does not know a consumer's types.
//!
//! HTML rustdoc is published on
//! [GitHub Pages](https://amsokol.github.io/scheda/scheda/).
//!
//! # Crate features
//!
//! There are no default features. `cargo add scheda` does not pull
//! Tantivy or USearch. `search` turns `find` on; `simd` turns `search`
//! on; the reverse does not. `encrypt` is also off. Git is not a feature:
//! every build invokes the system `git` executable, so `git` must be
//! installed and on `PATH`. Remotes (`https://`, `http://`, `ssh://`,
//! `git@`, and file paths) all work and authenticate exactly like a plain
//! `git` command, including `~/.ssh/config`, `ProxyCommand`, credential
//! helpers, and host-key checking.
//!
//! - **`find`** — Tantivy `Vault::find` / `Collection::find`, and the
//!   background indexer that keeps the Index on the last commit. Writes
//!   from a build without it are commits like any other: the next handle
//!   with `find` indexes them from the commit its Index last reached.
//! - **`search`** — hybrid `Vault::search` / `Collection::search`
//!   (BM25 + USearch). Pulls `find` in. `open` with an embedder without
//!   this feature fails (`index_vector`): the vector leg is absent.
//! - **`simd`** — USearch SIMD kernels via the `numkong` crate
//!   (`usearch/numkong`). Pulls `search` in. Needed for fast in-process
//!   ANN.
//! - **`openmp`** — USearch OpenMP executor (`usearch/openmp`). Needs
//!   libomp (Linux `libomp-dev`). Not enabled in the Bazel CI build.
//! - **`encrypt`** — age v1 ASCII-armor Record files for collections whose
//!   Schema marks `encrypted`. Off by default. A build without it refuses
//!   `OpenOptions.encryption` at `open` (`unsupported_encryption`) and
//!   refuses `collection` of an encrypted name the same way; other
//!   collections still work.
//!
//! # Background indexer
//!
//! With `find`, `open` starts one thread per handle that keeps the Index on
//! the last commit: it wakes on this handle's commits and otherwise reads
//! `HEAD` every `OpenOptions.index_poll_ms` (1 s by default), so commits
//! made by `git`, another process, or a build without `find` are indexed
//! too. Only committed state is indexed; `find` / `search` are eventually
//! consistent. Call `Vault::wait_indexed` before a query that must see a
//! write just made, and `Vault::index_status` to see how far the Index
//! is behind and why. Several processes may open one Vault: one of them
//! indexes at a time (`{vault}/.scheda/index.lock`, and `vector.lock` for
//! a process whose embedder matches the Index vectors); the others follow
//! what it writes and take over when it exits.
//!
//! The thread never outlives the handle: dropping the last `Vault` /
//! `Collection` clone stops and joins it, after at most one embedder call.
//! [`Vault::close`] does the same and reports a failure. Scheda installs no
//! signal handler. A program that wants a clean exit on `SIGINT` /
//! `SIGTERM` catches the signal itself and calls `close` (or drops the
//! handle) before exiting. Being killed instead loses nothing: the Index
//! records progress only after its writes are durable, and the next `open`
//! resumes from there.
//!
//! # Tokio and other async runtimes
//!
//! Core operations are synchronous and do not start an async executor:
//! disk I/O and git child processes block the calling thread, and a build
//! without the `tokio` feature has no runtime dependency at all. Share a
//! [`Vault`] with [`std::sync::Arc`] — the handle is `Send + Sync`.
//! Concurrent `find` / `search` on one handle share the Index (many
//! readers). The Vault write lock is one writer (`locked` if a second
//! writer races it); it belongs to the thread that took it and is held for
//! an open transaction and for one mutating call outside a transaction,
//! then released. Writes do not touch the Index. `reindex` (with `find`)
//! takes no write lock: it builds from commits and holds only the flock
//! `{vault}/.scheda/reindex.lock`, so a second `reindex` is `locked`.
//!
//! The opt-in `tokio` feature adds `nonblocking`: `AsyncVault` and
//! `AsyncCollection` run each call that does I/O, or calls the embedder,
//! Projector, or reranker, on Tokio's blocking pool, so runtime worker
//! threads stay free. They have no `begin` / `commit` / `discard`, because
//! two awaits may resume on two threads; `AsyncVault::transaction` runs
//! `begin`, a sync closure, and `commit` on one blocking thread instead.
//! `AsyncVault::wait_indexed` waits for the indexer's rounds without
//! holding a blocking thread.
//!
//! ```rust,ignore (needs the tokio feature and a runtime)
//! use scheda::{AsyncVault, OpenOptions};
//!
//! async fn add_two(path: &str) -> Result<(), scheda::Error> {
//!     let vault = AsyncVault::open(path, OpenOptions::new(), None).await?;
//!     vault
//!         .transaction("add two notes", |vault| {
//!             let notes = vault.collection("notes")?;
//!             notes.add("alpha", b"one", None)?;
//!             notes.add("beta", b"two", None)
//!         })
//!         .await?;
//!     vault.close().await
//! }
//! ```
//!
//! Without the feature, wrap the sync calls in `spawn_blocking` yourself;
//! a batch of writes then has to stay inside one `spawn_blocking`.

#![doc(html_root_url = "https://amsokol.github.io/scheda/scheda")]

#[cfg(feature = "search")]
mod ann;
#[cfg(feature = "encrypt")]
mod crypto;
#[cfg(feature = "find")]
mod diff;
mod embed;
mod error;
mod git;
mod id;
mod index;
#[cfg(feature = "find")]
mod indexer;
mod isolated;
mod journal;
mod json;
mod marks;
#[cfg(feature = "tokio")]
pub mod nonblocking;
mod projector;
#[cfg(feature = "find")]
mod query;
mod reindex;
mod schema;
#[cfg(feature = "find")]
mod search;
mod store;
mod vault;
mod writer;

#[cfg(feature = "search")]
#[doc(hidden)]
pub use ann::AnnIndex;
pub use embed::{Embedder, Reranker};
#[doc(hidden)]
pub use error::redact_remote_text;
pub use error::{Code, Repeat};
#[cfg(feature = "find")]
pub use indexer::IndexStatus;
pub use journal::JournalLines;
pub use json::{schema_from_json, schema_from_value};
pub use projector::{IndexDoc, IndexValue, Projector};
#[doc(inline)]
pub use scheda_api::api::collection::v1::{CollectionInfo, Journal, Schema, Singleton, Unit};
#[doc(inline)]
pub use scheda_api::api::v1::{
    Author, ColumnKind, Encryption, Error, History, IndexColumn, IndexSchema, OpenOptions, Record,
    ReindexResult, VaultInfo, history,
};
#[cfg(feature = "find")]
#[doc(inline)]
pub use scheda_api::api::v1::{Filter, FindQuery, FindResult, filter};
#[cfg(feature = "search")]
#[doc(inline)]
pub use scheda_api::api::v1::{SearchHits, SearchQuery, search_hits};
pub use schema::Kind;
pub use store::CollectionDocuments;
pub use vault::{Collection, Vault};
#[cfg(feature = "tokio")]
pub use nonblocking::{AsyncCollection, AsyncVault};

#[cfg(feature = "encrypt")]
pub use crypto::parse_age_identity;
