//! The integration tests of Scheda: the objects of the contract against a
//! Vault on disk, as a caller uses them. One test binary, so the library is
//! linked once; each module is one surface.

mod common;
mod probe;

mod collections;
mod consumer;
mod documents;
mod index;
mod mirror;
mod objects;
mod open;
mod pack;
mod ports;
mod query;
mod reads;
mod runtime;
mod search;
mod texts;
mod write;
