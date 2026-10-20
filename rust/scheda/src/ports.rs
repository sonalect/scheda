//! The ports of a Vault: the services the caller implements and Scheda
//! calls.
//!
//! An [`Embedder`](EmbedderAsync) turns texts into vectors, a
//! [`Reranker`](RerankerAsync) scores Documents against a query, and a
//! [`Projector`](ProjectorAsync) says what of a Document the Index holds. A
//! caller gives each as a reference ([`EmbedderRef`], …) that holds its
//! implementation in the form it has, blocking or async; Scheda calls it in
//! its async form: a blocking implementation runs on the blocking pool of
//! the runtime the port was given on, an async one is awaited. The caller
//! wraps nothing.
//!
//! Every call of a port runs as a task of its own, so a panic in the port
//! fails the call with `PANICKED` and does not unwind into Scheda, and a
//! call Scheda drops (a Vault that closes) stops the port's work at its
//! next `await`.
//!
//! A build without the feature `index` has no Index to use a port, so it
//! accepts none: a Vault opened with one, and a `Set*` call that gives one,
//! are `UNIMPLEMENTED`; removing a port that is not there is nothing to do.
//!
//! What Scheda does with a port once it holds one: it installs it (reads
//! and validates the descriptor of an embedder and of a projector once) and
//! calls it through one place each: the texts of an embedder and the
//! Documents of a reranker travel as streams, one item at a time, moved and
//! never cloned; each reply is checked (the length of a vector, the number
//! of vectors and scores) before anything uses it; a failure of the port
//! keeps its code and its retry flag and gains a stack frame.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(feature = "index")]
use std::sync::{Mutex, PoisonError};
use std::task::Poll;

use buffa_types::google::protobuf::Empty;
use protocontract::{BoxFuture, BoxIter, Stream};
use scheda_api::v1::embedder_ref::EmbedderRef;
use scheda_api::v1::projector_ref::ProjectorRef;
use scheda_api::v1::reranker_ref::RerankerRef;
use scheda_api::v1::score_item::Item;
#[cfg(feature = "index")]
use scheda_api::v1::{
    CollectionKey, DynEmbedderAsync, DynProjectorAsync, DynRerankerAsync, IndexDoc, IndexSchema,
    ProjectorAsync, ProjectorDescriptor, RawDocument,
};
use scheda_api::v1::{
    Document, DocumentScore, EmbedderAsync, EmbedderDescriptor, EmbedderSync, Query, RerankerAsync,
    RerankerSync, ScoreItem, Text, VaultErrorCode, Vector,
};

#[cfg(feature = "index")]
use crate::error::Context;
use crate::error::{Error, Fault};
#[cfg(feature = "index")]
use crate::stored;

/// The ports a Vault is opened with: a reference for each port it has, none
/// for a port it does without.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ports {
    /// Says which columns the Index holds and their values.
    pub(crate) projector: Option<ProjectorRef>,
    /// Turns texts into vectors for the vector leg and for the `vector`
    /// columns of the Index.
    pub(crate) embedder: Option<EmbedderRef>,
    /// Scores Documents against a query, for a search that reranks.
    pub(crate) reranker: Option<RerankerRef>,
}

/// The ports of one Vault, installed: each in the async form, the embedder
/// and the projector with the descriptor they declared. Each may be
/// replaced or removed at any time; a call in progress keeps the port it
/// started with.
///
/// A slot holds a plain `Option<Arc<…>>` that no panic can leave half
/// written, so a poisoned lock is read as a clean one.
#[derive(Default)]
pub(crate) struct Installed {
    #[cfg(feature = "index")]
    embedder: Mutex<Option<Arc<EmbedderPort>>>,
    #[cfg(feature = "index")]
    reranker: Mutex<Option<Arc<RerankerPort>>>,
    #[cfg(feature = "index")]
    projector: Mutex<Option<Arc<ProjectorPort>>>,
}

impl fmt::Debug for Installed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("Installed");
        #[cfg(feature = "index")]
        out.field("embedder", &self.embedder().is_some())
            .field("reranker", &self.reranker().is_some())
            .field("projector", &self.projector().is_some());
        out.finish()
    }
}

impl Installed {
    /// Installs `ports`: each one's descriptor is read and checked.
    ///
    /// # Errors
    ///
    /// Those of [`Installed::set_projector`], [`Installed::set_embedder`],
    /// and [`Installed::set_reranker`].
    pub(crate) async fn new(ports: Ports) -> Result<Installed, Error> {
        let installed = Installed::default();
        if let Some(projector) = ports.projector {
            installed.set_projector(Some(projector)).await?;
        }
        if let Some(embedder) = ports.embedder {
            installed.set_embedder(Some(embedder)).await?;
        }
        if let Some(reranker) = ports.reranker {
            installed.set_reranker(Some(reranker)).await?;
        }
        Ok(installed)
    }

    /// Installs `embedder`, or removes the embedder for `None`.
    ///
    /// # Errors
    ///
    /// `UNIMPLEMENTED` for an embedder in a build without `index`;
    /// `INVALID_ARGUMENT` for a reference that holds no implementation, or
    /// a descriptor that breaks its rules; the failure of the port.
    pub(crate) async fn set_embedder(&self, embedder: Option<EmbedderRef>) -> Result<(), Error> {
        #[cfg(feature = "index")]
        {
            let port = match embedder {
                Some(port) => Some(Arc::new(install_embedder(&port).await?)),
                None => None,
            };
            *slot(&self.embedder) = port;
            Ok(())
        }
        #[cfg(not(feature = "index"))]
        refuse(embedder.is_some(), "an embedder")
    }

    /// Installs `reranker`, or removes the reranker for `None`.
    ///
    /// # Errors
    ///
    /// As [`Installed::set_embedder`].
    pub(crate) async fn set_reranker(&self, reranker: Option<RerankerRef>) -> Result<(), Error> {
        #[cfg(feature = "index")]
        {
            let port = match reranker {
                Some(port) => Some(Arc::new(RerankerPort {
                    port: port.to_async_offload().map_err(unusable("reranker"))?,
                })),
                None => None,
            };
            *slot(&self.reranker) = port;
            Ok(())
        }
        #[cfg(not(feature = "index"))]
        refuse(reranker.is_some(), "a reranker")
    }

    /// Installs `projector`, or removes the projector for `None`.
    ///
    /// # Errors
    ///
    /// As [`Installed::set_embedder`].
    pub(crate) async fn set_projector(&self, projector: Option<ProjectorRef>) -> Result<(), Error> {
        #[cfg(feature = "index")]
        {
            let port = match projector {
                Some(port) => Some(Arc::new(install_projector(&port).await?)),
                None => None,
            };
            *slot(&self.projector) = port;
            Ok(())
        }
        #[cfg(not(feature = "index"))]
        refuse(projector.is_some(), "a Projector")
    }

    /// The embedder now installed, if any.
    #[cfg(feature = "index")]
    pub(crate) fn embedder(&self) -> Option<Arc<EmbedderPort>> {
        slot(&self.embedder).clone()
    }

    /// The reranker now installed, if any.
    #[cfg(feature = "index")]
    pub(crate) fn reranker(&self) -> Option<Arc<RerankerPort>> {
        slot(&self.reranker).clone()
    }

    /// The projector now installed, if any.
    #[cfg(feature = "index")]
    pub(crate) fn projector(&self) -> Option<Arc<ProjectorPort>> {
        slot(&self.projector).clone()
    }
}

#[cfg(feature = "index")]
fn slot<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `UNIMPLEMENTED` for a port, in a build that has no Index to use one;
/// removing one is nothing to do.
#[cfg(not(feature = "index"))]
fn refuse(given: bool, what: &str) -> Result<(), Error> {
    if given {
        return Err(Fault::new(
            VaultErrorCode::Unimplemented,
            format!("{what} needs a build with the `index` feature: there is no Index without it"),
        )
        .into());
    }
    Ok(())
}

/// `INVALID_ARGUMENT` for a port reference whose async form cannot be
/// taken: it holds no implementation (`UNBOUND`), or it holds a blocking
/// one and no tokio runtime is current (`NO_RUNTIME`). The caller gave it,
/// so the mistake is the caller's; protocontract's error stays the source.
#[cfg(feature = "index")]
fn unusable(what: &str) -> impl FnOnce(Error) -> Error + '_ {
    move |error| {
        Fault::new(
            VaultErrorCode::InvalidArgument,
            format!(
                "the {what} cannot be used: {error}; give a reference that holds an \
                 implementation"
            ),
        )
        .with_source(error)
        .into()
    }
}

/// `INVALID_ARGUMENT` for a message a port gave that breaks its rules.
#[cfg(feature = "index")]
fn invalid_reply(what: &str, fault: Fault) -> Fault {
    Fault::new(
        VaultErrorCode::InvalidArgument,
        format!("{what} is not valid: {}", fault.message()),
    )
    .with_source(fault)
}

/// Runs `work` as a task of its own: a panic in it is `PANICKED`, and the
/// task is aborted when the returned future is dropped.
///
/// # Errors
///
/// What `work` returns; `PANICKED` when it panicked; `NO_RUNTIME` outside a
/// tokio runtime.
#[cfg(feature = "index")]
pub(crate) async fn task<T, F>(work: F) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>> + Send + 'static,
    T: Send + 'static,
{
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
        Error::new(
            protocontract::RuntimeCode::NoRuntime,
            "a port is called on a tokio runtime",
        )
    })?;
    let mut handle = Abort(runtime.spawn(work));
    match (&mut handle.0).await {
        Ok(outcome) => outcome,
        Err(error) if error.is_panic() => {
            let payload = error.into_panic();
            Err(Error::new(
                protocontract::RuntimeCode::Panicked,
                format!(
                    "a port panicked: {}",
                    scheda_blocking::panic_message(payload.as_ref())
                ),
            ))
        }
        Err(error) => Err(Error::new(
            protocontract::RuntimeCode::Cancelled,
            format!("a call of a port was stopped: {error}"),
        )),
    }
}

