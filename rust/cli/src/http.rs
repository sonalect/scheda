//! The HTTP ports: an OpenAI-style embedder (`POST …/embeddings`) and a
//! Cohere or Jina style reranker (`POST …/rerank`), built on the port
//! adapters of the library.

use std::env;
use std::num::NonZeroUsize;
use std::time::Duration;

use protocontract::{ErrorCode, Failure};
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::reranker_ref::RerankerRef;
use scheda::v1::{EmbedderErrorCode, RerankerErrorCode};
use scheda::{Error, FixedEmbedder, FixedReranker, redact_remote_text};
use serde_json::{Value, json};

use crate::stdio::invalid;

const EMBEDDINGS_LEAF: &str = "embeddings";
const RERANK_LEAF: &str = "rerank";

/// How many texts one embeddings request carries at most.
const EMBED_PORTION: NonZeroUsize = match NonZeroUsize::new(2048) {
    Some(n) => n,
    None => unreachable!(),
};

/// How many Documents one rerank request carries at most.
const RERANK_PORTION: NonZeroUsize = match NonZeroUsize::new(1000) {
    Some(n) => n,
    None => unreachable!(),
};

/// How often a request a signal interrupted (`EINTR`) is sent again.
const INTERRUPTED_ATTEMPTS: u32 = 3;

/// The embedder at `url`, as a reference for `VaultOpening`. Its model id
/// is `model` exactly: the URL may carry userinfo or a key in its query,
/// and a host change is not a model change.
///
/// # Errors
///
/// `INVALID_ARGUMENT` for an empty model, a URL that is not http or https,
/// and a key variable that is unset or empty.
pub(crate) fn embedder(
    url: &str,
    model: &str,
    dimensions: u32,
    key_env: Option<&str>,
) -> Result<EmbedderRef, Error> {
    let model = require_model(model)?;
    let post = Post::new(join_url(url, EMBEDDINGS_LEAF)?, env_bearer(key_env)?);
    let length = usize::try_from(dimensions).unwrap_or(usize::MAX);
    let sent_model = model.clone();
    Ok(EmbedderRef::from_sync(FixedEmbedder::new(
        model,
        dimensions,
        EMBED_PORTION,
        move |texts: Vec<String>| {
            let input = match texts.as_slice() {
                [one] => json!(one),
                many => json!(many),
            };
            let body = json!({"model": sent_model, "input": input});
            post.send(&body)
                .and_then(|value| parse_embeddings(&value, texts.len(), length))
                .map_err(|problem| {
                    problem.into_error(
                        EmbedderErrorCode::EMBEDDER_ERROR_CODE_UNAVAILABLE,
                        EmbedderErrorCode::EMBEDDER_ERROR_CODE_REFUSED,
                    )
                })
        },
    )))
}

/// The reranker at `url`, as a reference for `VaultOpening`.
///
/// # Errors
///
/// As [`embedder`].
pub(crate) fn reranker(
    url: &str,
    model: &str,
    key_env: Option<&str>,
) -> Result<RerankerRef, Error> {
    let model = require_model(model)?;
    let post = Post::new(join_url(url, RERANK_LEAF)?, env_bearer(key_env)?);
    Ok(RerankerRef::from_sync(FixedReranker::new(
        RERANK_PORTION,
        move |query: &str, documents: Vec<scheda::v1::Document>| {
            let texts: Vec<String> = documents
                .iter()
                .map(|document| {
                    String::from_utf8_lossy(document.raw.as_deref().unwrap_or_default())
                        .into_owned()
                })
                .collect();
            let body = json!({"model": model, "query": query, "documents": texts});
            post.send(&body)
                .and_then(|value| parse_scores(&value, texts.len()))
                .map_err(|problem| {
                    problem.into_error(
                        RerankerErrorCode::RERANKER_ERROR_CODE_UNAVAILABLE,
                        RerankerErrorCode::RERANKER_ERROR_CODE_REFUSED,
                    )
                })
        },
    )))
}

fn require_model(model: &str) -> Result<String, Error> {
    let model = model.trim();
    if model.is_empty() {
        return Err(invalid("embedder/reranker model is empty"));
    }
    Ok(model.to_owned())
}

fn join_url(base: &str, leaf: &str) -> Result<String, Error> {
    let url = base.trim();
    if url.is_empty() {
        return Err(invalid("embedder/reranker URL is empty"));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(invalid("embedder/reranker URL must be http or https"));
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
        Ok(_) | Err(_) => Err(invalid(format!(
            "environment variable {name} is not set or empty"
        ))),
    }
}

/// Why a call to the service failed, before it is given the code of the
/// port that made it.
#[derive(Debug)]
enum Problem {
    /// The service cannot answer now, or the credentials must change.
    Unavailable {
        /// What happened, without the body of the reply.
        message: String,
        /// Whether the same call may succeed later.
        retryable: bool,
    },
    /// The service will never answer this input.
    Refused(String),
}

