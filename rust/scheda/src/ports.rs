//! The ports of a handle: the services the caller implements and Scheda
//! calls.
//!
//! An [`Embedder`](EmbedderSync) turns texts into vectors, a
//! [`Reranker`](RerankerSync) scores Documents against a query, and a
//! [`Projector`](ProjectorSync) says what of a Document the Index holds. A
//! caller gives each in the form its implementation has: [`Port::Sync`] for
//! the blocking trait, [`Port::Async`] (feature `tokio`) for the async one.
//! Scheda bridges between the two, so the caller wraps nothing.
//!
//! The rest of this module is what Scheda does with a port once it holds
//! one. It installs the ports (checks the build can use them, reads and
//! validates the descriptor of an embedder once) and calls them through one
//! place each: the texts of an embedder and the Documents of a reranker
//! travel as streams, one item at a time, moved and never cloned; each
//! reply is checked (the length of a vector, the number of vectors and
//! scores) before anything uses it; a failure of the port keeps its code and
//! its retry flag and gains a stack frame.

// The indexer and the search that call the ports are not built yet. Each
// layer that uses an item makes the expectation fail while one is unused,
// which is the reminder to drop it.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the indexer and the search that call the ports are not built yet"
    )
)]

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda_api::v1::score_item::Item;
use scheda_api::v1::{
    CollectionKey, Document, DocumentScore, DynEmbedderAsync, DynProjectorAsync, DynRerankerAsync,
    EmbedderDescriptor, EmbedderSync, IndexDoc, IndexSchema, ProjectorSync, Query, RawDocument,
    RerankerSync, ScoreItem, Text, VaultErrorCode, Vector,
};

use crate::error::{self, Context, Error, Fault};
use crate::stored;

/// A port in the form its implementation has.
///
/// `S` is the blocking trait of the port (`dyn EmbedderSync`, …) and `A` the
/// async one behind dynamic dispatch (`DynEmbedderAsync`, …). Scheda bridges
/// between them: a blocking call of the Vault calls an async port through
/// `protocontract::Blocking`, so the caller wraps nothing.
///
/// A build without the `tokio` feature has no `Async` form: nothing in it
/// could run a future, and `Sync` is the only variant a caller can build.
pub enum Port<S: ?Sized, A> {
    /// An implementation of the blocking trait.
    Sync(Arc<S>),
    /// An implementation of the async trait. Scheda calls it on the Tokio
    /// runtime that is current when the port is installed, so installing
    /// one outside a runtime is `INVALID_ARGUMENT`.
    #[cfg(feature = "tokio")]
    Async(A),
    /// Never built: keeps `A` in use in a build without `Async`. A `match`
    /// on a `Port` taken by value needs no arm for it.
    #[cfg(not(feature = "tokio"))]
    #[doc(hidden)]
    Unreachable(
        std::convert::Infallible,
        std::marker::PhantomData<fn() -> A>,
    ),
}

impl<S: ?Sized, A: Clone> Clone for Port<S, A> {
    fn clone(&self) -> Self {
        match self {
            Port::Sync(port) => Port::Sync(Arc::clone(port)),
            #[cfg(feature = "tokio")]
            Port::Async(port) => Port::Async(port.clone()),
            #[cfg(not(feature = "tokio"))]
            Port::Unreachable(never, _) => match *never {},
        }
    }
}

impl<S: ?Sized, A> fmt::Debug for Port<S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Port::Sync(_) => f.write_str("Port::Sync"),
            #[cfg(feature = "tokio")]
            Port::Async(_) => f.write_str("Port::Async"),
            #[cfg(not(feature = "tokio"))]
            Port::Unreachable(never, _) => match *never {},
        }
    }
}

/// The ports a handle is opened with. A port left out is a service the
/// handle does without.
///
/// An embedder and a reranker need a build with the `search` feature, a
/// projector one with the `find` feature; a port given to a build that
/// cannot use it is `UNIMPLEMENTED`.
#[derive(Clone, Debug, Default)]
pub struct Ports {
    /// Turns texts into vectors for the vector leg of a search. Its
    /// descriptor is read once, when it is installed.
    pub embedder: Option<Port<dyn EmbedderSync, DynEmbedderAsync>>,
    /// Scores Documents against a query, for a search that reranks.
    pub reranker: Option<Port<dyn RerankerSync, DynRerankerAsync>>,
    /// Says which columns the Index holds and their values.
    pub projector: Option<Port<dyn ProjectorSync, DynProjectorAsync>>,
}

