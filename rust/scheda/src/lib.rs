//! Scheda: a git-backed document store.
//!
//! [`Scheda`] opens a Vault and returns a reference to it
//! ([`v1::vault_ref::VaultRef`]); the Vault gives its collections, its
//! mirror, and its Index the same way. A caller takes the form of an object
//! it needs from its reference (`to_sync`, or with the `tokio` feature an
//! async form) and calls the traits of the contract ([`v1::VaultSync`],
//! [`v1::CollectionSync`], …). The caller's ports (an embedder, a reranker,
//! a Projector) are given the same way, as references.

mod check;
mod collection;
mod collections;
mod commit;
#[cfg(feature = "encrypt")]
mod crypto;
mod document_calls;
mod documents;
mod error;
#[cfg(feature = "find")]
mod filter;
mod fs;
mod git;
mod id;
mod idmap;
mod index;
mod indexer;
mod isolated;
mod journal;
#[cfg(feature = "find")]
mod lexical;
mod marker;
mod mirror;
mod ports;
#[cfg(feature = "find")]
mod queries;
mod reads;
mod repo;
#[cfg(feature = "search")]
mod search;
mod service;
mod stored;
mod vault;
#[cfg(feature = "search")]
mod vector;
#[cfg(feature = "find")]
mod who;
mod writer;

#[cfg(feature = "encrypt")]
pub use crypto::parse_age_identity;
pub use error::{Context, Error, Fault, error_report, redact_remote_text};
pub use ports::{FixedEmbedder, FixedReranker};
pub use scheda_api::v1;
pub use service::Scheda;
pub use stored::parse_command;