impl Problem {
    /// This problem as the failure of a port with the codes given.
    fn into_error<C: ErrorCode>(self, unavailable: C, refused: C) -> Error {
        match self {
            Problem::Unavailable { message, retryable } => Failure::new(unavailable, message)
                .with_retryable(retryable)
                .into(),
            Problem::Refused(message) => Failure::new(refused, message).into(),
        }
    }

    fn unusable_reply(message: &str) -> Problem {
        Problem::Unavailable {
            message: message.to_owned(),
            retryable: false,
        }
    }
}

/// The class of a non-success HTTP status. The message is the status and
/// the host only: the body may echo the request, which carries the key.
/// 408, 429 and 5xx are transient; 401 and 403 need other credentials; any
/// other status refuses the input.
fn status_problem(status: u16, host: &str) -> Problem {
    let message = format!("HTTP {status} from {host}");
    match status {
        408 | 429 | 500..=599 => Problem::Unavailable {
            message,
            retryable: true,
        },
        401 | 403 => Problem::Unavailable {
            message,
            retryable: false,
        },
        _ => Problem::Refused(message),
    }
}

/// A JSON service at one URL.
struct Post {
    agent: ureq::Agent,
    url: String,
    key: Option<String>,
}

impl Post {
    fn new(url: String, key: Option<String>) -> Post {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        Post { agent, url, key }
    }

    /// POSTs `body` and parses the JSON answer. A request a signal
    /// interrupted is sent again, up to [`INTERRUPTED_ATTEMPTS`] times: the
    /// embedding and rerank calls change nothing.
    fn send(&self, body: &Value) -> Result<Value, Problem> {
        let mut attempt = 1;
        loop {
            match self.send_once(body) {
                Err(Sent::Interrupted(_)) if attempt < INTERRUPTED_ATTEMPTS => attempt += 1,
                Err(Sent::Interrupted(err)) => return Err(transport(&err, true)),
                Err(Sent::Failed(problem)) => return Err(problem),
                Ok(value) => return Ok(value),
            }
        }
    }

    fn send_once(&self, body: &Value) -> Result<Value, Sent> {
        let mut request = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json");
        if let Some(key) = &self.key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = request
            .send_json(body)
            .map_err(|err| Sent::classify(err, true))?;
        let status = response.status();
        if !status.is_success() {
            // The body is not read: an error page may echo the request.
            return Err(Sent::Failed(status_problem(
                status.as_u16(),
                &host_for_error(&self.url),
            )));
        }
        response
            .body_mut()
            .read_json()
            .map_err(|err| Sent::classify(err, false))
    }
}

/// How one POST failed: interrupted by a signal, or for good.
enum Sent {
    Interrupted(ureq::Error),
    Failed(Problem),
}

impl Sent {
    /// `retryable` is whether the failure happened before an answer began:
    /// then the same call may succeed later.
    fn classify(err: ureq::Error, retryable: bool) -> Sent {
        match &err {
            ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::Interrupted => {
                Sent::Interrupted(err)
            }
            _ => Sent::Failed(transport(&err, retryable)),
        }
    }
}

fn transport(err: &ureq::Error, retryable: bool) -> Problem {
    Problem::Unavailable {
        message: redact_remote_text(&err.to_string()),
        retryable,
    }
}

fn host_for_error(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let hostport = rest.rsplit_once('@').map_or(rest, |(_, hostport)| hostport);
    let host = hostport
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(hostport)
        .trim();
    if host.is_empty() {
        String::from("embedder/reranker")
    } else {
        host.to_owned()
    }
}

fn parse_embeddings(
    value: &Value,
    count: usize,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>, Problem> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Problem::unusable_reply("embeddings response has no data"))?;
    let mut slots: Vec<Option<Vec<f32>>> = vec![None; count];
    for (i, row) in data.iter().enumerate() {
        let index = row
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(i);
        let embedding = row
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| Problem::unusable_reply("embeddings response has no embedding"))?;
        let mut vector = Vec::with_capacity(embedding.len());
        for item in embedding {
            let Some(n) = item.as_f64() else {
                return Err(Problem::unusable_reply(
                    "embeddings response is not float32",
                ));
            };
            vector.push(n as f32);
        }
        if vector.len() != dimensions {
            return Err(Problem::unusable_reply(
                "embedding length does not match --embedder-dimensions",
            ));
        }
        let Some(slot) = slots.get_mut(index) else {
            return Err(Problem::unusable_reply(
                "embeddings response index is out of range",
            ));
        };
        *slot = Some(vector);
    }
    slots
        .into_iter()
        .map(|vector| {
            vector.ok_or_else(|| Problem::unusable_reply("embeddings response is missing a text"))
        })
        .collect()
}

