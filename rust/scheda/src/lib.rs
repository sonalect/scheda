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
//! This version of Scheda has no Index to query or search: `Vault.Index`,
//! the queries, the counts, and the searches answer `UNIMPLEMENTED`, and so
//! does a port (an embedder, a reranker, a Projector) given at open or to a
//! `Set*` call.

mod check;
mod collection;
mod collections;
mod commit;
#[cfg(feature = "encrypt")]
mod crypto;
mod document_calls;
mod documents;
mod error;
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
mod marker;
mod mirror;
mod ports;
#[cfg(feature = "index")]
mod queries;
mod reads;
mod repo;
#[cfg(feature = "index")]
mod search;
mod service;
#[cfg(feature = "index")]
mod store;
mod stored;
#[cfg(feature = "index")]
mod tables;
#[cfg(test)]
mod testing;
mod vault;
mod writer;

#[cfg(feature = "encrypt")]
pub use crypto::parse_age_identity;
pub use error::{Context, Error, Fault, error_report, redact_remote_text};
pub use ports::{FixedEmbedder, FixedReranker};
pub use scheda_api::v1;
pub use service::Scheda;
pub use stored::parse_command;
