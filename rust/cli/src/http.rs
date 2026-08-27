//! HTTP embedder / reranker (`14`): OpenAI embeddings and Cohere/Jina rerank.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use scheda::{Embedder, Error, Record, Reranker};
use serde_json::{Value, json};

const EMBEDDINGS_LEAF: &str = "embeddings";
const RERANK_LEAF: &str = "rerank";

pub fn load_embedder(
    url: &str,
    model: &str,
    dimensions: usize,
    key_env: Option<&str>,
) -> Result<Arc<dyn Embedder>, Error> {
    let model = require_model(model)?;
    if dimensions == 0 {
        return Err(Error::invalid(
            "--embedder-dimensions must be a positive integer",
        ));
    }
    let url = join_url(url, EMBEDDINGS_LEAF)?;
    let model_id = format!("{url} {model}");
    Ok(Arc::new(OpenAiEmbedder {
        url,
        model,
        model_id,
        key: env_bearer(key_env)?,
        dimensions,
    }))
}

pub fn load_reranker(
    url: &str,
    model: &str,
    key_env: Option<&str>,
) -> Result<Arc<dyn Reranker>, Error> {
    Ok(Arc::new(CohereReranker {
        url: join_url(url, RERANK_LEAF)?,
        model: require_model(model)?,
        key: env_bearer(key_env)?,
    }))
}

fn require_model(model: &str) -> Result<String, Error> {
    let model = model.trim();
    if model.is_empty() {
        return Err(Error::invalid("embedder/reranker model is empty"));
    }
    Ok(model.to_owned())
}

fn join_url(base: &str, leaf: &str) -> Result<String, Error> {
    let url = base.trim();
    if url.is_empty() {
        return Err(Error::invalid("embedder/reranker URL is empty"));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(Error::invalid(
            "embedder/reranker URL must be http or https",
        ));
    }
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (url, None),
    };
    let path = path.trim_end_matches('/');
    let mut joined = format!("{path}/{leaf}");
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        joined.push('?');
        joined.push_str(query);
    }
    Ok(joined)
}

fn env_bearer(name: Option<&str>) -> Result<Option<String>, Error> {
    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(None);
    };
    match env::var(name) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(_) => Err(Error::invalid(format!(
            "environment variable {name} is not set or empty"
        ))),
    }
}

struct OpenAiEmbedder {
    url: String,
    model: String,
    model_id: String,
    key: Option<String>,
    dimensions: usize,
}

impl Embedder for OpenAiEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        let body = json!({
            "model": self.model,
            "input": text,
        });
        let value = post_json(
            &self.url,
            self.key.as_deref(),
            body,
            "[OpenAiEmbedder::embed] POST embeddings",
        )?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .ok_or_else(|| Error::invalid("embeddings response has no data"))?;
        let embedding = data
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::invalid("embeddings response has no embedding"))?;
        let mut vector = Vec::with_capacity(embedding.len());
        for item in embedding {
            let Some(n) = item.as_f64() else {
                return Err(Error::invalid("embeddings response is not float32"));
            };
            vector.push(n as f32);
        }
        if vector.is_empty() {
            return Err(Error::invalid("embeddings response is empty"));
        }
        if vector.len() != self.dimensions {
            return Err(Error::invalid(
                "embedding length does not match --embedder-dimensions",
            ));
        }
        Ok(vector)
    }
}

struct CohereReranker {
    url: String,
    model: String,
    key: Option<String>,
}

impl Reranker for CohereReranker {
    fn score(&self, query: &str, record: &Record) -> Result<f64, Error> {
        let document = document_text(record)?;
        let body = json!({
            "model": self.model,
            "query": query,
            "documents": [document],
        });
        let value = post_json(
            &self.url,
            self.key.as_deref(),
            body,
            "[CohereReranker::score] POST rerank",
        )?;
        let results = value
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::invalid("rerank response has no results"))?;
        let score = results
            .first()
            .and_then(|hit| hit.get("relevance_score"))
            .and_then(Value::as_f64)
            .ok_or_else(|| Error::invalid("rerank response has no relevance_score"))?;
        Ok(score)
    }
}

fn document_text(record: &Record) -> Result<String, Error> {
    serde_json::to_string(record).map_err(|err| Error::invalid(err.to_string()))
}

fn post_json(url: &str, key: Option<&str>, body: Value, frame: &str) -> Result<Value, Error> {
    let mut request = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
        .post(url);
    request = request.header("Content-Type", "application/json");
    if let Some(key) = key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }
    let mut response = request
        .send_json(&body)
        .map_err(|err| map_ureq(err, frame))?;
    let status = response.status();
    let value: Value = response
        .body_mut()
        .read_json()
        .map_err(|err| map_ureq(err, frame))?;
    if !status.is_success() {
        return Err(Error::invalid(format!("HTTP {status} from {url}")).context(frame));
    }
    Ok(value)
}

fn map_ureq(err: ureq::Error, frame: &str) -> Error {
    let msg = err.to_string();
    let lowered = msg.to_ascii_lowercase();
    let error = if lowered.contains("connection refused") {
        Error::io_connection_refused(msg)
    } else if lowered.contains("timed out") || lowered.contains("timeout") {
        Error::io_timed_out(msg)
    } else {
        Error::io_other(msg)
    };
    error.context(frame)
}