/// The ports of one handle, installed: each in the blocking form, the
/// embedder with the descriptor it declared.
#[derive(Default)]
pub(crate) struct Installed {
    embedder: Mutex<Option<Arc<EmbedderPort>>>,
    reranker: Option<Arc<RerankerPort>>,
    projector: Option<Arc<ProjectorPort>>,
}

impl fmt::Debug for Installed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Installed")
            .field("reranker", &self.reranker.is_some())
            .field("projector", &self.projector.is_some())
            .finish_non_exhaustive()
    }
}

impl Installed {
    /// Installs `ports`: checks that the build can use them, bridges an
    /// async port to the blocking form, and reads and validates the
    /// descriptor of the embedder.
    ///
    /// # Errors
    ///
    /// `UNIMPLEMENTED` for a port the build cannot use; `INVALID_ARGUMENT`
    /// for an async port outside a Tokio runtime and for a descriptor that
    /// breaks its rules; the port's own failure when `Describe` fails.
    pub(crate) fn new(ports: Ports) -> Result<Installed, Error> {
        require_build(
            ports.embedder.is_some(),
            ports.reranker.is_some(),
            ports.projector.is_some(),
        )?;
        let embedder = ports
            .embedder
            .map(install_embedder)
            .transpose()
            .context("install the embedder")?;
        let reranker = ports
            .reranker
            .map(|port| reranker_sync(port).map(|sync| Arc::new(RerankerPort { sync })))
            .transpose()
            .context("install the reranker")?;
        let projector = ports
            .projector
            .map(|port| projector_sync(port).map(|sync| Arc::new(ProjectorPort { sync })))
            .transpose()
            .context("install the projector")?;
        Ok(Installed {
            embedder: Mutex::new(embedder.map(Arc::new)),
            reranker,
            projector,
        })
    }

    /// Replaces the embedder; `None` removes it. The new embedder is
    /// installed (and its descriptor read) before the old one is let go, so
    /// a failure leaves the old one in place.
    ///
    /// # Errors
    ///
    /// As [`Installed::new`] for the embedder.
    pub(crate) fn set_embedder(
        &self,
        embedder: Option<Port<dyn EmbedderSync, DynEmbedderAsync>>,
    ) -> Result<(), Error> {
        require_build(embedder.is_some(), false, false)?;
        let installed = embedder
            .map(install_embedder)
            .transpose()
            .context("install the embedder")?
            .map(Arc::new);
        *error::mutex(&self.embedder, "replace the embedder")? = installed;
        Ok(())
    }

    /// The embedder now installed, if any. A call in progress keeps the one
    /// it started with.
    pub(crate) fn embedder(&self) -> Result<Option<Arc<EmbedderPort>>, Error> {
        Ok(error::mutex(&self.embedder, "read the embedder")?.clone())
    }

    /// The reranker, if the handle has one.
    pub(crate) fn reranker(&self) -> Option<&Arc<RerankerPort>> {
        self.reranker.as_ref()
    }

    /// The projector, if the handle has one.
    pub(crate) fn projector(&self) -> Option<&Arc<ProjectorPort>> {
        self.projector.as_ref()
    }
}

/// `UNIMPLEMENTED` naming the feature when a port is given to a build that
/// cannot use it.
fn require_build(embedder: bool, reranker: bool, projector: bool) -> Result<(), Error> {
    let unimplemented = |what: &str, feature: &str| -> Error {
        Fault::new(
            VaultErrorCode::Unimplemented,
            format!("{what} needs a build with the `{feature}` feature"),
        )
        .into()
    };
    if cfg!(not(feature = "search")) && embedder {
        return Err(unimplemented("an embedder", "search"));
    }
    if cfg!(not(feature = "search")) && reranker {
        return Err(unimplemented("a reranker", "search"));
    }
    if cfg!(not(feature = "find")) && projector {
        return Err(unimplemented("a projector", "find"));
    }
    Ok(())
}

/// The Tokio runtime an async port is called on.
#[cfg(feature = "tokio")]
fn current_handle() -> Result<tokio::runtime::Handle, Error> {
    tokio::runtime::Handle::try_current().map_err(|err| {
        Fault::new(
            VaultErrorCode::InvalidArgument,
            "an async port needs a Tokio runtime, and none is current; install it from inside \
             a runtime or give a blocking port",
        )
        .with_source(err)
        .into()
    })
}

