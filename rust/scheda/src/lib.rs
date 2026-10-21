//! Scheda: a git-backed document store.
//!
//! [`Scheda`] opens a Vault and returns a reference to it
//! ([`v1::vault_ref::VaultRef`]); the Vault gives its collections and its
//! mirror the same way. The objects are async, on the tokio runtime of the
//! caller: a caller takes the async form of an object from its reference
//! (`to_async_inline`) and calls the traits of the contract
//! ([`v1::VaultAsync`], [`v1::CollectionAsync`], …). A caller without
//! async code takes the blocking form (`to_sync`) inside a multi-thread
//! runtime.
//!
//! A build with the feature `index` (the default) keeps an Index of the
//! Vault on Lance in `.scheda/index/`: queries, counts, and searches over
//! the Documents, `Vault.Index`, and the ports (a Projector, an embedder, a
//! reranker) that tell what the Index holds. An indexer task of each open
//! Vault keeps the Index at the state of `HEAD`. A build without the
//! feature has no Index: `Vault.Index`, the queries, the counts, the
//! searches, and every port, given at open or to a `Set*` call, answer
//! `UNIMPLEMENTED`; it keeps the id maps of the journals only, so a line of
//! a journal is still found by its id. The feature `encrypt` adds encrypted
//! collections; the Index holds none of them.

mod check;
mod collection;
mod collections;
mod commit;
#[cfg(feature = "encrypt")]
mod crypto;
mod document_calls;
mod documents;
mod error;
mod fallback;
#[cfg(feature = "index")]
mod filter;
mod fs;
mod gate;
#[cfg(feature = "index")]
mod generation;
mod git;
mod id;
mod idmap;
mod index;
mod indexer;
mod isolated;
mod journal;
mod layout;
mod marker;
mod mirror;
mod pack;
mod ports;
#[cfg(feature = "index")]
mod queries;
mod reads;
mod repo;
#[cfg(feature = "index")]
mod search;
mod service;
mod sha1;
#[cfg(feature = "index")]
mod store;
mod stored;
#[cfg(feature = "index")]
mod tables;
#[cfg(test)]
mod testing;
mod vault;
#[cfg(feature = "index")]
mod vectors;
mod writer;

#[cfg(feature = "encrypt")]
pub use crypto::parse_age_identity;
pub use error::{Context, Error, Fault, error_report, redact_remote_text};
pub use ports::{FixedEmbedder, FixedEmbedderAsync, FixedReranker, FixedRerankerAsync};
pub use scheda_api::v1;
pub use service::Scheda;
pub use stored::parse_command;
