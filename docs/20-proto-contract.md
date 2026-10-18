# 20. The proto contract

Document 20 of the Scheda package. Version 0.7, 18 October 2026.

Status: reviewed with the owner, method by method, on 17 October 2026.
Every decision stands (those marked *settled* were discussed; the rest
were accepted as proposed). The protos of §3 are written under
`api/scheda/v1/`. The work goes on the branch `proto-contract`, layer by
layer, without transitional code (§4). P1, P3, P4, and P5 are done; P2 is
done but for `ErrorReport`; P6 is next.

Work order: define the whole library API as protobuf services and generate
its Rust surface with `protoc-gen-protocontract-buffa`, as the other
projects of the Sonalect organization define theirs. The owner's direction
(17 October 2026): the result must be right, not fast. Compatibility with
`0.21` is not kept. The current error contract (`api.v1.Error`, its codes,
`repeat`, the error-handling rule and skill) is dropped and replaced by the
generated one. The API takes Firestore (`google.firestore.v1`) as its model
where Scheda means the same thing, and hides git.

Order inside each layer: code and tests first, then `cargo test` and
`cargo clippy` green for what the layer holds; `bazel test //...` before
the branch merges (§4). One docs pass at the end (§5).

Decisions carry a recommendation each. The owner comments only on the ones
he disagrees with; the rest stand (`owner-review.md`).

---

## 1. Why

Scheda `0.21`:

1. **Messages are proto, operations are not.** `Vault` has 31 public
   methods and `Collection` 16; `AsyncVault` and `AsyncCollection` repeat
   44 of them by hand (`nonblocking.rs`). `07` calls itself a
   language-neutral contract, but its operations exist only as prose and
   Rust signatures.
2. **Caller code enters through three hand-written traits.** `Embedder`,
   `Reranker`, and `Projector` take borrowed arguments, expose getters and
   default methods, and are not proto. A consumer whose own contract
   already defines an embedder port keeps an adapter that copies every
   text on the way in.
3. **The error contract is wide and hand-made.** `api.v1.Error` has 68
   codes (39 of them one per `std::io::ErrorKind`, 14 per git failure
   class), `repeat`, `path`, `stack`, and 74 hand-written constructors and
   helpers on the generated type. Callers branch on few of them: the
   anchor consumer on `locked`, `not_found`, `not_a_vault`,
   `io_network_down`, `index_version` with `repeat`, `index_missing`, and
   `git_unborn_branch` / `git_not_found`; the Python CLI driver on
   `not_found` only.
4. **Writes serve one writer and expose git.** A second writer gets
   `locked` at once; `begin` / `commit` hold the write lock while the
   caller works; `transaction(message, f)` runs caller code inside; writes
   reach the work tree before the commit. The API speaks git: commit,
   autocommit, `HEAD`, commit ids, `git_*` codes.

The sibling projects define a component's API as `service`s. For each,
`protoc-gen-protocontract-buffa` generates a `…Sync` trait, a `…Async`
trait, a `Dyn…Async` handle, and the bridges `Inline`, `Offload`, and
`Blocking`. Every method fails with `protocontract::Error`, whose code is a
proto enum `<Service>ErrorCode`. Caller code is a **port**: a service the
component calls and the caller implements (`knowqore.stores.v1.Embedder`:
"The caller provides it; the store calls it"). Construction and lifecycle
stay outside the contract, as the implementation's constructors.

---

## 2. Decisions

**D1. One package `scheda.v1`; the Vault generation lives in the marker
only.** *Settled with the owner, 17 October 2026.*

- Every message and service is in `scheda.v1`, under `api/scheda/v1/`.
  `api` is a name any consumer's proto tree may already hold; a contract
  that other repositories import needs its own. `api.v1`,
  `api.collection.v1`, and `api` (`api.Version`, used by no library code
  since `18`) are deleted. Rust paths become `scheda_api::v1`.
- A separate package for the on-disk messages (`api.collection.v1`, kept
  apart so collection files could have generations of their own) is not
  needed. No Vault, Index, or collection file carries a proto type name
  (no type URL, no `Any`, no package string is written by
  `rust/scheda/src`): a file is Protobuf JSON, so its compatibility is its
  field names, whatever package the message is in. A breaking API is
  `scheda.v2`; that alone does not touch a file.
- **The generation.** The marker `{vault}/.scheda.yaml` is the one place
  that records the format generation of the Vault. `open` and `Check`
  read it first: another generation fails with `GENERATION` before any
  other file is read. A Vault without the marker gets one from `open`
  (with `OPEN_MODE_OPEN_EXISTING`, `NOT_A_VAULT`, as today). The generation is field
  number 1 of the marker in every generation, so a later Scheda reads it
  before it decides how to read the rest. Today there is only the check;
  no step moves a Vault from one generation to another.
- The marker holds only what applies to the whole Vault, cannot be derived
  from other files, and changes how they are read. Not what the
  collection files already say (which collections, which are encrypted),
  and not what changes on every write (the library version that wrote
  last), which would touch the marker in every write.
- A message stored on disk (`Collection`, `Schema`, `VaultMarker`) says
  so in its proto comment. A field added to it needs no new generation:
  an earlier Scheda reads past it (D21). The generation moves only for a
  change an earlier Scheda could not safely ignore: a field removed,
  renamed, or given another meaning, a file moved, or a new field that
  changes how other files are read.
- **No collection generation.** `CollectionInfo.version` is removed; a
  leftover `version` key in a collection file is an unknown field, read
  past like any other (D21). `Collection::version` and `Vault::versions`
  are removed.
- **The kind `document` is renamed `standard`**, so that `Document` names
  the object of the API (D7). The kinds are `standard`, `journal`, and
  `singleton`, and a schema always names one, so a caller can `match` on
  it. On disk the key `schema.document` becomes `schema.standard`.
- Both changes alter the on-disk format, so the marker moves to
  **generation 2**. A Vault of generation 1 is not migrated by code: the
  anchor consumer's Vaults are moved by hand (§6).

**D2. Four services; a missing feature answers `UNIMPLEMENTED`.**
*Settled with the owner, 17 October 2026.*

| Service | Who implements | Build |
| - | - | - |
| `Vault` | Scheda | every build |
| `Embedder` | the caller | `search` uses it |
| `Reranker` | the caller | `search` uses it |
| `Projector` | the caller | `find` uses it |

`QueryDocuments`, `SearchDocuments`, `GetIndexStatus`, and
`WatchIndexStatus` are methods of `Vault`,
not services of their own. A crate feature cannot remove one method of a
generated trait, so the trait and its messages exist in every build; in a
build without `find` (or `search`) those methods return `UNIMPLEMENTED`,
naming the feature, as a build without `encrypt` already refuses an
encrypted collection by code. The features stay: `find` pulls Tantivy,
and `search` pulls USearch and, with it, an embedder and a reranker to
supply. `Info` lists the features of the build (`FIND`, `SEARCH`,
`ENCRYPT`, …), so a caller learns up front instead of by a failure.

**D3. Construction and lifecycle stay Rust.**

```rust
pub struct Ports {
    pub embedder: Option<Port<dyn EmbedderSync, DynEmbedderAsync>>,
    pub reranker: Option<Port<dyn RerankerSync, DynRerankerAsync>>,
    pub projector: Option<Port<dyn ProjectorSync, DynProjectorAsync>>,
}

/// A port in the form its implementation has (D4).
pub enum Port<S: ?Sized, A> {
    Sync(Arc<S>),
    #[cfg(feature = "tokio")]
    Async(A),
}

impl Vault {
    pub fn open(path: impl AsRef<Path>, options: OpenOptions, ports: Ports)
        -> Result<Vault, Error>;
    pub fn set_embedder(&self, embedder: Option<Port<dyn EmbedderSync, DynEmbedderAsync>>)
        -> Result<(), Error>;
    pub fn close(self) -> Result<(), Error>;
}
```

`open` and `open_with` become one `open`. The getters `path`, `index_dir`,
`writer`, `has_remote`, `remote_names`, and `auto_sync` fold into `Info`
(`VaultInfo` already carries the path, the Index path, the Remote, and
`auto_sync`; it gains the writer). `versions` goes with D1,
`holds_write_lock` with D6. Recommendation: yes.

**How `open` treats the path is a scenario, not a flag.** *Settled with
the owner, 17 October 2026.* `OpenOptions.existing_only` becomes
`OpenOptions.mode`, an `OpenMode`: `OPEN_OR_CREATE` (the default, today
without the flag), `OPEN_EXISTING` (today with it), and the new
`CREATE_NEW` (a missing path or an empty folder becomes a new Vault,
anything else is `ALREADY_EXISTS`), for an installer that must not take
over what is already there.

**D4. Ports.**

- **Either form.** *Settled with the owner, 17 October 2026.* The form of
  a port is its implementation's choice: `Ports` takes the sync form
  (`EmbedderSync`, …) or the async form (`DynEmbedderAsync`, …), and
  Scheda bridges between them (`Offload`, `Blocking`), so the caller
  wraps nothing. A build without `tokio` takes the sync form only, since
  nothing in it could run a future. How Scheda calls a port follows its
  own caller, not the build (D11).
- **Embedder and Reranker take streams; the implementation forms the
  portions.** *Settled with the owner, 17 October 2026.* Scheda does not
  know how a model is best fed, so it does not cut work into portions:
  it streams the items one by one, and the implementation gathers them as
  its model wants (a size, a flush after a pause, several portions in
  flight, a rate limit).
- `Embedder`: `Describe` (model id and dimensions; the batch limit goes),
  `Embed(stream Text) returns (stream Vector)`, `EmbedQuery(Query)
  returns (Vector)`. `Embed` answers one vector per text, in the same
  order, and the same text gives the same vector whatever portion it
  travelled in; when the texts end, the implementation sends its last
  partial portion. An `Err` ends the stream: Scheda keeps the vectors
  that came before it and sends the rest again in the next indexer round,
  finer than today, where a failed batch is repeated whole. `EmbedQuery`
  stays a single call: one text, and its latency is the search's. The
  model id is why `Describe` stays: it is stored with the vectors, so an
  embedder of another model is seen (`MODEL_MISMATCH`, `Reindex`) instead
  of two models mixing in one Index; the dimensions let Scheda compare
  an embedder with the Index, and create the Index, before any vector.
  The default methods `embed` / `embed_many` go.
- `Reranker`: `Score(stream ScoreItem) returns (stream Score)`, no
  `Describe`. The first item is the query text, the rest the Documents
  (the stream has no header, as with `SearchDocuments`); one score per
  Document, in the same order. A failure ends the stream and fails the
  search's rerank.
- **An adapter for fixed portions.** Scheda ships a helper that turns "a
  function from a portion of texts to their vectors, and a portion size"
  into the stream form (and the same for scores), so an implementation
  that needs no more writes the portion function only; the size is still
  the implementation's.
- `Projector`: `Columns(CollectionKey) returns (IndexSchema)`,
  `Project(RawDocument) returns (IndexDoc)`. `IndexDoc` and `IndexValue`
  become proto: `IndexValue` is a `oneof` of text, integer, number,
  boolean, text list, and null. Whether `IndexDoc.values` is a `map` or a
  `repeated` pair depends on whether Scheda ever reads it in map order
  (P5 verifies; today it is a `BTreeMap`).