/// A task aborted when it is dropped.
#[cfg(feature = "index")]
struct Abort<T>(tokio::task::JoinHandle<T>);

#[cfg(feature = "index")]
impl<T> Drop for Abort<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The next item of `stream`.
#[cfg(feature = "index")]
async fn next<S: protocontract::Stream>(stream: std::pin::Pin<&mut S>) -> Option<S::Item> {
    let mut stream = stream;
    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

/// An installed embedder: the port in the async form, and the descriptor it
/// declared when it was installed. Nothing asks the port to describe itself
/// again.
#[cfg(feature = "index")]
pub(crate) struct EmbedderPort {
    port: DynEmbedderAsync,
    descriptor: EmbedderDescriptor,
    /// `descriptor.dimensions`, as a length.
    dimensions: usize,
}

/// Reads and validates the descriptor of `port` once.
#[cfg(feature = "index")]
async fn install_embedder(port: &EmbedderRef) -> Result<EmbedderPort, Error> {
    let port = port.to_async_offload().map_err(unusable("embedder"))?;
    let asked = port.clone();
    let descriptor = task(async move { asked.describe(Empty::default()).await })
        .await
        .context("Embedder::describe: ask the embedder")?;
    stored::validate(&descriptor)
        .map_err(|fault| invalid_reply("the descriptor of the embedder", fault))
        .context("Embedder::describe: check the descriptor")?;
    let dimensions =
        usize::try_from(descriptor.dimensions.unwrap_or_default()).unwrap_or(usize::MAX);
    Ok(EmbedderPort {
        port,
        descriptor,
        dimensions,
    })
}

/// The vectors an embedder gave before a call ended, and the failure that
/// ended it, if any.
///
/// Whatever the vectors are, they are right: the first `vectors.len()`
/// texts have their vector, in order, each of the declared length. A caller
/// that keeps them sends only the other texts the next time.
#[cfg(feature = "index")]
#[derive(Debug, Default)]
pub(crate) struct Embedded {
    /// One vector per text, for the texts from the first on, in order.
    pub(crate) vectors: Vec<Vec<f32>>,
    /// Why the vectors stop short of the texts, if they do.
    pub(crate) failure: Option<Error>,
}

#[cfg(feature = "index")]
impl Embedded {
    /// All the vectors, or the failure when the call stopped short.
    ///
    /// # Errors
    ///
    /// The failure that ended the call.
    #[cfg(test)]
    pub(crate) fn into_result(self) -> Result<Vec<Vec<f32>>, Error> {
        match self.failure {
            None => Ok(self.vectors),
            Some(error) => Err(error),
        }
    }
}

/// Checks the vectors of one `Embed` call as they come.
#[cfg(feature = "index")]
struct Gathering {
    dimensions: usize,
    expected: usize,
    out: Embedded,
}

#[cfg(feature = "index")]
impl Gathering {
    fn new(dimensions: usize, expected: usize) -> Gathering {
        Gathering {
            dimensions,
            expected,
            out: Embedded::default(),
        }
    }

    /// Takes the next item of the reply. `false` when the call is over: the
    /// reply failed or broke its contract.
    fn accept(&mut self, item: Result<Vector, Error>) -> bool {
        let index = self.out.vectors.len();
        if index == self.expected {
            return self.fail(
                Fault::new(
                    VaultErrorCode::InvalidArgument,
                    format!(
                        "the embedder gave more than the {} vectors asked for",
                        self.expected
                    ),
                )
                .into(),
            );
        }
        match item {
            Err(error) => self.fail(error),
            Ok(vector) if vector.values.len() != self.dimensions => self.fail(
                Fault::new(
                    VaultErrorCode::InvalidArgument,
                    format!(
                        "vector {index} has {} values, and the embedder declared {}",
                        vector.values.len(),
                        self.dimensions
                    ),
                )
                .into(),
            ),
            Ok(vector) => {
                self.out.vectors.push(vector.values);
                true
            }
        }
    }

    /// Ends the call with `error`; always `false`.
    fn fail(&mut self, error: Error) -> bool {
        self.out.failure = Some(error.context("Embedder::embed: read the vectors"));
        false
    }

    /// The reply ended: fewer vectors than texts is a failure of its own.
    fn finish(mut self) -> Embedded {
        if self.out.failure.is_none() && self.out.vectors.len() < self.expected {
            let error: Error = Fault::new(
                VaultErrorCode::InvalidArgument,
                format!(
                    "the embedder gave {} vectors for {} texts",
                    self.out.vectors.len(),
                    self.expected
                ),
            )
            .into();
            self.out.failure = Some(error.context("Embedder::embed: count the vectors"));
        }
        self.out
    }
}

#[cfg(feature = "index")]
impl EmbedderPort {
    /// The model whose vectors the embedder gives.
    pub(crate) fn model_id(&self) -> &str {
        self.descriptor.model_id.as_deref().unwrap_or_default()
    }

    /// The length of each vector.
    pub(crate) fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// The vectors of `texts`, one per text in the same order.
    ///
    /// The texts go to the embedder one by one as a stream, moved and not
    /// cloned; how it gathers them is its own business. Each vector must
    /// have the declared length, and there must be exactly as many vectors
    /// as texts. When the reply fails or breaks that contract after `n`
    /// vectors, the result holds those `n` and the failure, which keeps the
    /// code and the retry flag of the port. `stop` is looked at between two
    /// vectors: once it says so, the call ends with `CANCELLED` and keeps
    /// the vectors it has.
    pub(crate) async fn embed(
        &self,
        texts: Vec<String>,
        stop: impl Fn() -> bool + Send + 'static,
    ) -> Embedded {
        let expected = texts.len();
        let dimensions = self.dimensions;
        if expected == 0 {
            return Gathering::new(dimensions, 0).finish();
        }
        let port = self.port.clone();
        let call = task(async move {
            let mut gathering = Gathering::new(dimensions, expected);
            let input = protocontract::IterStream::new(
                texts
                    .into_iter()
                    .map(|text| Ok(Text::default().with_text(text)))
                    .collect::<Vec<Result<Text, Error>>>()
                    .into_iter(),
            );
            match port.embed(input).await {
                Err(error) => {
                    gathering.fail(error);
                }
                Ok(reply) => {
                    let mut reply = std::pin::pin!(reply);
                    while let Some(item) = next(reply.as_mut()).await {
                        if !gathering.accept(item) {
                            break;
                        }
                        if stop() && gathering.out.vectors.len() < expected {
                            gathering.fail(Error::new(
                                protocontract::RuntimeCode::Cancelled,
                                "the Vault is closing",
                            ));
                            break;
                        }
                    }
                }
            }
            Ok(gathering.finish())
        });
        match call.await {
            Ok(embedded) => embedded,
            Err(error) => Embedded {
                vectors: Vec::new(),
                failure: Some(error.context("Embedder::embed: ask the embedder")),
            },
        }
    }

    /// The vector of a search text.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for an empty text and for a vector of the wrong
    /// length; the failure of the port otherwise.
    pub(crate) async fn embed_query(&self, text: String) -> Result<Vec<f32>, Error> {
        let query = Self::query_of(text)?;
        let port = self.port.clone();
        let vector = task(async move { port.embed_query(query).await })
            .await
            .context("Embedder::embed_query: ask the embedder")?;
        self.checked(vector)
    }

    /// The query for a search text, checked.
    fn query_of(text: String) -> Result<Query, Error> {
        let query = Query::default().with_text(text);
        stored::validate(&query).context("Embedder::embed_query: check the query")?;
        Ok(query)
    }

    /// The values of the vector of a query, when it has the declared
    /// length.
    fn checked(&self, vector: Vector) -> Result<Vec<f32>, Error> {
        if vector.values.len() != self.dimensions {
            return Err(Error::from(Fault::new(
                VaultErrorCode::InvalidArgument,
                format!(
                    "the vector of the query has {} values, and the embedder declared {}",
                    vector.values.len(),
                    self.dimensions
                ),
            ))
            .context("Embedder::embed_query: check the vector"));
        }
        Ok(vector.values)
    }
}

/// An installed reranker, in the async form.
#[cfg(feature = "index")]
pub(crate) struct RerankerPort {
    port: DynRerankerAsync,
}

#[cfg(feature = "index")]
impl RerankerPort {
    /// One score per Document in `documents`, in their order; higher is
    /// better.
    ///
    /// The reranker gets the query first and then the Documents, one by
    /// one, moved and not cloned (each has its `raw` set by the caller).
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the reranker gives more or fewer scores than
    /// Documents; the failure of the port otherwise, with its code and
    /// retry flag.
    pub(crate) async fn score(
        &self,
        query: String,
        documents: Vec<Document>,
    ) -> Result<Vec<f64>, Error> {
        let expected = documents.len();
        if expected == 0 {
            return Ok(Vec::new());
        }
        let port = self.port.clone();
        task(async move {
            let items: Vec<Result<ScoreItem, Error>> = std::iter::once(Item::Query(query))
                .chain(documents.into_iter().map(Item::from))
                .map(|item| {
                    Ok(ScoreItem {
                        item: Some(item),
                        ..ScoreItem::default()
                    })
                })
                .collect();
            let reply = port
                .score(protocontract::IterStream::new(items.into_iter()))
                .await
                .context("Reranker::score: ask the reranker")?;
            let mut reply = std::pin::pin!(reply);
            let mut scores = Vec::with_capacity(expected);
            while let Some(item) = next(reply.as_mut()).await {
                let score: DocumentScore = item.context("Reranker::score: read the scores")?;
                if scores.len() == expected {
                    return Err(Error::from(Fault::new(
                        VaultErrorCode::InvalidArgument,
                        format!("the reranker gave more than the {expected} scores asked for"),
                    ))
                    .context("Reranker::score: count the scores"));
                }
                scores.push(score.score.unwrap_or_default());
            }
            if scores.len() != expected {
                return Err(Error::from(Fault::new(
                    VaultErrorCode::InvalidArgument,
                    format!(
                        "the reranker gave {} scores for {expected} Documents",
                        scores.len()
                    ),
                ))
                .context("Reranker::score: count the scores"));
            }
            Ok(scores)
        })
        .await
    }
}

/// An installed projector: the port in the async form, and the descriptor
/// it declared when it was installed.
#[cfg(feature = "index")]
pub(crate) struct ProjectorPort {
    port: DynProjectorAsync,
    descriptor: ProjectorDescriptor,
    /// The answers of `columns` for one state of `HEAD`. A new install is a
    /// new port, so its answers start empty.
    kept: tokio::sync::Mutex<Kept>,
}

/// What a projector answered to `columns` at the commit `head` of the main
/// repository, the one the declarations were read from.
#[cfg(feature = "index")]
#[derive(Default)]
struct Kept {
    head: Option<git2::Oid>,
    columns: std::collections::BTreeMap<String, Option<IndexSchema>>,
}

/// Installs a projector: takes its async form, then reads and validates its
/// descriptor, once.
#[cfg(feature = "index")]
async fn install_projector(port: &ProjectorRef) -> Result<ProjectorPort, Error> {
    let port = port.to_async_offload().map_err(unusable("projector"))?;
    let asked = port.clone();
    let descriptor = task(async move { asked.describe(Empty::default()).await })
        .await
        .context("Projector::describe: ask the projector")?;
    stored::validate(&descriptor)
        .map_err(|fault| invalid_reply("the descriptor of the projector", fault))
        .context("Projector::describe: check the descriptor")?;
    Ok(ProjectorPort {
        port,
        descriptor,
        kept: tokio::sync::Mutex::default(),
    })
}

#[cfg(feature = "index")]
impl ProjectorPort {
    /// The projection the projector declared when it was installed.
    pub(crate) fn projection_id(&self) -> &str {
        self.descriptor.projection_id.as_deref().unwrap_or_default()
    }

    /// The columns of `collection` as declared in the commit `head` of the
    /// main repository, or `None` when the projector declares none: its
    /// Documents are indexed by identity only.
    ///
    /// The projector is asked once per collection and per `head`: an answer
    /// is kept until a call names another `head`, and so is shared by the
    /// indexer and by every query. A failed call is not kept. Callers that
    /// ask at the same time wait for each other, so the projector is not
    /// asked twice at once.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the columns break the rules of an
    /// `IndexSchema` (a name used twice, a reserved name, a kind unset,
    /// `COLUMN_KIND_UNSPECIFIED`, or not defined);
    /// the failure of the port otherwise.
    pub(crate) async fn columns(
        &self,
        head: Option<git2::Oid>,
        collection: String,
    ) -> Result<Option<IndexSchema>, Error> {
        let mut kept = self.kept.lock().await;
        if kept.head != head {
            kept.head = head;
            kept.columns.clear();
        }
        if let Some(schema) = kept.columns.get(&collection) {
            return Ok(schema.clone());
        }
        let port = self.port.clone();
        let asked = collection.clone();
        let schema = task(async move {
            port.columns(CollectionKey::default().with_name(asked))
                .await
        })
        .await
        .context("Projector::columns: ask the projector")?;
        stored::validate(&schema)
            .map_err(|fault| invalid_reply("the columns of the projector", fault))
            .context("Projector::columns: check the columns")?;
        let schema = (!schema.columns.is_empty()).then_some(schema);
        kept.columns.insert(collection, schema.clone());
        Ok(schema)
    }

    /// The values of the columns for one Document, or `None` when the
    /// projector gives none: the Document is indexed by identity only.
    /// `raw` is moved into the call.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when a value breaks the rules of an `IndexValue`;
    /// the failure of the port otherwise.
    pub(crate) async fn project(
        &self,
        collection: String,
        id: String,
        raw: Vec<u8>,
    ) -> Result<Option<IndexDoc>, Error> {
        let document = RawDocument::default()
            .with_collection(collection)
            .with_id(id)
            .with_raw(raw);
        let port = self.port.clone();
        let values = task(async move { port.project(document).await })
            .await
            .context("Projector::project: ask the projector")?;
        stored::validate(&values)
            .map_err(|fault| invalid_reply("the values of the projector", fault))
            .context("Projector::project: check the values")?;
        Ok((!values.values.is_empty()).then_some(values))
    }
}

/// An embedder made of one function over a portion of texts.
///
/// `FixedEmbedder` implements the blocking embedder for an implementation
/// that needs no more than "these texts, those vectors": it reads the texts
/// one by one, calls the function with portions of at most the size given,
/// calls it with the last partial portion when the texts end, and sends the
/// vectors back in order. `EmbedQuery` is a portion of one, or a call of
/// the query function given by [`FixedEmbedder::with_query`].
///
/// The function must return one vector per text of its portion, in order;
/// a count that does not match fails the stream with `INVALID_ARGUMENT`.
/// A failure of the function ends the stream. For a function that waits on
/// an async client, [`FixedEmbedderAsync`] does the same without a thread.
pub struct FixedEmbedder<F> {
    model_id: String,
    dimensions: u32,
    portion: NonZeroUsize,
    embed: Arc<F>,
    query: Option<Box<QueryFn>>,
}

/// The query function of a [`FixedEmbedder`].
type QueryFn = dyn Fn(String) -> Result<Vec<f32>, Error> + Send + Sync;

/// The query function of a [`FixedEmbedderAsync`].
type QueryFnAsync = dyn Fn(String) -> BoxFuture<'static, Result<Vec<f32>, Error>> + Send + Sync;

impl<F> FixedEmbedder<F>
where
    F: Fn(Vec<String>) -> Result<Vec<Vec<f32>>, Error> + Send + Sync + 'static,
{
    /// An embedder of the model `model_id` that gives vectors of
    /// `dimensions` values, calling `embed` with portions of at most
    /// `portion` texts.
    ///
    /// Scheda reads `model_id` and `dimensions` once, when the embedder is
    /// installed, and refuses an empty model or no dimensions.
    pub fn new(
        model_id: impl Into<String>,
        dimensions: u32,
        portion: NonZeroUsize,
        embed: F,
    ) -> FixedEmbedder<F> {
        FixedEmbedder {
            model_id: model_id.into(),
            dimensions,
            portion,
            embed: Arc::new(embed),
            query: None,
        }
    }

    /// The same embedder, answering `EmbedQuery` with `query` instead of a
    /// portion of one: for a model that embeds a search text otherwise than
    /// a stored one (an instruction before the text, another task type).
    /// `query` gets the text of the query; the vector it returns must have
    /// `dimensions` values, else `EmbedQuery` fails with
    /// `INVALID_ARGUMENT`. A failure of `query` is the failure of
    /// `EmbedQuery`. The texts of `Embed` still go to the portion function.
    #[must_use]
    pub fn with_query<Q>(self, query: Q) -> FixedEmbedder<F>
    where
        Q: Fn(String) -> Result<Vec<f32>, Error> + Send + Sync + 'static,
    {
        FixedEmbedder {
            query: Some(Box::new(query)),
            ..self
        }
    }
}

impl<F> EmbedderSync for FixedEmbedder<F>
where
    F: Fn(Vec<String>) -> Result<Vec<Vec<f32>>, Error> + Send + Sync + 'static,
{
    fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
        Ok(descriptor(&self.model_id, self.dimensions))
    }

    fn embed(
        &self,
        texts: BoxIter<'static, Result<Text, Error>>,
    ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
        let embed = Arc::clone(&self.embed);
        Ok(Box::new(Portions::new(
            Box::new(texts.map(text_of)),
            self.portion,
            Box::new(move |portion| Ok(vectors(embed(portion)?))),
        )))
    }

    fn embed_query(&self, query: Query) -> Result<Vector, Error> {
        let text = query.text.unwrap_or_default();
        match &self.query {
            Some(query) => queried(query(text)?, self.dimensions),
            None => query_vector((self.embed)(vec![text])?),
        }
    }
}

