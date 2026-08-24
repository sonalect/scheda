//! In-process USearch HNSW (IDX-7). Not wired to `search` yet.

use std::path::Path;

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::Error;

/// Approximate nearest-neighbour index over `f32` cosine vectors.
///
/// Keys are `u64`. C9 will map `(collection, id)` onto them. This type is
/// `#[doc(hidden)]` until `search` uses it; it is not a LIB-2 name.
#[doc(hidden)]
pub struct AnnIndex {
    inner: Index,
}

impl std::fmt::Debug for AnnIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnIndex")
            .field("dimensions", &self.inner.dimensions())
            .field("len", &self.inner.size())
            .finish()
    }
}

impl AnnIndex {
    /// Empty cosine `f32` index with the given vector size.
    pub fn new(dimensions: usize) -> Result<Self, Error> {
        if dimensions == 0 {
            return Err(Error::invalid("vector dimensions must be > 0"));
        }
        let options = IndexOptions {
            dimensions,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..IndexOptions::default()
        };
        let inner = Index::new(&options).map_err(ann_err)?;
        inner.reserve(16).map_err(ann_err)?;
        Ok(Self { inner })
    }

    /// Open a dump written by [`save`](Self::save).
    pub fn restore(path: &Path) -> Result<Self, Error> {
        let inner = Index::restore(path_str(path)?).map_err(ann_err)?;
        Ok(Self { inner })
    }

    pub fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    pub fn len(&self) -> usize {
        self.inner.size()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn add(&self, key: u64, vector: &[f32]) -> Result<(), Error> {
        if vector.len() != self.inner.dimensions() {
            return Err(Error::invalid(format!(
                "vector length {} does not match dimensions {}",
                vector.len(),
                self.inner.dimensions()
            )));
        }
        self.ensure_capacity()?;
        self.inner.add(key, vector).map_err(ann_err)
    }

    pub fn contains(&self, key: u64) -> bool {
        self.inner.contains(key)
    }

    pub fn remove(&self, key: u64) -> Result<(), Error> {
        self.inner.remove(key).map(|_| ()).map_err(ann_err)
    }

    /// k nearest keys and cosine distances (lower is closer).
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>, Error> {
        if query.len() != self.inner.dimensions() {
            return Err(Error::invalid(format!(
                "query length {} does not match dimensions {}",
                query.len(),
                self.inner.dimensions()
            )));
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        let hits = self.inner.search(query, k).map_err(ann_err)?;
        Ok(hits.keys.into_iter().zip(hits.distances).collect())
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        self.inner.save(path_str(path)?).map_err(ann_err)
    }

    fn ensure_capacity(&self) -> Result<(), Error> {
        let size = self.inner.size();
        let cap = self.inner.capacity();
        if size < cap {
            return Ok(());
        }
        let next = cap.saturating_mul(2).max(size.saturating_add(16));
        self.inner.reserve(next).map_err(ann_err)
    }
}

fn path_str(path: &Path) -> Result<&str, Error> {
    path.to_str().ok_or_else(|| {
        Error::io(format!(
            "vector index path is not UTF-8: {}",
            path.display()
        ))
    })
}

fn ann_err(err: impl std::fmt::Display) -> Error {
    Error::index_unusable(err.to_string())
}