- **The columns stay in the Projector's code.** *Settled with the owner,
  17 October 2026.* No column, field, or format goes into the collection
  declaration: Scheda knows nothing of what a Document holds (`16`
  stands). Moving `Columns` into the declaration, and built-in
  extractors for JSON, YAML, or Markdown, were weighed and refused. The
  anchor consumer's present marker for a collection without columns
  (`{"_": true}`, a workaround for the old `replace`) is no longer needed:
  existence is the file (D6).
- The Projector stays single calls: a projection is work on one Document
  with nothing to gather.
- **Without values, identity only.** A handle without a Projector, a
  collection with no columns, and a Document the Projector gives no
  values for are all in the Index by identity (`collection`, `id`, and
  the listing order), so a query on `id` works. Only a Projector failure
  leaves a Document out (its file stays); a retryable one fails the round
  instead, which is tried again.
- `Describe` is read once when an embedder is installed and checked against
  its protovalidate rules. A descriptor that breaks them fails `open` or
  `set_embedder` with `INVALID_ARGUMENT`.
- Each port declares its codes: `<Port>ErrorCode { UNAVAILABLE, REFUSED }`.
  `UNAVAILABLE` is retryable: the service is down for now. `REFUSED`: this
  input will never succeed. Scheda keys its handling on
  `Error::retryable()`, as it keys on `repeat` today: a retryable failure
  makes the indexer wait and repeat the round, any other keeps today's
  non-retryable handling. For the embedder: the vectors that came back
  before any failure are kept for the next try (D23); a `REFUSED` text is
  remembered, its round fails, and the next try leaves the Document of
  that text out of the vector side (its file and its lexical entry stay),
  as a refused projection leaves a Document out.

Recommendation: yes.

**D5. The reranker is a port of the handle, not an argument.** `SearchDocuments`
reranks when the handle has a reranker and the query sets `rerank_limit`.
`rerank_limit` set without one is `INVALID_ARGUMENT`; unset keeps RRF
order. Today the reranker is an `Option<&dyn Reranker>` per call. The
anchor consumer passes `None` at both of its call sites; the CLI builds
one per `search` command, which a port given at `open` covers.
Recommendation: yes.

**D6. Writes: single operations and a write stream; git stays inside.**
*Settled with the owner, 17 October 2026.*

Why: Scheda `0.21` serves one writer. A second one gets `locked` at once,
and a transaction holds the write lock from `begin` to `commit` while the
caller works. Its writes go into the work tree before the commit, so
another reader's `get` sees changes that are not committed and the Index
does not, and a crash leaves them there until a later `begin` clears them
(`tx.log`). The anchor consumer therefore built its own write path around
Scheda: a durable queue of batches in front of the Vault, one writer
elected across processes by an OS lock, idempotent apply for replay after
a crash, and checks at submission (its decisions D129, D132, D137). The
trouble was not that there is a lock, but that it was held for as long as
the caller worked. Once the caller builds a write whole before it calls,
the lock is held for one commit only, and a writer can simply wait for it.
How Scheda writes must not show in the API.

- **Two shapes, one mechanism.** A single write (`CreateDocument`,
  `UpdateDocument`, `UpsertDocument`, `DeleteDocument`, `CreateCollection`,
  `UpdateCollection`, `DeleteCollection`) is atomic: when
  the call returns `Ok`, the write happened. Several writes are one call
  of a batch; when the stream ends and the call returns `Ok`, every write
  happened; an `Err` item, a broken stream, or a write that fails, and
  none happened. Each item of the stream is the request of one of the
  single calls, and both go the same way inside.
- **Two batches.** *Settled with the owner, 17 October 2026.*
  `BatchDocuments(stream DocumentOperation)` holds Document writes only
  (its items may touch several collections); `BatchCollections(stream
  CollectionOperation)` holds collection writes only, so several
  collections are declared, changed, or deleted in one atomic step. One
  batch never mixes the two: a collection is a deliberate step of its
  own, as a table in an SQL database is.
  `DocumentOperation` is one of `create`, `update`, `upsert`, `delete`
  (the requests of the four Document calls); `CollectionOperation` is one
  of `create`, `update`, `delete` (a `Collection`, or a
  `CollectionDeletion`).
- **No git in the API.** No call, field, or term says commit, autocommit,
  transaction, `HEAD`, or commit id: `begin`, `commit`, `discard`,
  `transaction`, `add`, `replace`, `remove`, and `create_collection` go.
  (There never was an autocommit flag; `07` LIB-4 refuses one.) Versions
  are opaque (`Document.revision`, D7; the versions of `IndexState`, P5).
  `auto_sync` and `Push` / `Pull` / `Sync` stay: Scheda works with the
  local Vault alone or with a remote mirror, and those calls are the
  mirror.
- **One verb per meaning.** *Settled with the owner, 17 October 2026.*
  The name of the call says what it does, as `INSERT` / `UPDATE` /
  `UPSERT` in SQL or `SET` in Redis; there is no `Precondition` message.

  | Call | Absent | Present | Options |
  | - | - | - | - |
  | `CreateDocument` | create (journal: append) | `ALREADY_EXISTS` | `keep_existing`: left alone, no error |
  | `UpdateDocument` | `NOT_FOUND` | replace | `revision` |
  | `UpsertDocument` | create | replace | none |
  | `DeleteDocument` | `NOT_FOUND` | delete | `ignore_missing`: nothing, no error; `revision` |

  Strict by default, so a mistake of the caller shows at once; tolerance
  of a repeat is asked for (`keep_existing`, `ignore_missing`), as a
  caller that receives the same batch twice needs it. With `revision`,
  the stored revision must equal it, or the call is
  `FAILED_PRECONDITION`. `UpdateDocument`, `UpsertDocument`, and
  `DeleteDocument` on a journal are `UNIMPLEMENTED`. Firestore's
  `UpdateDocument` is our `UpsertDocument`: its name hides an insert, and
  a strict update hides in a precondition.
- **The caller builds the stream, in memory.** Nothing reaches Scheda
  before an item is sent, and no lock is held while the caller builds. In
  Rust the sync form takes an iterator, the async form a stream.
- **Reads see stored state only.** No read sees a write before its call
  returns, the caller's own stream included: the stream is the caller's
  own data, and an overlay would put a transaction back into Scheda
  between calls and make `GetDocument`, `QueryDocuments`, and
  `SearchDocuments` merge two
  sources.
- **Atomic.** Every write is checked first, in order; one failure writes
  nothing and the error names the write by its index. Identical bytes
  change nothing; a call that changes nothing succeeds and stores nothing
  (`19` D5 kept).
- **`note`.** *Settled with the owner, 17 October 2026.* Every write of a
  Document carries a note from the caller: why it was made, which may
  carry the caller's own logic. It is required (empty is
  `INVALID_ARGUMENT`); Scheda does not make one up. The history of a
  Document (`GetDocumentHistory`) shows it; a stream's stored history
  text is the notes of its writes, in order. A collection takes no note:
  there is no history of a collection to show it, so Scheda describes it
  itself (`create collection notes (standard)`), and `CreateCollection` /
  `UpdateCollection` take the `Collection` as it is.
- **Messages.** Each Document write is one message with the Document (or
  its key, for a delete), its options, and its `note`:
  `DocumentCreation`, `DocumentUpdate`, `DocumentUpsert`,
  `DocumentDeletion`.
- **Replies.** *Settled with the owner, 17 October 2026.* As in
  Firestore: `CreateDocument`, `UpdateDocument`, and `UpsertDocument`
  return the `Document` as stored after the call, with its new revision;
  `DeleteDocument` returns `Empty`, since a delete either happened or did
  not. With `keep_existing` the reply is the Document that was already
  stored, not the one sent. Sending `raw` back costs no copy: the request
  is taken by value and its bytes move into the reply. No reply says
  created or kept, updated or unchanged, deleted or absent: the anchor
  consumer checks its own such outcomes only in its tests. A batch
  returns one entry per operation with what its single call returns:
  `DocumentBatchResult` the `Document`, `CollectionBatchResult` the
  `Collection`, or nothing for a delete.
- **When a call returns.** A write call returns when its writes are
  stored. It waits for the write lock at most `OpenOptions.write_timeout_ms`;
  past it the call is `ABORTED` (retryable) and writes nothing. Once it
  holds the lock it finishes. An error always means nothing was written.

How it works inside (not visible in the API). *Settled with the owner,
17 October 2026:* one short lock, no queue (alternative 5).

- **One short lock.** A write call first checks what does not depend on
  the state of the Vault (the collection exists, its kind takes the
  operation, a journal payload without a line break, slugs, encryption
  available); a failure is `INVALID_ARGUMENT` at once, and no lock is
  taken. Then it takes `flock` on `{vault}/.scheda/commit.lock`, waiting
  while another handle holds it, of this process or another. The writes
  of one handle take their turn inside the handle first, so one thread
  of a handle waits on the file. Holding the lock, the call reads the
  state it needs, checks the rest (`ALREADY_EXISTS`, `NOT_FOUND`,
  `revision`), makes one commit, writes the files, and lets the lock go.
  The lock is held for that alone: the caller built the write before it
  called. A write never fails on the lock while its timeout runs.
- **Commit first, files after.** The commit is built from git objects
  on the tree of `HEAD` (D22). `HEAD` moves by compare and swap on the old
  `HEAD`; a `git` command run by hand meanwhile makes the swap fail, and
  the call is `ABORTED` with nothing written. Only then are the files
  written into the work tree, and git's own index brought up to the
  commit for those paths. Scheda never writes a change into the Vault
  that is not committed.
- **A crash between the two.** *Settled with the owner, 17 October
  2026.* Nothing is written into the work tree before the commit, so a
  crash never leaves a part of a write there: before the swap on `HEAD`
  it leaves only git objects that nothing names (git's own collection
  removes them), after it the commit holds the whole write and only its
  files lag. The lock file is the note: holding the lock, the writer
  writes the paths of its write into `commit.lock` before it moves
  `HEAD`, and empties the file once the files are written. `open` and
  every take of the lock read it: a list left there means a writer died
  midway, and those paths are brought up to what `HEAD` holds, except a
  file a person changed (D22), then the file is emptied. If the
  commit had landed, the files become its files; if not, they were never
  touched, and nothing changes. Only the paths of the list: other changes
  in the work tree may be a person's edit (a Vault is readable and
  editable without Scheda) and are never discarded. A writer whose process
  died before its commit wrote nothing, and its call did not return
  `Ok`.