/// An embedder made of one async function over a portion of texts: the
/// async form of [`FixedEmbedder`], for a model client that is async.
///
/// `FixedEmbedderAsync` implements the async embedder: Scheda awaits it on
/// the runtime the Vault was opened on, with no blocking pool, and no thread
/// is held while the function's future waits. It portions as
/// [`FixedEmbedder`] does: it reads the texts one by one, calls the function
/// with portions of at most the size given, calls it with the last partial
/// portion when the texts end, and sends the vectors back in order.
/// `EmbedQuery` is a portion of one, or a call of the query function given
/// by [`FixedEmbedderAsync::with_query`]. One portion is in flight at a
/// time.
///
/// The future must give one vector per text of its portion, in order; a
/// count that does not match fails the stream with `INVALID_ARGUMENT`. A
/// failure of the function ends the stream. A stream that is dropped drops
/// the future in flight.
///
/// A Vault takes it as `EmbedderRef::from_async(FixedEmbedderAsync::new(…))`.
/// The future owns what it uses: the function clones the client it holds
/// into an `async move` block.
pub struct FixedEmbedderAsync<F> {
    model_id: String,
    dimensions: u32,
    portion: NonZeroUsize,
    embed: Arc<F>,
    query: Option<Box<QueryFnAsync>>,
}

