# The object API: Scheda, Vault, Collection, Mirror, Index

Approved by the owner on 18 October 2026 (all thirteen items below, no
changes). The protos are written under `api/scheda/v1/` (new:
`scheda.proto`, `mirror.proto`); `buf build`, `buf lint`, and `buf format`
pass, and protoc-gen-protocontract-buffa 0.4.0 accepts them (eight
services, seven references). Nothing is generated or implemented on them
yet; they replace the hand-written port wiring of P5 and the one-service
`Vault` that P6 was to implement.

## The mechanism

protocontract-buffa 0.4.0 (released 18 October 2026, crates.io and
GitHub): an empty message marked
`option (protocontract.v1.service) = "scheda.v1.Embedder";` stands for an
implementation of that service. The plugin emits its type in a module
named after it (`embedder_ref::EmbedderRef`), which holds the
implementation in the form it was given; buffa maps the message there by
an `extern_path` of its own, the same in both plugins. The receiver takes
the form it needs (`to_sync`, `to_async_inline`, `to_async_offload`); a
matching form comes back as the same implementation, with no bridge. A
decoded reference is `UNBOUND` when used, and has no JSON form.
`json=true` and `validate=true` on the plugin give it serde and
`Validate`. The option lives in `protocontract/v1/options.proto`,
shipped in the `protocontract` crate under `proto/`.

## The decisions

1. Root service `Scheda` (empty constructor): `Info`, and
   `Open(VaultOpening) returns (VaultRef)`.
2. `Scheda.Info` gives the version and the build features; `VaultInfo`
   keeps `path`, `index_path`, `writer`.
3. `VaultOpening` = `path` + `OpenOptions` + the three port references.
4. Ports change at any time: `Vault.SetProjector` / `SetEmbedder` /
   `SetReranker` and the `Remove…` pairs; the indexer takes the new set
   into its next round.
5. `Vault` keeps the set of collections and their declarations (List,
   Get → `CollectionRef`, Create, Update, Delete, Batch) and what spans
   collections (`BatchDocuments`, `QueryDocuments`, `CountDocuments`,
   `SearchDocuments`). Create and Update return the declaration.
6. The data message `Collection` is `CollectionDeclaration` (a service and
   a message of one package cannot share a name); the file on disk keeps
   field names only, so it does not change.
7. Short method names in the narrow services: `Collection.Get / GetMany /
   History / Create / Update / Upsert / Delete / Batch / Query / Count /
   Search`, `Mirror.Info / Set / Remove / Push / Pull / Sync`,
   `Index.Status / Watch / Reindex`.
8. Requests of a collection carry `id` only (`DocumentId`,
   `DocumentCreation { id, raw, keep_existing, note }`, …); `Document` is
   output only. The Vault wraps: `VaultDocumentOperation { collection,
   operation }`, `VaultSearch { collections, search }`; `VaultQuery`
   repeats the fields of `DocumentQuery` (its `start_after` is a
   `DocumentKey`).
9. `Vault.Close`: new calls through any object of the Vault fail with
   `FAILED_PRECONDITION`; it returns when running calls end, the indexer
   stops, and no lock is held; a second `Close` is nothing; a Vault whose
   references are all dropped closes by itself.
10. `CollectionRef` names its collection and finds it again on each call.
11. `Mirror.Info` gives `url`, `auto_sync`, and `last_auto_sync_error`:
    the answer to O4 (where `auto_sync` failures show).
12. `Projector.Describe` gives `ProjectorDescriptor { projection_id }`,
    stored with the Index; another projection makes it `INDEX_STALE` for
    that Vault, which does not index, until a `Reindex` with the new
    Projector.
13. Request field numbers start again at 1 (no request is stored on disk).

## Settled before the refactor (18 October 2026)

All accepted by the owner, and written into `docs/20-proto-contract.md`
as D24 with the stages R1–R5 that replace P6 and P7:

- The plugin v0.4.0 is in the bazel_utils catalog from bazel_utils
  v0.4.1 (released 18 October 2026); Scheda pins both.
- `protocontract/v1/options.proto` reaches buf through a crate_universe
  annotation on `protocontract` (`@scheda_cargo//:protocontract_protos`),
  staged at its import path as protovalidate is.
- The structs behind the references are private: `Scheda.Open` returns a
  `VaultRef`, the caller works through the traits.
- `UNBOUND` and `NO_RUNTIME` from a port reference become
  `INVALID_ARGUMENT`, protocontract's error kept as the source.
- `Close` waits for an item being made, not for a held stream; the next
  item after it is `FAILED_PRECONDITION`.
- A method of a later layer is `todo!` until its layer; P8 checks none is
  left.

**Why:** docs/20 holds the decision now; this note keeps the mechanism
and the history in one place for a session that starts from the notes.
