//! The generated contract: the traits of the services in every build, the
//! `Offload` / `Blocking` bridges with `tokio`, and `Validate` impls that
//! hold the rules of the protos.

use protovalidate_buffa::Validate;
use scheda_api::v1::{
    DynEmbedderAsync, DynProjectorAsync, DynRerankerAsync, DynVaultAsync, EmbedderAsync,
    EmbedderSync, ProjectorAsync, ProjectorSync, RerankerAsync, RerankerSync, VaultAsync,
    VaultMarker, VaultSync,
};

fn vault_sync<T: ?Sized + VaultSync>() {}
fn embedder_sync<T: ?Sized + EmbedderSync>() {}
fn reranker_sync<T: ?Sized + RerankerSync>() {}
fn projector_sync<T: ?Sized + ProjectorSync>() {}

fn vault_async<T: VaultAsync>() {}
fn embedder_async<T: EmbedderAsync>() {}
fn reranker_async<T: RerankerAsync>() {}
fn projector_async<T: ProjectorAsync>() {}

#[test]
fn both_forms_exist_in_every_build() {
    vault_sync::<dyn VaultSync>();
    embedder_sync::<dyn EmbedderSync>();
    reranker_sync::<dyn RerankerSync>();
    projector_sync::<dyn ProjectorSync>();
    vault_async::<DynVaultAsync>();
    embedder_async::<DynEmbedderAsync>();
    reranker_async::<DynRerankerAsync>();
    projector_async::<DynProjectorAsync>();
}

#[cfg(feature = "tokio")]
#[test]
fn bridges_with_tokio() {
    use protocontract::{Blocking, Offload};

    vault_sync::<Blocking<DynVaultAsync>>();
    embedder_sync::<Blocking<DynEmbedderAsync>>();
    reranker_sync::<Blocking<DynRerankerAsync>>();
    projector_sync::<Blocking<DynProjectorAsync>>();

    vault_async::<Offload<Blocking<DynVaultAsync>>>();
    embedder_async::<Offload<Blocking<DynEmbedderAsync>>>();
    reranker_async::<Offload<Blocking<DynRerankerAsync>>>();
    projector_async::<Offload<Blocking<DynProjectorAsync>>>();
}

#[test]
fn marker_is_generation_two() {
    let mut marker = VaultMarker::default();
    assert!(marker.validate().is_err(), "a marker without a generation");
    marker.version = Some(1);
    assert!(marker.validate().is_err(), "generation 1");
    marker.version = Some(2);
    assert!(marker.validate().is_ok(), "generation 2");
}