impl<F, Fut> FixedEmbedderAsync<F>
where
    F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<Vec<f32>>, Error>> + Send + 'static,
{
    /// An embedder of the model `model_id` that gives vectors of
    /// `dimensions` values, calling `embed` with portions of at most
    /// `portion` texts and awaiting its future.
    ///
    /// Scheda reads `model_id` and `dimensions` once, when the embedder is
    /// installed, and refuses an empty model or no dimensions.
    pub fn new(
        model_id: impl Into<String>,
        dimensions: u32,
        portion: NonZeroUsize,
        embed: F,
    ) -> FixedEmbedderAsync<F> {
        FixedEmbedderAsync {
            model_id: model_id.into(),
            dimensions,
            portion,
            embed: Arc::new(embed),
            query: None,
        }
    }

    /// The same embedder, answering `EmbedQuery` by awaiting the future of
    /// `query` instead of a portion of one: for a model that embeds a
    /// search text otherwise than a stored one (an instruction before the
    /// text, another task type). `query` gets the text of the query; the
    /// vector its future gives must have `dimensions` values, else
    /// `EmbedQuery` fails with `INVALID_ARGUMENT`. A failure of the future
    /// is the failure of `EmbedQuery`. The texts of `Embed` still go to the
    /// portion function.
    #[must_use]
    pub fn with_query<Q, QFut>(self, query: Q) -> FixedEmbedderAsync<F>
    where
        Q: Fn(String) -> QFut + Send + Sync + 'static,
        QFut: Future<Output = Result<Vec<f32>, Error>> + Send + 'static,
    {
        let query =
            move |text| -> BoxFuture<'static, Result<Vec<f32>, Error>> { Box::pin(query(text)) };
        FixedEmbedderAsync {
            query: Some(Box::new(query)),
            ..self
        }
    }
}

impl<F, Fut> EmbedderAsync for FixedEmbedderAsync<F>
where
    F: Fn(Vec<String>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<Vec<f32>>, Error>> + Send + 'static,
{
    async fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
        Ok(descriptor(&self.model_id, self.dimensions))
    }

    async fn embed<R>(
        &self,
        texts: R,
    ) -> Result<impl Stream<Item = Result<Vector, Error>> + Send + use<F, Fut, R>, Error>
    where
        R: Stream<Item = Result<Text, Error>> + Send + 'static,
    {
        let embed = Arc::clone(&self.embed);
        Ok(PortionStream::new(
            Box::pin(texts),
            text_of,
            self.portion,
            Box::new(move |portion| {
                let reply = embed(portion);
                Box::pin(async move { Ok(vectors(reply.await?)) })
            }),
        ))
    }

    async fn embed_query(&self, query: Query) -> Result<Vector, Error> {
        let text = query.text.unwrap_or_default();
        match &self.query {
            Some(query) => queried(query(text).await?, self.dimensions),
            None => query_vector((self.embed)(vec![text]).await?),
        }
    }
}

/// What both forms of the fixed embedder describe.
fn descriptor(model_id: &str, dimensions: u32) -> EmbedderDescriptor {
    EmbedderDescriptor::default()
        .with_model_id(model_id)
        .with_dimensions(dimensions)
}

/// The text of an item of an `Embed` stream.
fn text_of(text: Result<Text, Error>) -> Result<String, Error> {
    Ok(text?.text.unwrap_or_default())
}

/// The replies for the vectors a portion function gave.
fn vectors(values: Vec<Vec<f32>>) -> Vec<Vector> {
    values
        .into_iter()
        .map(|values| Vector {
            values,
            ..Vector::default()
        })
        .collect()
}

/// The vector of a query: the only one the portion function gave for it.
fn query_vector(mut vectors: Vec<Vec<f32>>) -> Result<Vector, Error> {
    match (vectors.pop(), vectors.is_empty()) {
        (Some(values), true) => Ok(Vector {
            values,
            ..Vector::default()
        }),
        _ => Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            "the embedding function did not give exactly one vector for the query",
        )
        .into()),
    }
}

/// The vector a query function gave, when it has the `dimensions` the
/// embedder declares.
fn queried(values: Vec<f32>, dimensions: u32) -> Result<Vector, Error> {
    if u32::try_from(values.len()).ok() != Some(dimensions) {
        return Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            format!(
                "the query function gave a vector of {} values, and the embedder declares {dimensions}",
                values.len()
            ),
        )
        .into());
    }
    Ok(Vector {
        values,
        ..Vector::default()
    })
}

/// A reranker made of one function over a query and a portion of
/// Documents.
///
/// `FixedReranker` implements the blocking reranker for an implementation
/// that needs no more than "this query, these Documents, those scores": it
/// reads the query (the first item) and then the Documents one by one, calls
/// the function with portions of at most the size given, calls it with the
/// last partial portion when the Documents end, and sends the scores back in
/// order.
///
/// The function must return one score per Document of its portion, in
/// order; a count that does not match fails the stream with
/// `INVALID_ARGUMENT`. A failure of the function ends the stream. For a
/// function that waits on an async client, [`FixedRerankerAsync`] does the
/// same without a thread.
pub struct FixedReranker<F> {
    portion: NonZeroUsize,
    score: Arc<F>,
}

impl<F> FixedReranker<F>
where
    F: Fn(&str, Vec<Document>) -> Result<Vec<f64>, Error> + Send + Sync + 'static,
{
    /// A reranker that calls `score` with the query and portions of at most
    /// `portion` Documents.
    pub fn new(portion: NonZeroUsize, score: F) -> FixedReranker<F> {
        FixedReranker {
            portion,
            score: Arc::new(score),
        }
    }
}

impl<F> RerankerSync for FixedReranker<F>
where
    F: Fn(&str, Vec<Document>) -> Result<Vec<f64>, Error> + Send + Sync + 'static,
{
    fn score(
        &self,
        mut score_items: BoxIter<'static, Result<ScoreItem, Error>>,
    ) -> Result<BoxIter<'static, Result<DocumentScore, Error>>, Error> {
        let query = query_of(score_items.next())?;
        let score = Arc::clone(&self.score);
        Ok(Box::new(Portions::new(
            Box::new(score_items.map(document_of)),
            self.portion,
            Box::new(move |portion| Ok(scores(score(&query, portion)?))),
        )))
    }
}

/// A reranker made of one async function over a query and a portion of
/// Documents: the async form of [`FixedReranker`], for a model client that
/// is async.
///
/// `FixedRerankerAsync` implements the async reranker: Scheda awaits it on
/// the runtime the Vault was opened on, with no blocking pool, and no thread
/// is held while the function's future waits. It portions as
/// [`FixedReranker`] does: it reads the query (the first item) and then the
/// Documents one by one, calls the function with portions of at most the
/// size given, calls it with the last partial portion when the Documents
/// end, and sends the scores back in order. One portion is in flight at a
/// time.
///
/// The function gets the query as an `Arc<str>` that its future may keep:
/// every portion shares the one copy. The future must give one score per
/// Document of its portion, in order; a count that does not match fails the
/// stream with `INVALID_ARGUMENT`. A failure of the function ends the
/// stream. A stream that is dropped drops the future in flight.
///
/// A Vault takes it as `RerankerRef::from_async(FixedRerankerAsync::new(…))`.
pub struct FixedRerankerAsync<F> {
    portion: NonZeroUsize,
    score: Arc<F>,
}

impl<F, Fut> FixedRerankerAsync<F>
where
    F: Fn(Arc<str>, Vec<Document>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<f64>, Error>> + Send + 'static,
{
    /// A reranker that calls `score` with the query and portions of at most
    /// `portion` Documents, and awaits its future.
    pub fn new(portion: NonZeroUsize, score: F) -> FixedRerankerAsync<F> {
        FixedRerankerAsync {
            portion,
            score: Arc::new(score),
        }
    }
}

impl<F, Fut> RerankerAsync for FixedRerankerAsync<F>
where
    F: Fn(Arc<str>, Vec<Document>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<f64>, Error>> + Send + 'static,
{
    async fn score<R>(
        &self,
        score_items: R,
    ) -> Result<impl Stream<Item = Result<DocumentScore, Error>> + Send + use<F, Fut, R>, Error>
    where
        R: Stream<Item = Result<ScoreItem, Error>> + Send + 'static,
    {
        let mut score_items = Box::pin(score_items);
        let first = std::future::poll_fn(|cx| score_items.as_mut().poll_next(cx)).await;
        let query: Arc<str> = Arc::from(query_of(first)?);
        let score = Arc::clone(&self.score);
        Ok(PortionStream::new(
            score_items,
            document_of,
            self.portion,
            Box::new(move |portion| {
                let reply = score(Arc::clone(&query), portion);
                Box::pin(async move { Ok(scores(reply.await?)) })
            }),
        ))
    }
}

/// The query of a score stream: its first item.
fn query_of(first: Option<Result<ScoreItem, Error>>) -> Result<String, Error> {
    match first {
        Some(Ok(ScoreItem {
            item: Some(Item::Query(query)),
            ..
        })) => Ok(query),
        Some(Err(error)) => Err(error),
        _ => Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            "the first item of a score stream is the query",
        )
        .into()),
    }
}

/// The Document of an item of a score stream after the first.
fn document_of(item: Result<ScoreItem, Error>) -> Result<Document, Error> {
    match item?.item {
        Some(Item::Document(document)) => Ok(*document),
        _ => Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            "every item of a score stream after the first is a Document",
        )
        .into()),
    }
}

