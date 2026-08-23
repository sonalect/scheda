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
    /// Create a Vault at `path` (`git init`, no Remote). LIB-2 / GIT-9.
    pub fn init(path: impl AsRef<Path>) -> Result<Self, Error> {
        let _path = path.as_ref();
        todo!("Vault::init is not implemented (C1)")
    }

    /// Clone `url` into `path`. LIB-2 name `clone` (GIT-9).
    pub fn clone(url: &str, path: impl AsRef<Path>) -> Result<Self, Error> {
        let _url = url;
        let _path = path.as_ref();
        todo!("Vault::clone is not implemented (C7)")
    }

    /// Open an existing Vault at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let _path = path.as_ref();
        todo!("Vault::open is not implemented (C1)")
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn collection(&self, name: &str) -> Result<Collection, Error> {
        let _name = name;
        todo!("Vault::collection is not implemented (C2)")
    }

    /// Create one git commit for changes already written to the working tree.
    pub fn commit(&self, message: &str) -> Result<(), Error> {
        let _message = message;
        todo!("Vault::commit is not implemented (C6)")
    }

    pub fn push_remote(&self) -> Result<(), Error> {
        todo!("Vault::push_remote is not implemented (C7)")
    }

    pub fn pull_remote(&self) -> Result<(), Error> {
        todo!("Vault::pull_remote is not implemented (C7)")
    }

    pub fn sync_remote(&self) -> Result<(), Error> {
        todo!("Vault::sync_remote is not implemented (C7)")
    }

    pub fn check(&self) -> Result<(), Error> {
        todo!("Vault::check is not implemented (C5)")
    }

    pub fn reindex(&self) -> Result<(), Error> {
        todo!("Vault::reindex is not implemented (C5)")
    }
}

impl Collection {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn insert_one(&self, document: Document) -> Result<InsertOne, Error> {
        let _document = document;
        todo!("Collection::insert_one is not implemented (C3)")
    }

    pub fn find_one(&self, id: &str) -> Result<Option<Document>, Error> {
        let _id = id;
        todo!("Collection::find_one is not implemented (C3)")
    }

    /// Replace a whole `document`. Errors on `journal`.
    pub fn replace_one(&self, id: &str, document: Document) -> Result<ReplaceOne, Error> {
        let _id = id;
        let _document = document;
        todo!("Collection::replace_one is not implemented (C3)")
    }

    pub fn find(&self, filter: &Filter) -> Result<Vec<Document>, Error> {
        let _filter = filter;
        todo!("Collection::find is not implemented (C5)")
    }

    /// `find` with an empty filter (LIB-10).
    pub fn scan(&self) -> Result<Vec<Document>, Error> {
        self.find(&Filter)
    }

    /// Hybrid retrieval. Not in **core** (LIB-11).
    pub fn search(&self, query: &str) -> Result<SearchHits, Error> {
        let _query = query;
        Err(Error::unsupported(
            "search is not implemented until the search phase",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::Collection;
    use crate::{Code, Error};

    #[test]
    fn search_is_unsupported_and_does_not_write() {
        let collection = Collection {
            name: String::from("notes"),
        };
        let err = collection
            .search("anything")
            .expect_err("search must fail in core");
        assert_eq!(err.code(), Code::Unsupported);
        assert_eq!(err.code().as_str(), "unsupported");
    }

    #[test]
    fn unsupported_code_is_not_only_the_message() {
        let err = Error::unsupported("replace_one on journal");
        assert_eq!(err.code(), Code::Unsupported);
        assert_ne!(err.to_string(), "unsupported");
    }
}
