use std::path::{Path, PathBuf};

use crate::Error;

/// A git working tree that holds collections of records.
#[derive(Debug)]
pub struct Vault {
    path: PathBuf,
}

/// A named set of records with one write mode (`document`, `journal`, or `singleton`).
#[derive(Debug)]
pub struct Collection {
    name: String,
}

/// One collection record. Fields and body are not modelled yet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Document {
    pub id: String,
}

/// Field filter for `find`. Shape is not modelled yet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Filter;

/// Result of inserting a record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InsertOne {
    pub id: String,
}

/// Result of replacing a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplaceOne {
    pub id: String,
}

/// Hybrid search hits. Ranking is not modelled yet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchHits;

impl Vault {
    /// Open an existing Vault at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let _path = path.as_ref();
        todo!("Vault::open is not implemented")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn collection(&self, name: &str) -> Result<Collection, Error> {
        let _name = name;
        todo!("Vault::collection is not implemented")
    }

    /// Create one git commit for changes already written to the working tree.
    pub fn commit(&self, message: &str) -> Result<(), Error> {
        let _message = message;
        todo!("Vault::commit is not implemented")
    }
}

impl Collection {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn insert_one(&self, document: Document) -> Result<InsertOne, Error> {
        let _document = document;
        todo!("Collection::insert_one is not implemented")
    }

    pub fn find_one(&self, id: &str) -> Result<Option<Document>, Error> {
        let _id = id;
        todo!("Collection::find_one is not implemented")
    }

    /// Replace a whole `document`. Errors on `journal`.
    pub fn replace_one(&self, id: &str, document: Document) -> Result<ReplaceOne, Error> {
        let _id = id;
        let _document = document;
        todo!("Collection::replace_one is not implemented")
    }

    pub fn find(&self, filter: &Filter) -> Result<Vec<Document>, Error> {
        let _filter = filter;
        todo!("Collection::find is not implemented")
    }

    pub fn search(&self, query: &str) -> Result<SearchHits, Error> {
        let _query = query;
        todo!("Collection::search is not implemented")
    }
}