/// The replies for the scores a portion function gave.
fn scores(values: Vec<f64>) -> Vec<DocumentScore> {
    values
        .into_iter()
        .map(|score| DocumentScore::default().with_score(score))
        .collect()
}

/// The portioning of the fixed adapters, both forms: it cuts the items of
/// an input into portions of at most `size`, has the portion function run
/// on each, checks that it gave one result per item, and gives the results
/// one by one, in order. The input is read only once every result before is
/// given; when it ends, the last partial portion is run too. A failure of
/// the input or of the function, or a count that does not match, is the
/// last item: nothing is read or run after it.
///
/// It reads and calls nothing itself. A driver asks [`Cut::step`] what to
/// do next, does it, and hands back what came of it: [`Portions`] by calling
/// an iterator and a function, [`PortionStream`] by polling a stream and a
/// future.
struct Cut<X, Y> {
    size: usize,
    /// The items read for the next portion.
    portion: Vec<X>,
    /// How many items the portion being run has.
    running: usize,
    /// The results of the last portion run, not given yet.
    ready: VecDeque<Y>,
    /// Whether the input may give more items.
    open: bool,
    /// The failure to give once `ready` is empty.
    failure: Option<Error>,
}

/// What a driver of [`Cut`] does next.
enum Step<X, Y> {
    /// Give this item of the reply.
    Give(Result<Y, Error>),
    /// Read the next item of the input and hand it to [`Cut::read`].
    Read,
    /// Run the portion function on these items and hand its result to
    /// [`Cut::ran`].
    Run(Vec<X>),
    /// The reply has ended.
    End,
}

impl<X, Y> Cut<X, Y> {
    fn new(size: NonZeroUsize) -> Cut<X, Y> {
        Cut {
            size: size.get(),
            portion: Vec::new(),
            running: 0,
            ready: VecDeque::new(),
            open: true,
            failure: None,
        }
    }

    fn step(&mut self) -> Step<X, Y> {
        if let Some(reply) = self.ready.pop_front() {
            return Step::Give(Ok(reply));
        }
        if let Some(error) = self.failure.take() {
            return Step::Give(Err(error));
        }
        if self.open && self.portion.len() < self.size {
            return Step::Read;
        }
        if self.portion.is_empty() {
            return Step::End;
        }
        self.running = self.portion.len();
        Step::Run(std::mem::take(&mut self.portion))
    }

    /// The next item of the input; `None` when it ended.
    fn read(&mut self, item: Option<Result<X, Error>>) {
        match item {
            Some(Ok(item)) => self.portion.push(item),
            Some(Err(error)) => self.fail(error),
            None => self.open = false,
        }
    }

    /// What the portion function gave for the portion of the last
    /// [`Step::Run`].
    fn ran(&mut self, replies: Result<Vec<Y>, Error>) {
        match replies {
            Ok(replies) if replies.len() == self.running => self.ready.extend(replies),
            Ok(replies) => self.fail(
                Fault::new(
                    VaultErrorCode::InvalidArgument,
                    format!(
                        "the portion function gave {} results for a portion of {}",
                        replies.len(),
                        self.running
                    ),
                )
                .into(),
            ),
            Err(error) => self.fail(error),
        }
    }

    fn fail(&mut self, error: Error) {
        self.open = false;
        self.portion.clear();
        self.failure = Some(error);
    }
}

/// What a blocking portion function does: one reply per item of the
/// portion.
type PortionFn<X, Y> = Box<dyn Fn(Vec<X>) -> Result<Vec<Y>, Error> + Send + Sync>;

/// The reply of a blocking fixed adapter: [`Cut`] driven by reading an
/// iterator and calling a function, as the replies are asked for.
struct Portions<X, Y> {
    input: BoxIter<'static, Result<X, Error>>,
    run: PortionFn<X, Y>,
    cut: Cut<X, Y>,
}

impl<X, Y> Portions<X, Y> {
    fn new(
        input: BoxIter<'static, Result<X, Error>>,
        size: NonZeroUsize,
        run: PortionFn<X, Y>,
    ) -> Portions<X, Y> {
        Portions {
            input,
            run,
            cut: Cut::new(size),
        }
    }
}

impl<X, Y> Iterator for Portions<X, Y> {
    type Item = Result<Y, Error>;

    fn next(&mut self) -> Option<Result<Y, Error>> {
        loop {
            match self.cut.step() {
                Step::Give(reply) => return Some(reply),
                Step::Read => self.cut.read(self.input.next()),
                Step::Run(portion) => self.cut.ran((self.run)(portion)),
                Step::End => return None,
            }
        }
    }
}

/// What an async portion function does: one reply per item of the portion,
/// once its future is done.
type PortionFuture<X, Y> = Box<dyn Fn(Vec<X>) -> BoxFuture<'static, Result<Vec<Y>, Error>> + Send>;

/// The reply of an async fixed adapter: [`Cut`] driven by polling the input
/// stream and the future of the portion function, on the task that polls
/// the reply. Nothing blocks: a poll that finds either one waiting returns
/// `Pending`, and its waker wakes the task.
struct PortionStream<I: Stream, X, Y> {
    input: Pin<Box<I>>,
    /// Takes an item of the input apart.
    take: fn(I::Item) -> Result<X, Error>,
    run: PortionFuture<X, Y>,
    /// The future of the portion being run.
    running: Option<BoxFuture<'static, Result<Vec<Y>, Error>>>,
    cut: Cut<X, Y>,
}

impl<I: Stream, X, Y> PortionStream<I, X, Y> {
    fn new(
        input: Pin<Box<I>>,
        take: fn(I::Item) -> Result<X, Error>,
        size: NonZeroUsize,
        run: PortionFuture<X, Y>,
    ) -> PortionStream<I, X, Y> {
        PortionStream {
            input,
            take,
            run,
            running: None,
            cut: Cut::new(size),
        }
    }
}

impl<I, X, Y> Stream for PortionStream<I, X, Y>
where
    I: Stream,
    X: Unpin,
    Y: Unpin,
{
    type Item = Result<Y, Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<Y, Error>>> {
        let this = self.get_mut();
        loop {
            if let Some(running) = &mut this.running {
                let replies = std::task::ready!(running.as_mut().poll(cx));
                this.running = None;
                this.cut.ran(replies);
            }
            match this.cut.step() {
                Step::Give(reply) => return Poll::Ready(Some(reply)),
                Step::Read => {
                    let item = std::task::ready!(this.input.as_mut().poll_next(cx));
                    this.cut.read(item.map(this.take));
                }
                Step::Run(portion) => this.running = Some((this.run)(portion)),
                Step::End => return Poll::Ready(None),
            }
        }
    }
}

