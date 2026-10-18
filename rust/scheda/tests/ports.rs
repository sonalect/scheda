//! The ports through the public handle: installing them at `open` and
//! `set_embedder`, what a build without their feature says, and the
//! fixed-portion adapters called as the blocking traits they implement.

mod common;

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::reranker_ref::RerankerRef;
use scheda::v1::score_item::Item;
use scheda::v1::{
    Document, DocumentScore, EmbedderDescriptor, EmbedderSync, OpenOptions, Query, RerankerSync,
    SchedaSync, ScoreItem, Text, VaultErrorCode as Code, VaultOpening, Vector,
};
use scheda::{Error, FixedEmbedder, FixedReranker, Scheda};

#[cfg(feature = "search")]
use common::open;
use common::{Scratch, opening, snapshot, vault};

const DIMS: u32 = 3;

fn vector_for(text: &str) -> Vec<f32> {
    vec![text.len() as f32, 1.0, 2.0]
}

fn texts(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("text {i}")).collect()
}

/// An embedder that counts its `Describe` calls and declares what it is
/// told to.
struct Counting {
    model_id: &'static str,
    dimensions: u32,
    describes: Arc<AtomicUsize>,
}

impl Counting {
    fn new(describes: &Arc<AtomicUsize>) -> Counting {
        Counting {
            model_id: "model-a",
            dimensions: DIMS,
            describes: Arc::clone(describes),
        }
    }
}

impl EmbedderSync for Counting {
    fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
        self.describes.fetch_add(1, Ordering::SeqCst);
        Ok(EmbedderDescriptor::default()
            .with_model_id(self.model_id)
            .with_dimensions(self.dimensions))
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

fn embedder_ref(embedder: Counting) -> EmbedderRef {
    EmbedderRef::from_sync(embedder)
}

/// The request that opens the Vault at `path` with `embedder` installed.
fn opening_with(path: &std::path::Path, embedder: Counting) -> VaultOpening {
    let mut opening = opening(path, OpenOptions::default());
    opening.embedder = embedder_ref(embedder).into();
    opening
}

/// Opens as `opening` asks and keeps the failure.
fn refused(opening: VaultOpening) -> Error {
    Scheda.open(opening).err().unwrap()
}

fn scratch_vault(name: &str) -> (Scratch, std::path::PathBuf) {
    let scratch = Scratch::new(name);
    let path = scratch.join("v");
    vault(&path);
    (scratch, path)
}

#[cfg(feature = "search")]
mod installed {
    use super::*;

    #[test]
    fn open_reads_the_descriptor_once() {
        let (_scratch, path) = scratch_vault("describe-once");
        let describes = Arc::new(AtomicUsize::new(0));
        let vault = Scheda
            .open(opening_with(&path, Counting::new(&describes)))
            .unwrap()
            .to_sync()
            .unwrap();
        assert_eq!(describes.load(Ordering::SeqCst), 1);
        let second = Arc::new(AtomicUsize::new(0));
        vault
            .set_embedder(embedder_ref(Counting::new(&second)))
            .unwrap();
        assert_eq!(second.load(Ordering::SeqCst), 1);
        assert_eq!(
            describes.load(Ordering::SeqCst),
            1,
            "the first is not asked again"
        );
        vault.remove_embedder(Empty::default()).unwrap();
    }