- **Details decided for P4** (17 October 2026; the owner may revisit):
  1. *The commit starts no process (D22):* the new blobs, a tree built on
     the tree of `HEAD`, and the commit are written through the library,
     and the branch moves by compare and swap on the old `HEAD`.
  2. *The message:* its first line is the first line of the first note
     (a collection write: Scheda's own text, `create collection notes
     (standard)`); each Document write adds one trailer `Scheda-Note:`
     whose value is the Protobuf JSON of its key and note on one line, so
     the history of a Document shows its own note even when a batch wrote
     several.
  3. *`Document.revision`* is the id of the Document's blob (D22); a
     history entry's revision is that version's blob id.
  4. *Isolated collections:* their Documents live in the nested
     repository under `.isolated/`, a repository of its own, and two
     repositories cannot take one atomic step. So one batch writes the
     main tree or the isolated collections, never both:
     `INVALID_ARGUMENT`.
  5. *Journal `id` maps* come with the Index (P5); until then a journal
     line is found by a scan of its segments.
- **The mirror.** `Pull` and `Sync` move `HEAD` and the work tree, so
  they take the same lock. `Push` sends what is committed and takes no
  lock. With `auto_sync`, a write pushes after it lets the lock go; a
  failed push does not undo the write and shows in the status. `SetMirror`
  and `RemoveMirror` take the lock, so they never meet a running `Pull`.
  `open` takes no lock of its own: its writes (the marker of a new Vault,
  the `auto_sync` pull) take it as any write does.
- **Order.** *Settled with the owner, 17 October 2026.* Writers go in the
  order they take the lock; there is no priority. A commit is short (the
  indexer embeds and indexes on its own thread, after it), so a write
  waits only for the few before it. The anchor consumer keeps a buffer of
  its own, to answer its MCP clients at once and write through Scheda
  after: that is a need of a server, not of the store.

Why not the alternatives:

1. *Closure transactions:* caller code inside a call cannot be written in
   a contract, cannot cross an `await`, and leaves the CLI and agents with
   no batch.
2. *An interactive bidirectional stream* (`Transaction(stream Step)
   returns (stream Reply)`): it would work, since `Offload` keeps a
   bidirectional call on one blocking thread (start and pump run inside one
   `spawn_blocking`). But the sync form takes the requests as an
   iterator, so an interactive caller has to feed it from a channel while
   it reads the replies, and the write lock is held for as long as the
   caller thinks.
3. *A writer object from a constructor* (the anchor consumer's own
   `claim_writer`): outside the contract, so every binding would invent
   its own.
4. *Several commits at once.* One work tree has one index and one `HEAD`;
   git itself serializes commits (`.git/index.lock`). Parallel commits
   need separate clones or branches and a merge.
5. *A durable queue with a committer role* (the first draft of this
   decision, after the anchor consumer's own path): each call puts its
   batch in a queue directory, one committer elected by `flock` applies
   the batches, and each call waits for an answer file. A write needs no
   durability before `Ok` (a call that did not return `Ok` wrote
   nothing), and the queue brought answer files, a hand-off of the role,
   tickets for replay, and a batch of a later version meeting an earlier
   committer, for no gain once the lock is short. The anchor consumer
   rejected waiting for the lock (D132) when the lock was held for a whole
   transaction; held for one commit, a wait is short.
6. *`Submit` and `Await` as public calls* (an answer at once, the write
   later): no call holds the lock for long, so a caller has no reason to
   answer before its write is stored. A server that must answer its own
   clients at once keeps its own buffer.

Checked against the anchor consumer's real batches: every read inside its
transactions only picks between add, replace, unchanged, and remove
(`knowledge.rs:92`, `writes.rs:237`, `knowledge.rs:569`, `bookmark.rs:79`
in `knowqore-store-scheda`). Each maps to one call: `UpsertDocument`
(a mutable record), `CreateDocument` with `keep_existing` (an immutable
one), `DeleteDocument` with `ignore_missing`; a moved Lesson is a delete
and an upsert in one `BatchDocuments`. Its collection batch
(`lifecycle.rs:336`) is one `BatchCollections` of `create` and `update`
operations. Its config write
(`lifecycle.rs:251`) reads first to pick add or replace, which is
`UpsertDocument`. After its move it keeps its domain encoding (`Encode`:
its records into writes) and its intake buffer, which answers its MCP
clients at once and then writes through Scheda; its writer role, writer
lock, and hand-off go.

New for transports: the CLI and an agent can send a batch
(`scheda batch-documents` and `scheda batch-collections` read the
operations as Protobuf JSON).

**D7. Firestore as the model of the API.** *Settled with the owner,
17 October 2026.* Where Scheda means what Firestore means, it takes
Firestore's shape and names; the noun is `Document`.

| Firestore | Scheda | Note |
| - | - | - |
| `GetDocument` | `GetDocument(DocumentKey)` | |
| `BatchGetDocuments` (stream), `ListDocuments` | `GetDocuments` (stream) | one call: given ids or the whole collection (D8) |
| `ListCollectionIds` | `ListCollections` | the catalog leaves `Info` |
| `CreateDocument` | `CreateDocument` | `keep_existing` is ours |
| `UpdateDocument` (insert or update) | `UpsertDocument` | no `update_mask`: a Document is written whole |
| `UpdateDocument` with `exists: true` | `UpdateDocument` | strict: absent is `NOT_FOUND` |
| `DeleteDocument` | `DeleteDocument` | strict; `ignore_missing` gives Firestore's behaviour |
| `Precondition { exists \| update_time }` | the verb, and `revision` | existence is in the verb; an opaque revision, not a time |
| `Commit(repeated Write)`; the SDKs' `WriteBatch` | `BatchDocuments(stream DocumentOperation)` | the stream is the batch; not Firestore's `BatchWrite`, which is not atomic |
| none | `BatchCollections(stream CollectionOperation)` | Firestore does not declare collections |
| `Write { update \| delete, current_document }` | `Write { create \| update \| upsert \| delete }` | each arm is the request of a single Document call; no collection writes in a batch |
| `Document { name, fields, create_time, update_time }` | `Document { collection, id, raw, revision }` | `raw` is opaque bytes |

Not taken: `BeginTransaction`, `Rollback`, and transactional reads (D6);
`BatchWrite` (not atomic); Firestore's bidirectional `Write`; `Listen`;
`RunAggregationQuery`, `PartitionQuery`, `ExecutePipeline`; structured
`fields`; the `projects/…/databases/…` path.

Scheda's own, outside the model: declared collections with a kind
(`standard`, `journal`, `singleton`), encryption, and isolation; a journal
takes `CreateDocument` as an append; `GetDocumentHistory` (Firestore has
none; see below); `QueryDocuments` and `SearchDocuments` (D19: the
shape of `RunQuery` and its `FindNearest`, but another filter language
and hybrid retrieval); `Push`, `Pull`, `Sync`, `Check`, `Reindex`,
`GetIndexStatus`, `WatchIndexStatus`, and the ports.

- **No collection handle.** Every request names its collection
  (`DocumentKey { collection, id }`, `CollectionKey { name }`).
  `Collection` and `AsyncCollection` go. `Collection::schema`, `kind`,
  `segment_max_bytes`, and `singleton_id` become
  `GetCollection(CollectionKey) returns (Collection)`.
- **`Collection`** is the object of a collection, as `Document` is of a
  Document: `CollectionInfo` (the message of the collection file, `name`
  and `schema`) is renamed, now that the Rust handle no longer holds the
  name. The file does not change: it carries field names, not the
  message name. `ListCollections` returns `Collection`s.
- **Create and update a collection.** `create_collection` today also
  replaces an existing declaration (LIB-21). After Firestore's split,
  `CreateCollection(Collection) returns (Collection)` fails with
  `ALREADY_EXISTS` on an existing name, and
  `UpdateCollection(Collection) returns (Collection)` changes the schema of
  an existing one (absent: `NOT_FOUND`), keeping today's rule: the kind
  and `isolated` stay unless the collection holds no Documents. Both
  return the `Collection` as stored, defaults filled in (as Firestore's
  `CreateDocument` returns the Document).
- **Delete a collection.** *Settled with the owner, 17 October 2026.*
  `DeleteCollection(CollectionDeletion) returns (Empty)` removes the
  declaration of an empty collection, its directory (and
  `{vault}/.isolated/{name}/` for an isolated one) in one write. A
  collection that holds Documents is `FAILED_PRECONDITION`: the caller
  deletes them first, so no call wipes a collection by mistake. Absent is
  `NOT_FOUND`, or nothing with `ignore_missing`. No note, as for the other
  collection writes. The name can be declared again afterwards. Firestore
  has no such call: its collections vanish when their last Document does;
  ours are declared, so they are deleted too.
- **`QueryDocuments` and `SearchDocuments` scope** (D19). They take
  exactly one of `collections`
  (Vault Filter scope) or `collection` (collection Filter scope: a bare
  Projector field is that collection), so the two scopes of LIB-10 and
  LIB-11 stay distinct.
- **`Document.revision`.** An opaque token that changes whenever the
  stored bytes of a `standard` or `singleton` Document change. Set by
  every read; unset on journal lines. Read-modify-write is
  `GetDocument`, compute, a write with `revision`, and on
  `FAILED_PRECONDITION` read again.
- **`GetDocumentHistory(DocumentKey) returns (stream DocumentHistoryEntry)`**
  is today's `history(id, offset, count)`: the past versions of one
  Document, newest first, as a lazy stream (D8) instead of pages: the
  caller reads as many as it needs and stops. Each version has its `revision` (today the commit
  id, `hash`), `time`, `author`, `note` (today the commit message), and
  the `raw` of that version. A journal line never changes and has one
  version only, so history of a journal is `UNIMPLEMENTED` (today it
  answers with the commits of the whole segment file).

**D8. One read of many Documents, as a stream.** *Settled with the
owner, 17 October 2026.* `GetDocuments(DocumentSelection) returns (stream
Document)` replaces `ids`, `documents`, `lines`, and a batch get.
`DocumentSelection` names one collection, `include_raw`, and a required
`oneof selection` of two shapes, so no wrong mix can be written:

- `ids` (an `Ids` message): exactly those, in the order given; an id that
  is not stored is skipped; an empty list is nothing.
- `listing` (a `Listing { limit, start_after }` message): the collection,
  `standard` and `singleton` in `id` order, a journal in append order.
  *Pages, settled with the owner, 17 October 2026:* `start_after` (an
  `id`) begins the walk after that `id`, and `limit` stops it after that
  many Documents, so a list shown page by page reads each page from where
  the last one ended, never from the start again, and a Document added
  meanwhile does not shift the pages. Both unset: the whole collection.
- Neither set is `INVALID_ARGUMENT`: the whole collection is asked for by
  name (`listing`), never by leaving something out, so a caller whose
  list of ids came out empty does not read everything.
- `raw` only when `include_raw` asks; without it, keys and revisions
  only, which is today's `ids`. The reply is a lazy stream.

It needs no Index. One call reads one collection.

**Every returned stream is lazy.** *Settled with the owner, 17 October
2026.* A stream (`GetDocuments`, `GetDocumentHistory`, and any later one)
reads the next item only when the caller asks for it, never the whole
answer first: a collection may hold millions of Documents. A caller that
stops early pays for what it read and no more. In the sync form a stream
is a `BoxIter`; the async form gets a stream instead of today's whole
`Vec`, with protocontract's bounded channel in between (16 items).
Concretely:

- `GetDocuments` walks the trees of the collection in the commit it
  started on (D22) as it goes, and reads a Document's blob only when that
  Document is next. Yielding in `id` order sorts the entries of one tree
  at a time, so memory follows the largest directory, not the collection.
  A journal is read segment by segment, line by line.
- `GetDocumentHistory` reads the git log of the file as it goes and the
  bytes of a version only when that version is next.
- No stream collects its items into a `Vec` inside Scheda, the async form
  included.

`QueryDocuments` is such a stream too (D19): Tantivy's collector needs a
page up front, so Scheda asks it for one page at a time, from one
snapshot. `WatchIndexStatus` (D18) is a stream that never ends by itself.

**D9. Errors.**

- `scheda::Error` is `protocontract::Error`, and `VaultErrorCode` is the
  enum of the `Vault` service: the plugin names it in the rustdoc of
  `VaultSync` and `VaultAsync`.
- A code is what a caller acts on. Where Scheda means what a gRPC
  canonical code (`google.rpc.Code`, which Firestore returns) means, the
  code takes that name; the rest are Scheda's own, without git terms. The
  cause travels as `std::error::Error::source`: the `std::io::Error`, the
  git stderr, the Tantivy or USearch error. Nothing of today's detail is
  lost: `source().downcast_ref::<std::io::Error>()` still gives the
  `ErrorKind`.

  | `VaultErrorCode` | Retryable | Replaces | The caller |
  | - | - | - | - |
  | `INVALID_ARGUMENT` | no | `invalid` (`not_utf8` goes, D20) | fixes the request |
  | `NOT_FOUND` | no | `not_found` | treats it as absent |
  | `ALREADY_EXISTS` | no | `duplicate_id` | treats it as present |
  | `FAILED_PRECONDITION` | no | new (D6 `revision`) | reads again |
  | `UNIMPLEMENTED` | no | `unsupported`, `unsupported_encryption`, a method of a feature the build lacks (D2) | calls something else or uses another build |
  | `ABORTED` | yes | `locked`, `git_locked`; a write that did not get the write lock within its timeout, or whose `HEAD` moved under it (D6) | tries again |
  | `UNAVAILABLE` | yes | `io_network_*`, `io_connection_*`, `io_host_unreachable`, `io_timed_out`, `git_timeout`, a port's `UNAVAILABLE` | tries again later |
  | `UNAUTHENTICATED` | no | `git_auth`, `git_certificate` | supplies credentials for the mirror |
  | `NOT_A_VAULT` | no | `not_a_vault` | opens another path |
  | `GENERATION` | no | new (D1) | moves the Vault to this generation |
  | `CRYPTO` | no | `crypto` | supplies the secret |
  | `MIRROR_AHEAD` | no | `not_fast_forward` | pulls, then tries again |
  | `MIRROR_CONFLICT` | no | `git_merge_conflict`, `git_conflict` | resolves by hand |
  | `UNRESOLVED_CONFLICT` | no | `conflict_markers` (D20: from git's unmerged entries) | finishes the merge |
  | `INDEX_REBUILDING` | yes | `index_version` with `repeat: allowed`, `index_missing` (D19) | tries again soon |
  | `INDEX_STALE` | no | `index_version` with `repeat: forbidden` | runs `Reindex` |
  | `INDEX_BROKEN` | no | `index_corrupt`, `index_lexical`, `index_vector` | runs `Reindex` |
  | `STORAGE` | per kind | every other `io_*` | reports it |
  | `REPLICATION` | no | every other `git_*` | reports it |

  `STORAGE` is retryable for the transient kinds (`Interrupted`,
  `WouldBlock`, `ResourceBusy`). A panic of a port or of Scheda under the
  async form is protocontract's `RuntimeCode::Panicked`.
- `retryable` replaces `repeat`. A method that knows better than the
  table (today's single `with_repeat` site) sets it on the instance.
- `path`: one Scheda type, `scheda::Fault`, implements
  `protocontract::Fault` with a code, a message, an optional path, the
  retry flag, and the source. `Display` prints the path; diagnostics read
  it with `downcast_ref::<scheda::Fault>()`. Callers do not branch on it.
- `stack` is protocontract's (`Context::context`, `with_context`).
- Removed: `api.v1.Error`, `Code`, `Repeat`, the 74 error items in
  `rust/api/lib.rs` (`error_ctors!`, `known_code`, `known_repeat`, …).
  `redact_remote_text` stays internal: messages still must not carry
  credentials.
- For transports, `scheda.v1.ErrorReport { code, message, retryable,
  stack, path }`, with `code` the code's name
  (`VAULT_ERROR_CODE_NOT_FOUND`, or a runtime or port code). CLI stderr and
  `IndexState.last_error` use it; the library API does not.

Recommendation: yes, with the table as above.

**D10. Copies.** Requests are taken by value: moved, not cloned. Scheda
copies only what it still needs after a port call:

- `Embed`: the texts are Scheda's own (Projector output): moved.
- `Project`: the indexer reads the Document from a commit and needs only
  its collection and id after projection. `upsert_legs` and
  `project_record` take it by value and move `raw` into `RawDocument` (P5
  verifies that no later step reads `raw`).
- `Score`: the Documents are read for scoring and moved into the
  `ScoreItem` stream. With `include_raw`, the bytes of the hits are needed
  afterwards: those are copied (at most `rerank_limit` Documents).
- `protoc-gen-buffa` has no `bytes_type` option (checked 17 October 2026:
  the plugin refuses `bytes_type=…` and `use_bytes_type`; only
  `buffa-build` has it), so `bytes` stays `Vec<u8>`. Adding the option
  upstream is out of scope (§6).

Recommendation: yes.

**D11. Threads, and async at the edges.** *Settled with the owner,
17 October 2026.*

- **The background work runs on dedicated OS threads, in every build.**
  The indexer does blocking work only: files and `fsync`, git, Tantivy
  and USearch writes, `flock`. That must not run on
  a runtime's worker threads, and a loop that never ends is not what
  `spawn_blocking` is for (it would hold a pool thread for good). Tokio
  tasks instead would tie the life of a handle to a runtime (a runtime
  stopped first would cancel a write midway; a sync caller has none),
  and would make two implementations of the background work. A write
  runs on the thread of its call (D6), never behind the indexer: an
  indexer round can embed for minutes.
- **`AsyncVault`** (feature `tokio`) implements `VaultAsync` by hand,
  not through the generated `impl … for Offload<Vault>`:
  - a call runs the sync call on the blocking pool (`Offload::call`);
  - a wait is native and holds no thread: the background threads notify
    through async channels. `WatchIndexStatus` reads a `watch` channel,
    as `wait_indexed` does today. A write awaits its turn among the
    writes of the handle on an async lock, then takes the file lock and
    commits on the blocking pool, so one pool thread of a handle at most
    waits on another process;
  - `AsyncVault::open` runs `Vault::open` on the blocking pool.
- **A port is called the way Scheda itself is called.** A sync call of
  the Vault calls ports synchronously (an async port through `Blocking`).
  An async call awaits them (a sync port through `Offload`). So a search
  of `AsyncVault` runs its blocking steps (the lexical leg, the ANN,
  reading the Documents) on the blocking pool and awaits `EmbedQuery`
  and `Score`, which wait on a network: no pool thread stands idle
  while a model answers. The ranking (fusion, rerank, the summary) is
  one shared logic; only the calls of the ports differ. The indexer is a
  sync caller on its own thread, whatever form its embedder has.
- The hand-written surface of `nonblocking.rs` goes; D6 removes its
  transaction special case.

**D12. Proto comments are the method contract.** Each rpc comment states
what the call does, its rules, and the codes it fails with. The plugin
turns it into the rustdoc of both traits, and any other binding reads the
same text. `07` keeps only what spans methods: the shape of a binding,
ports, the write model, consistency, what the API refuses. The per-method
LIB items move into the rpc comments and leave `07`; they are not kept in
both places. Recommendation: yes.

**D13. buf lint.** Requests that are a domain object take it directly
(`GetDocument(DocumentKey) returns (Document)`), as protocontract
recommends for an in-process contract; the rpc names follow Firestore
(D7). Relax `SERVICE_SUFFIX`, `RPC_REQUEST_STANDARD_NAME`,
`RPC_RESPONSE_STANDARD_NAME`, and `RPC_REQUEST_RESPONSE_UNIQUE`. A call
without arguments takes `google.protobuf.Empty`. Recommendation: yes.

**D14. One mirror; isolated collections stay on the machine.** *Settled
with the owner, 17 October 2026.*

- `Push`, `Pull`, and `Sync` take no argument and work with the mirror of
  the Vault only; `Pull` and `Sync` take the write lock (D6). `push_isolated`,
  `pull_isolated`, and `sync_isolated` go.
- **Isolated collections never leave the machine.** Today the nested
  repository under `{vault}/.isolated/` may have a mirror of its own,
  reached by explicit calls (`09` ISO-6). That goes: Scheda never contacts
  a remote for `.isolated/`, whatever that repository has configured.
  Isolated is for data that may be lost or rebuilt (a cache, a derived
  fold). Private data that must survive goes into an encrypted collection
  of the main tree, which reaches the mirror as ciphertext only. The
  anchor consumer learned this the hard way (its D51): it kept its
  person-to-alias mapping isolated, a laptop reinstall lost it, and it
  moved the mapping to an encrypted collection.
- Pull from a mirror whose branch does not exist yet is "nothing to
  pull", so `Sync` against an empty mirror pushes. The anchor consumer
  does that by hand today, on `git_unborn_branch` / `git_not_found`
  (`lifecycle.rs:233`).
- **Set and remove the mirror.** *Settled with the owner, 17 October
  2026.* Today a mirror is given only when `open` clones; on an existing
  Vault `remote_repo` is only compared with `origin`, and adding or
  changing one takes git by hand. `SetMirror(MirrorSetting { url,
  replace_existing }) returns (Empty)`: no mirror, it is set; the same
  URL, nothing happens (a repeat is safe); another URL is
  `ALREADY_EXISTS` naming the current one, unless `replace_existing`.
  `RemoveMirror(Empty) returns (Empty)` detaches it, so the Vault works
  locally only; none is `NOT_FOUND`. Neither contacts the network
  (ISO-1): only `Pull`, `Push`, and `Sync` do. A URL that carries a user
  and password is `INVALID_ARGUMENT`: no secret is kept in the
  repository's settings; credentials come from git, as today. Both go
  take the write lock, so they never meet a running `Pull`.
- **How a pull joins** (decided for P4, 17 October 2026; the owner may
  revisit). `Pull` fetches with the command line (D22), then joins in
  process: a mirror that only adds to the Vault's history moves `HEAD`
  forward; histories that both moved are joined Document by Document,
  never line by line, since a Document is bytes (D20): a file changed on
  one side takes that side, a file changed on both sides the same way
  stays, and a file changed on both sides differently is
  `MIRROR_CONFLICT` with nothing changed. A join is one commit with both
  parents, made under the write lock like any write, and the work tree
  follows it as after a write (D22).
- **`auto_sync`** (the same): the pull at `open` and the push after a
  write are tried, and a failure neither fails `open` nor undoes the
  write; the handle keeps the last failure. Where a caller reads it is
  O4.
- **The word is mirror.** `origin` is git's name and stays inside.
  `Info.remote` becomes `Info.mirror`; `OpenOptions.remote_repo` becomes
  `OpenOptions.mirror` (the URL `open` clones from when the path holds no
  Vault yet).

**D15. `Check` returns a report.** Today `check` fails on the first
problem and puts its file on `Error.path`. `Check(google.protobuf.Empty)
returns (CheckReport)` lists every problem (code, path, message); the call
itself fails only when it cannot run. Recommendation: yes.

**D16. CLI over the contract.** Each subcommand is one rpc: stdout is the
Protobuf JSON of its reply, stderr the `ErrorReport` JSON. New
`batch-documents` and `batch-collections` subcommands read a batch of
operations from stdin. The OpenAI and model2vec
embedders, the Cohere reranker, and the whole-file Projector become port
implementations. The Python driver's `not_found` constant becomes
`VAULT_ERROR_CODE_NOT_FOUND`. Recommendation: yes.

**D17. Version.** Every public name, signature, error code, the CLI error
shape, and the on-disk generation change: `0.22.0` (breaking, minor while
major is `0`), per `.claude/skills/versioning/SKILL.md`. `CHANGELOG.md`
says what a caller changes, under `Changed` and `Removed`.
Recommendation: yes.

**D18. The Index: status as a stream, `Reindex` starts.** *Settled with
the owner, 17 October 2026.* Closes O1.

- The calls on the Index are a group of their own in `Vault`: `Reindex`,
  `GetIndexStatus`, `WatchIndexStatus`.
- `GetIndexStatus(Empty) returns (IndexState)`: the state now, once.
- `WatchIndexStatus(Empty) returns (stream IndexState)` replaces
  `wait_indexed(timeout)`. The first item is the state now, at once; then
  one item for each change (an indexer round, the Index catching up with
  the last write, a rebuild starting, moving on, ending). The stream does
  not end by itself: the caller drops it when it has what it waited for,
  and its own timeout is when it stops reading. The async form is fed by
  the indexer's notifications and holds no thread of the blocking pool.
- `Reindex(Empty) returns (Empty)` starts the rebuild and returns; the
  rebuild runs in the background, and its progress and its result (today
  `ReindexResult`: Documents read, skipped, segments) are in `IndexState`.
  A caller is no longer held for the hours a rebuild with a new model can
  take. A `Reindex` while one runs is `ABORTED`, as `locked` is today.
- In a build without `find`, `GetIndexStatus` and `WatchIndexStatus` are
  `UNIMPLEMENTED` (there is no Index); `Reindex` works in every build:
  without `find` it rebuilds the journal `id` maps, the small index under
  `.scheda/` a journal finds a line by.

**D19. Query selects, search ranks.** *Settled with the owner,
17 October 2026.* Today `find` and `search` overlap: both rank by BM25,
both take the same Filter, and their names say the same thing. They part
by what they promise.

- **`QueryDocuments(DocumentQuery) returns (stream Document)`** selects
  by a condition, as SQL's `WHERE` or Firestore's `RunQuery`: the exact
  set, in a stable order, without scores. `DocumentQuery` holds the scope
  (exactly one of `collections`, the Vault Filter scope, or `collection`,
  the collection Filter scope; D7), the `filter`, `include_raw`, `offset`,
  and an optional `limit` (unset: every match). The order is `id` for
  `standard` and `singleton`, append order for a journal; ordering by a
  column waits for a caller that needs it. A lazy stream (D8): Scheda
  reads the Index in portions and reads Document blobs of `HEAD` as it
  goes (D22), from one snapshot of the Index for the whole stream, so the background
  indexer moves nothing under it: no duplicate and no gap.
  `FindResult.total` leaves the reply.
- **Pages of a query.** *Settled with the owner, 17 October 2026.*
  `DocumentQuery` also takes `start_after` (a `DocumentKey`; across
  several collections the order is collection, then `id`): the page
  begins after that Document. Unlike `offset`, a Document added between
  two pages does not shift them, and a deep page is a condition on the
  key, not a skip over everything before it. The reply stays a lazy
  stream; `offset` stays for a caller that wants it.
- **`CountDocuments(DocumentQuery) returns (Count)`** counts the matches
  of the same query in the Index, opening no file: "247 found, page 3 of
  10", or "how many are left" without reading them, as Firestore's
  `RunAggregationQuery`. It keeps what `find`'s `total` gives today.
  There is no count for a search: the vector leg has no "matches", only
  an order, so a count of a fused ranking says nothing; a count within a
  filter is `CountDocuments` with that filter.
- **`SearchDocuments(DocumentSearch) returns (stream SearchItem)`**
  ranks by relevance, today's `search`: the best `k`, no promise of order
  beyond that. `DocumentSearch` holds the scope, the subject (exactly one
  of `text`, or `like`: the key of a Document to find others like,
  today's `more_like_this`, whose vector leg uses the stored vector and
  calls no model), an optional `filter` that only narrows where it
  searches, `k`, the two leg weights (0 turns a leg off), `rerank_limit`
  (D5), and `include_raw`.
- **A search is a stream too.** `k` may be large (a caller may want a
  million), so the reply is a lazy stream (D8). The ranking itself is
  computed whole first, since the best one is known only after both legs
  are fused (and reranked); but it is keys and scores only, tens of bytes
  a hit. The Documents are read as the stream reaches them, so a large
  `k` reads no file ahead. The first item is always a `SearchSummary`
  (today's `vector_leg`: which legs ran, or why not), known from the
  handle before the search runs; the rest are `SearchHit { Document
  document; score; rerank_score }`, best first. The same `Document` as
  every read: the two calls differ in their wrapper only. The limit of
  10 000 on `k` and `rerank_limit` goes: `k` is bounded by the Index;
  reranking a million hits is a million model calls, which is the
  caller's choice.
- **Pages of a search.** *Settled with the owner, 17 October 2026.*
  `DocumentSearch` also takes `offset`: the stream skips the first hits
  of the ranking and opens none of their files. A ranking is computed
  again by each call (a model may answer a little differently, the Index
  may have moved), so page 2 from a second call need not continue page 1:
  the contract says so. Pages that must agree come from one search with
  a large enough `k`, read as a stream and paged by the caller. The reply
  stays a lazy stream.
- **One Filter, yes or no only.** `all`, `term`, `term_set`, `phrase`,
  `phrase_prefix`, `fuzzy`, `regex`, `range`, `exists`, `boolean`. What
  only changes a score (`boost`, `const_score`, `disjunction_max`) goes:
  a query has no score, and a search ranks by itself.
  `more_like_this` leaves the Filter for `like`.
- **An Index not built yet is `INDEX_REBUILDING`, for both.** An empty
  answer would be a silent wrong one: for a query, empty means "nothing
  matches", and its promise is the exact set; and a stream of Documents
  has no place for a signal. So both calls fail with `INDEX_REBUILDING`
  (retryable) until the Index has a commit (D23); the caller waits
  with `WatchIndexStatus` or tries again. A caller that wants "nothing
  yet" (the anchor consumer turns today's `index_missing` into an empty
  list) decides so itself. An Index that is built but behind the last
  writes answers from what it holds, as today: eventual consistency. The
  search summary keeps its own cases (no embedder, another model, no
  vectors yet while the lexical leg answers).
- The anchor consumer's one search maps as it is: `{ collection:
  "lesson-index", text: intent, k: entries, lexical_weight: 0 }`. It
  calls no `find`.

**D20. A Document is bytes.** *Settled with the owner, 17 October 2026.*
Scheda does not care what a Document holds, so it no longer asks for
UTF-8. The rule came from `04` STO-2 ("readable without the library":
a person opens the file in an editor, `git diff` shows it) and from `16`
decision 1, which put arbitrary binary off as "later". Git keeps any
file; that is enough.

- A `standard` or `singleton` Document is any bytes: text, an image,
  protobuf wire. `not_utf8` goes.
- A journal line keeps the one rule its own form sets, not one about the
  content: it is `{id}: {payload}\n`, so a payload holds no `\n` and no
  `\r`. Otherwise it is bytes too.
- An encrypted collection seals any bytes (ISO-9 loses "UTF-8
  plaintext").
- `Check` no longer checks an encoding, and finds an unfinished merge
  from git's own state (unmerged entries), not from `<<<<<<<` lines in a
  file: a binary file has none, and bytes that happen to look like one
  are not a conflict.
- What is indexed stays the Projector's affair. `raw` is a `bytes` field,
  so Protobuf JSON already carries it as base64; CLI `get` writes the
  bytes as they are.

**D21. Stored data reads what it knows; commands refuse what they do
not.** *Settled with the owner, 17 October 2026.*

- **Stored files are read the Protobuf way.** Every file Scheda keeps as
  a message (the marker, a collection file, the files under `.scheda/`)
  is decoded with unknown fields ignored, then checked by its
  protovalidate rules. A field this Scheda does not know is not an error;
  a broken rule is `INVALID_ARGUMENT`. Today's strict key check
  (`json.rs`, "refuse unknown keys") goes. So a later Scheda may add a
  field without a new generation, and an earlier one keeps reading the
  Vault (D1).
- **A rewrite keeps what it does not know.** A Scheda that rewrites a
  stored file (`UpdateCollection`, …) changes the fields it knows only;
  the fields it does not know stay as they were, since the fields of a
  later version are not its to change. One mechanism for every stored
  message, no code per message and no textual patching of YAML: reading
  records the path and value of each key the generated deserializer
  passes over (serde's ignored-key hook, as the `serde_ignored` crate
  gives it); writing encodes the new message and puts each recorded key
  back under its parent. An unknown field belongs to the message that
  holds it: when that message goes (another arm of a `oneof`, a field
  cleared), its unknown fields go with it, as in Protobuf.
- **Commands are strict.** Protobuf JSON a caller sends (CLI stdin, an
  agent's request) is parsed as Protobuf JSON is by default: an unknown
  field is `INVALID_ARGUMENT`. An earlier binary that silently dropped an
  option of a later caller would do something other than asked, and the
  caller would not know. The same hook serves both: a passed-over key is
  recorded for stored data and refused for a command.

**D22. Everything works on `HEAD`, through a git library.** *Settled with
the owner, 17 October 2026.*

- **`HEAD` is the Vault.** Writes, reads, and indexing all work on `HEAD`.
  A write checks against `HEAD` and builds its commit on it (D6); a read
  (`GetDocument`, `GetDocuments`, `GetDocumentHistory`, the Documents a
  query or a search returns) reads the git objects of `HEAD`, a stream
  those of the commit that was `HEAD` when it started, so it reads one
  snapshot; the indexer reads commits (`17`). The files in the work tree
  are a person's view of the Vault: equal to `HEAD`, or a change a person
  has not committed. No call reads them. So a person's uncommitted edit
  is never seen by a read and never committed by a write, and a crash
  between a commit and its files changes nothing the API answers.
- **The work tree after a write.** Scheda writes the files of a write
  itself (not libgit2's checkout: no filters, owner-only modes, the
  symlink checks) and brings git's index up for them. A file a person
  changed and did not commit is left as it is: the write lands in `HEAD`,
  and git shows the person's file as changed against it. `Check` lists
  such files. Recovery after a crash (D6) keeps to the same rule.
- **A library for local work, the command line for the network.** Local
  git work goes through `git2` (libgit2), in process: objects and trees
  read and written, the branch moved by compare and swap
  (`reference_matching`), git's index, conflicts, status. The `git`
  command line stays only where no library does what system git does:
  `clone`, `fetch`/`pull`, and `push`, with the person's SSH
  configuration, agent, and credential helpers. `gix` was weighed: no
  push, no SHA-256, no reftable, and it does not yet add or remove index
  entries. `git2` is built without its network features (no OpenSSL, no
  libssh2), with fsync of the git directory turned on (libgit2 leaves it
  off). A normal read or write starts no process.
- **`Document.revision`** is the id of the Document's blob in `HEAD`:
  known from the tree without reading the file (it replaces D6 detail 3).
- **Signing.** `OpenOptions.signing` names an SSH or an OpenPGP key to
  sign every commit of the handle with; Scheda names the key, and the
  secret stays in its file or agent. Not built yet: a set value makes
  `open` `UNIMPLEMENTED` (§6). Without it commits are not signed. Git's
  `commit.gpgSign` is not read: Scheda's commits are the handle's
  (`OpenOptions.author`), not the person's whose git configuration it is.
- **Formats.** A repository the bundled libgit2 cannot open (one with
  SHA-256 objects or reftable refs, which libgit2 1.9 does not read) is
  `UNIMPLEMENTED` at `open`. A Vault Scheda makes uses SHA-1 objects and
  loose refs.

**D23. The indexer: one commit for the whole Index.** *Settled with the
owner, 17 October 2026.*

- **A round.** The Index records the one commit it reflects. A round
  takes the index lock (`flock` on `{vault}/.scheda/index.lock`), reads
  `HEAD`, takes the difference between the Index's commit and `HEAD`,
  applies it to every part of the Index (the lexical side, the vector
  side, the journal `id` maps), records `HEAD` as the Index's commit, and
  lets the lock go. Writes, reads, and pulls go on meanwhile: a round
  works between two commits. Then, if `HEAD` moved, another round;
  otherwise the indexer sleeps until it is woken, at once by a write of
  its own process, or by a look at `HEAD` every `index_poll_ms` (reading
  a ref file, no process) for writes of other processes, commits made by
  hand, and pulls.
- **One writer of the Index.** Only the indexer writes the Index, and only
  from commits. Every Index file is written whole (a temporary file, then
  a rename). The lock is held for one round, so the indexer of any
  process can take the next one, and one that dies leaves nothing behind.
- **Both sides move together.** An embedder that fails fails the round,
  which is tried again later; the vectors made before the failure are
  kept for it (D4), and a text that did not change keeps its stored
  vector, so a round that walks every Document again (the lexical side
  ahead of the Index after a failure) sends only the texts that changed. So a model that does not answer holds the lexical
  side back too: the price of one commit for the whole Index.
- **Who indexes.** *Settled with the owner, 17 October 2026.* Handles of
  one Vault may hold different ports (a service with a Projector and an
  embedder, command-line tools with none). A handle takes a round only
  when it has every port the Index needs: with `find`, a Projector; with
  `search`, when a column of a collection is `vector`, an embedder whose
  model is the Index's (any model while the Index has none). A handle
  without them never takes a round: it reads, and its `role` is false;
  its `Reindex` is `FAILED_PRECONDITION`, naming the port it lacks.
  Columns other than the Index's make it `INDEX_STALE`, and an embedder
  of another model sets `model_mismatch` (its search's vector leg says so,
  D19), until a `Reindex` by a handle with the new ports rebuilds the
  Index with them. A Vault opened only by handles without ports is not
  indexed: its queries are `INDEX_REBUILDING`, `GetDocuments` lists
  without the Index, and a journal line is found by a scan.
- **The journal `id` maps** are part of the Index, written by the indexer
  only. `GetDocument` does not depend on them: a line not in the map is
  looked for in the segments changed since the map's commit.
- **One Index, no generations.** A rebuild (`Reindex`, another model,
  another format) is a round of the same indexer under the same lock,
  from an empty Index: it first removes the Index's commit, then indexes
  `HEAD` from nothing, and records the commit last. An Index without a
  commit is not built yet, so meanwhile a query or a search is
  `INDEX_REBUILDING` (retryable, D19), and writes go on as always (they
  do not touch the Index). A rebuild is rare; nothing is built beside the
  Index, and nothing switches under a reader.
- **`IndexState`** has one version (the stored state the Index has
  reached), one count of Documents still to index, and one role, instead
  of a version, a count, and a role per side (P5 changes the proto).
- Why: Scheda `0.21` let writes update the journal `id` maps while the
  indexer and a rebuild wrote them too, wrote some Index files in place,
  switched generations under readers, and let a lock leak into a `git`
  child; each was a race the fast disk of the gate exposed. Each follows
  from breaking one of the rules above. *Settled with the owner:* the
  simplest indexer that cannot race, even at the price of a rebuild that
  makes queries wait.

---

## 3. The contract (sketch)

Names follow D7; the protos under `api/scheda/v1/` hold the messages.
`Empty` is `google.protobuf.Empty`.

```protobuf
package scheda.v1;

// Fails with the codes of `VaultErrorCode`.
service Vault {
  // The Vault.
  rpc Info(Empty) returns (VaultInfo);
  rpc Check(Empty) returns (CheckReport);
  rpc SetMirror(MirrorSetting) returns (Empty);
  rpc RemoveMirror(Empty) returns (Empty);
  rpc Push(Empty) returns (Empty);
  rpc Pull(Empty) returns (Empty);
  rpc Sync(Empty) returns (Empty);

  // Collections.
  rpc ListCollections(Empty) returns (Collections);
  rpc GetCollection(CollectionKey) returns (Collection);
  rpc CreateCollection(Collection) returns (Collection);
  rpc UpdateCollection(Collection) returns (Collection);
  rpc DeleteCollection(CollectionDeletion) returns (Empty);
  rpc BatchCollections(stream CollectionOperation) returns (CollectionBatchResult);

  // Documents.
  rpc GetDocument(DocumentKey) returns (Document);
  rpc GetDocuments(DocumentSelection) returns (stream Document);
  rpc GetDocumentHistory(DocumentKey) returns (stream DocumentHistoryEntry);
  rpc CreateDocument(DocumentCreation) returns (Document);
  rpc UpdateDocument(DocumentUpdate) returns (Document);
  rpc UpsertDocument(DocumentUpsert) returns (Document);
  rpc DeleteDocument(DocumentDeletion) returns (Empty);
  rpc BatchDocuments(stream DocumentOperation) returns (DocumentBatchResult);

  // Index. The two status calls are `UNIMPLEMENTED` without `find`.
  rpc Reindex(Empty) returns (Empty);
  rpc GetIndexStatus(Empty) returns (IndexState);
  rpc WatchIndexStatus(Empty) returns (stream IndexState);

  // Queries (D19). `UNIMPLEMENTED` without `find` (`SearchDocuments`:
  // without `search`).
  rpc QueryDocuments(DocumentQuery) returns (stream Document);
  rpc CountDocuments(DocumentQuery) returns (Count);
  rpc SearchDocuments(DocumentSearch) returns (stream SearchItem);
}

// One item of `BatchDocuments`: the request of one Document call.
message DocumentOperation {
  oneof operation {
    DocumentCreation create = 1;
    DocumentUpdate update = 2;
    DocumentUpsert upsert = 3;
    DocumentDeletion delete = 4;
  }
}

// One item of `BatchCollections`: the request of one collection call.
message CollectionOperation {
  oneof operation {
    Collection create = 1;
    Collection update = 2;
    CollectionDeletion delete = 3;
  }
}

message CollectionDeletion {
  string name = 1;
  bool ignore_missing = 2;
}

message DocumentCreation {
  Document document = 1;
  bool keep_existing = 2;
  string note = 3;
}

message DocumentUpdate {
  Document document = 1;
  string revision = 2;  // set: replace only that revision
  string note = 3;
}

message DocumentUpsert {
  Document document = 1;
  string note = 2;
}

message DocumentDeletion {
  DocumentKey document = 1;
  bool ignore_missing = 2;
  string revision = 3;
  string note = 4;
}

// Ports: the caller implements them, Scheda calls them.
service Embedder {
  rpc Describe(Empty) returns (EmbedderDescriptor);  // model id, dimensions
  rpc Embed(stream Text) returns (stream Vector);
  rpc EmbedQuery(Query) returns (Vector);
}
service Reranker {
  // The first item is the query text, the rest the Documents.
  rpc Score(stream ScoreItem) returns (stream Score);
}
service Projector {
  rpc Columns(CollectionKey) returns (IndexSchema);
  rpc Project(RawDocument) returns (IndexDoc);
}
```

---

## 4. Layers

*How the work goes, settled with the owner, 17 October 2026.*

- **No transitional code.** No bridge, adapter, or conversion between the
  old types and the new. The old surface and its generated code are
  deleted at the start. The tag `v0.21.x` is the old version, and `main`
  becomes the new one when the branch merges.
- **What stays right moves, it is not written again.** The Document
  files, the journal, encryption, isolation, git, Tantivy, USearch, and
  the indexer move onto the new types directly, with their tests, in the
  layer that needs them.
- **Layers, bottom up.** `rust/scheda/src/lib.rs` declares the modules of
  finished layers only; a module not moved yet stays in the tree, not
  compiled, as material for its layer. So each layer compiles and passes
  its own tests. The old sources wait, not compiled, in
  `rust/scheda/src/old/` and `rust/scheda/tests/old/`; a layer takes what
  it needs from there, and P8 deletes what is left. The integration
  tests, the CLI, and the Python driver come back in the layers that bring
  their surface. A signature may
  still change in a later layer (`open` gains the ports in P5): nothing
  outside the branch calls it yet.
- **Branch and gates.** The work goes on the branch `proto-contract`. A
  layer is committed, with its `CHANGELOG.md` lines, when `cargo test`
  and `cargo clippy` (with and without `--all-features`) are green for
  what it holds; the owner waived the `bazel test //...` gate for the
  commits of this branch (17 October 2026). A box is ticked when its
  *Check:* passes in that run. `bazel test //...` runs once P8 is in and
  is green before the branch merges into `main`.
- Every negative test asserts its code with `error.is(…)`.

### P1. The contract and its generate

D1, D13.

- [x] buf as in the sibling projects: the module root is `api/`, one
      `api/BUILD.bazel` over `**/*.proto`, one `buf.gen.yaml`. The
      buffa, packaging, protovalidate, and `protoc-gen-protocontract-buffa`
      (v0.3.1, `gate_tokio_feature`) plugins write into `rust/api`;
      messages and traits are `scheda_api::v1`. The runtime crate
      `protocontract` comes from crates.io, not from git. buf lint
      relaxed per D13.
- [x] Deleted: `api/v1`, `api/collection/v1`, and `api/version.proto`
      with their `BUILD.bazel` files and the three
      `buf.gen.*.rust.yaml`; the generated `rust/api/api/` and
      `rust/api/version/`; the hand-written error items of
      `rust/api/lib.rs`.
- [x] `rust/scheda/src/lib.rs` declares no old module. The CLI, the
      Python driver, and the integration tests are out of the build until
      their layer.
- [x] `version_field_test` reads the new protos; `run-commands.md` and
      `proto-validation.md` name the new target and paths.
- *Check:* `bazel test //api/... //rust/api/...` green (lint, format,
  generate test); `cargo test -p scheda-api` and `cargo clippy -p
  scheda-api` green, with and without `tokio`.
- *Check:* the sync and async traits of the four services and the
  `Dyn…Async` handles exist in every build; the `Offload` and `Blocking`
  bridges only with `tokio` (`rust/api/tests/contract.rs`); the generated
  `Validate` refuses a marker of generation 1.

### P2. Errors

D9.

- [x] `scheda::Error` is `protocontract::Error`; `scheda::Fault` carries
      the code, the message, the path, the retry flag, and the source.
      One module maps causes to `VaultErrorCode` per the D9 table: the
      `std::io::ErrorKind`s, git's failures (the classifier of git's
      stderr moves here), Tantivy, USearch, age.
