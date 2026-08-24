//! Git-backed document store.
//!
//! Records live as text files in a **Vault** (a git working tree). The search
//! index is derived and lives under `{vault}/.scheda/`, which `init` gitignores.
//! Scheda does not know a consumer's types.
//!
//! This crate is the Rust binding of the [design
//! package](https://github.com/amsokol/scheda/tree/main/docs). HTML rustdoc is
//! published on [GitHub Pages](https://amsokol.github.io/scheda/scheda/).
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
//!   crate. Needed for fast in-process ANN; `search` is still `unsupported`
//!   until C9 wires it.
//! - **`openmp`** — USearch OpenMP executor (`usearch/openmp`). Needs
//!   libomp (Linux `libomp-dev`). Not enabled in the Bazel default build.

#![doc(html_root_url = "https://amsokol.github.io/scheda/scheda")]

mod ann;
mod error;
mod fnv;
mod git;
mod index;
mod journal;
mod query;
mod schema;
mod store;
mod vault;

#[doc(hidden)]
pub use ann::AnnIndex;
pub use error::Code;
#[doc(inline)]
pub use scheda_generated::api::v1::{
    Error, Filter, HistoryEntry, OpenOptions, Record, ReindexCounts,
};
pub use schema::{Kind, collection_dir};
pub use vault::{Collection, InsertOne, ReplaceOne, SearchHits, Vault};