    #[test]
    fn a_descriptor_that_breaks_its_rules_fails_open_and_set_embedder() {
        let (_scratch, path) = scratch_vault("bad-descriptor");
        for (model_id, dimensions) in [("model-a", 0), ("", DIMS)] {
            let describes = Arc::new(AtomicUsize::new(0));
            let broken = || Counting {
                model_id,
                dimensions,
                describes: Arc::clone(&describes),
            };
            let before = snapshot(&path);
            let error = refused(opening_with(&path, broken()));
            assert_eq!(snapshot(&path), before, "a refused port writes nothing");
            assert!(
                error.is(Code::InvalidArgument),
                "open: {model_id:?} {dimensions}"
            );
            assert_eq!(describes.load(Ordering::SeqCst), 1);

            let vault = open(&path, OpenOptions::default()).unwrap();
            let error = vault.set_embedder(embedder_ref(broken())).unwrap_err();
            assert!(
                error.is(Code::InvalidArgument),
                "set: {model_id:?} {dimensions}"
            );
            assert_eq!(describes.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn a_reranker_is_installed_at_open() {
        let (_scratch, path) = scratch_vault("reranker");
        let reranker = FixedReranker::new(NonZeroUsize::MIN, |_: &str, portion: Vec<Document>| {
            Ok(vec![0.0; portion.len()])
        });
        let mut request = opening(&path, OpenOptions::default());
        request.reranker = RerankerRef::from_sync(reranker).into();
        Scheda.open(request).unwrap();
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn an_async_port_outside_a_runtime_is_invalid_argument() {
        use scheda::v1::{EmbedderAsync, Vector};

        struct Waiting;
        impl EmbedderAsync for Waiting {
            async fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
                Ok(EmbedderDescriptor::default()
                    .with_model_id("m")
                    .with_dimensions(DIMS))
            }
            async fn embed<R>(
                &self,
                _texts: R,
            ) -> Result<
                impl protocontract::Stream<Item = Result<Vector, Error>> + Send + use<R>,
                Error,
            >
            where
                R: protocontract::Stream<Item = Result<Text, Error>> + Send + 'static,
            {
                Ok(protocontract::IterStream::new(Vec::new().into_iter()))
            }
            async fn embed_query(&self, _query: Query) -> Result<Vector, Error> {
                Ok(Vector::default())
            }
        }

        let (_scratch, path) = scratch_vault("async-outside");
        let mut request = opening(&path, OpenOptions::default());
        request.embedder = EmbedderRef::from_async(Waiting).into();
        let error = refused(request);
        assert!(error.is(Code::InvalidArgument));
        let vault = open(&path, OpenOptions::default()).unwrap();
        let error = vault
            .set_embedder(EmbedderRef::from_async(Waiting))
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument));
    }
}

#[cfg(not(feature = "search"))]
#[test]
fn an_embedder_or_a_reranker_without_search_is_unimplemented() {
    let (_scratch, path) = scratch_vault("no-search");
    let describes = Arc::new(AtomicUsize::new(0));
    let before = snapshot(&path);
    let error = refused(opening_with(&path, Counting::new(&describes)));
    assert!(error.is(Code::Unimplemented));
    assert_eq!(snapshot(&path), before, "a refused port writes nothing");
    assert_eq!(
        describes.load(Ordering::SeqCst),
        0,
        "nothing was asked of it"
    );

    let reranker = FixedReranker::new(NonZeroUsize::MIN, |_: &str, portion: Vec<Document>| {
        Ok(vec![0.0; portion.len()])
    });
    let mut request = opening(&path, OpenOptions::default());
    request.reranker = RerankerRef::from_sync(reranker).into();
    let error = refused(request);
    assert!(error.is(Code::Unimplemented));

    let vault = common::open(&path, OpenOptions::default()).unwrap();
    let error = vault
        .set_embedder(embedder_ref(Counting::new(&describes)))
        .unwrap_err();
    assert!(error.is(Code::Unimplemented));
    vault.remove_embedder(Empty::default()).unwrap();
}

#[cfg(not(feature = "find"))]
#[test]
fn a_projector_without_find_is_unimplemented() {
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::{
        CollectionKey, IndexDoc, IndexSchema, ProjectorDescriptor, ProjectorSync, RawDocument,
    };

    struct Empty0;
    impl ProjectorSync for Empty0 {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("test@1"))
        }
        fn columns(&self, _collection_key: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema::default())
        }
        fn project(&self, _raw_document: RawDocument) -> Result<IndexDoc, Error> {
            Ok(IndexDoc::default())
        }
    }

    let (_scratch, path) = scratch_vault("no-find");
    let mut request = opening(&path, OpenOptions::default());
    request.projector = ProjectorRef::from_sync(Empty0).into();
    let error = refused(request);
    assert!(error.is(Code::Unimplemented));
}