- [ ] `ErrorReport` from an `Error`, for the CLI and
      `IndexState.last_error`. Waits for a way to read the message of a
      fault of any type from `protocontract::Error` (asked of the owner,
      17 October 2026).
- [x] `.claude/rules/error-handling.md` and
      `.claude/skills/error-handling/` rewritten for the new model, so
      the later layers follow it.
- *Check:* one test per row of the D9 table: the cause gives the code and
  its retry flag.
- *Check:* an I/O failure keeps its `std::io::ErrorKind` through
  `source()`; a file failure keeps its path through `scheda::Fault`;
  `ErrorReport` carries the code's name, the message, the retry flag, the
  stack, and the path.

### P3. Files on disk

D1, D3 (`OpenMode`), D20, D21.

- [x] The stored-message module of D21: unknown fields recorded on read,
      protovalidate, the recorded fields put back on write; the strict
      parse for commands.
- [x] `Vault::open(path, options)` and `close`, with `OpenMode`: `open`
      opens a Vault that exists and refuses what its mode refuses; the
      marker of generation 2 is read before any other file by `open`
      (`Check` reads it the same way in P6); another generation is
      `GENERATION`. Making a Vault and cloning one wait for P4, which
      brings the commit.
- [x] Collection files on `Collection` and `Schema`: no `version`, the
      kind `standard` (`Kind::Document` → `Kind::Standard`).
