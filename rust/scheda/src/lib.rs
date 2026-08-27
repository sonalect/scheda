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
//! Default features are `remote-git` and `numkong`.
//! `default-features = false` turns both off (file remotes still work).
//!
//! - **`remote-git`** *(default)* — libgit2 HTTPS and SSH (`git2/https`,
//!   `git2/ssh`). TLS is platform OpenSSL / Secure Transport / WinHTTP.
//!   Without it, `https://` and `git@` remotes are `unsupported`.
//! - **`numkong`** *(default)* — USearch SIMD kernels via the `numkong`
//!   crate. Needed for fast in-process ANN used by hybrid `search`.
//! - **`openmp`** — USearch OpenMP executor (`usearch/openmp`). Needs
//!   libomp (Linux `libomp-dev`). Not enabled in the Bazel default build.
//!
//! # Tokio and other async runtimes
//!
//! Core operations are synchronous and do not start an async executor.
//! There is no `tokio` crate feature: disk I/O and libgit2 block the calling
//! thread. From a Tokio task, run them on the blocking pool so worker threads
//! stay free. Share a [`Vault`] with [`std::sync::Arc`] — the handle is
//! `Send + Sync`. Remote git (`push` / `pull` / `sync`) and `reindex` /
//! `search` are the calls that must not sit on a worker thread. The Vault
//! write lock is taken by mutating calls only: one writer, `locked` if a
//! second writer races it.
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

mod ann;
mod embed;
mod error;
mod git;
mod id;
mod index;
mod journal;
mod query;
mod schema;
mod search;
mod store;
mod vault;

#[doc(hidden)]
pub use ann::AnnIndex;
pub use embed::{Embedder, Reranker};
pub use error::{Code, Repeat};
pub use journal::JournalLines;
#[doc(inline)]
pub use scheda_generated::api::v1::{
    Body, CollectionInfo, Edge, Error, Field, Filter, FindQuery, FindResult, History, Journal,
    Object, OpenOptions, Record, RecordAdd, RecordReplace, ReindexResult, Schema, SearchHits,
    SearchQuery, Singleton, Text, Unit, VaultInfo, filter, history, search_hits,
};
pub use schema::Kind;
pub use vault::{Collection, Vault};