#[cfg(all(test, feature = "index"))]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use protocontract::{Failure, IterStream, Stream};
    use scheda_api::v1::{
        ColumnKind, EmbedderAsync, EmbedderErrorCode, IndexColumn, IndexValue, ProjectorErrorCode,
        RerankerErrorCode,
    };

    use scheda_api::v1::ProjectorSync;

    use super::*;

    const DIMS: usize = 3;

    /// The vector every test embedder gives for `text`.
    fn vector_for(text: &str) -> Vec<f32> {
        let first = f32::from(text.bytes().next().unwrap_or(0));
        vec![text.len() as f32, first, 7.0]
    }

    fn texts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("text {i}")).collect()
    }

    fn expected(texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|text| vector_for(text)).collect()
    }

    /// An embedder that reads all its texts and answers as scripted.
    #[derive(Default)]
    struct Script {
        model_id: String,
        dimensions: u32,
        describes: Arc<AtomicUsize>,
        /// Fails with a retryable `UNAVAILABLE` instead of this vector.
        fail_at: Option<usize>,
        /// Gives a vector of one value too few at this index.
        short_at: Option<usize>,
        /// Gives one vector more than texts.
        extra: bool,
        /// Gives one vector fewer than texts.
        missing: bool,
        /// The length of the vector of `embed_query`.
        query_values: usize,
        /// The address of each text, as the embedder received it.
        seen: Arc<Mutex<Vec<usize>>>,
    }

    impl Script {
        fn new() -> Script {
            Script {
                model_id: String::from("model-a"),
                dimensions: DIMS as u32,
                query_values: DIMS,
                ..Script::default()
            }
        }
    }

    impl EmbedderSync for Script {
        fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            self.describes.fetch_add(1, Ordering::SeqCst);
            Ok(EmbedderDescriptor::default()
                .with_model_id(self.model_id.clone())
                .with_dimensions(self.dimensions))
        }

        fn embed(
            &self,
            texts: BoxIter<'static, Result<Text, Error>>,
        ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
            let mut reply = Vec::new();
            for (index, text) in texts.enumerate() {
                let text = text?.text.unwrap_or_default();
                self.seen.lock().unwrap().push(text.as_ptr() as usize);
                if self.fail_at == Some(index) {
                    let failure = Failure::new(EmbedderErrorCode::Unavailable, "the model is down")
                        .with_retryable(true);
                    reply.push(Err(failure.into()));
                    break;
                }
                let mut values = vector_for(&text);
                if self.short_at == Some(index) {
                    values.pop();
                }
                reply.push(Ok(Vector {
                    values,
                    ..Vector::default()
                }));
            }
            if self.extra {
                reply.push(Ok(Vector {
                    values: vec![0.0; DIMS],
                    ..Vector::default()
                }));
            }
            if self.missing {
                reply.pop();
            }
            Ok(Box::new(reply.into_iter()))
        }

        fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            drop(query);
            Ok(Vector {
                values: vec![1.0; self.query_values],
                ..Vector::default()
            })
        }
    }

    async fn embedder(script: Script) -> EmbedderPort {
        install_embedder(&EmbedderRef::from_sync(script))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn vectors_come_back_in_the_order_of_the_texts() {
        let texts = texts(20);
        let want = expected(&texts);
        let embedded = embedder(Script::new()).await.embed(texts, || false).await;
        assert!(embedded.failure.is_none());
        assert_eq!(embedded.vectors, want);
    }

    #[tokio::test]
    async fn no_texts_call_nothing() {
        let script = Script::new();
        let seen = Arc::clone(&script.seen);
        let embedded = embedder(script).await.embed(Vec::new(), || false).await;
        assert!(embedded.vectors.is_empty() && embedded.failure.is_none());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failure_after_the_seventh_vector_returns_the_seven_and_the_error() {
        let texts = texts(20);
        let want = expected(&texts[..7]);
        let script = Script {
            fail_at: Some(7),
            ..Script::new()
        };
        let embedded = embedder(script).await.embed(texts, || false).await;
        assert_eq!(embedded.vectors, want);
        let error = embedded.failure.expect("the port failed");
        assert!(error.is(EmbedderErrorCode::Unavailable));
        assert!(error.retryable());
        assert!(!error.stack().is_empty(), "Scheda adds a frame");
        assert!(
            embedder(Script::new())
                .await
                .embed(self::texts(1), || false)
                .await
                .into_result()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_vector_of_the_wrong_length_is_invalid_argument_and_keeps_the_ones_before_it() {
        let texts = texts(10);
        let want = expected(&texts[..3]);
        let script = Script {
            short_at: Some(3),
            ..Script::new()
        };
        let embedded = embedder(script).await.embed(texts, || false).await;
        assert_eq!(embedded.vectors, want);
        let error = embedded.failure.expect("the vector is short");
        assert!(error.is(VaultErrorCode::InvalidArgument));
        assert!(error.to_string().contains("vector 3"), "names the index");
    }

    #[tokio::test]
    async fn more_or_fewer_vectors_than_texts_is_invalid_argument() {
        for (extra, missing) in [(true, false), (false, true)] {
            let texts = texts(5);
            let script = Script {
                extra,
                missing,
                ..Script::new()
            };
            let embedded = embedder(script).await.embed(texts, || false).await;
            assert_eq!(embedded.vectors.len(), if extra { 5 } else { 4 });
            let error = embedded.into_result().unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
    }

    #[tokio::test]
    async fn the_texts_are_moved_to_the_embedder_and_never_cloned() {
        let texts = texts(12);
        let sent: Vec<usize> = texts.iter().map(|text| text.as_ptr() as usize).collect();
        let script = Script::new();
        let seen = Arc::clone(&script.seen);
        embedder(script)
            .await
            .embed(texts, || false)
            .await
            .into_result()
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), sent);
    }

    #[tokio::test]
    async fn a_query_vector_has_the_declared_length_and_the_query_is_not_empty() {
        let error = embedder(Script {
            query_values: DIMS + 1,
            ..Script::new()
        })
        .await
        .embed_query(String::from("what"))
        .await
        .unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = embedder(Script::new())
            .await
            .embed_query(String::new())
            .await
            .unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let port = embedder(Script::new()).await;
        assert_eq!(
            port.embed_query(String::from("what")).await.unwrap().len(),
            DIMS
        );
        assert_eq!(port.model_id(), "model-a");
        assert_eq!(port.dimensions(), DIMS);
    }

    #[tokio::test]
    async fn a_descriptor_that_breaks_its_rules_fails_the_install_after_one_describe() {
        for (model_id, dimensions) in [("model-a", 0), ("", 3)] {
            let script = Script {
                model_id: model_id.to_owned(),
                dimensions,
                ..Script::new()
            };
            let describes = Arc::clone(&script.describes);
            let error = install_embedder(&EmbedderRef::from_sync(script))
                .await
                .err()
                .unwrap();
            assert!(error.is(VaultErrorCode::InvalidArgument));
            assert_eq!(describes.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn describe_is_called_once_per_install_whatever_is_embedded_after() {
        let script = Script::new();
        let describes = Arc::clone(&script.describes);
        let port = embedder(script).await;
        port.embed(texts(4), || false).await.into_result().unwrap();
        port.embed_query(String::from("q")).await.unwrap();
        port.embed(texts(2), || false).await.into_result().unwrap();
        assert_eq!(describes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failure_of_describe_keeps_the_code_of_the_port() {
        struct Down;
        impl EmbedderSync for Down {
            fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
                Err(Failure::new(EmbedderErrorCode::Refused, "no").into())
            }
            fn embed(
                &self,
                _texts: BoxIter<'static, Result<Text, Error>>,
            ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
                Ok(Box::new(std::iter::empty()))
            }
            fn embed_query(&self, _query: Query) -> Result<Vector, Error> {
                Ok(Vector::default())
            }
        }
        let error = install_embedder(&EmbedderRef::from_sync(Down))
            .await
            .err()
            .unwrap();
        assert!(error.is(EmbedderErrorCode::Refused));
    }

    #[tokio::test]
    async fn an_embedder_a_reranker_and_a_projector_are_installed_and_removed() {
        for ports in [
            Ports {
                embedder: Some(EmbedderRef::from_sync(Script::new())),
                ..Ports::default()
            },
            Ports {
                reranker: Some(RerankerRef::from_sync(Judge::default())),
                ..Ports::default()
            },
            Ports {
                projector: Some(ProjectorRef::from_sync(proj(
                    Vec::new(),
                    IndexDoc::default(),
                ))),
                ..Ports::default()
            },
        ] {
            Installed::new(ports).await.unwrap();
        }
        let installed = Installed::default();
        installed
            .set_embedder(Some(EmbedderRef::from_sync(Script::new())))
            .await
            .unwrap();
        assert!(installed.embedder().is_some());
        installed.set_embedder(None).await.unwrap();
        installed.set_reranker(None).await.unwrap();
        installed.set_projector(None).await.unwrap();
        assert!(installed.embedder().is_none());
        assert!(installed.reranker().is_none());
        assert!(installed.projector().is_none());
    }

    /// An embedder with the portions of the same function as `FixedEmbedder`
    /// forms, written by hand on the stream.
    struct ByHand;

    impl EmbedderSync for ByHand {
        fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(EmbedderDescriptor::default()
                .with_model_id("model-a")
                .with_dimensions(DIMS as u32))
        }

        fn embed(
            &self,
            texts: BoxIter<'static, Result<Text, Error>>,
        ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
            Ok(Box::new(texts.map(|text| {
                Ok(Vector {
                    values: vector_for(&text?.text.unwrap_or_default()),
                    ..Vector::default()
                })
            })))
        }

        fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            Ok(Vector {
                values: vector_for(&query.text.unwrap_or_default()),
                ..Vector::default()
            })
        }
    }

    /// The two forms of a fixed-portion adapter.
    #[derive(Clone, Copy, Debug)]
    enum Form {
        Blocking,
        Async,
    }

    const FORMS: [Form; 2] = [Form::Blocking, Form::Async];

    /// A fixed embedder of `model-a` over `embed`, in `form`. The async one
    /// awaits a yield before it calls `embed`, so each of its futures waits
    /// once.
    fn fixed_embedder<F>(form: Form, portion: usize, embed: F) -> EmbedderRef
    where
        F: Fn(Vec<String>) -> Result<Vec<Vec<f32>>, Error> + Send + Sync + 'static,
    {
        let portion = NonZeroUsize::new(portion).unwrap();
        match form {
            Form::Blocking => {
                EmbedderRef::from_sync(FixedEmbedder::new("model-a", DIMS as u32, portion, embed))
            }
            Form::Async => {
                let embed = Arc::new(embed);
                EmbedderRef::from_async(FixedEmbedderAsync::new(
                    "model-a",
                    DIMS as u32,
                    portion,
                    move |texts| {
                        let embed = Arc::clone(&embed);
                        async move {
                            tokio::task::yield_now().await;
                            embed(texts)
                        }
                    },
                ))
            }
        }
    }

    /// A fixed reranker over `score`, in `form`, installed; the async one
    /// as [`fixed_embedder`].
    fn fixed_reranker<F>(form: Form, portion: usize, score: F) -> Arc<RerankerPort>
    where
        F: Fn(&str, Vec<Document>) -> Result<Vec<f64>, Error> + Send + Sync + 'static,
    {
        let portion = NonZeroUsize::new(portion).unwrap();
        let port = match form {
            Form::Blocking => RerankerRef::from_sync(FixedReranker::new(portion, score)),
            Form::Async => {
                let score = Arc::new(score);
                RerankerRef::from_async(FixedRerankerAsync::new(
                    portion,
                    move |query: Arc<str>, documents| {
                        let score = Arc::clone(&score);
                        async move {
                            tokio::task::yield_now().await;
                            score(&query, documents)
                        }
                    },
                ))
            }
        };
        Arc::new(RerankerPort {
            port: port.to_async_offload().unwrap(),
        })
    }

    #[tokio::test]
    async fn the_adapter_calls_its_function_with_portions_and_flushes_the_last_one() {
        for form in FORMS {
            let portions = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&portions);
            let adapter = fixed_embedder(form, 10, move |portion: Vec<String>| {
                log.lock().unwrap().push(portion.len());
                Ok(portion.iter().map(|text| vector_for(text)).collect())
            });
            let texts = texts(25);
            let port = install_embedder(&adapter).await.unwrap();
            let by_hand = install_embedder(&EmbedderRef::from_sync(ByHand))
                .await
                .unwrap();
            let got = port
                .embed(texts.clone(), || false)
                .await
                .into_result()
                .unwrap();
            assert_eq!(*portions.lock().unwrap(), [10, 10, 5], "{form:?}");
            assert_eq!(
                got,
                by_hand.embed(texts, || false).await.into_result().unwrap()
            );
            assert_eq!(
                port.embed_query(String::from("q")).await.unwrap(),
                vector_for("q")
            );
            assert_eq!(
                *portions.lock().unwrap(),
                [10, 10, 5, 1],
                "a portion of one, {form:?}"
            );
            assert_eq!(port.model_id(), "model-a");
            assert_eq!(port.dimensions(), DIMS);
        }
    }

    #[tokio::test]
    async fn the_adapter_fails_the_stream_when_its_function_miscounts_or_fails() {
        for form in FORMS {
            let count = install_embedder(&fixed_embedder(form, 4, |portion: Vec<String>| {
                Ok(portion
                    .iter()
                    .skip(1)
                    .map(|text| vector_for(text))
                    .collect())
            }))
            .await
            .unwrap();
            let embedded = count.embed(texts(6), || false).await;
            assert!(embedded.vectors.is_empty());
            let error = embedded.failure.unwrap();
            assert!(error.is(VaultErrorCode::InvalidArgument), "{form:?}");
            let error = count.embed_query(String::from("q")).await.unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument), "{form:?}");

            let calls = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&calls);
            let failing = fixed_embedder(form, 4, move |portion: Vec<String>| {
                if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                    return Err(Failure::new(EmbedderErrorCode::Unavailable, "down")
                        .with_retryable(true)
                        .into());
                }
                Ok(portion.iter().map(|text| vector_for(text)).collect())
            });
            let embedded = install_embedder(&failing)
                .await
                .unwrap()
                .embed(texts(10), || false)
                .await;
            assert_eq!(embedded.vectors.len(), 4, "the first portion is kept");
            let error = embedded.failure.unwrap();
            assert!(error.is(EmbedderErrorCode::Unavailable) && error.retryable());
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "the stream ends at the failure, {form:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_failed_text_ends_the_adapter_stream_and_its_portion_is_not_run() {
        let failed = || -> Result<Text, Error> {
            Err(Failure::new(EmbedderErrorCode::Unavailable, "no text").into())
        };
        let items = || {
            vec![
                Ok(Text::default().with_text("a")),
                Ok(Text::default().with_text("b")),
                Ok(Text::default().with_text("c")),
                failed(),
                Ok(Text::default().with_text("d")),
            ]
        };
        let embed = |log: Arc<Mutex<Vec<usize>>>| {
            move |portion: Vec<String>| {
                log.lock().unwrap().push(portion.len());
                Ok(portion.iter().map(|text| vector_for(text)).collect())
            }
        };
        let portion = NonZeroUsize::new(2).unwrap();

        let blocking_log = Arc::new(Mutex::new(Vec::new()));
        let adapter = FixedEmbedder::new("m", 3, portion, embed(Arc::clone(&blocking_log)));
        let blocking: Vec<Result<Vector, Error>> =
            EmbedderSync::embed(&adapter, Box::new(items().into_iter()))
                .unwrap()
                .collect();

        let async_log = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&async_log);
        let adapter = FixedEmbedderAsync::new("m", 3, portion, move |texts| {
            std::future::ready(embed(Arc::clone(&log))(texts))
        });
        let reply = EmbedderAsync::embed(&adapter, IterStream::new(items().into_iter()))
            .await
            .unwrap();
        let mut reply = std::pin::pin!(reply);
        let mut asynced = Vec::new();
        while let Some(item) = next(reply.as_mut()).await {
            asynced.push(item);
        }

        for (got, log) in [(blocking, blocking_log), (asynced, async_log)] {
            assert_eq!(got.len(), 3, "two vectors, then the failure, then the end");
            assert!(got[..2].iter().all(Result::is_ok));
            assert!(
                got[2]
                    .as_ref()
                    .unwrap_err()
                    .is(EmbedderErrorCode::Unavailable)
            );
            assert_eq!(*log.lock().unwrap(), [2], "`c` is never run");
        }
    }

    #[tokio::test]
    async fn the_async_adapters_await_their_futures_on_the_runtime_thread() {
        let here = std::thread::current().id();
        let threads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&threads);
        let embedder = FixedEmbedderAsync::new(
            "model-a",
            DIMS as u32,
            NonZeroUsize::new(2).unwrap(),
            move |texts: Vec<String>| {
                let seen = Arc::clone(&seen);
                async move {
                    // A timer of this current-thread runtime: it fires only
                    // while nothing holds the one thread.
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    seen.lock().unwrap().push(std::thread::current().id());
                    Ok(texts.iter().map(|text| vector_for(text)).collect())
                }
            },
        );
        let port = install_embedder(&EmbedderRef::from_async(embedder))
            .await
            .unwrap();
        let got = port.embed(texts(5), || false).await.into_result().unwrap();
        assert_eq!(got, expected(&texts(5)));
        assert_eq!(
            port.embed_query(String::from("q")).await.unwrap(),
            vector_for("q")
        );

        let seen = Arc::clone(&threads);
        let reranker = RerankerRef::from_async(FixedRerankerAsync::new(
            NonZeroUsize::new(2).unwrap(),
            move |_query: Arc<str>, documents: Vec<Document>| {
                let seen = Arc::clone(&seen);
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    seen.lock().unwrap().push(std::thread::current().id());
                    Ok(vec![1.0; documents.len()])
                }
            },
        ));
        let reranker = RerankerPort {
            port: reranker.to_async_offload().unwrap(),
        };
        let documents = vec![document("a", "1"), document("b", "2"), document("c", "3")];
        let scores = reranker.score(String::from("q"), documents).await.unwrap();
        assert_eq!(scores, [1.0, 1.0, 1.0]);

        let threads = threads.lock().unwrap();
        assert_eq!(
            threads.len(),
            3 + 1 + 2,
            "the portions of both, and the query"
        );
        assert!(threads.iter().all(|thread| *thread == here));
    }

    fn document(id: &str, raw: &str) -> Document {
        Document::default()
            .with_collection("notes")
            .with_id(id)
            .with_raw(raw.as_bytes().to_vec())
    }

    /// A reranker that scores a Document by the length of its `raw`, and
    /// keeps what it was sent.
    #[derive(Default)]
    struct Judge {
        /// What the stream held, in order: the query, then the ids.
        sent: Arc<Mutex<Vec<String>>>,
        /// Gives one score too few.
        drop_one: bool,
        /// Fails with this code instead of answering.
        refuse: bool,
    }

    impl RerankerSync for Judge {
        fn score(
            &self,
            score_items: BoxIter<'static, Result<ScoreItem, Error>>,
        ) -> Result<BoxIter<'static, Result<DocumentScore, Error>>, Error> {
            if self.refuse {
                return Err(Failure::new(RerankerErrorCode::Refused, "no").into());
            }
            let mut scores = Vec::new();
            for item in score_items {
                match item?.item {
                    Some(Item::Query(query)) => self.sent.lock().unwrap().push(query),
                    Some(Item::Document(document)) => {
                        let raw = document.raw.unwrap_or_default();
                        self.sent
                            .lock()
                            .unwrap()
                            .push(document.id.unwrap_or_default());
                        scores.push(Ok(DocumentScore::default().with_score(raw.len() as f64)));
                    }
                    None => {}
                }
            }
            if self.drop_one {
                scores.pop();
            }
            Ok(Box::new(scores.into_iter()))
        }
    }

    fn reranker(judge: impl RerankerSync + 'static) -> Arc<RerankerPort> {
        let port = RerankerRef::from_sync(judge).to_async_offload().unwrap();
        Arc::new(RerankerPort { port })
    }

    #[tokio::test]
    async fn a_reranker_gets_the_query_first_and_scores_each_document_in_order() {
        let judge = Judge::default();
        let sent = Arc::clone(&judge.sent);
        let documents = vec![
            document("a", "1"),
            document("b", "123"),
            document("c", "12"),
        ];
        let scores = reranker(judge)
            .score(String::from("q"), documents)
            .await
            .unwrap();
        assert_eq!(scores, [1.0, 3.0, 2.0]);
        assert_eq!(*sent.lock().unwrap(), ["q", "a", "b", "c"]);
    }

    #[tokio::test]
    async fn a_reranker_that_miscounts_is_invalid_argument_and_a_refusal_keeps_its_code() {
        let documents = || vec![document("a", "1"), document("b", "2")];
        let error = reranker(Judge {
            drop_one: true,
            ..Judge::default()
        })
        .score(String::from("q"), documents())
        .await
        .unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = reranker(Judge {
            refuse: true,
            ..Judge::default()
        })
        .score(String::from("q"), documents())
        .await
        .unwrap_err();
        assert!(error.is(RerankerErrorCode::Refused));
        assert!(!error.retryable());
        let scores = reranker(Judge::default())
            .score(String::from("q"), Vec::new())
            .await
            .unwrap();
        assert!(scores.is_empty());
    }

    #[tokio::test]
    async fn the_reranker_adapter_scores_in_portions_and_in_order() {
        for form in FORMS {
            let portions = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&portions);
            let adapter = fixed_reranker(form, 3, move |query: &str, portion: Vec<Document>| {
                log.lock().unwrap().push((query.to_owned(), portion.len()));
                Ok(portion
                    .iter()
                    .map(|document| document.raw.as_deref().unwrap_or_default().len() as f64)
                    .collect())
            });
            let documents: Vec<Document> = (1..=7)
                .map(|n| document(&format!("d{n}"), &"x".repeat(n)))
                .collect();
            let scores = adapter.score(String::from("q"), documents).await.unwrap();
            assert_eq!(scores, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
            let log = portions.lock().unwrap();
            assert_eq!(
                *log,
                [
                    ("q".to_owned(), 3),
                    ("q".to_owned(), 3),
                    ("q".to_owned(), 1)
                ],
                "{form:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_reranker_adapter_fails_the_stream_when_its_function_miscounts_or_fails() {
        let documents =
            || -> Vec<Document> { (1..=5).map(|n| document(&format!("d{n}"), "x")).collect() };
        for form in FORMS {
            let short = fixed_reranker(form, 2, |_: &str, portion: Vec<Document>| {
                Ok(vec![0.0; portion.len() - 1])
            });
            let error = short
                .score(String::from("q"), documents())
                .await
                .unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument), "{form:?}");

            let calls = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&calls);
            let failing = fixed_reranker(form, 2, move |_: &str, portion: Vec<Document>| {
                if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                    return Err(Failure::new(RerankerErrorCode::Refused, "no").into());
                }
                Ok(vec![0.0; portion.len()])
            });
            let error = failing
                .score(String::from("q"), documents())
                .await
                .unwrap_err();
            assert!(error.is(RerankerErrorCode::Refused), "{form:?}");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "the stream ends at the failure, {form:?}"
            );
        }
    }

    /// A projector that records the address of the bytes it was given.
    struct Proj {
        schema: IndexSchema,
        values: IndexDoc,
        raw_at: Arc<Mutex<Vec<usize>>>,
        fail: bool,
    }

    impl ProjectorSync for Proj {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("proj@1"))
        }

        fn columns(&self, _collection_key: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(self.schema.clone())
        }

        fn project(&self, raw_document: RawDocument) -> Result<IndexDoc, Error> {
            if self.fail {
                return Err(Failure::new(ProjectorErrorCode::Refused, "no").into());
            }
            let raw = raw_document.raw.unwrap_or_default();
            self.raw_at.lock().unwrap().push(raw.as_ptr() as usize);
            Ok(self.values.clone())
        }
    }

    #[tokio::test]
    async fn a_projector_describes_itself_once_and_a_descriptor_that_breaks_its_rules_is_refused() {
        let port = projector(proj(Vec::new(), IndexDoc::default())).await;
        assert_eq!(port.projection_id(), "proj@1");

        struct Blank;

        impl ProjectorSync for Blank {
            fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
                Ok(ProjectorDescriptor::default())
            }

            fn columns(&self, _collection_key: CollectionKey) -> Result<IndexSchema, Error> {
                Ok(IndexSchema::default())
            }

            fn project(&self, _raw_document: RawDocument) -> Result<IndexDoc, Error> {
                Ok(IndexDoc::default())
            }
        }

        let error = install_projector(&ProjectorRef::from_sync(Blank))
            .await
            .err()
            .unwrap();
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    fn column(name: &str) -> IndexColumn {
        IndexColumn::default()
            .with_name(name)
            .with_kind(ColumnKind::COLUMN_KIND_TEXT)
            .with_scalar_index(true)
    }

    async fn projector(proj: Proj) -> Arc<ProjectorPort> {
        Arc::new(
            install_projector(&ProjectorRef::from_sync(proj))
                .await
                .unwrap(),
        )
    }

    fn proj(columns: Vec<IndexColumn>, values: IndexDoc) -> Proj {
        Proj {
            schema: IndexSchema {
                columns,
                ..IndexSchema::default()
            },
            values,
            raw_at: Arc::default(),
            fail: false,
        }
    }

    fn one_value() -> IndexDoc {
        let mut doc = IndexDoc::default();
        doc.values.insert(
            String::from("title"),
            IndexValue {
                value: Some(scheda_api::v1::index_value::Value::Text(String::from("t"))),
                ..IndexValue::default()
            },
        );
        doc
    }

    #[tokio::test]
    async fn the_projector_gets_raw_moved_and_an_empty_result_leaves_the_document_out() {
        let with_values = proj(vec![column("title")], one_value());
        let seen = Arc::clone(&with_values.raw_at);
        let port = projector(with_values).await;
        let raw = b"# a note".to_vec();
        let at = raw.as_ptr() as usize;
        let doc = port
            .project(String::from("notes"), String::from("a.md"), raw)
            .await
            .unwrap();
        assert!(doc.is_some_and(|doc| doc.values.contains_key("title")));
        assert_eq!(*seen.lock().unwrap(), [at], "moved, not copied");

        let empty = projector(proj(vec![column("title")], IndexDoc::default())).await;
        let doc = empty
            .project(String::from("notes"), String::from("a.md"), Vec::new())
            .await
            .unwrap();
        assert!(doc.is_none());
    }

    #[tokio::test]
    async fn columns_are_checked_and_none_leaves_the_collection_out() {
        let schema = projector(proj(
            vec![column("title"), column("body")],
            IndexDoc::default(),
        ))
        .await
        .columns(None, String::from("notes"))
        .await
        .unwrap()
        .unwrap();
        assert_eq!(schema.columns.len(), 2);
        let none = projector(proj(Vec::new(), IndexDoc::default()))
            .await
            .columns(None, String::from("notes"))
            .await
            .unwrap();
        assert!(none.is_none());
        for columns in [
            vec![column("title"), column("title")],
            vec![column("id")],
            vec![column("collection")],
            vec![column("")],
            vec![column("title").with_kind(ColumnKind::COLUMN_KIND_UNSPECIFIED)],
            vec![IndexColumn {
                kind: Some(buffa::EnumValue::Unknown(9)),
                ..column("title")
            }],
        ] {
            let error = projector(proj(columns, IndexDoc::default()))
                .await
                .columns(None, String::from("notes"))
                .await
                .unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
    }

    #[tokio::test]
    async fn a_projector_failure_keeps_its_code() {
        let mut failing = proj(vec![column("title")], IndexDoc::default());
        failing.fail = true;
        let error = projector(failing)
            .await
            .project(String::from("notes"), String::from("a.md"), Vec::new())
            .await
            .unwrap_err();
        assert!(error.is(ProjectorErrorCode::Refused));
    }

    /// An async embedder that reads its texts with `poll_next`.
    struct Waiting;

    impl EmbedderAsync for Waiting {
        async fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(EmbedderDescriptor::default()
                .with_model_id("model-async")
                .with_dimensions(DIMS as u32))
        }

        async fn embed<R>(
            &self,
            texts: R,
        ) -> Result<impl Stream<Item = Result<Vector, Error>> + Send + use<R>, Error>
        where
            R: Stream<Item = Result<Text, Error>> + Send + 'static,
        {
            let mut texts = Box::pin(texts);
            let mut reply = Vec::new();
            while let Some(text) = std::future::poll_fn(|cx| texts.as_mut().poll_next(cx)).await {
                reply.push(Ok(Vector {
                    values: vector_for(&text?.text.unwrap_or_default()),
                    ..Vector::default()
                }));
            }
            Ok(IterStream::new(reply.into_iter()))
        }

        async fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            Ok(Vector {
                values: vector_for(&query.text.unwrap_or_default()),
                ..Vector::default()
            })
        }
    }

    #[tokio::test]
    async fn a_reference_without_an_implementation_is_invalid_argument() {
        let error = install_embedder(&EmbedderRef::default())
            .await
            .err()
            .unwrap();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = install_projector(&ProjectorRef::default())
            .await
            .err()
            .unwrap();
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[tokio::test]
    async fn an_async_embedder_is_awaited_on_either_runtime() {
        let port = install_embedder(&EmbedderRef::from_async(Waiting))
            .await
            .unwrap();
        assert_eq!(port.model_id(), "model-async");
        let texts = texts(9);
        let want = expected(&texts);
        assert_eq!(
            port.embed(texts, || false).await.into_result().unwrap(),
            want
        );
        assert_eq!(
            port.embed_query(String::from("q")).await.unwrap(),
            vector_for("q")
        );
    }

    #[tokio::test]
    async fn a_port_that_panics_fails_the_call_with_panicked() {
        struct Boom;

        impl EmbedderSync for Boom {
            fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
                Ok(EmbedderDescriptor::default()
                    .with_model_id("boom")
                    .with_dimensions(3))
            }

            fn embed(
                &self,
                _texts: BoxIter<'static, Result<Text, Error>>,
            ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
                panic!("the model exploded")
            }

            fn embed_query(&self, _query: Query) -> Result<Vector, Error> {
                panic!("the model exploded")
            }
        }

        let port = install_embedder(&EmbedderRef::from_sync(Boom))
            .await
            .unwrap();
        let error = port.embed_query(String::from("q")).await.unwrap_err();
        assert!(error.is(protocontract::RuntimeCode::Panicked), "{error}");
        let embedded = port.embed(texts(2), || false).await;
        assert!(embedded.vectors.is_empty());
        let error = embedded.failure.unwrap();
        assert!(error.is(protocontract::RuntimeCode::Panicked), "{error}");
    }
}

#[cfg(all(test, not(feature = "index")))]
mod thin_tests {
    use scheda_api::v1::embedder_ref::EmbedderRef;

    use super::*;

    #[tokio::test]
    async fn a_build_without_the_index_takes_no_port_and_removing_one_is_nothing() {
        let embedder = FixedEmbedder::new(
            "m",
            2,
            NonZeroUsize::new(1).unwrap(),
            |texts: Vec<String>| Ok(texts.iter().map(|_| vec![0.0, 0.0]).collect()),
        );
        let error = Installed::new(Ports {
            embedder: Some(EmbedderRef::from_sync(embedder)),
            ..Ports::default()
        })
        .await
        .err()
        .unwrap();
        assert!(error.is(VaultErrorCode::Unimplemented));
        let installed = Installed::default();
        installed.set_embedder(None).await.unwrap();
        installed.set_reranker(None).await.unwrap();
        installed.set_projector(None).await.unwrap();
    }
}
