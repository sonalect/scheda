//! Scheda: a git-backed document store.
//!
//! The crate is being rebuilt layer by layer on the protobuf contract
//! (`scheda-api`), as `docs/20-proto-contract.md` §4 describes. A module
//! is declared here when its layer is finished; the rest stays in the tree
//! as material for its layer.

#[macro_use]
mod error;

mod check;
mod collection;
mod collections;
mod commit;
#[cfg(feature = "encrypt")]
mod crypto;
mod document_calls;
mod documents;
mod fs;
mod git;
mod id;
mod idmap;
mod index;
mod index_calls;
mod indexer;
mod isolated;
mod journal;
mod marker;
mod mirror;
mod ports;
mod repo;
mod stored;
mod vault;
mod writer;

#[cfg(feature = "encrypt")]
pub use crypto::parse_age_identity;
pub use error::{Context, Error, Fault, redact_remote_text};
pub use ports::{FixedEmbedder, FixedReranker, Port, Ports};
pub use scheda_api::v1;
pub use vault::Vault;