- [x] A Document is bytes (D20): the UTF-8 rule and `not_utf8` deleted;
      a journal payload is refused only for `\n` or `\r`; `Check` finds
      an unfinished merge from git's unmerged entries. The per-layer
      checks collect their problems (D15) and `check::run` runs them all;
      the `Check` call itself is P6.
- [x] The Document files, the journal segments, encryption, isolation,
      and the slugs on the new types, with their unit tests.
- [x] Fixtures under `rust/scheda/testdata` and `python/cli/testdata`
      moved to generation 2.
- *Check:* a marker of generation 1 fails `open` with `GENERATION` before
  any collection file is read; each refusal of each `OpenMode` (a missing
  path, an empty folder, a git work tree without the marker, another
  folder, a Vault) has its code and writes nothing.
- *Check:* an unknown key in the marker and in a collection file is read
  past, a leftover `version` included; a collection file whose kind is
  the old key `document:` is `INVALID_ARGUMENT` (it names no kind this
  Scheda knows).
- *Check:* a collection file rewritten keeps the keys this Scheda does
  not know, at their depth (the top level, and inside `schema.journal`);
  a change of the kind drops the unknown keys of the old kind; a command
  with an unknown field is `INVALID_ARGUMENT`.
- *Check:* at the file layer, a PNG and bytes that are not UTF-8
  round-trip through `standard`, `singleton`, and an encrypted
  collection unchanged; a journal payload with `\n` or `\r` is
  `INVALID_ARGUMENT`, one with other non-UTF-8 bytes is stored; `Check`
  passes over a binary file that holds `<<<<<<<` and reports a real
  unmerged path.