/// The blocking form of an embedder: the port itself, or an async port
/// behind `Blocking` on the current Tokio runtime. The one place that
/// chooses between the two.
fn embedder_sync(
    port: Port<dyn EmbedderSync, DynEmbedderAsync>,
) -> Result<Arc<dyn EmbedderSync>, Error> {
    match port {
        Port::Sync(port) => Ok(port),
        #[cfg(feature = "tokio")]
        Port::Async(port) => Ok(Arc::new(protocontract::Blocking::new(
            port,
            current_handle()?,
        ))),
    }
}

/// The blocking form of a reranker, as [`embedder_sync`].
fn reranker_sync(
    port: Port<dyn RerankerSync, DynRerankerAsync>,
) -> Result<Arc<dyn RerankerSync>, Error> {
    match port {
        Port::Sync(port) => Ok(port),
        #[cfg(feature = "tokio")]
        Port::Async(port) => Ok(Arc::new(protocontract::Blocking::new(
            port,
            current_handle()?,
        ))),
    }
}

/// The blocking form of a projector, as [`embedder_sync`].
fn projector_sync(
    port: Port<dyn ProjectorSync, DynProjectorAsync>,
) -> Result<Arc<dyn ProjectorSync>, Error> {
    match port {
        Port::Sync(port) => Ok(port),
        #[cfg(feature = "tokio")]
        Port::Async(port) => Ok(Arc::new(protocontract::Blocking::new(
            port,
            current_handle()?,
        ))),
    }
}

/// `INVALID_ARGUMENT` for a message a port gave that breaks its rules.
fn invalid_reply(what: &str, fault: Fault) -> Fault {
    Fault::new(
        VaultErrorCode::InvalidArgument,
        format!("{what} is not valid: {}", fault.message()),
    )
    .with_source(fault)
}

/// An installed embedder: the port in the blocking form, and the descriptor
/// it declared when it was installed. Nothing asks the port to describe
/// itself again.
pub(crate) struct EmbedderPort {
    sync: Arc<dyn EmbedderSync>,
    descriptor: EmbedderDescriptor,
    /// `descriptor.dimensions`, as a length.
    dimensions: usize,
}

