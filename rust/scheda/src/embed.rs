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
    /// Maximum number of texts to embed at once.
    ///
    /// Must be greater than 0. The library uses [`embed`](Self::embed) when
    /// there is one text or this is 1, and [`embed_many`](Self::embed_many)
    /// in chunks of this size otherwise.
    fn many_limit(&self) -> usize;
    /// Embed `text`. Length of the result must equal [`dimensions`](Self::dimensions).
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error>;
    /// Embed `texts`. One vector per text, each of length
    /// [`dimensions`](Self::dimensions).
    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error>;
}

/// Caller-supplied pair scorer. Not an `api/v1` field.
///
/// `score(query_text, record) →` higher is better. Scheda does not ship a
/// model. `None` on `search` → RRF order. The CLI may pass one via
/// `--reranker-cohere-url` (with `--reranker-model`) on `search`.
pub trait Reranker: Send + Sync {
    /// Maximum number of records to score at once.
    ///
    /// Must be greater than 0. The library uses [`score`](Self::score) when
    /// there is one Record or this is 1, and [`score_many`](Self::score_many)
    /// in chunks of this size otherwise.
    fn many_limit(&self) -> usize;
    /// Score one Record against `query`. Higher is better.
    fn score(&self, query: &str, record: &Record) -> Result<f64, Error>;
    /// Score `records` against `query`. One score per Record. Higher is better.
    fn score_many(&self, query: &str, records: &[&Record]) -> Result<Vec<f64>, Error>;
}

#[cfg(feature = "search")]
pub(crate) fn require_many_limit(limit: usize, what: &str) -> Result<usize, Error> {
    if limit == 0 {
        return Err(Error::invalid(format!(
            "{what} many_limit must be greater than 0"
        )));
    }
    Ok(limit)
}

#[cfg(feature = "search")]
pub(crate) fn embed_all(embedder: &dyn Embedder, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
    let limit = require_many_limit(embedder.many_limit(), "embedder")?;
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    if texts.len() == 1 || limit == 1 {
        return texts.iter().map(|text| embedder.embed(text)).collect();
    }
    let mut out = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(limit) {
        if chunk.len() == 1 {
            out.push(embedder.embed(chunk[0])?);
            continue;
        }
        let vectors = embedder.embed_many(chunk)?;
        if vectors.len() != chunk.len() {
            return Err(Error::invalid(
                "embedder embed_many must return one vector per text",
            ));
        }
        out.extend(vectors);
    }
    Ok(out)
}

#[cfg(feature = "search")]
pub(crate) fn score_all(
    reranker: &dyn Reranker,
    query: &str,
    records: &[&Record],
) -> Result<Vec<f64>, Error> {
    let limit = require_many_limit(reranker.many_limit(), "reranker")?;
    if records.is_empty() {
        return Ok(Vec::new());
    }
    if records.len() == 1 || limit == 1 {
        return records
            .iter()
            .map(|record| reranker.score(query, record))
            .collect();
    }
    let mut out = Vec::with_capacity(records.len());
    for chunk in records.chunks(limit) {
        if chunk.len() == 1 {
            out.push(reranker.score(query, chunk[0])?);
            continue;
        }
        let scores = reranker.score_many(query, chunk)?;
        if scores.len() != chunk.len() {
            return Err(Error::invalid(
                "reranker score_many must return one score per record",
            ));
        }
        out.extend(scores);
    }
    Ok(out)
}
