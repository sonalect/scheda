//! Git-backed document store.
//!
//! Records live as text files in a **Vault** (a git working tree). The search
//! index is derived and lives outside git. Scheda does not know a consumer's types.
//!
//! This crate is the Rust binding of the [design
//! package](https://github.com/amsokol/scheda/tree/main/docs). HTML rustdoc is
//! published on [GitHub Pages](https://amsokol.github.io/scheda/scheda/).

#![doc(html_root_url = "https://amsokol.github.io/scheda/scheda")]

mod error;
mod git;
mod schema;
mod vault;

pub use error::Code;
#[doc(inline)]
pub use scheda_generated::api::v1::{Error, OpenOptions, Record};
pub use schema::Kind;
pub use vault::{Collection, Filter, InsertOne, ReplaceOne, SearchHits, Vault};