fn parse_scores(value: &Value, count: usize) -> Result<Vec<f64>, Problem> {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| Problem::unusable_reply("rerank response has no results"))?;
    let mut scores = vec![None; count];
    for hit in results {
        let index = hit
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| Problem::unusable_reply("rerank response is missing index"))?;
        let score = hit
            .get("relevance_score")
            .and_then(Value::as_f64)
            .ok_or_else(|| Problem::unusable_reply("rerank response has no relevance_score"))?;
        let Some(slot) = scores.get_mut(index) else {
            return Err(Problem::unusable_reply(
                "rerank response index is out of range",
            ));
        };
        *slot = Some(score);
    }
    scores
        .into_iter()
        .map(|score| {
            score.ok_or_else(|| Problem::unusable_reply("rerank response is missing a document"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use scheda::v1::{EmbedderErrorCode, Query};

    use super::{Post, Problem, embedder, host_for_error, status_problem};

    /// A server that answers once with an HTML error page, as a proxy does.
    #[test]
    fn send_checks_the_status_before_the_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read the whole request (headers, then the `{}` body) so the
            // close below is not a reset.
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.ends_with(b"{}") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&chunk[..n]),
                }
            }
            let body = "<html><body>Bad Gateway key=sk-leak</body></html>";
            let _ = write!(
                stream,
                "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        let post = Post::new(format!("http://{addr}/v1/embeddings"), None);
        let problem = post.send(&serde_json::json!({})).unwrap_err();
        server.join().unwrap();
        let Problem::Unavailable { message, retryable } = problem else {
            panic!("a 502 is unavailable");
        };
        assert!(retryable);
        assert_eq!(message, format!("HTTP 502 from {addr}"));
        assert!(!message.contains("sk-leak"));
    }

    #[test]
    fn statuses_map_to_transient_credential_and_refused_problems() {
        for (status, retryable) in [
            (408, Some(true)),
            (429, Some(true)),
            (500, Some(true)),
            (503, Some(true)),
            (504, Some(true)),
            (401, Some(false)),
            (403, Some(false)),
            (400, None),
            (404, None),
            (422, None),
        ] {
            let message = format!("HTTP {status} from api.example.test");
            match (status_problem(status, "api.example.test"), retryable) {
                (
                    Problem::Unavailable {
                        message: got,
                        retryable: r,
                    },
                    Some(want),
                ) => {
                    assert_eq!(got, message, "status {status}");
                    assert_eq!(r, want, "status {status}");
                }
                (Problem::Refused(got), None) => assert_eq!(got, message, "status {status}"),
                (other, _) => panic!("status {status}: {other:?}"),
            }
        }
    }

    #[test]
    fn host_for_error_drops_userinfo_path_and_query() {
        assert_eq!(
            host_for_error("https://user:secret@api.example.test/v1/embeddings?token=leak"),
            "api.example.test"
        );
        assert_eq!(
            host_for_error("http://127.0.0.1:8080/v1/rerank"),
            "127.0.0.1:8080"
        );
    }

    /// A server that answers one request with `status` and `body`; the URL
    /// of its API root.
    fn serve_once(status: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&chunk[..n]),
                }
            }
            // The body of the request follows the headers; read what is
            // left of it so the close below is not a reset.
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
            let _ = stream.read(&mut chunk);
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        format!("http://{addr}/v1")
    }

    /// The code and the retry flag of the failure of `embed_query` on a
    /// service at `url`.
    fn embed_failure(url: &str) -> (Option<EmbedderErrorCode>, bool) {
        let port = embedder(url, "m", 2, None).unwrap().to_sync().unwrap();
        let error = port
            .embed_query(Query::default().with_text("x"))
            .unwrap_err();
        (error.code_as::<EmbedderErrorCode>(), error.retryable())
    }

    #[test]
    fn the_embedder_fails_with_the_code_of_its_port() {
        use EmbedderErrorCode::{EMBEDDER_ERROR_CODE_REFUSED, EMBEDDER_ERROR_CODE_UNAVAILABLE};

        // Nothing listens: a transport failure, which may pass.
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", dead.local_addr().unwrap());
        drop(dead);
        assert_eq!(
            embed_failure(&url),
            (Some(EMBEDDER_ERROR_CODE_UNAVAILABLE), true)
        );

        assert_eq!(
            embed_failure(&serve_once("503 Service Unavailable", "{}")),
            (Some(EMBEDDER_ERROR_CODE_UNAVAILABLE), true)
        );
        assert_eq!(
            embed_failure(&serve_once("401 Unauthorized", "{}")),
            (Some(EMBEDDER_ERROR_CODE_UNAVAILABLE), false)
        );
        assert_eq!(
            embed_failure(&serve_once("400 Bad Request", "{}")),
            (Some(EMBEDDER_ERROR_CODE_REFUSED), false)
        );
        // A 2xx answer that is not the expected JSON, or has the wrong
        // vector length.
        assert_eq!(
            embed_failure(&serve_once("200 OK", "{\"nodata\":1}")),
            (Some(EMBEDDER_ERROR_CODE_UNAVAILABLE), false)
        );
        assert_eq!(
            embed_failure(&serve_once("200 OK", "not json")),
            (Some(EMBEDDER_ERROR_CODE_UNAVAILABLE), false)
        );
        assert_eq!(
            embed_failure(&serve_once(
                "200 OK",
                "{\"data\":[{\"index\":0,\"embedding\":[1.0,2.0,3.0]}]}"
            )),
            (Some(EMBEDDER_ERROR_CODE_UNAVAILABLE), false)
        );
    }
}
