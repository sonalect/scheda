//! Caller-supplied embedder and reranker.
//!
//! Handles live in [`crate::vault`]. Traits are not `api/v1` fields.

use crate::{Error, Record};

/// Caller-supplied embedding. Not an `api/v1` field.
///
/// `embed(text) → vector`. Scheda does not ship a model. No embedder → vector
/// leg off; `find`, BM25, and writes still work. The CLI may pass one via
/// `--embedder-openai-url` (with `--embedder-model` and
/// `--embedder-dimensions`) or `--model2vec-embedder`.
pub trait Embedder: Send + Sync {
    /// Model id stored with vectors so a later embedder can detect mismatch.
    fn model_id(&self) -> &str;
    /// Length of each vector. Must be stable for the life of the Index.
    fn dimensions(&self) -> usize;
    /// Embed `text`. Length of the result must equal [`dimensions`](Self::dimensions).
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error>;
}

/// Caller-supplied pair scorer. Not an `api/v1` field.
///
/// `score(query_text, record) →` higher is better. Scheda does not ship a
/// model. `None` on `search` → RRF order. The CLI may pass one via
/// `--reranker-cohere-url` (with `--reranker-model`) on `search`.
pub trait Reranker: Send + Sync {
    /// Score one Record against `query`. Higher is better.
    fn score(&self, query: &str, record: &Record) -> Result<f64, Error>;

    /// Default: score each record. HTTP rerankers override to batch.
    fn score_many(&self, query: &str, records: &[Record]) -> Result<Vec<f64>, Error> {
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            out.push(self.score(query, record)?);
        }
        Ok(out)
    }
}