#[cfg(feature = "find")]
#[test]
fn a_projector_is_installed_at_open() {
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::{
        CollectionKey, IndexDoc, IndexSchema, ProjectorDescriptor, ProjectorSync, RawDocument,
    };

    struct Empty0;
    impl ProjectorSync for Empty0 {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("test@1"))
        }
        fn columns(&self, _collection_key: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema::default())
        }
        fn project(&self, _raw_document: RawDocument) -> Result<IndexDoc, Error> {
            Ok(IndexDoc::default())
        }
    }

    let (_scratch, path) = scratch_vault("find");
    let mut request = opening(&path, OpenOptions::default());
    request.projector = ProjectorRef::from_sync(Empty0).into();
    Scheda.open(request).unwrap();
}

#[test]
fn the_embedding_adapter_calls_its_function_with_portions_and_equals_a_stream_by_hand() {
    let portions = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&portions);
    let adapter = FixedEmbedder::new(
        "model-a",
        DIMS,
        NonZeroUsize::new(10).unwrap(),
        move |portion: Vec<String>| {
            log.lock().unwrap().push(portion.len());
            Ok(portion.iter().map(|text| vector_for(text)).collect())
        },
    );
    let descriptor = adapter.describe(Empty::default()).unwrap();
    assert_eq!(descriptor.model_id.as_deref(), Some("model-a"));
    assert_eq!(descriptor.dimensions, Some(DIMS));

    let input = |n| -> BoxIter<'static, Result<Text, Error>> {
        Box::new(
            texts(n)
                .into_iter()
                .map(|text| Ok(Text::default().with_text(text))),
        )
    };
    let got: Vec<Vector> = adapter
        .embed(input(25))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(*portions.lock().unwrap(), [10, 10, 5]);

    let describes = Arc::new(AtomicUsize::new(0));
    let by_hand: Vec<Vector> = Counting::new(&describes)
        .embed(input(25))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(got, by_hand);

    let query = adapter
        .embed_query(Query::default().with_text("query"))
        .unwrap();
    assert_eq!(query.values, vector_for("query"));
    assert_eq!(*portions.lock().unwrap(), [10, 10, 5, 1]);
}

#[test]
fn the_reranking_adapter_reads_the_query_first_and_scores_in_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&calls);
    let adapter = FixedReranker::new(
        NonZeroUsize::new(4).unwrap(),
        move |query: &str, portion: Vec<Document>| {
            log.lock().unwrap().push((query.to_owned(), portion.len()));
            Ok(portion
                .iter()
                .map(|document| document.raw.as_deref().unwrap_or_default().len() as f64)
                .collect())
        },
    );
    let items = std::iter::once(Item::Query(String::from("what")))
        .chain((1..=9).map(|n| {
            Item::from(
                Document::default()
                    .with_id(format!("d{n}"))
                    .with_raw(vec![0u8; n]),
            )
        }))
        .map(|item| {
            Ok(ScoreItem {
                item: Some(item),
                ..ScoreItem::default()
            })
        });
    let scores: Vec<f64> = adapter
        .score(Box::new(items))
        .unwrap()
        .map(|score: Result<DocumentScore, Error>| score.map(|score| score.score.unwrap_or(-1.0)))
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(scores, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("what".to_owned(), 4),
            ("what".to_owned(), 4),
            ("what".to_owned(), 1)
        ]
    );

    let no_query: BoxIter<'static, Result<ScoreItem, Error>> =
        Box::new(std::iter::once(Ok(ScoreItem {
            item: Some(Item::from(Document::default())),
            ..ScoreItem::default()
        })));
    let error = adapter.score(no_query).err().unwrap();
    assert!(
        error.is(Code::InvalidArgument),
        "the first item must be the query"
    );
}