### P4. Writes and the mirror

D6, D14.

- [x] The write path of D6: the checks before the lock, the write lock
      (`flock` on `.scheda/commit.lock`, the writes of a handle in turn
      before it), the timeout (`ABORTED`), commit first and files after
      with the swap on `HEAD`, the list of paths in `commit.lock` and
      its recovery at `open` and at every take of the lock; a
      symlink on the path of a file is refused before it is written
      (`documents::encode` does not see the Vault). Proto:
      `RESOURCE_EXHAUSTED` removed from `VaultErrorCode`, the comments of
      `ABORTED` and `write_timeout_ms` say the lock, not a queue.
- [x] `CreateDocument`, `UpdateDocument`, `UpsertDocument`,
      `DeleteDocument`, `BatchDocuments`, `CreateCollection`,
      `UpdateCollection`, `DeleteCollection`, `BatchCollections`, and
      `GetDocument` as methods of `Vault`; `note` required on every
      Document write; `Document.revision`.
- [x] `open` makes a Vault where its mode allows (a missing path, an
      empty folder, a git work tree without the marker): the marker and
      `.gitignore` in one commit, or a clone of `OpenOptions.mirror`.
- [x] `Pull`, `Push`, `Sync` without arguments, `Pull` and `Sync` under
      the write lock; pull
      of an empty mirror is "nothing to pull", so `Sync` pushes;
      `SetMirror`, `RemoveMirror`; nothing contacts a remote for
      `.isolated/`.
