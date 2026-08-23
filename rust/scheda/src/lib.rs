//! Git-backed document store.
//!
//! Records live as text files in a **Vault** (a git working tree). The search
//! index is derived and lives outside git. Scheda does not know a consumer's types.

mod error;
mod vault;

pub use error::{Code, Error};
pub use vault::{Collection, Document, Filter, InsertOne, ReplaceOne, SearchHits, Vault};