/// Reads and validates the descriptor of `port` once.
fn install_embedder(port: Port<dyn EmbedderSync, DynEmbedderAsync>) -> Result<EmbedderPort, Error> {
    let sync = embedder_sync(port)?;
    let descriptor = sync
        .describe(Empty::default())
        .context("Embedder::describe: ask the embedder")?;
    stored::validate(&descriptor)
        .map_err(|fault| invalid_reply("the descriptor of the embedder", fault))
        .context("Embedder::describe: check the descriptor")?;
    let dimensions =
        usize::try_from(descriptor.dimensions.unwrap_or_default()).unwrap_or(usize::MAX);
    Ok(EmbedderPort {
        sync,
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
#[derive(Debug, Default)]
pub(crate) struct Embedded {
    /// One vector per text, for the texts from the first on, in order.
    pub(crate) vectors: Vec<Vec<f32>>,
    /// Why the vectors stop short of the texts, if they do.
    pub(crate) failure: Option<Error>,
}

impl Embedded {
    /// All the vectors, or the failure when the call stopped short.
    ///
    /// # Errors
    ///
    /// The failure that ended the call.
    pub(crate) fn into_result(self) -> Result<Vec<Vec<f32>>, Error> {
        match self.failure {
            None => Ok(self.vectors),
            Some(error) => Err(error),
        }
    }
}

/// Checks the vectors of one `Embed` call as they come, so a blocking read
/// and an awaiting one share the checks.
struct Gathering {
    dimensions: usize,
    expected: usize,
    out: Embedded,
}

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
    /// code and the retry flag of the port.
    pub(crate) fn embed(&self, texts: Vec<String>) -> Embedded {
        let expected = texts.len();
        let mut gathering = Gathering::new(self.dimensions, expected);
        if expected == 0 {
            return gathering.finish();
        }
        let input: BoxIter<'static, Result<Text, Error>> = Box::new(
            texts
                .into_iter()
                .map(|text| Ok(Text::default().with_text(text))),
        );
        let reply = match self.sync.embed(input) {
            Ok(reply) => reply,
            Err(error) => {
                gathering.fail(error);
                return gathering.finish();
            }
        };
        for item in reply {
            if !gathering.accept(item) {
                return gathering.finish();
            }
        }
        gathering.finish()
    }

    /// The vector of a search text.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for an empty text and for a vector of the wrong
    /// length; the failure of the port otherwise.
    pub(crate) fn embed_query(&self, text: String) -> Result<Vec<f32>, Error> {
        let query = Query::default().with_text(text);
        stored::validate(&query).context("Embedder::embed_query: check the query")?;
        let vector = self
            .sync
            .embed_query(query)
            .context("Embedder::embed_query: ask the embedder")?;
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

/// An installed reranker, in the blocking form.
pub(crate) struct RerankerPort {
    sync: Arc<dyn RerankerSync>,
}

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
    pub(crate) fn score(&self, query: String, documents: Vec<Document>) -> Result<Vec<f64>, Error> {
        let expected = documents.len();
        if expected == 0 {
            return Ok(Vec::new());
        }
        let items: BoxIter<'static, Result<ScoreItem, Error>> = Box::new(
            std::iter::once(Item::Query(query))
                .chain(documents.into_iter().map(Item::from))
                .map(|item| {
                    Ok(ScoreItem {
                        item: Some(item),
                        ..ScoreItem::default()
                    })
                }),
        );
        let reply = self
            .sync
            .score(items)
            .context("Reranker::score: ask the reranker")?;
        let mut scores = Vec::with_capacity(expected);
        for item in reply {
            let score = item.context("Reranker::score: read the scores")?;
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
    }
}

/// An installed projector, in the blocking form.
pub(crate) struct ProjectorPort {
    sync: Arc<dyn ProjectorSync>,
}

impl ProjectorPort {
    /// The columns of `collection`, or `None` when the projector declares
    /// none: its Documents are indexed by identity only.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the columns break the rules of an
    /// `IndexSchema` (a name used twice, `id` or `collection` as a name, an
    /// unset kind); the failure of the port otherwise.
    pub(crate) fn columns(&self, collection: String) -> Result<Option<IndexSchema>, Error> {
        let schema = self
            .sync
            .columns(CollectionKey::default().with_name(collection))
            .context("Projector::columns: ask the projector")?;
        stored::validate(&schema)
            .map_err(|fault| invalid_reply("the columns of the projector", fault))
            .context("Projector::columns: check the columns")?;
        Ok((!schema.columns.is_empty()).then_some(schema))
    }

    /// The values of the columns for one Document, or `None` when the
    /// projector gives none: the Document is indexed by identity only. `raw` is
    /// moved into the call.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when a value breaks the rules of an `IndexValue`;
    /// the failure of the port otherwise.
    pub(crate) fn project(
        &self,
        collection: String,
        id: String,
        raw: Vec<u8>,
    ) -> Result<Option<IndexDoc>, Error> {
        let document = RawDocument::default()
            .with_collection(collection)
            .with_id(id)
            .with_raw(raw);
        let values = self
            .sync
            .project(document)
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
/// vectors back in order. `EmbedQuery` is a portion of one.
///
/// The function must return one vector per text of its portion, in order;
/// a count that does not match fails the stream with `INVALID_ARGUMENT`.
/// A failure of the function ends the stream.
pub struct FixedEmbedder<F> {
    model_id: String,
    dimensions: u32,
    portion: NonZeroUsize,
    embed: Arc<F>,
}

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
        }
    }
}

impl<F> EmbedderSync for FixedEmbedder<F>
where
    F: Fn(Vec<String>) -> Result<Vec<Vec<f32>>, Error> + Send + Sync + 'static,
{
    fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
        Ok(EmbedderDescriptor::default()
            .with_model_id(self.model_id.clone())
            .with_dimensions(self.dimensions))
    }

    fn embed(
        &self,
        texts: BoxIter<'static, Result<Text, Error>>,
    ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
        let embed = Arc::clone(&self.embed);
        let input = texts.map(|text| text.map(|text| text.text.unwrap_or_default()));
        Ok(Box::new(Portions::new(
            Box::new(input),
            self.portion,
            Box::new(move |portion| {
                Ok(embed(portion)?
                    .into_iter()
                    .map(|values| Vector {
                        values,
                        ..Vector::default()
                    })
                    .collect())
            }),
        )))
    }

    fn embed_query(&self, query: scheda_api::v1::Query) -> Result<Vector, Error> {
        let mut vectors = (self.embed)(vec![query.text.unwrap_or_default()])?;
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
/// `INVALID_ARGUMENT`. A failure of the function ends the stream.
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
        let query = match score_items.next() {
            Some(Ok(ScoreItem {
                item: Some(Item::Query(query)),
                ..
            })) => query,
            Some(Err(error)) => return Err(error),
            _ => {
                return Err(Fault::new(
                    VaultErrorCode::InvalidArgument,
                    "the first item of a score stream is the query",
                )
                .into());
            }
        };
        let documents = score_items.map(|item| match item?.item {
            Some(Item::Document(document)) => Ok(*document),
            _ => Err(Error::from(Fault::new(
                VaultErrorCode::InvalidArgument,
                "every item of a score stream after the first is a Document",
            ))),
        });
        let score = Arc::clone(&self.score);
        Ok(Box::new(Portions::new(
            Box::new(documents),
            self.portion,
            Box::new(move |portion| {
                Ok(score(&query, portion)?
                    .into_iter()
                    .map(|score| DocumentScore::default().with_score(score))
                    .collect())
            }),
        )))
    }
}

