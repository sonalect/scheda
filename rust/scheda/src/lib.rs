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
//! - **`find`** — Tantivy `Vault::find` / `Collection::find`.
//!   Writes without it mark the Index stale, so a later build with
//!   `find` knows its lexical segments are behind and `find` fails
//!   closed until `reindex` rebuilds them. A build without `find`
//!   does not report that marker from `check`: it has no lexical
//!   segments and no way to rebuild them.
//! - **`search`** — hybrid `Vault::search` / `Collection::search`
//!   (BM25 + USearch). Pulls `find` in. `open` with an embedder without
//!   this feature fails (`index_vector`): the vector leg is absent.
//! - **`simd`** — USearch SIMD kernels via the `numkong` crate
//!   (`usearch/numkong`). Pulls `search` in. Needed for fast in-process
//!   ANN.
//! - **`openmp`** — USearch OpenMP executor (`usearch/openmp`). Needs
//!   libomp (Linux `libomp-dev`). Not enabled in the Bazel CI build.
//! - **`encrypt`** — age v1 ASCII-armor Record files for collections whose
//!   declaration marks `encrypted`. Off by default. A build without it refuses
//!   `OpenOptions.encryption` at `open` (`unsupported_encryption`) and
//!   refuses `collection` of an encrypted name the same way; other
//!   collections still work.
//!
//! # Tokio and other async runtimes
//!
//! Core operations are synchronous and do not start an async executor.
//! There is no `tokio` crate feature: disk I/O and git child processes
//! block the calling thread. From a Tokio task, run them on the blocking
//! pool so worker threads stay free. Share a [`Vault`] with
//! [`std::sync::Arc`] — the handle is `Send + Sync`. Concurrent `find` /
//! `search` on one handle share the Index (many readers). A write still
//! takes the exclusive side of that lock, and the Vault write lock is one
//! writer (`locked` if a second writer races it). Remote git (`push` /
//! `pull` / `sync`) and `reindex` / `search` are the calls that must not
//! sit on a worker thread. The Vault write lock is held for an open
//! transaction and for one mutating call outside a transaction, then
//! released. `reindex` takes that lock only for start and publish; the
//! file walk does not hold it. A second flock, `reindex.lock`, is held
//! for the whole rebuild. The lexical Index writer uses the same
//! lifetime, so a second handle can write after `commit` without waiting
//! on Tantivy's directory lock.
//!
//! ```rust,ignore (tokio is not a crate dependency)
//! use std::sync::Arc;
//! use scheda::{Record, Vault};
//!
//! async fn get_one(vault: Arc<Vault>, id: String) -> Result<Record, scheda::Error> {
//!     tokio::task::spawn_blocking(move || vault.collection("notes")?.get(&id))
//!         .await
//!         .map_err(|err| {
//!             scheda::Error::io_other(err.to_string())
//!                 .context("join blocking task")
//!         })?
//! }
//! ```

#![doc(html_root_url = "https://amsokol.github.io/scheda/scheda")]

#[cfg(feature = "search")]
mod ann;
#[cfg(feature = "encrypt")]
mod crypto;
mod embed;
mod error;
mod git;
mod id;
mod index;
mod isolated;
mod journal;
mod json;
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
pub use journal::JournalLines;
pub use json::{declaration_from_json, declaration_from_value};
pub use projector::{IndexDoc, IndexValue, Projector};
#[doc(inline)]
pub use scheda_generated::api::v1::{
    Author, CollectionInfo, ColumnKind, Declaration, Encryption, Error, History, IndexColumn,
    IndexSchema, Journal, OpenOptions, Record, ReindexResult, Singleton, Unit, VaultInfo, history,
};
#[cfg(feature = "find")]
#[doc(inline)]
pub use scheda_generated::api::v1::{Filter, FindQuery, FindResult, filter};
#[cfg(feature = "search")]
#[doc(inline)]
pub use scheda_generated::api::v1::{SearchHits, SearchQuery, search_hits};
pub use schema::Kind;
pub use store::CollectionDocuments;
pub use vault::{Collection, Vault};

#[cfg(feature = "encrypt")]
pub use crypto::parse_age_identity;
