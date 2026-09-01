//! Caller-supplied embedder and reranker.
//!
//! Handles live in [`crate::vault`]. Traits are not `api/v1` fields.

use std::num::NonZeroUsize;

use crate::{Error, Record};

/// Caller-supplied embedding. Not an `api/v1` field.
///
/// `embed_many(texts) → vectors`. Scheda does not ship a model. No embedder →
/// vector leg off; `find`, BM25, and writes still work. The CLI may pass one
/// via `--embedder-openai-url` (with `--embedder-model` and
/// `--embedder-dimensions`) or `--model2vec-embedder`.
pub trait Embedder: Send + Sync {
    /// Model id stored with vectors so a later embedder can detect mismatch.
    fn model_id(&self) -> &str;
    /// Length of each vector. Must be stable for the life of the Index.
    ///
    /// Zero is not representable. Absence of a vector leg is `None` for the
    /// caller-supplied embedder, not a zero dimension.
    fn dimensions(&self) -> NonZeroUsize;
    /// Maximum number of texts to embed at once.
    ///
    /// The library uses [`embed`](Self::embed) when there is one text or this
    /// is 1, and [`embed_many`](Self::embed_many) in chunks of this size
    /// otherwise.
    fn many_limit(&self) -> NonZeroUsize;
    /// Embed `text`. Length of the result must equal [`dimensions`](Self::dimensions).
    ///
    /// Provided: calls [`embed_many`](Self::embed_many) with one element and
    /// takes that vector. Fails with `invalid` if the batch result is not
    /// exactly one vector. The required method is
    /// [`embed_many`](Self::embed_many). An override must stay consistent with
    /// the batch path: the same text must yield the same vector whether it
    /// travelled alone or in a padded batch, or the vector space splits with
    /// no error.
    fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        let vectors = self.embed_many(&[text])?;
        let mut iter = vectors.into_iter();
        match (iter.next(), iter.next()) {
            (Some(vector), None) => Ok(vector),
            _ => Err(Error::invalid(
                "embedder must return one vector for a single text",
            )),
        }
    }
    /// Embed `texts`. One vector per text, each of length
    /// [`dimensions`](Self::dimensions).
    ///
    /// Required. The library calls this for every batch larger than one
    /// (chunks of [`many_limit`](Self::many_limit)). The default
    /// [`embed`](Self::embed) also calls this with a one-element slice.
    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error>;
}

/// Caller-supplied pair scorer. Not an `api/v1` field.
///
/// `score_many(query_text, records) →` higher is better. Scheda does not ship
/// a model. `None` on `search` → RRF order. The CLI may pass one via
/// `--reranker-cohere-url` (with `--reranker-model`) on `search`.
pub trait Reranker: Send + Sync {
    /// Maximum number of records to score at once.
    ///
    /// The library uses [`score`](Self::score) when there is one Record or
    /// this is 1, and [`score_many`](Self::score_many) in chunks of this size
    /// otherwise.
    fn many_limit(&self) -> NonZeroUsize;
    /// Score one Record against `query`. Higher is better.
    ///
    /// Provided: calls [`score_many`](Self::score_many) with one Record and
    /// takes that score. Fails with `invalid` if the batch result is not
    /// exactly one score. The required method is
    /// [`score_many`](Self::score_many). An override must stay consistent with
    /// the batch path: the same Record must yield the same score whether it
    /// travelled alone or in a padded batch, or ranking splits with no error.
    fn score(&self, query: &str, record: &Record) -> Result<f64, Error> {
        let scores = self.score_many(query, &[record])?;
        let mut iter = scores.into_iter();
        match (iter.next(), iter.next()) {
            (Some(score), None) => Ok(score),
            _ => Err(Error::invalid(
                "reranker must return one score for a single record",
            )),
        }
    }
    /// Score `records` against `query`. One score per Record. Higher is better.
    ///
    /// Required. The library calls this for every batch larger than one
    /// (chunks of [`many_limit`](Self::many_limit)). The default
    /// [`score`](Self::score) also calls this with a one-element slice.
    fn score_many(&self, query: &str, records: &[&Record]) -> Result<Vec<f64>, Error>;
}

#[cfg(feature = "search")]
pub(crate) fn embed_all(embedder: &dyn Embedder, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
    let limit = embedder.many_limit().get();
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
            return Err(Error::invalid("embedder must return one vector per text"));
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
    let limit = reranker.many_limit().get();
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
            return Err(Error::invalid("reranker must return one score per record"));
        }
        out.extend(scores);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const FOUR: NonZeroUsize = match NonZeroUsize::new(4) {
        Some(n) => n,
        None => unreachable!(),
    };
    const THIRTY_TWO: NonZeroUsize = match NonZeroUsize::new(32) {
        Some(n) => n,
        None => unreachable!(),
    };

    struct OnlyManyEmbedder {
        many_calls: AtomicU64,
    }

    impl Embedder for OnlyManyEmbedder {
        fn model_id(&self) -> &str {
            "only-many"
        }

        fn dimensions(&self) -> NonZeroUsize {
            FOUR
        }

        fn many_limit(&self) -> NonZeroUsize {
            THIRTY_TWO
        }

        fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
            self.many_calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![vec![1.0, 0.0, 0.0, 0.0]; texts.len()])
        }
    }

    #[test]
    fn embed_default_delegates_to_embed_many() {
        let embedder = OnlyManyEmbedder {
            many_calls: AtomicU64::new(0),
        };
        let vector = embedder.embed("hello").unwrap();
        assert_eq!(vector, vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(embedder.many_calls.load(Ordering::Relaxed), 1);
    }

    struct OnlyManyReranker {
        many_calls: AtomicU64,
    }

    impl Reranker for OnlyManyReranker {
        fn many_limit(&self) -> NonZeroUsize {
            THIRTY_TWO
        }

        fn score_many(&self, _query: &str, records: &[&Record]) -> Result<Vec<f64>, Error> {
            self.many_calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![1.0; records.len()])
        }
    }

    #[test]
    fn score_default_delegates_to_score_many() {
        let reranker = OnlyManyReranker {
            many_calls: AtomicU64::new(0),
        };
        let record = Record::default();
        let score = reranker.score("q", &record).unwrap();
        assert_eq!(score, 1.0);
        assert_eq!(reranker.many_calls.load(Ordering::Relaxed), 1);
    }
}
