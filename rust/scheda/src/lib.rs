//! Scheda: a git-backed document store.
//!
//! [`Scheda`] opens a Vault and returns a reference to it
//! ([`v1::vault_ref::VaultRef`]); the Vault gives its collections and its
//! mirror the same way. A caller takes the form of an object it needs from
//! its reference (`to_sync`, or an async form on a Tokio runtime) and
//! calls the traits of the contract ([`v1::VaultSync`],
//! [`v1::CollectionSync`], …).
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
mod fs;
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
mod reads;
mod repo;
mod service;
mod stored;
mod vault;
mod writer;

#[cfg(feature = "encrypt")]
pub use crypto::parse_age_identity;
pub use error::{Context, Error, Fault, error_report, redact_remote_text};
pub use ports::{FixedEmbedder, FixedReranker};
pub use scheda_api::v1;
pub use service::Scheda;
pub use stored::parse_command;
