//! Scheda: a git-backed document store.
//!
//! The crate is being rebuilt layer by layer on the protobuf contract
//! (`scheda-api`), as `docs/20-proto-contract.md` §4 describes. A module
//! is declared here when its layer is finished; the rest stays in the tree
//! as material for its layer.

#[macro_use]
mod error;

mod collection;
mod fs;
mod git;
mod id;
mod marker;
mod stored;
mod vault;
mod writer;

pub use error::{Context, Error, Fault, redact_remote_text};
pub use scheda_api::v1;
pub use vault::Vault;