/// What a portion function does: one reply per item of the portion.
type PortionFn<X, Y> = Box<dyn Fn(Vec<X>) -> Result<Vec<Y>, Error> + Send + Sync>;

/// A reply stream made by cutting an input stream into portions of at most
/// `size` items, calling a function on each, and giving its results one by
/// one. The input is read as the replies are asked for; when it ends, the
/// last partial portion goes to the function too.
struct Portions<X, Y> {
    /// `None` once the input ended or the stream failed.
    input: Option<BoxIter<'static, Result<X, Error>>>,
    size: usize,
    run: PortionFn<X, Y>,
    ready: VecDeque<Y>,
}

impl<X, Y> Portions<X, Y> {
    fn new(
        input: BoxIter<'static, Result<X, Error>>,
        size: NonZeroUsize,
        run: PortionFn<X, Y>,
    ) -> Portions<X, Y> {
        Portions {
            input: Some(input),
            size: size.get(),
            run,
            ready: VecDeque::new(),
        }
    }
}

impl<X, Y> Iterator for Portions<X, Y> {
    type Item = Result<Y, Error>;

    fn next(&mut self) -> Option<Result<Y, Error>> {
        loop {
            if let Some(reply) = self.ready.pop_front() {
                return Some(Ok(reply));
            }
            let input = self.input.as_mut()?;
            let mut portion = Vec::with_capacity(self.size);
            let mut ended = false;
            let mut failure = None;
            while portion.len() < self.size {
                match input.next() {
                    Some(Ok(item)) => portion.push(item),
                    Some(Err(error)) => {
                        failure = Some(error);
                        break;
                    }
                    None => {
                        ended = true;
                        break;
                    }
                }
            }
            if failure.is_some() || ended {
                self.input = None;
            }
            if let Some(error) = failure {
                return Some(Err(error));
            }
            if portion.is_empty() {
                return None;
            }
            let count = portion.len();
            match (self.run)(portion) {
                Ok(replies) if replies.len() == count => self.ready.extend(replies),
                Ok(replies) => {
                    self.input = None;
                    return Some(Err(Fault::new(
                        VaultErrorCode::InvalidArgument,
                        format!(
                            "the portion function gave {} results for a portion of {count}",
                            replies.len()
                        ),
                    )
                    .into()));
                }
                Err(error) => {
                    self.input = None;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use protocontract::Failure;
    use scheda_api::v1::{
        ColumnKind, EmbedderErrorCode, IndexColumn, IndexValue, ProjectorErrorCode,
        RerankerErrorCode,
    };

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

    fn embedder(script: Script) -> EmbedderPort {
        install_embedder(Port::Sync(Arc::new(script))).unwrap()
    }

    #[test]
    fn vectors_come_back_in_the_order_of_the_texts() {
        let texts = texts(20);
        let want = expected(&texts);
        let embedded = embedder(Script::new()).embed(texts);
        assert!(embedded.failure.is_none());
        assert_eq!(embedded.vectors, want);
    }

    #[test]
    fn no_texts_call_nothing() {
        let script = Script::new();
        let seen = Arc::clone(&script.seen);
        let embedded = embedder(script).embed(Vec::new());
        assert!(embedded.vectors.is_empty() && embedded.failure.is_none());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failure_after_the_seventh_vector_returns_the_seven_and_the_error() {
        let texts = texts(20);
        let want = expected(&texts[..7]);
        let script = Script {
            fail_at: Some(7),
            ..Script::new()
        };
        let embedded = embedder(script).embed(texts);
        assert_eq!(embedded.vectors, want);
        let error = embedded.failure.expect("the port failed");
        assert!(error.is(EmbedderErrorCode::Unavailable));
        assert!(error.retryable());
        assert!(!error.stack().is_empty(), "Scheda adds a frame");
        assert!(
            embedder(Script::new())
                .embed(self::texts(1))
                .into_result()
                .is_ok()
        );
    }

    #[test]
    fn a_vector_of_the_wrong_length_is_invalid_argument_and_keeps_the_ones_before_it() {
        let texts = texts(10);
        let want = expected(&texts[..3]);
        let script = Script {
            short_at: Some(3),
            ..Script::new()
        };
        let embedded = embedder(script).embed(texts);
        assert_eq!(embedded.vectors, want);
        let error = embedded.failure.expect("the vector is short");
        assert!(error.is(VaultErrorCode::InvalidArgument));
        assert!(error.to_string().contains("vector 3"), "names the index");
    }

    #[test]
    fn more_or_fewer_vectors_than_texts_is_invalid_argument() {
        for (extra, missing) in [(true, false), (false, true)] {
            let texts = texts(5);
            let script = Script {
                extra,
                missing,
                ..Script::new()
            };
            let embedded = embedder(script).embed(texts);
            assert_eq!(embedded.vectors.len(), if extra { 5 } else { 4 });
            let error = embedded.into_result().unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
    }

    #[test]
    fn the_texts_are_moved_to_the_embedder_and_never_cloned() {
        let texts = texts(12);
        let sent: Vec<usize> = texts.iter().map(|text| text.as_ptr() as usize).collect();
        let script = Script::new();
        let seen = Arc::clone(&script.seen);
        embedder(script).embed(texts).into_result().unwrap();
        assert_eq!(*seen.lock().unwrap(), sent);
    }

    #[test]
    fn a_query_vector_has_the_declared_length_and_the_query_is_not_empty() {
        let error = embedder(Script {
            query_values: DIMS + 1,
            ..Script::new()
        })
        .embed_query(String::from("what"))
        .unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = embedder(Script::new())
            .embed_query(String::new())
            .unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let port = embedder(Script::new());
        assert_eq!(port.embed_query(String::from("what")).unwrap().len(), DIMS);
        assert_eq!(port.model_id(), "model-a");
        assert_eq!(port.dimensions(), DIMS);
    }

    #[test]
    fn a_descriptor_that_breaks_its_rules_fails_the_install_after_one_describe() {
        for (model_id, dimensions) in [("model-a", 0), ("", 3)] {
            let script = Script {
                model_id: model_id.to_owned(),
                dimensions,
                ..Script::new()
            };
            let describes = Arc::clone(&script.describes);
            let error = install_embedder(Port::Sync(Arc::new(script)))
                .err()
                .unwrap();
            assert!(error.is(VaultErrorCode::InvalidArgument));
            assert_eq!(describes.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn describe_is_called_once_per_install_whatever_is_embedded_after() {
        let script = Script::new();
        let describes = Arc::clone(&script.describes);
        let port = embedder(script);
        port.embed(texts(4)).into_result().unwrap();
        port.embed_query(String::from("q")).unwrap();
        port.embed(texts(2)).into_result().unwrap();
        assert_eq!(describes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failure_of_describe_keeps_the_code_of_the_port() {
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
        let error = install_embedder(Port::Sync(Arc::new(Down))).err().unwrap();
        assert!(error.is(EmbedderErrorCode::Refused));
    }

    #[cfg(not(feature = "search"))]
    #[test]
    fn an_embedder_or_a_reranker_without_search_is_unimplemented() {
        let ports = Ports {
            embedder: Some(Port::Sync(Arc::new(Script::new()))),
            ..Ports::default()
        };
        let error = Installed::new(ports).err().unwrap();
        assert!(error.is(VaultErrorCode::Unimplemented));
        let error = Installed::default()
            .set_embedder(Some(Port::Sync(Arc::new(Script::new()))))
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Unimplemented));
        Installed::default().set_embedder(None).unwrap();
    }

    #[cfg(feature = "search")]
    #[test]
    fn set_embedder_replaces_the_embedder_and_a_failure_keeps_the_old_one() {
        let installed = Installed::new(Ports::default()).unwrap();
        assert!(installed.embedder().unwrap().is_none());
        installed
            .set_embedder(Some(Port::Sync(Arc::new(Script::new()))))
            .unwrap();
        let other = Script {
            model_id: String::from("model-b"),
            ..Script::new()
        };
        installed
            .set_embedder(Some(Port::Sync(Arc::new(other))))
            .unwrap();
        assert_eq!(installed.embedder().unwrap().unwrap().model_id(), "model-b");
        let broken = Script {
            dimensions: 0,
            ..Script::new()
        };
        let error = installed
            .set_embedder(Some(Port::Sync(Arc::new(broken))))
            .unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        assert_eq!(installed.embedder().unwrap().unwrap().model_id(), "model-b");
        installed.set_embedder(None).unwrap();
        assert!(installed.embedder().unwrap().is_none());
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

    #[test]
    fn the_adapter_calls_its_function_with_portions_and_flushes_the_last_one() {
        let portions = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&portions);
        let adapter = FixedEmbedder::new(
            "model-a",
            DIMS as u32,
            NonZeroUsize::new(10).unwrap(),
            move |portion: Vec<String>| {
                log.lock().unwrap().push(portion.len());
                Ok(portion.iter().map(|text| vector_for(text)).collect())
            },
        );
        let texts = texts(25);
        let port = install_embedder(Port::Sync(Arc::new(adapter))).unwrap();
        let by_hand = install_embedder(Port::Sync(Arc::new(ByHand))).unwrap();
        let got = port.embed(texts.clone()).into_result().unwrap();
        assert_eq!(*portions.lock().unwrap(), [10, 10, 5]);
        assert_eq!(got, by_hand.embed(texts).into_result().unwrap());
        assert_eq!(
            port.embed_query(String::from("q")).unwrap(),
            vector_for("q")
        );
        assert_eq!(
            *portions.lock().unwrap(),
            [10, 10, 5, 1],
            "a portion of one"
        );
    }

    #[test]
    fn the_adapter_fails_the_stream_when_its_function_miscounts_or_fails() {
        let count = FixedEmbedder::new(
            "model-a",
            DIMS as u32,
            NonZeroUsize::new(4).unwrap(),
            |portion: Vec<String>| {
                Ok(portion
                    .iter()
                    .skip(1)
                    .map(|text| vector_for(text))
                    .collect())
            },
        );
        let embedded = install_embedder(Port::Sync(Arc::new(count)))
            .unwrap()
            .embed(texts(6));
        assert!(embedded.vectors.is_empty());
        let error = embedded.failure.unwrap();
        assert!(error.is(VaultErrorCode::InvalidArgument));

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let failing = FixedEmbedder::new(
            "model-a",
            DIMS as u32,
            NonZeroUsize::new(4).unwrap(),
            move |portion: Vec<String>| {
                if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                    return Err(Failure::new(EmbedderErrorCode::Unavailable, "down")
                        .with_retryable(true)
                        .into());
                }
                Ok(portion.iter().map(|text| vector_for(text)).collect())
            },
        );
        let embedded = install_embedder(Port::Sync(Arc::new(failing)))
            .unwrap()
            .embed(texts(10));
        assert_eq!(embedded.vectors.len(), 4, "the first portion is kept");
        let error = embedded.failure.unwrap();
        assert!(error.is(EmbedderErrorCode::Unavailable) && error.retryable());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the stream ends at the failure"
        );
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
        let sync = reranker_sync(Port::Sync(Arc::new(judge))).unwrap();
        Arc::new(RerankerPort { sync })
    }

    #[test]
    fn a_reranker_gets_the_query_first_and_scores_each_document_in_order() {
        let judge = Judge::default();
        let sent = Arc::clone(&judge.sent);
        let documents = vec![
            document("a", "1"),
            document("b", "123"),
            document("c", "12"),
        ];
        let scores = reranker(judge).score(String::from("q"), documents).unwrap();
        assert_eq!(scores, [1.0, 3.0, 2.0]);
        assert_eq!(*sent.lock().unwrap(), ["q", "a", "b", "c"]);
    }

    #[test]
    fn a_reranker_that_miscounts_is_invalid_argument_and_a_refusal_keeps_its_code() {
        let documents = || vec![document("a", "1"), document("b", "2")];
        let error = reranker(Judge {
            drop_one: true,
            ..Judge::default()
        })
        .score(String::from("q"), documents())
        .unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = reranker(Judge {
            refuse: true,
            ..Judge::default()
        })
        .score(String::from("q"), documents())
        .unwrap_err();
        assert!(error.is(RerankerErrorCode::Refused));
        assert!(!error.retryable());
        let scores = reranker(Judge::default())
            .score(String::from("q"), Vec::new())
            .unwrap();
        assert!(scores.is_empty());
    }

    #[test]
    fn the_reranker_adapter_scores_in_portions_and_in_order() {
        let portions = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&portions);
        let adapter = FixedReranker::new(
            NonZeroUsize::new(3).unwrap(),
            move |query: &str, portion: Vec<Document>| {
                log.lock().unwrap().push((query.to_owned(), portion.len()));
                Ok(portion
                    .iter()
                    .map(|document| document.raw.as_deref().unwrap_or_default().len() as f64)
                    .collect())
            },
        );
        let documents: Vec<Document> = (1..=7)
            .map(|n| document(&format!("d{n}"), &"x".repeat(n)))
            .collect();
        let scores = reranker(adapter)
            .score(String::from("q"), documents)
            .unwrap();
        assert_eq!(scores, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        let log = portions.lock().unwrap();
        assert_eq!(
            *log,
            [
                ("q".to_owned(), 3),
                ("q".to_owned(), 3),
                ("q".to_owned(), 1)
            ]
        );
    }

    /// A projector that records the address of the bytes it was given.
    struct Proj {
        schema: IndexSchema,
        values: IndexDoc,
        raw_at: Arc<Mutex<Vec<usize>>>,
        fail: bool,
    }

    impl ProjectorSync for Proj {
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

    fn column(name: &str) -> IndexColumn {
        IndexColumn::default()
            .with_name(name)
            .with_kind(ColumnKind::COLUMN_KIND_TEXT)
            .with_find(true)
    }

    fn projector(proj: Proj) -> Arc<ProjectorPort> {
        let sync = projector_sync(Port::Sync(Arc::new(proj))).unwrap();
        Arc::new(ProjectorPort { sync })
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

    #[test]
    fn the_projector_gets_raw_moved_and_an_empty_result_leaves_the_document_out() {
        let with_values = proj(vec![column("title")], one_value());
        let seen = Arc::clone(&with_values.raw_at);
        let port = projector(with_values);
        let raw = b"# a note".to_vec();
        let at = raw.as_ptr() as usize;
        let doc = port
            .project(String::from("notes"), String::from("a.md"), raw)
            .unwrap();
        assert!(doc.is_some_and(|doc| doc.values.contains_key("title")));
        assert_eq!(*seen.lock().unwrap(), [at], "moved, not copied");

        let empty = projector(proj(vec![column("title")], IndexDoc::default()));
        let doc = empty
            .project(String::from("notes"), String::from("a.md"), Vec::new())
            .unwrap();
        assert!(doc.is_none());
    }

    #[test]
    fn columns_are_checked_and_none_leaves_the_collection_out() {
        let schema = projector(proj(
            vec![column("title"), column("body")],
            IndexDoc::default(),
        ))
        .columns(String::from("notes"))
        .unwrap()
        .unwrap();
        assert_eq!(schema.columns.len(), 2);
        let none = projector(proj(Vec::new(), IndexDoc::default()))
            .columns(String::from("notes"))
            .unwrap();
        assert!(none.is_none());
        for columns in [
            vec![column("title"), column("title")],
            vec![column("id")],
            vec![column("collection")],
            vec![column("")],
        ] {
            let error = projector(proj(columns, IndexDoc::default()))
                .columns(String::from("notes"))
                .unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
    }

    #[test]
    fn a_projector_failure_keeps_its_code() {
        let mut failing = proj(vec![column("title")], IndexDoc::default());
        failing.fail = true;
        let error = projector(failing)
            .project(String::from("notes"), String::from("a.md"), Vec::new())
            .unwrap_err();
        assert!(error.is(ProjectorErrorCode::Refused));
    }

    #[cfg(not(feature = "tokio"))]
    #[test]
    fn a_build_without_tokio_has_the_sync_form_only() {
        let port: Port<dyn EmbedderSync, DynEmbedderAsync> = Port::Sync(Arc::new(ByHand));
        match port {
            Port::Sync(port) => assert!(port.describe(Empty::default()).is_ok()),
        }
    }

    #[cfg(feature = "tokio")]
    mod with_tokio {
        use protocontract::{IterStream, Stream};
        use scheda_api::v1::EmbedderAsync;

        use super::*;

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
                while let Some(text) = std::future::poll_fn(|cx| texts.as_mut().poll_next(cx)).await
                {
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

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_async_embedder_is_called_by_the_blocking_helpers_through_blocking() {
            let port = install_embedder(Port::Async(DynEmbedderAsync::new(Waiting))).unwrap();
            assert_eq!(port.model_id(), "model-async");
            let texts = texts(9);
            let want = expected(&texts);
            assert_eq!(port.embed(texts).into_result().unwrap(), want);
            assert_eq!(
                port.embed_query(String::from("q")).unwrap(),
                vector_for("q")
            );
        }

        #[test]
        fn an_async_port_outside_a_runtime_is_invalid_argument() {
            let error = install_embedder(Port::Async(DynEmbedderAsync::new(Waiting)))
                .err()
                .unwrap();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
    }
}