- [x] D22: local git work through `git2`; every read on `HEAD` (a stream
      on the commit it started on); the declarations and the marker read
      from `HEAD`; the work tree after a write and its recovery leave a
      person's change alone; `revision` is the blob id; `Check` lists
      files a person changed; a repository libgit2 cannot open is
      `UNIMPLEMENTED`; the command line kept for the network only.
- *Check:* a read and a write (without a mirror) start no process.
- *Check:* a person's uncommitted edit of a Document is seen by no read,
  committed by no write of another Document, and survives a write of the
  same Document, which lands in `HEAD` while git shows the person's file
  as changed and `Check` lists it; a commit made by hand is read at once.
- *Check:* a stream started before a write reads on the commit it
  started on.
- *Check:* `Document.revision` is the blob id of the Document in `HEAD`.
- *Check:* `OpenOptions.signing` set is `UNIMPLEMENTED`; a repository
  with `commit.gpgSign` set gets Scheda's commits unsigned.
- *Check:* each mode that makes a Vault does, with a marker of
  generation 2, in one commit; two `open`s of the same new path at once
  make one Vault.
- *Check:* every row and option of the D6 table × `standard`,
  `singleton`, `journal`, encrypted, isolated; a Document write without
  a `note` is `INVALID_ARGUMENT`; the bytes of P3 round-trip through the
  write calls.
- *Check:* a failing write in the middle of a stream, an `Err` item, and
  a dropped stream each store nothing; the first names its index.
- *Check:* identical bytes store nothing and the reply keeps the old
  revision; with `keep_existing` the reply is the stored Document.
- *Check:* a stale `revision` is `FAILED_PRECONDITION`.
- *Check:* `UpdateCollection` keeps the keys of the collection file this
      Scheda does not know (D21), and writes no default the caller did not
      set (the defaults `open` fills in stay in memory).
- *Check:* a `BatchCollections` whose third operation fails declares,
      changes, and deletes nothing; one that succeeds is one write.
- *Check:* `DeleteCollection` of a collection with Documents is
      `FAILED_PRECONDITION` and changes nothing; of an empty one (plain,
      journal, isolated) removes the declaration and the directory; the name
      can be created again.
- *Check:* a write whose call has not returned is seen by no
  `GetDocument` and leaves the work tree untouched.
- *Check:* 64 concurrent write calls from one handle, and from three
      processes, each end stored whole, one commit each; none fails on the
      lock.
- *Check:* a call that cannot take the lock within its timeout (another
      process holds it) is `ABORTED` and stores nothing, then or later.
- *Check:* a writer killed after the commit and before the files: the
      next `open`, and the next take of the lock by a running handle, bring
      exactly those paths to `HEAD` and empty `commit.lock`; killed before
      the commit: nothing is stored and nothing in the work tree changes; a
      person's edit of another file survives both; a `git commit` run by hand
      between the read and the swap makes the write `ABORTED` with nothing
      stored.
- *Check:* `commit.lock` holds paths only, never the bytes of a
      Document, and is empty after every write that returned.
- *Check:* `Sync` against an empty mirror pushes.
- *Check:* a nested repository under `.isolated/` with an `origin`
  configured: no call of Scheda contacts it.
- *Check:* `SetMirror` on a Vault without one sets it; again with the
  same URL changes nothing; with another URL is `ALREADY_EXISTS`, and
  with `replace_existing` replaces it; a URL with a password is
  `INVALID_ARGUMENT`; none of these touches the network.
  `RemoveMirror` detaches it; again is `NOT_FOUND`.

### P5. The Index and the ports

D3, D4, D5, D10, D18, D23.

- [x] `Ports` and `Port` (D3, D4); `Vault::open(path, options, ports)`,
      `set_embedder`; the embedder's `Describe` read and validated once
      per install; `Embed` and `Score` fed as streams, the portions left
      to the implementation; the fixed-portion adapter; the reranker on
      the handle.
- [x] `IndexDoc` and `IndexValue` from proto; the indexer, the lexical
      and vector legs, and the journal `id` maps on the new types; `raw`
      moved into `Project` on the indexing path; `Score` takes the
      Documents by value.
- [x] The indexer of D23: one commit for the whole Index, the round under
      `index.lock`, the wake by a write and by a look at `HEAD`, both
      sides together, who indexes (the ports a handle needs for a
      round), the journal `id` maps written by the indexer only,
      one Index with no generations: a rebuild is a round from an empty
      Index, and meanwhile queries and searches are `INDEX_REBUILDING`.
      The vector side on USearch is one file, `index/vector/vectors`
      (the model, the dimensions, the key map with the digest of each
      text, and the HNSW index), written whole; `index/vector/state`
      names the write it stands on and the state it reached, so a round
      that changes no vector does not rewrite the vectors. The lexical
      side offers the texts of the `vector` columns it projected, so a
      Document is projected once per round, and a text that did not
      change keeps its vector.
- [x] `IndexState` with one opaque version, one count, and one role
      (D23), and the rebuild's progress and result; `Reindex` starts a
      rebuild; `GetIndexStatus` and `WatchIndexStatus` per D18.
- The test ports of the old tests (21 embedders, 8 rerankers, 9
  projectors) move to the generated traits with the tests that use them,
  in P6, P7, and P8: every one of those tests needs a surface of a later
  layer. *Settled with the owner, 18 October 2026.*
- *Check:* an async embedder given as `Port::Async` feeds the indexer;
  a build without `tokio` has no `Port::Async`
  (`vector::tests::an_async_embedder_feeds_the_indexer`;
  `tests/ports.rs`: `a_build_without_tokio_has_no_async_port`, in the
  build without features).
- *Check:* a descriptor that breaks its rules fails `open` with
  `INVALID_ARGUMENT`; a counting port sees one `Describe` per install
  (`tests/ports.rs`:
  `a_descriptor_that_breaks_its_rules_fails_open_and_set_embedder`,
  `open_reads_the_descriptor_once`;
  `ports::tests::describe_is_called_once_per_install_whatever_is_embedded_after`).
- *Check:* a retryable port failure makes the indexer repeat the round;
  `REFUSED` keeps today's non-retryable handling: the text the embedder
  stops at is remembered, and its Document is left out of the vector
  side, as a refused projection leaves one out of the lexical side
  (`vector::tests::a_failing_embedder_fails_the_round_keeps_the_vectors_and_the_next_round_sends_the_rest`,
  `vector::tests::a_text_the_embedder_refuses_leaves_its_document_out_and_the_round_goes_on`,
  `lexical::tests::a_retryable_projector_failure_fails_the_round_and_it_is_tried_again`).
- *Check:* an embedder that fails after the 7th vector of 20: the 7
  vectors are kept, and the next round sends only the other 13; vectors
  come back in the order of the texts whatever portions the
  implementation forms; the last partial portion is flushed when the
  texts end
  (`vector::tests::a_failing_embedder_fails_the_round_keeps_the_vectors_and_the_next_round_sends_the_rest`,
  `vector::tests::the_portions_an_embedder_forms_do_not_change_the_vectors`,
  `ports::tests::a_failure_after_the_seventh_vector_returns_the_seven_and_the_error`,
  `ports::tests::the_adapter_calls_its_function_with_portions_and_flushes_the_last_one`).
