//! In-process USearch HNSW.

use std::path::Path;

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::Error;
use crate::error::{Code, Context, ContextAs, ensure};

/// Approximate nearest-neighbour index over `f32` cosine vectors.
///
/// Keys are `u64` hashes of `(collection, id)`. Hidden from rustdoc; not a
/// library operation name.
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
    const INITIAL_CAPACITY: usize = 16;

    /// Empty cosine `f32` index with the given vector size.
    pub fn new(dimensions: usize) -> Result<Self, Error> {
        ensure!(
            dimensions > 0,
            Error::invalid("vector dimensions must be > 0").context("[AnnIndex::new] dimensions")
        );
        let options = IndexOptions {
            dimensions,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..IndexOptions::default()
        };
        let inner =
            Index::new(&options).context(Code::IndexVector, "[AnnIndex::new] create new index")?;
        inner
            .reserve(Self::INITIAL_CAPACITY)
            .with_context(Code::IndexVector, || {
                format!(
                    "[AnnIndex::new] reserve capacity {} vectors",
                    Self::INITIAL_CAPACITY
                )
            })?;
        Ok(Self { inner })
    }

    /// Open a dump written by [`save`](Self::save).
    pub fn restore(path: &Path) -> Result<Self, Error> {
        let inner =
            Index::restore(path_str(path).context("[AnnIndex::restore] path must be UTF-8")?)
                .context(Code::IndexVector, "[AnnIndex::restore] open dump")?;
        Ok(Self { inner })
    }

    /// Vector length this index was created with.
    pub fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    /// Number of stored keys.
    pub fn len(&self) -> usize {
        self.inner.size()
    }

    /// True when no keys are stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or replace `key`. `vector` length must match [`dimensions`](Self::dimensions).
    pub fn add(&self, key: u64, vector: &[f32]) -> Result<(), Error> {
        ensure!(
            vector.len() == self.inner.dimensions(),
            Error::invalid(format!(
                "vector length {} does not match dimensions {}",
                vector.len(),
                self.inner.dimensions()
            ))
            .context("[AnnIndex::add] vector length")
        );
        self.ensure_capacity()?;
        self.inner
            .add(key, vector)
            .context(Code::IndexVector, "[AnnIndex::add] insert vector")
    }

    /// Stored vector for `key`, or `None` if the key is absent.
    pub fn get(&self, key: u64) -> Result<Option<Vec<f32>>, Error> {
        if !self.inner.contains(key) {
            return Ok(None);
        }
        let mut vector = Vec::new();
        let n = self
            .inner
            .export(key, &mut vector)
            .context(Code::IndexVector, "[AnnIndex::get] export vector")?;
        if n == 0 {
            return Ok(None);
        }
        let dim = self.inner.dimensions();
        vector.truncate(dim);
        Ok(Some(vector))
    }

    /// Whether `key` is in the index.
    pub fn contains(&self, key: u64) -> bool {
        self.inner.contains(key)
    }

    /// Delete `key` if present.
    pub fn remove(&self, key: u64) -> Result<(), Error> {
        self.inner
            .remove(key)
            .map(|_| ())
            .with_context(Code::IndexVector, || {
                format!("[AnnIndex::remove] delete key {key}")
            })
    }

    /// k nearest keys and cosine distances (lower is closer).
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>, Error> {
        ensure!(
            query.len() == self.inner.dimensions(),
            Error::invalid(format!(
                "query length {} does not match dimensions {}",
                query.len(),
                self.inner.dimensions()
            ))
            .context("[AnnIndex::search] query length")
        );
        if k == 0 {
            return Ok(Vec::new());
        }
        let hits = self
            .inner
            .search(query, k)
            .with_context(Code::IndexVector, || {
                format!("[AnnIndex::search] query k={k}")
            })?;
        Ok(hits.keys.into_iter().zip(hits.distances).collect())
    }

    /// Like [`search`](Self::search), keeping only keys for which `predicate` is true.
    pub fn filtered_search(
        &self,
        query: &[f32],
        k: usize,
        predicate: impl Fn(u64) -> bool,
    ) -> Result<Vec<(u64, f32)>, Error> {
        ensure!(
            query.len() == self.inner.dimensions(),
            Error::invalid(format!(
                "query length {} does not match dimensions {}",
                query.len(),
                self.inner.dimensions()
            ))
            .context("[AnnIndex::filtered_search] query length")
        );
        if k == 0 {
            return Ok(Vec::new());
        }
        let hits = self
            .inner
            .filtered_search(query, k, predicate)
            .with_context(Code::IndexVector, || {
                format!("[AnnIndex::filtered_search] query k={k}")
            })?;
        Ok(hits.keys.into_iter().zip(hits.distances).collect())
    }

    /// Write a dump that [`restore`](Self::restore) can open.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        self.inner
            .save(path_str(path).context("[AnnIndex::save] path must be UTF-8")?)
            .context(Code::IndexVector, "[AnnIndex::save] write dump")
    }

    fn ensure_capacity(&self) -> Result<(), Error> {
        let size = self.inner.size();
        let cap = self.inner.capacity();
        if size < cap {
            return Ok(());
        }
        let next = cap
            .saturating_mul(2)
            .max(size.saturating_add(Self::INITIAL_CAPACITY));
        self.inner
            .reserve(next)
            .with_context(Code::IndexVector, || {
                format!("[AnnIndex::add] reserve capacity {next}")
            })
    }
}

fn path_str(path: &Path) -> Result<&str, Error> {
    path.to_str().ok_or_else(|| {
        Error::not_utf8(format!(
            "vector index path is not UTF-8: {}",
            path.display()
        ))
    })
}
