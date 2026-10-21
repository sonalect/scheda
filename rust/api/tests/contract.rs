//! The generated contract: the blocking and the async traits of the
//! services, the `Offload` / `Blocking` bridges, `Validate` impls that hold
//! the rules of the protos, and the references to a service.

use std::sync::Arc;

use buffa::Message;
use buffa_types::google::protobuf::Empty;
use protocontract::{BoxIter, Error, RuntimeCode};
use protovalidate_buffa::Validate;
use scheda_api::v1::embedder_ref::EmbedderRef;
use scheda_api::v1::{
    CollectionAsync, CollectionSync, DynCollectionAsync, DynEmbedderAsync, DynIndexAsync,
    DynMirrorAsync, DynProjectorAsync, DynRerankerAsync, DynSchedaAsync, DynVaultAsync,
    EmbedderAsync, EmbedderDescriptor, EmbedderSync, IndexAsync, IndexSync, MirrorAsync,
    MirrorSync, ProjectorAsync, ProjectorSync, Query, RerankerAsync, RerankerSync, SchedaAsync,
    SchedaSync, Text, VaultAsync, VaultMarker, VaultOpening, VaultSync, Vector,
};

fn scheda_sync<T: ?Sized + SchedaSync>() {}
fn collection_sync<T: ?Sized + CollectionSync>() {}
fn mirror_sync<T: ?Sized + MirrorSync>() {}
fn index_sync<T: ?Sized + IndexSync>() {}
fn scheda_async<T: SchedaAsync>() {}
fn collection_async<T: CollectionAsync>() {}
fn mirror_async<T: MirrorAsync>() {}
fn index_async<T: IndexAsync>() {}
fn vault_sync<T: ?Sized + VaultSync>() {}
fn embedder_sync<T: ?Sized + EmbedderSync>() {}
fn reranker_sync<T: ?Sized + RerankerSync>() {}
fn projector_sync<T: ?Sized + ProjectorSync>() {}

fn vault_async<T: VaultAsync>() {}
fn embedder_async<T: EmbedderAsync>() {}
fn reranker_async<T: RerankerAsync>() {}
fn projector_async<T: ProjectorAsync>() {}

#[test]
fn both_forms_exist() {
    scheda_sync::<dyn SchedaSync>();
    collection_sync::<dyn CollectionSync>();
    mirror_sync::<dyn MirrorSync>();
    index_sync::<dyn IndexSync>();
    scheda_async::<DynSchedaAsync>();
    collection_async::<DynCollectionAsync>();
    mirror_async::<DynMirrorAsync>();
    index_async::<DynIndexAsync>();
    vault_sync::<dyn VaultSync>();
    embedder_sync::<dyn EmbedderSync>();
    reranker_sync::<dyn RerankerSync>();
    projector_sync::<dyn ProjectorSync>();
    vault_async::<DynVaultAsync>();
    embedder_async::<DynEmbedderAsync>();
    reranker_async::<DynRerankerAsync>();
    projector_async::<DynProjectorAsync>();
}

#[test]
fn bridges_exist() {
    use protocontract::{Blocking, Offload};

    scheda_sync::<Blocking<DynSchedaAsync>>();
    collection_sync::<Blocking<DynCollectionAsync>>();
    mirror_sync::<Blocking<DynMirrorAsync>>();
    index_sync::<Blocking<DynIndexAsync>>();
    vault_sync::<Blocking<DynVaultAsync>>();
    embedder_sync::<Blocking<DynEmbedderAsync>>();
    reranker_sync::<Blocking<DynRerankerAsync>>();
    projector_sync::<Blocking<DynProjectorAsync>>();

    scheda_async::<Offload<Blocking<DynSchedaAsync>>>();
    collection_async::<Offload<Blocking<DynCollectionAsync>>>();
    mirror_async::<Offload<Blocking<DynMirrorAsync>>>();
    index_async::<Offload<Blocking<DynIndexAsync>>>();
    vault_async::<Offload<Blocking<DynVaultAsync>>>();
    embedder_async::<Offload<Blocking<DynEmbedderAsync>>>();
    reranker_async::<Offload<Blocking<DynRerankerAsync>>>();
    projector_async::<Offload<Blocking<DynProjectorAsync>>>();
}

#[test]
fn marker_is_generation_four() {
    let mut marker = VaultMarker::default();
    assert!(marker.validate().is_err(), "a marker without a generation");
    for older in 1..=3 {
        marker.version = Some(older);
        assert!(marker.validate().is_err(), "generation {older}");
    }
    marker.version = Some(4);
    assert!(marker.validate().is_ok(), "generation 4");
}

/// An embedder that answers nothing a test asks of it.
struct Mute;

impl EmbedderSync for Mute {
    fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
        Ok(EmbedderDescriptor::default())
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

#[test]
fn a_reference_gives_back_the_blocking_implementation_it_holds() {
    let shared: Arc<dyn EmbedderSync> = Arc::new(Mute);
    let reference = EmbedderRef::from_shared(Arc::clone(&shared));
    assert!(reference.is_bound());
    assert!(Arc::ptr_eq(&reference.to_sync().unwrap(), &shared));
    assert_eq!(
        reference.clone(),
        reference,
        "equal to itself: the same implementation"
    );
    assert_ne!(EmbedderRef::from_sync(Mute), reference);
}

#[test]
fn a_default_and_a_decoded_reference_hold_nothing() {
    let error = EmbedderRef::default().to_sync().err().unwrap();
    assert!(error.is(RuntimeCode::Unbound));

    let opening = VaultOpening {
        path: Some("/vault".to_owned()),
        embedder: EmbedderRef::from_sync(Mute).into(),
        ..VaultOpening::default()
    };
    let decoded = VaultOpening::decode_from_slice(&opening.encode_to_vec()).unwrap();
    assert_eq!(decoded.path.as_deref(), Some("/vault"));
    let embedder = decoded.embedder.as_option().unwrap();
    assert!(!embedder.is_bound());
    assert!(embedder.to_sync().err().unwrap().is(RuntimeCode::Unbound));
}

#[test]
fn a_message_with_a_reference_has_no_json_form_and_is_valid() {
    let bare = VaultOpening {
        path: Some("/vault".to_owned()),
        ..VaultOpening::default()
    };
    assert!(
        serde_json::to_string(&bare).is_ok(),
        "no reference: JSON as before"
    );
    let with_port = VaultOpening {
        embedder: EmbedderRef::from_sync(Mute).into(),
        ..bare.clone()
    };
    assert!(serde_json::to_string(&with_port).is_err());
    assert!(with_port.validate().is_ok());
    assert!(
        VaultOpening::default().validate().is_err(),
        "the path is required"
    );
}