- *Check:* after a failed round and a move of `HEAD`, a round that walks
  every Document again sends the embedder only the texts that changed
  (`vector::tests::a_text_that_did_not_change_is_not_embedded_again_when_a_round_walks_every_document`).
- *Check:* the fixed-portion adapter with a size of 10 calls its
  function with portions of at most 10 and gives the same vectors as a
  hand-written stream; a reranker gets the query first and returns one
  score per Document, in order (`tests/ports.rs`:
  `the_embedding_adapter_calls_its_function_with_portions_and_equals_a_stream_by_hand`,
  `the_reranking_adapter_reads_the_query_first_and_scores_in_order`).
- *Check:* who indexes: a handle without a Projector, or without the
  embedder a `vector` column needs, never takes the lock of a round, its
  `role` is false, and its `Reindex` is `FAILED_PRECONDITION` naming the
  port; an embedder of another model sets `model_mismatch`, and a
  `Reindex` from it makes the vectors again with its model; a Vault
  opened only without ports is not indexed and still finds its journal
  lines; such a handle asks its Projector for the columns once per state
  of `HEAD`, not at every look
  (`vector::tests::a_handle_without_a_projector_or_a_needed_embedder_never_takes_a_round`,
  `vector::tests::an_embedder_of_another_model_indexes_nothing_and_a_reindex_from_it_remakes_the_vectors`,
  `vector::tests::a_handle_that_cannot_index_asks_its_projector_for_the_columns_once_per_head`;
  `tests/index.rs`:
  `a_process_without_ports_leaves_the_indexing_to_the_one_with_them`,
  `a_vault_opened_only_without_ports_finds_its_lines_without_an_index`).
- *Check:* the vector side: the values of a Document are in it, replaced
  on a change, removed on a delete; one `Project` per changed Document
  per round for both sides; a journal line gets its vector; a damaged
  `vectors` is `INDEX_BROKEN` and a `Reindex` repairs it
  (`vector::tests::a_documents_vector_values_are_in_the_index_replaced_on_change_and_removed_on_delete`,
  `vector::tests::the_projection_is_shared_one_project_per_changed_document_per_round`,
  `vector::tests::a_journal_line_gets_its_vector`,
  `vector::tests::a_damaged_vector_index_is_broken_and_a_reindex_repairs_it`).
- *Check:* `WatchIndexStatus` gives the state at once, then one item per
  indexer round, and shows a rebuild that `Reindex` started from its
  start to its result; `Reindex` returns before the rebuild ends; a
  second one meanwhile is `ABORTED`
  (`indexer::tests::the_watch_gives_the_state_at_once_and_then_one_item_per_round`,
  `indexer::tests::a_rebuild_waits_for_the_lock_and_a_second_request_is_aborted`;
  `tests/index.rs`:
  `reindex_returns_before_the_rebuild_a_second_is_aborted_and_the_maps_match`).

### P6. The `Vault` service

D2, D3, D5, D7, D8, D15, D19.

- [ ] `impl VaultSync for Vault` in every build, the methods of a missing
      feature answering `UNIMPLEMENTED` (D2); `Info` with the writer, the
      mirror, and the features of the build; `ListCollections`,
      `GetCollection`, `GetDocuments` (per D8: `ids`, an empty `ids`,
      `listing`, and neither), `GetDocumentHistory`; `Check` returns
      `CheckReport`.
- [ ] `QueryDocuments`, `CountDocuments`, and `SearchDocuments` per D19:
      the scope per D7, the Filter without score constructs, `like` in
      place of `more_like_this`, a search as a stream with its summary
      first and no 10 000 limit, an Index not built yet answering
      `INDEX_REBUILDING`; `rerank_limit` per D5.
- [ ] The integration tests under `rust/scheda/tests/` back in the
      build, on the new surface, with their test ports (embedders,
      rerankers, and projectors of `tests/old/`) on the generated traits.
- *Check:* every integration test passes on the new surface.
- *Check:* the streams are lazy: reading the first Document of a
  collection of 100 000 opens one Document file, and a stream dropped
  after it opens no more; the same for the first version of a long
  history.
- *Check:* an `Arc<dyn VaultSync>` drives a Vault end to end.
- *Check:* in a build without `find`, `QueryDocuments`,
  `GetIndexStatus`, and `WatchIndexStatus` are `UNIMPLEMENTED` naming the
  feature, `Reindex` rebuilds the journal `id` maps, and `Info` lists no
  `FIND`; the same for `SearchDocuments` without `search`
  (`tests/thin.rs`).
- *Check:* a write whose call has not returned is seen by no
  `GetDocuments`, `QueryDocuments`, or `SearchDocuments`.
- *Check:* `QueryDocuments` streams lazily from one snapshot: Documents
  written while the stream is read do not appear in it, none is repeated
  or skipped; `limit` and `offset` hold; the order is `id` (a journal:
  append order).
- *Check:* `CountDocuments` equals the number of Documents
  `QueryDocuments` streams for the same query and opens no Document
  file.
- *Check:* pages: `QueryDocuments` and `GetDocuments` with `limit` and
  `start_after` read a collection page by page with no Document twice or
  missed, also when Documents are added between pages; `GetDocuments`
  with neither `ids` nor `listing` is `INVALID_ARGUMENT`;
  `SearchDocuments` with `offset` skips that many hits and opens none of
  their files; every one stays a lazy stream.
- *Check:* `SearchDocuments` with `like` finds a Document's neighbours
  without calling the embedder; on a Vault whose Index was never built,
  a query and a search are both `INDEX_REBUILDING`.
- *Check:* a search with `k` above 10 000 is accepted; its first item is
  the summary; its Documents are opened as the stream reaches them, so a
  stream dropped after the first hit opens one file.
- *Check:* `rerank_limit` without a reranker is `INVALID_ARGUMENT`; a
  sync embedder given to a sync search is called on the caller's thread.
- *Check:* no public name, field, or code of the contract carries a git
  term (commit, `HEAD`, a commit id, `git_`).

### P7. Async

D11.

- [ ] `AsyncVault` implements `VaultAsync`; the hand-written
      `nonblocking.rs` surface deleted; the test ports of
      `tests/old/nonblocking.rs` on the generated traits.
- *Check:* the async surface passes on a multi-thread and a
  current-thread runtime; the lazy-stream check of P6 holds in the async
  form.
- *Check:* on a blocking pool of one thread, an open `WatchIndexStatus`
  stream does not stop another call.
- *Check:* a panicking port under the async form is `Panicked`.
- *Check:* on a blocking pool of one thread, an async search whose async
  embedder takes a second to answer does not stop another call (the
  embedder is awaited, not blocked on); a write waiting for the
  write lock does not either.
- *Check:* the indexer is an OS thread that ends with `close`, also when
  the runtime that opened the `AsyncVault` has been dropped first.

### P8. CLI and acceptance

D16.

- [ ] CLI per D16, with the `batch-documents` and `batch-collections`
      subcommands, and the test ports of its tests on the generated
      traits; the Python driver follows.
- [ ] `tests/consumer.rs` rewritten from the anchor consumer's real use:
      open with a Projector and an embedder port; streams with
      `UpsertDocument`, `CreateDocument` with `keep_existing`, and
      `DeleteDocument` with `ignore_missing`, each sent twice; a
      `BatchCollections` of `create` and `update`; vector-only
      `SearchDocuments`; `set_embedder`; `GetIndexStatus` and
      `WatchIndexStatus` until the Index has a write; `Sync` to an empty
      mirror.
- *Check:* the CLI and the library give the same JSON for `Info`,
  `QueryDocuments`, one `SearchDocuments`, and the same error `code`.
- *Check:* `tests/consumer.rs` green.
- *Check:* `rust/scheda/src/old/` and `rust/scheda/tests/old/` are
  gone; no `expect(dead_code, …)` left from the layering (`git grep -n
  'expect(' rust/scheda/src`); `bazel test //...` green on the branch;
  then it merges into `main`.

---

## 5. Docs pass (once, after the code)

- `02`: Document, Port, Write, Revision, generation, mirror; Record,
  transaction, and autocommit terms removed.
- `01`, `03`, `04`: the marker carries the generation (2); kinds
  `standard`, `journal`, `singleton`; GIT-2 becomes the write model of D6,
  without git terms in the API; STO-2 and every "opaque UTF-8" become
  "bytes" (D20).
- `05`: the collection file has no `version`; the kind key is `standard`;
  SCH-13 retired; an unknown key is read past and kept on a rewrite, a
  command's is refused (D21).
- `06`: IDX-8 and QRY-5 are ports.
- `09`: ISO-6 rewritten per D14: isolated collections never leave the
  machine; what is private and must survive is an encrypted collection.
  ISO-9 seals any bytes (D20).
- `07`: rewritten per D12.
- `08`: AGT-4 and AGT-5 use `ErrorReport`; batches are possible.
- `13`, `14`: codes, CLI subcommands, the stderr shape.
- `10`: the deferrals of §6.
- README, `CHANGELOG.md`, `.claude/notes/README.md` (pointer).
  (`run-commands.md` and `proto-validation.md` move in P1, since the
  layers after it follow them.)

---

## 6. Out of scope

- A remote transport (connect-rust). The services allow it later; a port
  would then be a service the caller serves, or a bidirectional stream.
- An interactive transaction (D6, alternative 2), and a read of one's
  own writes before the call returns (D6).
- A priority among writers (D6, "Order").
- Signing commits with the key `OpenOptions.signing` names (D22): the
  commit buffer signed by `ssh-keygen -Y sign` or `gpg`, then stored with
  its `gpgsig` header; the key checked at `open`.
- `Listen` (a watch on changes, D7).
- A `bytes_type` option in `protoc-gen-buffa` (D10).
- The anchor consumer's move to `0.22.0`: in its own repository, after
  the release. Its call sites that change: ~15 `collection(name)` sites,
  four `transaction` closures (D6 maps each), the error mapping in its
  `error.rs`, `PortEmbedder` (an adapter from one contract to the other,
  without the text copy), `KnowqoreProjector`, the `Sync` fallback (D14),
  and its intake buffer, writer role, and hand-off (D6).
- Moving the anchor consumer's Vaults to generation 2 (D1): by hand, on
  the owner's request, in its test repository: drop `version` from every
  `.collection.yaml`, rename the key `document:` to `standard:`, set the
  marker to `version: 2`, one commit.

---

## 7. Open questions

To settle with the owner before the stage that needs them.

- **O1. `Reindex` blocks or starts.** *Closed by D18 (17 October 2026):*
  it starts the rebuild; `WatchIndexStatus` shows it.
- **O2. A count of matches.** *Closed by D19 (17 October 2026):*
  `CountDocuments(DocumentQuery)`; no count for a search.
- **O4. Where a failed `auto_sync` shows** (D14). The handle keeps the
  last failure of a pull at `open` or a push after a write. Recommendation:
  `VaultInfo` gains it as an `ErrorReport`, once `ErrorReport` can carry
  the message of any fault (P2).
- **O3. A queued batch across versions.** *Gone with the queue (D6,
  alternative 5; 17 October 2026):* each process applies its own writes
  with its own code, so no Scheda applies a write of another version.
