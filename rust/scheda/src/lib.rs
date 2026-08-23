//! Git-backed document store.
//!
//! Records live as text files in a **Vault** (a git working tree). The search
//! index is derived and lives outside git. Scheda does not know a consumer's types.

mod error;
mod git;
mod vault;

pub use error::Code;
pub use scheda_generated::api::v1::{Error, OpenOptions, Record};
pub use vault::{Collection, Filter, InsertOne, ReplaceOne, SearchHits, Vault};
