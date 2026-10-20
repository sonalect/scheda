# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
While the major version is 0, compatible additions bump the patch; breaking
API or on-disk format changes bump the minor.

## [Unreleased]

### Added

- `FixedEmbedder::with_query` and `FixedEmbedderAsync::with_query`: an
  optional query function, for a model that embeds a search text otherwise
  than a stored one (an instruction before the text, another task type).
  The blocking form takes `Fn(String) -> Result<Vec<f32>, Error>`, the
  async form a function that returns a future of that. When given,
  `EmbedQuery` calls it with the text instead of the portion function; a
  vector of another length than the declared dimensions is
  `INVALID_ARGUMENT`, and its failure is the failure of `EmbedQuery`.
  `Embed` still goes to the portion function. Without it, `EmbedQuery` is
  a portion of one, as before; `new` is unchanged.

### Fixed

- `Index.Status` and `Index.Watch` show a port mismatch as soon as the
  ports change. Before, a Vault whose Index was at `HEAD` and that was
  given an embedder of another model or a Projector of another
  `projection_id` (`Vault.SetEmbedder`, `SetProjector`, or the ports of
  `VaultOpening`) failed its searches with `INDEX_STALE` while
  `IndexState.last_error` stayed unset until a write moved `HEAD`; the
  same for an embedder removed while a `vector` column needs one. Now the
  round such a change wakes compares the ports with the Index, with no
  write: `last_error` is `INDEX_STALE` with the reason a search gives, or
  `FAILED_PRECONDITION` naming the port the Vault lacks, `Index.Watch`
  emits that state, and setting the Index's port again clears it. A
  caller can read the state to decide whether to call `Index.Reindex`,
  with no probe search. For the same reason a Vault of other ports lost
  its `INDEX_STALE` from the state once another Vault brought the Index
  to `HEAD`; it now keeps it. A look with no port changed and the same
  live Index compares nothing, so an idle look costs what it did. Once
  `Index.Reindex` is asked for, a refusal found by a look made before the
  request is not published, and `scheda index reindex` reads past an
  `INDEX_STALE` that comes before its rebuild starts instead of failing.
- An indexer round over a commit that changes many files in one folder is
  linear in the files it changes. It looked each changed file up by path,
  which read and hashed the folder's tree once per file: for 100 000 new
  files in one collection folder, about 400 GB hashed, and `Vault.Close`,
  or a runtime dropped meanwhile, waited for that step. The blob of each
  file now comes from the tree comparison itself.
- TLS trust of the emSign intermediates whose keys were compromised:
  `rustls-webpki` 0.103.16 rejects certificate paths through them
  (RUSTSEC-2026-0382), `webpki-roots` 1.0.10 drops the emSign roots
  (RUSTSEC-2026-0381), and in the Python sidecar lock (`uv.lock`) `certifi`
  2026.10.19 drops them too (GHSA-6q48-6c7w-9gw2). All three were taken
  inside the quarantine of freshly published versions on the owner's word,
  after a check: each was published by the project's own maintainers or
  its GitHub release workflow, matches its tag upstream, and changes only
  what its advisory names.

## [0.23.0] - 2026-10-20

This release is breaking: the two Index flags of a collection become one
setting, and the Vault format moves to generation 3. To move from `0.22.1`
(or `0.22.0`), migrate each Vault by hand (no code does it):

- Set the marker `.scheda.yaml` to `version: 3`.
- In each collection file, replace the keys `excludeLexicalIndex` and
  `excludeVectorIndex`: both `true` becomes `indexScope: INDEX_SCOPE_IDS`;
  only `excludeVectorIndex: true` becomes
  `indexScope: INDEX_SCOPE_NO_VECTORS`; only `excludeLexicalIndex: true`
  has no equivalent, so drop it (the columns are then held too) or use
  `INDEX_SCOPE_IDS` (no vectors); neither key set, drop both. Commit.
- Until then `open` fails with `GENERATION`. A leftover old key in a
  generation 3 file is read past and ignored, so remove it.
- From `0.21.0`: the steps of `0.22.0` below, with the marker set straight
  to `version: 3`, plus these.

### Changed

- `Schema.exclude_lexical_index` and `Schema.exclude_vector_index` are
  replaced by one enum, `Schema.index_scope` (`indexScope` in Protobuf JSON
  and in a collection file; field 6): `INDEX_SCOPE_ALL` (unset; identity,
  values, and vectors), `INDEX_SCOPE_NO_VECTORS` (identity and values; the
  embedder is never called for the collection and a search ranks it by the
  lexical leg only), and `INDEX_SCOPE_IDS` (identity only; a query or count
  that filters or orders, and a search of it, is `UNIMPLEMENTED`). An
  undefined value is `INVALID_ARGUMENT`. Changing the scope never makes the
  Index stale: the next round rebuilds the table of that collection. The
  strict parse of a caller's command refuses a command that still carries
  `excludeLexicalIndex` or `excludeVectorIndex`.
- The Vault format is generation 3: `VaultMarker.version` is `3`, and a
  Vault of generation 2 is `GENERATION`.
- On Unix, `git clone`, `fetch`, and `push` run in a session of their own,
  without the caller's terminal. `ssh` can no longer ask at that terminal
  (a key's passphrase, an unknown host key): it fails at once, and the call
  fails with the code of that failure, as it did before in a program with
  no terminal. A signal of the terminal (Ctrl-C) no longer reaches `git`:
  a program that wants a running push or fetch ended on such a signal
  catches it and drops the call, which kills `git` (Scheda installs no
  signal handler, and `Vault.Close` waits for the call).

### Removed

- The state "values out, vectors kept" (`exclude_lexical_index` alone): the
  vector leg alone ranking a collection without columns. A search of a
  collection that holds identity only is now `UNIMPLEMENTED`, as before
  when both flags were set.
- `Schema.exclude_lexical_index` and `Schema.exclude_vector_index`, without
  a reserved field number; field 6 is now `index_scope`.

### Fixed

- A call dropped while `git` runs (`clone`, `fetch`, `push`, and the push
  of `auto_sync`) kills every process `git` started, not `git` alone. For a
  mirror reached by `file://`, `git` runs the mirror's side on the same
  machine (`sh`, `git-receive-pack` or `git-upload-pack`, and the mirror's
  hooks), and for one reached by SSH it runs `ssh`; those kept running after
  the drop, and never ended while a hook of the mirror waited. On Unix the
  process group of the command is killed; on Windows its job object.
- An `OpenOptions.mode` the contract does not define is `INVALID_ARGUMENT`.
  It was taken as `OPEN_MODE_OPEN_OR_CREATE`, so `open` could create a
  Vault the caller did not ask for.
- A Projector column whose `kind` is `COLUMN_KIND_UNSPECIFIED` or not
  defined makes its answer `INVALID_ARGUMENT`, as the contract says. The
  column was left out of the Index without a word.
- The Bazel build no longer compiles protoc and the protobuf C++ library
  from source (about 2,800 C++ compile actions of protobuf, Abseil, and
  zlib). The build scripts of the Lance crates run the prebuilt protoc of
  the proto toolchain with the well-known types (`//bazel:protoc_prefix`,
  `protoc_prefix` of `bazel_utils_buf`); `//bazel:protoc` and
  `//bazel:protoc_include` are gone. A Bazel build or test that reaches
  `@protobuf//:protoc` or a C++ compile of module `protobuf` now fails
  (`bazel/no_protobuf_source.bzl`). Cargo is unchanged: `PROTOC`, or
  `protoc` on `PATH`.
- The Bazel build compiles the Rust graph once, not twice. The Python
  version transition of aspect_rules_py's `py_binary` put the Rust CLI that
  `//python/cli` runs, and every crate under it, in a second configuration
  (about 830 of 1,680 Rust actions); `.bazelrc` now sets
  `@aspect_rules_py//py:python_version` to the version the transition
  writes.

## [0.22.1] - 2026-10-20

Two compatible additions; nothing changes for a caller of `0.22.0`.

### Added

- `DocumentHistoryEntry.write_id` (field 6; `writeId` in Protobuf JSON and
  in the output of the CLI's `collection NAME history`): an opaque id of
  the write that made the version, compared for equality only. Every
  version one write made has the same one: one call, or one
  `Collection.Batch` or `Vault.BatchDocuments`, across Documents and
  collections. Two writes never share one, also in the same second with
  the same note, isolated collections included. It is the same on every
  read, after a reopen, and in a clone, and the version that deleted a
  Document has one too. A caller that keeps one record under ids that
  change over time (deleting it under the old id and creating it under the
  new one in one batch) joins the two versions of that batch by it.
- `FixedEmbedderAsync` and `FixedRerankerAsync`: the async forms of the
  fixed-portion adapters, for a model client that is async. They implement
  `EmbedderAsync` and `RerankerAsync`, so a Vault awaits them on its
  runtime, with no blocking pool and no thread held while the function's
  future waits. `FixedEmbedderAsync::new(model_id, dimensions, portion, f)`
  takes an `f(Vec<String>)` that returns a future of
  `Result<Vec<Vec<f32>>, Error>`; `FixedRerankerAsync::new(portion, f)`
  takes an `f(Arc<str>, Vec<Document>)` that returns a future of
  `Result<Vec<f64>, Error>`, the query shared by every portion. Hand them
  over with `EmbedderRef::from_async` and `RerankerRef::from_async`. They
  portion as `FixedEmbedder` and `FixedReranker` do (the two forms share
  one portioning): portions of at most the given size, the last partial
  portion when the input ends, results in order, `EmbedQuery` a portion of
  one, a count that does not match `INVALID_ARGUMENT`, a failure of the
  function the end of the stream, the model id and dimensions read once at
  install.

## [0.22.0] - 2026-10-20

This release is breaking. The whole API is protobuf services in one
package, `scheda.v1`, passed as references; Scheda is async, on the tokio
runtime of its caller; the Index is on Lance in place of Tantivy and
USearch; and the Vault format is generation 2. To move from `0.21.0`:

- Change the calls to the new objects (`Scheda.open` returns a `VaultRef`,
  which gives collections, the mirror, and the Index), the errors to
  `VaultErrorCode`, and the features to `index` and `encrypt`. Open under
  a tokio runtime, or call `to_sync()` inside a multi-thread one.
- Move each Vault to generation 2 by hand (no code does it): set the
  marker `.scheda.yaml` to `version: 2`, drop `version` from every
  collection file, rename the key `document` to `standard`, and commit.
  Until then `open` fails with `GENERATION`.
- Leave `.scheda/index/` alone. The first round of the new indexer removes
  the files of the old Index (`lexical/`, `vector/`, `columns`, `stale`)
  and builds the Index on Lance again. The CLI indexes only with an
  embedder flag.
- Rewrite the CLI calls: the subcommands are the calls of the contract
  (see `Changed` and `Removed`).

### Added

- The feature `index`, on by default: the Index on Lance (`lance`,
  `lance-index`, `lance-linalg`, pinned `=13.0.0`, no cloud object
  stores), its queries, counts, searches, and the ports. Building it needs
  `protoc` (`PROTOC`, or on `PATH`); `default-features = false` drops it,
  and then `Vault.Index`, the queries, counts, searches, and every port are
  `UNIMPLEMENTED`. `Scheda.Info` lists `FEATURE_INDEX` in a build with it.
- The Index is under `.scheda/index/`: generations `g-…`, the live one
  named by `current`, each a `commit` file (written last) and one table
  per collection with the Projector's columns, a vector and the digest of
  its text per `vector` column, and the files it was made from. The
  indexer writes changed rows in place, compacts small fragments, builds
  the indices (a B-tree per `scalar_index` column, an inverted index per
  `fts` column, IVF_PQ per `vector` column from 10 000 rows on), and
  prunes a table version an hour after it was replaced. A round cut short
  is rolled back to the committed versions by the next one. Nothing is
  indexed from an encrypted collection. A Document the Projector gives no
  values is indexed by its identity; only a failure of the Projector
  leaves it out.
- One background indexer per handle follows `HEAD`; the indexers of
  several handles and processes take turns, one round at a time, and only
  they write the Index. `Index.Reindex` builds a new generation beside the
  live one, which answers until the switch, with the ports of the Vault
  that asked, and returns at once; the old generation is removed an hour
  after. A failed rebuild is tried again after a pause; meanwhile another
  `Reindex` of that Vault is `ABORTED`, and one that another Vault on the
  path served with the same ports and columns is done without a second
  rebuild. `Index.Status` gives an `IndexState` (one `version`, one
  `pending` count of files, one `role`, `stored_version`, `rebuild.cause`)
  and `Index.Watch` streams it, the state now first, replacing
  `wait_indexed`. A journal line is found at once after its write.
  An Index not built yet is `INDEX_REBUILDING`; a damaged one is
  `INDEX_BROKEN` until `Reindex`. Only a `Reindex` changes the projection,
  the columns, or the embedder model: a Vault whose ports differ from the
  Index's finds it `INDEX_STALE` (in `IndexState.last_error` and on its
  queries and searches) and does not index.
- Who indexes: a handle with every port the Index needs (a Projector, and
  an embedder when a collection has a `vector` column). A handle without
  the embedder does not index (`IndexState.last_error` is
  `FAILED_PRECONDITION` naming the port), its `Reindex` is
  `FAILED_PRECONDITION`, and its queries and searches read the Index other
  handles keep, the searches without the vector leg. A text of a `vector`
  column is embedded once: a text whose digest the Index holds keeps its
  vector, a failed round's vectors and Projector answers are kept for the
  next try, and a rebuild with the same embedder model and vector length
  takes the vectors the live generation holds. A text the embedder
  refuses (`REFUSED`) is confirmed alone (sent alone, then the texts after
  it split by halves) and only it keeps no vector; a blank text has none.
  Changing `exclude_lexical_index` or `exclude_vector_index` of a
  collection does not make the Index stale: the next round rebuilds that
  collection's table.
- Queries select: `Collection.Query` and `Vault.QueryDocuments` stream a
  `QuerySummary` with the total, then the Documents a typed `Filter`
  matches, exactly and without scores, in a total order: the `order_by`
  keys (a Document without a value last, in both directions), then the
  collection, then `id` (a journal in append order). `offset`, `limit`,
  and `start_after` page it; `start_after` is a condition on the order, so
  a deep page costs what the first does, and in an `order_by` or a journal
  order an id the Index no longer holds is `FAILED_PRECONDITION` (start
  again). `Collection.Count` and `Vault.CountDocuments` count the same,
  opening no file. A call reads one snapshot of the Index; the Documents
  come from git at the state it reflects. Without names, a Vault query or
  search covers every collection that is neither isolated nor encrypted,
  and reads up to four collections at once (on 8 collections of 2000 to
  5000 Documents, debug build: a query 2.4 times and a search 2.5 times
  faster than one after another, the same results and scores). A Vault
  query applies one filter to every collection in scope: a column one
  collection lacks has no value there, a column none declares is
  `INVALID_ARGUMENT`. A filter names `id` and the Projector's columns, each
  with a value of its kind; deeper than `filter_max_depth` (default 32, at
  most 100) it is `INVALID_ARGUMENT`. An encrypted collection, and one with
  `exclude_lexical_index` when the query filters or orders, is
  `UNIMPLEMENTED` when named and left out of a Vault query otherwise. A
  collection the Index has not reached yet is empty. `limit: 0` gives
  nothing. With `include_raw`, a page reads a journal segment once.
- Searches rank: `Collection.Search` and `Vault.SearchDocuments` stream a
  `SearchSummary` first (whether the vector leg ran, or the first reason it
  did not: not asked, no `vector` column in scope, no embedder, no vector
  in the Index yet), then the hits best first, each with its lexical,
  vector, fused, and rerank scores. The lexical leg is BM25 over the `fts`
  columns, each collection by the statistics of its own table; the vector
  leg is the cosine distance over the `vector` columns, through IVF_PQ
  ranked again with the full vectors, and by an exact scan of the rows no
  index covers yet, so a write is found as soon as the Index holds it. The
  legs are fused by Reciprocal Rank Fusion with the request's weights (0
  turns a leg off; both 0 is `INVALID_ARGUMENT`). The `filter` narrows both
  legs before they rank. The subject is a `text` or a `like` (a Document of
  any collection: others like it, from its indexed text and stored
  vectors, calling no model; never itself). `k` has no upper limit (each
  leg asks for no more hits than its table holds); `offset` skips hits
  without opening their files. `rerank_limit` reranks the first fused hits
  with the Vault's reranker and is `INVALID_ARGUMENT` without one or below
  `k`. A collection with `exclude_lexical_index` is ranked by the vector
  leg, one with `exclude_vector_index` by the lexical leg; one with both is
  `UNIMPLEMENTED` when named and left out of a Vault search.
- `not_before` on a query, a count, or a search: the call waits until the
  Index reflects a state that contains the token's, so a caller finds its
  own write without polling. The token is the `version` of a write's
  reply, of `Vault.Info`, or of `IndexState`. Past `index_wait_timeout_ms`
  (default 60 s) the call is `DEADLINE_EXCEEDED` (retryable);
  `Vault.Close` ends the wait with `FAILED_PRECONDITION`; a token whose
  commit is no longer in the history of `HEAD` does not wait; text that is
  not a token is `INVALID_ARGUMENT`. An Index built with other ports than
  this Vault's is `INDEX_STALE` at once, not waited for; a rebuild this
  handle asked for is waited for.
- Reads of many: `Collection.GetMany` streams the Documents of given ids
  (in their order, the absent skipped) or of the whole collection page by
  page (`listing { limit, start_after }`: `id` order, a journal in append
  order), lazily and on one commit; without `include_raw` it reads no file
  and gives keys and revisions. `Collection.History` streams the versions
  of a Document newest first (revision, time, author, note, bytes; the
  deleting version has no revision and no bytes); a journal is
  `UNIMPLEMENTED`. Both read one Document per item, nothing ahead of the
  reader.
- Documents are read by `Collection.Get` and written by
  `Collection.Create` (in a journal: an append), `Update`, `Upsert`,
  `Delete`, and `Batch`, and across collections by `Vault.BatchDocuments`
  (`VaultDocumentOperation { collection, operation }`). A batch is one
  atomic step: an error writes nothing and names the operation by its
  index. The requests carry the `id` and the bytes
  (`DocumentCreation { id, raw, keep_existing, note }`, `DocumentId`, …);
  `Document` is output only. Each write needs a `note`, shown in the
  Document's history. The calls are strict (`ALREADY_EXISTS`,
  `NOT_FOUND`); `keep_existing` and `ignore_missing` make a repeat
  harmless; `revision` writes only over the revision the caller read
  (`FAILED_PRECONDITION` otherwise). A reply is the Document as stored,
  with its new `revision` (opaque, changes with the stored bytes). One
  batch writes the isolated collections or the others, never both
  (`INVALID_ARGUMENT`). A Document is any bytes: text, an image, protobuf
  wire; the UTF-8 rule and the `not_utf8` code are gone, an encrypted
  collection seals any bytes, and a journal payload holds any bytes except
  `\n` and `\r`.
- A write answers with the state token of the Vault it made:
  `Collection.Create`, `Update`, `Upsert`, and `Delete` return
  `DocumentWriteResult { document, version }` (`document` unset after
  `Delete`), and the batch results carry `version`. A write that stored
  nothing gives the token of the state it found; an empty batch reads it
  without the write lock. `VaultInfo.version` is the token of the stored
  state.
- Several handles and processes write to one Vault by taking turns on a
  short write lock instead of failing with `locked`: a write is one commit,
  made before any file of the Vault changes, so no reader sees a change
  that is not committed. A write that cannot take the lock within
  `OpenOptions.write_timeout_ms` (default 60 s) fails with `ABORTED` and
  writes nothing; a write cut short by a crash is finished by the next
  `open` or write, which touches only that write's files.
- The API works on the commit `HEAD` names, not on the files of the work
  tree: a read never sees a change a person has not committed, and a write
  never commits one. A write still updates the files for people, but
  leaves a file a person changed as it is.
- Collections, on `Vault`: `CreateCollection` refuses a declared name
  (`ALREADY_EXISTS`) instead of replacing it; `UpdateCollection` changes a
  declared one (`NOT_FOUND` otherwise), its kind, `isolated`, and
  `encrypted` only while it holds no Documents (`FAILED_PRECONDITION`), and
  keeps the keys of the file this version does not know;
  `DeleteCollection` removes an empty collection; `BatchCollections`
  declares, changes, and deletes several in one step. A collection object
  names its collection and finds it again on each call: after
  `DeleteCollection` its calls are `NOT_FOUND`, and they work again once
  the name is declared again. The data message `Collection` is
  `CollectionDeclaration` (the file on disk is unchanged).
- `Vault.Check` returns a `CheckReport` of every problem and fails only
  when it cannot run. It reads the marker of the commit `HEAD` names first
  (another generation, a broken marker, or none is the one problem
  reported, path `.scheda.yaml`); a collection file that does not load is a
  problem with its path, and the other collections are still checked. An
  unfinished merge is found from git's own state and reported as
  `UNRESOLVED_CONFLICT` with its path, so a binary file that holds
  `<<<<<<<` lines passes. It skips the Lance tables while a round of the
  indexer holds `.scheda/index.lock`.
- The mirror: `open` of a missing path or an empty folder with
  `OpenOptions.mirror` clones it; `Mirror.Push`, `Pull`, and `Sync` take no
  argument and work with the mirror only; `Mirror.Set` and `Remove` set and
  detach it without touching the network. `Pull` joins histories that both
  moved, Document by Document: a Document changed on both sides
  differently is `MIRROR_CONFLICT` and nothing changes. A mirror that
  holds nothing yet is nothing to pull, so `Sync` against it pushes. With
  `auto_sync`, a failed pull at `open` or push after a write fails
  neither: the write is stored, and `Mirror.Info` shows the failure in
  `last_auto_sync_error`, with the URL and `auto_sync`.
- `Mirror.Pull` fetches the mirror's branches into references of Scheda's
  own (`refs/scheda/mirror/*`, `git fetch --prune`; `FETCH_HEAD` is no
  longer written) and takes the write lock only for the join. The fetches
  of one Vault take turns on `.scheda/fetch.lock` (owner-only, created on
  first use); a pull that finds another pull's fetch running waits up to
  `write_timeout_ms` and then is `ABORTED`, retryable.
- Ports: the caller's `Embedder`, `Reranker`, and `Projector` are given as
  references (`EmbedderRef::from_sync(x)`, `from_async(x)`, …) in
  `VaultOpening`, and change at any time: `Vault.SetEmbedder`,
  `SetReranker`, `SetProjector`, and a `Remove…` for each; setting or
  removing one wakes the indexer. A reference that holds no
  implementation is `INVALID_ARGUMENT`; a port in a build without `index`
  is `UNIMPLEMENTED`. The Vault calls a port in its async form (a blocking
  port runs on the blocking pool, an async one is awaited, a panic in
  either is `PANICKED`). The traits are the generated contract
  (`EmbedderSync`, …): the embedder and the reranker take their items as a
  stream and form their own portions, `FixedEmbedder` and `FixedReranker`
  turn a portion function into one, and the descriptors of the embedder
  and of the Projector (`Projector.Describe`, new: a `projection_id`) are
  read once per install and validated. The Projector is asked once per
  collection, per installed Projector, and per state of `HEAD`, and the
  indexer and the queries share the answers. The Index records the
  projection it was built with; change `projection_id` whenever the same
  Document may project to other values. The reranker is the Vault's, not
  an argument of a search.
- Async only, on the tokio runtime of the caller (either flavor, with the
  time and I/O drivers on: `Builder::enable_all`). `Scheda` implements
  `SchedaAsync`, not `SchedaSync`; the objects behind every reference are
  async (`VaultAsync`, `CollectionAsync`, `MirrorAsync`): take them with
  `to_async_inline()`. A caller without async code builds a multi-thread
  runtime and takes `to_sync()` inside it (protocontract's `Blocking`); on
  a current-thread runtime that form fails with `CANNOT_BLOCK` instead of
  hanging. tokio is a required dependency of `scheda` and `scheda-api`; the
  feature `tokio` is gone (drop it from the feature list). No call holds a
  thread while it waits: a write waits for its turn (first come, first
  served) and for `commit.lock` with an `await`; `git clone`, `fetch`, and
  `push` run as child processes that are awaited; `open` waits for another
  `open` of the same new path the same way. The work itself runs on the
  blocking pool, one step at a time (one commit, one Document read).
- A call whose future is dropped stops at its next `await`: dropped while
  it waits for the write lock, a write stores nothing and takes no lock;
  dropped during its commit, the commit completes and is stored; dropped
  during the push of `auto_sync`, the `git` process is killed, the commit
  stays local, and the next push carries it.
- `Vault.Close` replaces `Vault::close(self)`: every call through the
  Vault, its collections, its mirror, and its Index then fails with
  `FAILED_PRECONDITION`; it returns once the calls that run have ended, the
  indexer has stopped, and no lock is held. A second `Close` does nothing.
  An open stream does not hold it up: its next item is
  `FAILED_PRECONDITION`. A Vault whose references are all dropped closes by
  itself. The indexer is a task on the runtime that opened the Vault, not
  a thread; a Vault dropped without `Close` tells it to stop and does not
  wait.
- `Scheda.Open`: `OpenOptions.mode` (`OPEN_MODE_OPEN_OR_CREATE`, the
  default; `OPEN_MODE_OPEN_EXISTING`; and `OPEN_MODE_CREATE_NEW`, which
  refuses anything already at the path) replaces `existing_only`.
  `OpenOptions.signing` names a key to sign commits with; it is not built
  yet and `open` fails with `UNIMPLEMENTED` when it is set. Scheda's
  commits are not signed, and git's `commit.gpgSign` is not read.
- Errors: `scheda::Error` is `protocontract::Error`, and its codes are
  `scheda::v1::VaultErrorCode`, named after the gRPC canonical codes where
  Scheda means the same (`NOT_FOUND`, `ALREADY_EXISTS`, `UNAVAILABLE`, …)
  and Scheda's own otherwise (`NOT_A_VAULT`, `GENERATION`, `CRYPTO`,
  `MIRROR_AHEAD`, `INDEX_REBUILDING`, `STORAGE`, `REPLICATION`, …), plus
  `DEADLINE_EXCEEDED` (retryable). Branch with
  `error.is(VaultErrorCode::NotFound)` or `error.code_as()`;
  `error.retryable()` replaces `repeat`; the cause is
  `std::error::Error::source` (an I/O failure keeps its `ErrorKind`), and
  the file a failure is about is on `scheda::Fault`
  (`error.downcast_ref::<scheda::Fault>()`). The 39 `io_*` and 14 `git_*`
  codes fold into `STORAGE`, `UNAVAILABLE`, `UNAUTHENTICATED`,
  `MIRROR_AHEAD`, `MIRROR_CONFLICT`, `ABORTED`, and `REPLICATION`.
- `scheda::parse_command` parses the Protobuf JSON of a request strictly
  (a key the message does not know is `INVALID_ARGUMENT` naming its path)
  and validates it, for a transport that takes requests as JSON.
  `scheda::error_report` writes a failure as `v1::ErrorReport` (the name of
  its code, the message, the retry flag, the stack, the path), the form the
  CLI prints on stderr.
- A stored file (the marker, a collection file) with a key this version
  does not know is read past instead of refused, and a rewrite keeps the
  key, so a later version can add a field without a new generation.
  Protobuf JSON a caller sends is still refused when it has a key the
  version does not know.

### Changed

- The API is a set of objects, each a service, passed as references. The
  library contract is protobuf in one package, `scheda.v1`
  (`api/scheda/v1/`); the messages and the traits are generated by
  `protoc-gen-protocontract-buffa` into `scheda::v1`, with the `Offload`
  and `Blocking` bridges. `scheda::Scheda` (the only public type besides
  the error, the fixed-portion adapters, and `parse_age_identity`) opens a
  Vault: `Scheda.open(VaultOpening { path, options, projector, embedder,
  reranker })` returns a `VaultRef`, and `vault_ref.to_async_inline()?`
  gives the Vault. The Vault gives `GetCollection` → `CollectionRef`,
  `Mirror` → `MirrorRef`, and `Index` → `IndexRef`. `scheda::Vault`,
  `scheda::Ports`, and `scheda::Port` are gone, and the structs behind the
  references are not public. `to_async_offload()` of a Scheda reference is
  no longer needed.
- The Vault format is generation 2. The marker `.scheda.yaml` holds
  `version: 2`; `open` reads it before any other file, and a Vault of
  another generation fails with `GENERATION` (see the steps above). A
  collection file has no `version`, and the kind `document` is named
  `standard` (`schema: {standard: {}}`).
- Scheda works on git through the `git2` library in process: a read or a
  write without a mirror starts no `git` process. A repository with
  SHA-256 objects or reftable refs, which the library cannot read, fails
  `open` with `UNIMPLEMENTED`, told by its configuration
  (`extensions.objectformat`, `extensions.refstorage`).
- The default `segment_max_bytes` of a journal is 1 MiB, not 8 MiB. Every
  append rewrites its open segment as a new git object, so the cap bounds
  that cost. A declaration that sets the cap keeps it; a journal without
  one closes its segments sooner (an open segment over 1 MiB is closed by
  the next append, which opens a new one). Closed segments are never
  rewritten, and Documents and ids do not change.
- The loose objects of a repository are packed, in process (the pack
  builder of the git library, no `git` process, under the write lock),
  after a write that leaves more than about 1024 of them, or 8 MiB of them
  after a write that stored a blob of 64 KiB or more, and the nested
  repository of isolated collections is packed the same way. Measured:
  1000 appends of 200-byte lines to one journal left `.git/objects` of
  68.5 MB (80.5 MB on disk) in 4008 loose files, and the last 100 appends
  took 6 times as long as the first 100; now they leave about 7 MB at
  their worst (1 MB just after a packing) in 8 packs, each version of the
  segment a delta of a line, and the write that packs takes about 0.25 s
  where the others take 2 to 10 ms. Packs are merged, the four smallest at
  a time, when there are 16 (a merge of 1 MiB segments can take about
  2 s). A pack is complete before the loose objects it holds are removed,
  so a reader never misses an object; `.keep` packs and a multi-pack index
  are left alone.
- A round of the indexer compacts a table only when 8 fragments of fewer
  than 16 384 rows have piled up, or when a fragment has more than half of
  its rows deleted, so the partly full last fragment is not written again
  at each round. With 1 024-dimension vectors one Document written to a
  table with 4 000 unindexed rows writes about 20 KB on average (30
  rounds), and the ten rounds that built a table of 10 000 such rows wrote
  71 MB of data files, not 224 MB. A read opens at most 7 more small
  fragments. Answers do not change.
- Temporary files of a write are `.scheda.{pid}.{n}.tmp` (see `Fixed`).
- The CLI is the contract on the command line: each subcommand is one
  call, named after it in kebab case (`info`, `list-collections`,
  `create-collection NAME --schema JSON`, `batch-documents`,
  `query-documents --query JSON`, `search-documents --search JSON`,
  `collection NAME get|get-many|history|create|update|upsert|delete|batch|
  query|count|search`, `mirror info|set|remove|push|pull|sync`, `index
  status|watch|reindex`, `scheda-info`). `scheda` runs on a multi-thread
  runtime, and a batch reads its operations from stdin whole before it
  opens the Vault. stdout is the Protobuf JSON of the reply, one line, and
  a stream is JSON Lines; `collection … get` prints the Document as JSON
  (`raw` in base64) instead of its bytes. stderr is an `ErrorReport`
  (`"code": "VAULT_ERROR_CODE_NOT_FOUND"`, …). A request is its Protobuf
  JSON (`--schema`, `--query`, `--search`, or stdin), parsed strictly; a
  batch reads JSON Lines from stdin and writes all or nothing; each write
  takes `--note`, required (it replaces `--message`, which defaulted). The
  open options are global flags (`--mode`, `--mirror`, `--auto-sync`,
  `--author-name` and `--author-email`, `--writer`, `--write-timeout-ms`,
  `--index-wait-timeout-ms`, `--filter-max-depth`), and so are the
  reranker flags, now a port of the Vault used by a search with
  `rerankLimit`. A process indexes only with an embedder flag (the
  whole-file Projector marks `body` `vector`), unless a collection sets
  `excludeVectorIndex`. A query, count, or search answers once the Index
  has reached the state of the Vault at open (its `notBefore`, unless the
  request sets one), and fails with `DEADLINE_EXCEEDED` past
  `--index-wait-timeout-ms` (60 s); `index reindex` waits for the rebuild
  to end; `--no-wait` skips both. `query` and `query-documents` print the
  summary first (`{"summary": {"total": "N"}}`), then one `{"document": …}`
  per line. A write prints its `DocumentWriteResult`
  (`{"document": …, "version": …}`), a batch its result with `version`.
  `index watch --until-settled` replaces `wait-indexed` and stops once
  `version` equals `storedVersion` and no rebuild runs, or on `lastError`.
  An HTTP embedder or reranker failure carries the port's code:
  `UNAVAILABLE` (retryable for a transport failure, 408, 429, and 5xx; not
  for 401 and 403) or `REFUSED`.
- The command line's Projector has the projection
  `scheda-cli/whole-file@2`, recorded with the Index: one text column
  `body`, `fts` and `vector`, as in `0.21.0`, with no scalar index (a
  filter on `body` scans). An Index built with another projection is
  `INDEX_STALE` for the CLI until one `scheda index reindex`, which the
  failure's message says.
- Dependencies, each the newest version at least 2 days old: `serde_json`
  1.0.154, `sha2` 0.11.1, `protocontract` 0.4.1 and
  `protoc-gen-protocontract-buffa` v0.4.1 (references to a service; the
  docs of the generated reference modules link their types), `rules_python`
  2.4.2, `bazel_utils` 0.4.0, Buf CLI v1.74.0,
  `protoc-gen-protovalidate-buffa` v0.10.4 (generated code unchanged), and
  patch and minor updates in the Python sidecar lock (`uv.lock`). The
  Python sidecars and the CLI test driver use vllm 0.32.0 (it fixes
  PYSEC-2026-4319, a denial of service in vLLM's penalty handler) and
  huggingface-hub 2.2.0 with transformers 5.19.0. No API or format change.
- `tokio` 1.54.0, taken two days inside the quarantine of freshly
  published versions on the owner's word, after a check: it was published
  to crates.io by Alice Ryhl (`Darksonn`), an owner of the crate, 44
  seconds after the GitHub release `tokio-1.54.0`, and the package matches
  that tag's commit (`66d34e45`) file for file. It keeps the order of
  waiters on a `Mutex` whose task ran out of its cooperative budget, which
  the writes of a Vault rely on to take their turns in the order they
  came, and fixes a `spawn_blocking` hang. No API change.
- Rust toolchain and `rust-version` (MSRV) are 1.99.1 (from 1.99.0): build
  Scheda with Rust 1.99.1 or newer.

### Removed

- The features `find`, `search`, `simd`, and `openmp`, and with them
  Tantivy, USearch, `cxx`, `numkong`, and `sha1` (and the forks pinned for
  them). A build that turns one of them on no longer resolves: use the
  feature `index`. `FEATURE_FIND` and `FEATURE_SEARCH` are gone from
  `Feature` (now `FEATURE_INDEX` (1) and `FEATURE_ENCRYPT` (2)), and
  `VECTOR_LEG_MODEL_MISMATCH` from `SearchSummary` (another model is
  `INDEX_STALE`). The files of the old Index under `.scheda/index/`
  (`lexical/`, `vector/`, `columns`, `stale`) are removed by the first
  round of the Index on Lance.
- The proto packages `api`, `api.v1`, and `api.collection.v1`, and their
  Rust paths `scheda_api::api::…`, among them `api.v1.Error` with its
  codes, `repeat`, and helper functions (`Error::invalid`,
  `Error::io_not_found`, …): a failure is built as
  `scheda::Fault::new(code, message)`.
- `Collection::version`, `Vault::versions`, and the `version` key of a
  collection file.
- Transactions and the old write calls: `begin`, `commit`, `discard`,
  `transaction`, `add`, `replace`, `remove`, and `create_collection`.
- `push_isolated`, `pull_isolated`, and `sync_isolated`: isolated
  collections never leave the machine. `IndexState` loses its per-side
  fields and `model_mismatch` (its fields are numbered again).
- CLI subcommands without a call of their own: `open` (any subcommand
  opens; `--mode create-new` makes a Vault), `reindex`, `index-status`,
  and `wait-indexed` (now under `index`), `find` and `search` (now
  `query-documents` and `search-documents`, and `collection NAME query` and
  `search`), `push-isolated`, `pull-isolated`, and `sync-isolated`, and
  `collection NAME schema`, `add`, `replace`, `remove`, `documents`, and
  `ids` (now `describe`, `create`, `update`, `delete`, and `get-many`).

### Fixed

Defects of `0.21.0` that a caller could meet:

- `Pull` (and `Sync`, and the pull of `open` with `auto_sync`) held the
  write lock while it fetched, so a slow or stalled network held up the
  writers of any handle or process and failed them past
  `write_timeout_ms`. It now takes the lock only for the join (see
  `Added`).
- `Pull` moved `HEAD` to a commit of the mirror without looking at it, so
  a mirror written by another generation, one with no `.scheda.yaml`, or
  one with a collection file that does not load left a Vault whose every
  call then failed. It now reads the marker and the collection files of
  the commit it would move to first, and refuses with `GENERATION`,
  `NOT_A_VAULT`, or `INVALID_ARGUMENT` naming the file, with `HEAD`, the
  work tree, and git's index as they were. At `open` the refusal is kept
  as the Vault's sync failure.
- A push the mirror rejected for a reason other than holding writes the
  Vault lacks (a hook that declined, a protected branch) was reported as
  not a fast-forward, the code that is `MIRROR_AHEAD` now, as if the
  mirror held writes the Vault had not pulled. It is `REPLICATION` naming
  the mirror's reason; only `fetch first` and `non-fast-forward` are
  `MIRROR_AHEAD`. Failures of `git` are classified by a signal of their
  own where there is one, and by the text of `stderr` only for a refused
  login or certificate (`UNAUTHENTICATED`) and a network that cannot be
  reached or does not answer (`UNAVAILABLE`).
- A temporary file of a write (`.scheda.{n}.tmp`) was numbered per
  process, so one another process, or one that died, left under the same
  name failed the write and was removed by it. The name is now
  `.scheda.{pid}.{n}.tmp`, a name already taken is passed over, and a
  write removes only a temporary file it made.
- CLI: an HTTP embedder or reranker request interrupted by a signal
  (`EINTR`) is sent again, up to three times, instead of failing the
  command.

## [0.21.0] - 2026-10-12

### Added

- A Vault is now recognised by a marker: the file `.scheda.yaml` at its
  root, committed to git, with the content `version: 1` (the generation of
  the Vault format; `api.v1.VaultMarker`). It survives a clone, unlike
  `.scheda/`. A marker that cannot be read (not UTF-8, not YAML, an unknown
  key, a directory in its place) or whose `version` is not 1 makes `open`
  fail in both modes (`invalid`, `not_utf8`, or the I/O code, with `path`
  `.scheda.yaml`) before anything is written.
- `OpenOptions.existing_only` (`OpenOptions::existing_only(bool)`, unset
  means off), a second open mode for a caller that must not change a folder
  it opens by mistake. It opens only a git work tree that already holds the
  marker. A missing path is `not_found`, and a folder without the marker
  (an ordinary git repository, a folder that is not a git work tree, an
  empty folder, a file) is the new error code `not_a_vault`
  (`CODE_NOT_A_VAULT = 104`, `Error::not_a_vault`, `repeat` forbidden). It
  never runs `git init` and never clones, even with `remote_repo` set, and
  every refusal happens before anything is written: no `.scheda/`, no
  `.gitignore` change, no commit. `Vault` and `AsyncVault` behave the same.

### Changed

- `open` in the default mode (`existing_only` unset) now writes and commits
  the marker. A new Vault has it in its first commit with `.gitignore`; a
  clone of a remote without it gets it in one commit on top of the clone; a
  Vault made by an earlier version, and any other git work tree
  (an ordinary repository included), gets one commit that adds
  `.scheda.yaml` (and the `.gitignore` entries when they are missing),
  taken under the write lock like the `.gitignore` repair, and nothing
  else changes. A Vault that already tracks a valid marker is not committed
  to. A caller that counts commits or lists the files of a Vault sees the
  new file and, once, the extra commit. A caller that wants the old
  "refuse what is not mine" behaviour sets `existing_only`.

## [0.20.1] - 2026-10-12

### Changed

- Update `bazel_utils_*` modules from `v0.2.11` to `v0.3.4`. Their lint,
  format, and audit rules now run on all six platforms (Windows through
  busybox-w32, no host bash), so `//bazel:lint`, `//bazel:markdown`, the
  three `buf` lint tests, and `//rust:vuln` are no longer skipped on
  Windows. `bazel/compat.bzl` drops `NOT_WINDOWS`.
- Update the Sonalect forks that patch the build: `cxx` / `cxx-build` to
  `1.0.205-bazel-sandbox-fix` (`cxx` 1.0.205), `numkong` to
  `v7.8.5-bazel-sandbox-fix` and `usearch` to `v2.26.4-bazel-sandbox-fix`
  (`usearch` 2.26.4). The vector index (`search`) now builds against these.
- Update the transitive crates in `Cargo.lock` and the Python lock
  (`vllm` 0.31.0 and the rest of `uv.lock`), `rules_python` 2.4.1, Bazel
  9.3.0, `actions/deploy-pages` v5.0.1, and `protoc-gen-protovalidate-buffa`
  to `v0.10.3`. The generated `*.validate.rs` files under `rust/api/` are
  regenerated; the validation rules in `api/**/*.proto` are unchanged.
- `//python:vuln` runs `uv audit` on `uv.lock` (`uv_audit_test`) instead
  of pip-audit, which `bazel_utils` 0.3.0 removed.

### Fixed

- `uv.lock`: `multidict` from 6.7.1 to 6.9.1 (GHSA-54p9-h82j-f925), which
  `uv audit` reports and pip-audit did not.

## [0.20.0] - 2026-10-05

### Added

- `SearchHits.vector_leg` (`api.v1.SearchHits.VectorLeg`), set on every
  `search` reply (sync, async, and CLI JSON `vectorLeg`): the vector leg
  ran, or why not — not asked (`vector_weight` 0), no requested collection
  with vector slots, no embedder, no vectors in the Index yet, or an
  embedder of another model than the Index vectors. Decided from what the
  handle already holds, without extra I/O. A vector-only query with no hits
  can now tell "nothing matched" from "the vectors were not searched".
- Sync `Vault::transaction(message, f)`: `begin`, `f(&vault)`, then one
  commit per repository the writes changed. `f` returns the caller's own
  error type (`E: From<scheda::Error>`); its error is returned as is after
  the writes are discarded, and Scheda's failures (`begin`, the commit,
  `locked`, an empty message) arrive through `E::from`. A panic in `f`
  discards the writes and resumes the unwind.

### Changed

- `AsyncVault::transaction` now takes `F: FnOnce(&Vault) -> Result<T, E>`
  with `E: From<scheda::Error> + Send + 'static` and returns `Result<T, E>`,
  and runs `Vault::transaction` on one blocking thread. A closure whose
  error type was only implied by `?` needs an annotation, for example
  `|vault| -> Result<_, scheda::Error> { … }`. A panic is still
  `E::from(io_other)` and a cancelled task `E::from(io_interrupted)`;
  Scheda errors no longer carry the `[AsyncVault::transaction]` frame.
- `transaction` (sync and async) whose writes leave both repositories as at
  `HEAD` — nothing written, or only bytes `HEAD` already holds — returns
  `Ok` with the closure's value and makes no commit, where the async form
  used to fail with `invalid` ("nothing to commit"). A replayed batch is
  `Ok`. `Vault::commit` on its own is unchanged: `invalid` when there is
  nothing to commit.
- An `index_version` failure caused by a generation another projector built
  (or none) now carries `repeat: forbidden`, and its message names
  `reindex` from a handle with the projector the Index should follow:
  retrying never helps there. An Index of another format keeps
  `repeat: allowed` (the background rebuild fixes it).
- `search` without a Filter now checks the generation's projector, as `find`
  and a filtered `search` already did: a handle with another projector (or
  none) gets that `index_version` instead of hits from an Index its
  projector did not build.
- A `Vault` handle follows collection declarations made after it opened.
  `collection(name)`, and the names `find` / `search` take or a Filter leaf
  qualifies, read the declarations again from the work tree once before
  answering `not_found`. With `find`, the background indexer reads them
  again whenever the Index passes a commit (on any handle, with or without
  an indexing role), and `wait_indexed` does before it returns; `info()`
  then shows a changed declaration of a known collection. The work tree is
  read, so a handle's own uncommitted declaration stays. A `Collection`
  handle keeps the declaration it was made with.

### Fixed

- The background indexer no longer drops a journal line when two journal
  collections hold segments with the same bytes (same writer, `id`, and
  payload): git stores them as one blob, and only the first collection's
  line reached the Index although the marks moved to `HEAD`. Each journal
  now takes its own lines from a shared blob. A Vault indexed by an earlier
  release keeps the gap until `reindex`.

## [0.19.0] - 2026-10-05

### Added

- Each Index generation records the commit every leg reflects, in
  `{vault}/.scheda/index/<gen>/marks/` (`lexical`, `vector`, `ids`, and
  `isolated-` for the nested repository). `reindex` writes them with the
  commits it built from; the vector mark only when an embedder filled that
  leg (without one, the previous vectors and their mark are carried over).
- `Vault::index_status()` and `Vault::wait_indexed(timeout)` (`find`
  feature), with `IndexStatus`: `HEAD` of the Vault and of
  `{vault}/.isolated/`, the commit each Index leg has reached, how many
  Records each leg has yet to apply (`lexical_pending`, `vector_pending`),
  whether this handle's embedder has another model than the Index vectors
  (`model_mismatch`), which roles this handle holds, and the last indexing
  error. `wait_indexed` waits until
  every leg that has an owner reaches the `HEAD` seen at call time and fails
  with `io_timed_out` after the timeout; a leg nobody can write does not
  make it wait.
- `OpenOptions.index_poll_ms`: how often the background indexer reads
  `HEAD` (10 ms to 1 h, default 1 s).
- Background indexer, started by `open` in a build with `find`. One thread
  per handle follows `HEAD` (woken by this handle's commits, otherwise every
  `index_poll_ms`), applies the diff since the last indexed commit in
  batches, and is joined when the last handle drops. The lexical leg and the
  journal `id` maps belong to whoever holds `{vault}/.scheda/index.lock`
  (only a handle with the generation's projector takes it); the vector leg
  to whoever holds `vector.lock` (only with an embedder of the generation's
  model). Commits from outside Scheda (`git`, another process, a build
  without `find`) are indexed the same way.
  A handle without a role follows what the holders write: `find` sees new
  Tantivy segments at once, and the vector view reloads on the next poll
  or `search`, so a reader opened earlier needs no reopen. A handle that
  takes a role over continues from the last holder's vectors and journal
  order.
- CLI: `scheda index-status` prints where the Index stands, `scheda
  wait-indexed [--timeout-secs N]` prints it after the Index reaches `HEAD`
  (`io_timed_out` past the timeout), and the global `--no-wait` makes
  `find` / `search` skip their wait.
- Crate feature `tokio` (off by default): `scheda::nonblocking` with
  `AsyncVault` and `AsyncCollection` (re-exported at the crate root). Each
  method that does I/O runs the sync one on Tokio's blocking pool with
  owned arguments; cheap getters stay sync; `lines()` / `documents()`
  return a `Vec`. A panicking caller callback is `io_other`, a cancelled
  task `io_interrupted`. `AsyncVault::close().await` stops the indexer
  when it holds the last reference. There is no async `begin` / `commit`:
  `AsyncVault::transaction(message, |vault| …)` runs `begin`, the closure,
  and `commit` on one blocking thread, discards on an error or a panic
  (`io_other`), and is `locked` while the Vault holds a transaction on
  another thread. `AsyncVault::wait_indexed` holds no blocking thread
  while it waits: it wakes on the handle's indexer rounds, and a bounded
  wait needs the runtime's time driver. Adds `tokio 1.53.2` as an optional
  dependency.
- `Vault::close()`: stop and join the indexer (at most one embedder call in
  flight), then drop the handle, with an error where `Drop` has none. Scheda
  installs no signal handler; a program calls `close` on `SIGINT` /
  `SIGTERM`. Being killed instead is safe: the next `open` resumes.

### Changed

- `commit` stages all its paths in one git call, and `history` and
  `path_has_history` use a fixed number of git processes (the log, one object
  check, one batch read of the requested page) instead of one process per
  path or per commit. Results are unchanged. Git commands no longer write
  `core.longpaths` before every commit, `discard`, and reset: it is passed on
  each command and written once when `open` creates or clones a Vault.
- `find` and `search` are eventually consistent: `add` / `replace` /
  `remove` / `commit` no longer update the Index, the background indexer
  does after the commit. Call `Vault::wait_indexed(timeout)` before a query
  that must see a write you just made. Inside a transaction, queries do not
  see its uncommitted writes. Journal `get` still finds a line right after
  `add`.
- `replace` and `remove` find the Record by its file, not the Index: a file
  that exists but was never indexed can be replaced or removed. Removing a
  file git never tracked deletes it without a commit.
- An embedder failure no longer fails or rolls back a write: the indexer
  retries the vector leg with backoff (100 ms doubling to 30 s) and reports
  the failure in `index_status().last_error`. `find` and the lexical leg
  of `search` keep up with `HEAD` meanwhile. `set_embedder` retries at
  once; `wait_indexed` does not shorten the backoff.
- `discard`, `pull`, `sync`, and the isolated variants no longer rebuild the
  Index; the indexer applies what moved.
- An Index generation in another format is rebuilt in the background, and a
  handle follows `{vault}/.scheda/index/current` when another handle or
  process publishes a new generation (it no longer stays on the one it saw
  at `open`).
- The vector dump is written only when a vector slot was added or removed,
  and atomically: `vector/<seq>/` plus a `vector/current` pointer, so a
  reader never loads half of a save.
- CLI `find` / `search` wait up to 60 s for the Index to reach `HEAD`.
- Index format is `6` (`5` before this release); an older Index is rebuilt
  automatically. The first `open` on a Vault whose Index is in format `5`
  or older builds a new generation in the background; `find` / `search`
  fail with `index_version` until it is published, as for any older format.
- `reindex` (with `find`) builds the new generation from the commits at
  `HEAD`, the Vault's and `{vault}/.isolated/`'s, not from the files: a
  file edit no commit holds, or an open transaction's write, is not
  indexed. Commit hand edits and hand-written declarations. Writes made
  during the build are applied by the indexer after publish. It takes no
  `write.lock`: it is no longer `locked` by another handle's transaction,
  and `create_collection`, `discard`, `pull`, and `sync` are no longer
  `locked` while it runs. A second `reindex` is still `locked`. Without
  an embedder it keeps the existing vectors instead of dropping them.
  Without `find`, `reindex` holds `write.lock` for the whole call.
- A damaged Index (Tantivy or vector files that no longer read) is rebuilt
  in the background by the handle that holds the lexical role; until then
  `find` / `search` fail with `index_lexical` / `index_vector` as before.
- `search` with an embedder of another model than the Index vectors no
  longer embeds the query: that leg is off until `reindex`.
- `create_collection` over an existing declaration with another kind is
  `invalid` (kind never changes in place); before it rewrote the declaration
  and the Record files were read as the new kind. Changing `isolated` on a
  collection that already holds Record files (document or singleton files,
  journal segments) is `invalid` too, as for `encrypted`; the declaration file
  and the files are left untouched. An empty collection may still change
  `isolated`.
- `SearchQuery.k` and `SearchQuery.rerank_limit` are at most 10000; a larger
  value is `invalid`. Before they were unbounded, and a huge value made
  `search` allocate for hits that cannot exist. `FuzzyTerm.distance` is at
  most 2 (Tantivy's limit); 3 to 255 was accepted and failed at search time
  with `index_lexical`, and now is `invalid` up front.
- Indexer performance. The vector dump is saved once when a vector pass ends
  or stops (and at geometrically spaced checkpoints during a long first
  pass), not after every embedder batch, so embedding N slots writes O(N)
  bytes instead of O(N²). Journal `id` map changes of a pass, and of a
  restore, are written once per collection; owner-only modes are applied to
  the files written, and the whole `.scheda/` tree is walked only after a
  Tantivy commit. A journal segment is read and parsed once per pass, and a
  Record is projected once per leg, with `format` and the projector dump
  checked once per pass instead of per Record.
- CLI embedder `model_id` no longer carries the URL or the directory path.
  `--embedder-openai-url` / `--embedder-model` now store the model name
  alone (`--embedder-model`, exactly), so an API key in the URL query or
  userinfo never reaches the Index and a changed host is not a model change.
  `--model2vec-embedder` stores `model2vec:` plus 16 hex digits of the
  SHA-256 of `config.json` and the byte length of `model.safetensors`, so
  moving the model directory is not a model change. **Action:** an existing
  CLI Index built with an embedder reports `model_mismatch` (vectors off,
  `search` lexical only) until you run `reindex` once with the same embedder
  flags.
- CLI `--file` no longer reads stdin. A command spawned with an open stdin
  pipe that nobody closes used to block forever; a non-empty stdin together
  with `--file` is no longer `invalid`, it is ignored. A `--file` or stdin
  read failure now carries the I/O code of the OS error (`io_not_found`,
  `io_permission`, `io_is_a_directory`, ...) instead of `io_other`; the file
  path is in the message and `path` stays unset.
- CLI HTTP embedder and reranker check the HTTP status before reading the
  body: a non-JSON error page (an HTML 502) is reported as the status, not as
  a parse failure, and the body is never put in the message. 408, 429 and 5xx
  are retryable (`io_timed_out`, `io_resource_busy`, `io_other`, `repeat`
  allowed); 401 and 403 are `io_permission` with `repeat` forbidden; other
  statuses are `invalid`.
- `Debug` of `Encryption` (and so `OpenOptions`) prints the secret as
  `[REDACTED]` (`debug_redact` on the `passphrase` / `identity` fields), so
  `format!("{:?}", options)` and `tracing::debug!(?options)` no longer leak
  it. The Protobuf JSON mapping (`Serialize`) still carries the secret in
  clear text; do not log or persist that form.

### Fixed

- `pull` (and so `sync`, `pull_isolated`, and the `auto_sync` pull on `open`)
  on a branch with no upstream setting, which is a Vault created by `open`
  with `origin` added later, took the first branch git fetched instead of the
  current one. With a Remote that has another branch sorted first it failed
  with `not_fast_forward` although the current branch was up to date.
  `pull` and `pull_isolated` now fetch the current branch by name; on a
  branch the Remote does not have they fail with `git_not_found`.
- `Vault::info().remote` reported the alphabetically first remote. It is
  `origin` when that remote exists, otherwise empty.
- `range` on a Projector column of kind `integer` or `number` compared the
  numbers as text, so `n >= 5` over the values 9 and 10 returned only 9, and
  negative numbers and fractions ordered wrongly. It now compares by value
  (`integer` columns are exact over the whole `i64` range); a bound that is
  not a finite number is `invalid`. `term` and `term_set` are unchanged.
  This needs Index format `6`, which the background indexer builds on the
  first `open`.
- `check`, `get`, `lines`, and the background indexer no longer treat the text
  `<<<<<<<` or `>>>>>>>` inside a line as a git conflict. A conflict marker is
  a line that starts with `<<<<<<<` or `>>>>>>>` followed by a space or the
  end of the line. Before, one journal line quoting a diff made every other
  line of its segment unreadable by `get`, stopped `lines`, and dropped the
  segment from the Index; a document that merely contained the text failed
  `check`. A real conflicted file still fails `check` with `conflict_markers`.
- Record I/O no longer follows symlinks. A symlink as a Record file, as a
  journal segment, as a directory between the collection directory and the
  file, or as the collection directory or `.isolated/` made `get` read, and
  `add` / `replace` write, outside the Vault. These calls now fail with
  `invalid` and the link's Vault-relative `path`. `check` reports such a link
  as `invalid` with its path; `documents`, `ids`, and `lines` skip a symlinked
  file as before.
- `find` with a huge `count` or `offset` (`u32::MAX`) aborted the process
  while Tantivy reserved memory for the page. The page is now clamped to the
  documents that exist; an `offset` past the end is an empty page with the
  true `total`. `search` clamps each leg's pool the same way.
- `search` with a `filter` ranked the text query first and dropped the
  Records the filter excluded afterwards, so a filter that matched few
  Records could return nothing while `find` found them. The lexical leg now
  runs the filter as a clause next to the text query; ranking is still the
  text query's BM25.
- `search` with a reranker failed with `not_found` for the whole call when
  the Index still listed a Record whose file was just removed. That hit is
  skipped, as `find` skips it.
- A `regex_phrase` pattern Tantivy rejects, or any query Tantivy refuses
  as an invalid argument, failed at search time as `index_lexical` and
  flagged the Index generation as damaged, so the indexer ran a full check.
  It is `invalid` now (a `regex_phrase` pattern when the Filter is compiled)
  and flags nothing.
- `open` treated an Index it could not read (broken Tantivy files, an
  unreadable `ord.json` or vector dump) as an empty one, so `find` returned
  nothing and the ordering counters restarted. The handle now keeps the
  failure: `find` and `search` return its `index_*` code, and the generation
  is flagged so the lexical-role indexer rebuilds it.
- The vector leg changed USearch's search expansion on every query while
  several readers shared the index, a data race in the C++ library. The
  expansion is set once, when the index is created or restored.
- Journal order was corrupted when the lexical and the vector leg were held
  by different processes. The vector process rewrote (or, with an empty
  in-memory map, deleted) `ord.json`, the journal `ord` counters of the
  lexical process, so lines got equal `ord` values and `find` over a
  `journal` lost append order. Only the handle that advanced a counter now
  saves `ord.json`.
- Crash leftovers of another process were not restored. A handle read
  `tx.log` once, at `open`, so a write that a process left uncommitted after
  that was neither restored by this handle's next `begin`, `discard`, or
  write nor kept: the handle overwrote `tx.log` and the file stayed in the
  work tree, readable by `get` and never indexed. The handle now re-reads
  `tx.log` each time it takes `write.lock`.
- A restore that failed (a read-only directory, a git error) emptied
  `tx.log` first, so the list of leftover paths was lost. The list is now
  emptied only after the files are restored. `Vault` and `Collection` use
  one restore and commit routine instead of two copies.
- `find` and `search` on a handle that had not itself written to a `journal`
  skipped lines another handle or process appended after it opened (the
  reply carried the right `total` and fewer records). The handle reads the
  journal `id` map again when its file changed.

### Removed

- The `stale` marker a build without `find` wrote on every write, and the
  `index_lexical` failure it caused: the next `find` build indexes those
  commits from the last indexed one.
- The `reindex` catch-up queue and `reindex/active` marker under
  `{vault}/.scheda/reindex/`, and the 30 s publish window. `open` deletes a
  queue an older release left.

## [0.18.6] - 2026-10-01

### Changed

- Update Rust to `1.99.0` (`rust-version` in `Cargo.toml`, `RUST_VERSION` in
  `rust.MODULE.bazel`; `cargo-bazel-lock.json` repinned).

## [0.18.5] - 2026-10-01

### Changed

- `age` is a direct dependency of `scheda-cli` (same `=0.12.1` pin and `armor`
  feature as the workspace) instead of a crate_universe `cargo.spec`. The spec
  made every repin write a synthetic `direct-cargo-bazel-deps` package into
  `Cargo.lock` that the next `cargo` build removed again; the lockfile is now
  stable across both tools.

## [0.18.4] - 2026-09-30

### Changed

- Move `buf.gen.v1.rust.yaml`, `buf.gen.version.rust.yaml`, and
  `buf.gen.collection.rust.yaml` from the repo root to the `api` folder.
- Update `urllib3` to `2.8.0` (fixes CVE-2026-97687, CVE-2026-97688, and
  CVE-2026-97689).

## [0.18.3] - 2026-09-30

### Changed

- Update `cxx` and `cxx-build` patches to use `sonalect/cxx` fork.
- Update `usearch` patch to use `sonalect/USearch` fork.
- Update `bazel_utils_*` modules to `v0.2.11` from `sonalect/bazel_utils`, and
  `aspect_rules_py` from `sonalect/rules_py`.
- Update `pyjwt` to `2.15.1` (fixes CVE-2026-102274 and CVE-2026-101917).
- Buf uses one root `buf.yaml`. Protobuf and protovalidate protos come from
  Bazel `buf_deps` (`//api/third_party`) instead of BSR `deps`.
- Buf generate templates live in the repo root as `buf.gen.v1.rust.yaml`,
  `buf.gen.version.rust.yaml` and `buf.gen.collection.rust.yaml`.
- Buf `extern_path` for `api.collection.v1` resolves `CollectionInfo` and
  related types to `crate::api::collection::v1` instead of the relative
  `super::collection::v1` path in generated `api.v1` stubs.

## [0.18.2] - 2026-09-27

### Changed

- Update `model2vec-rs` to `0.3.0`.
- Update `numkong` to `v7.8.3-bazel-sandbox-fix` from `sonalect/NumKong`.
- Update vLLM sidecar to `0.30.0` and `huggingface-hub` to `1.33.0`.
- Update Buf `wellknowntypes` to `v36.2`.
- Pinned `bazel_utils_*` modules at `v0.2.10`.
- Update Bazel `protobuf` to `36.2`.
- Update Bazel `rules_python` to `2.3.4`.

## [0.18.1] - 2026-09-22

### Changed

- Generated Rust stubs live in `rust/api` (was `rust/generated`). The crate
  name stays `scheda-api`.

## [0.18.0] - 2026-09-22

### Changed

- Generated stubs crate is `scheda-api` (was `scheda-generated`).
  Protovalidate resolves message types under `crate::api`.

### Added

- `Vault::set_embedder` swaps the embedder on an already open handle.
  Later `collection` calls and `search` use the new value. A collection
  opened before the swap keeps the embedder it copied. `None` turns the
  vector leg off. The lexical index is unchanged.

## [0.17.1] - 2026-09-21

### Changed

- `OpenOptions.remote_repo` accepts an scp-style `git@host:path` as well as
  an absolute URI (`https://…`, `ssh://…`, `file:///…`). Git clone
  understands both; Scheda passes the string to the git CLI as-is. An empty
  string, a filesystem path, and a hybrid `ssh://git@host:path` stay
  `invalid`.

## [0.17.0] - 2026-09-19

### Added

- Proto `api.Version` (`api/version.proto`, package `api`): shared on-disk
  file version, field number 1, value const 1. Generate:
  `bazel run //api:generate`. The Vault loader deserializes each
  collection file to `Version` first, then `require_valid()`, then a
  library check that only version **1** is implemented.

### Changed

- On-disk `{name}/.collection.yaml` (also `.yml` / `.json`) is the
  Protobuf JSON mapping of `api.collection.v1.CollectionInfo` (`version`,
  `name`, nested `schema`). `Vault.info` list items are the same
  type. The loader deserializes `api.Version`, then `CollectionInfo`,
  then applies library slug checks and the journal default of 8 MiB.
- `Schema` (was `Declaration`), `Journal`, `Singleton`, and `Unit` live
  in `api.collection.v1`. `scheda::{Schema, CollectionInfo, Journal,
  Singleton, Unit}` compile. `Vault.create_collection` takes `Schema`
  and writes `version: 1` itself. `Collection::schema()` returns that
  message. CollectionInfo JSON and on-disk files use the key `schema`
  (a leftover `declaration` key is `invalid`). CLI
  `create-collection --schema` and `collection schema`.

### Removed

- `api.collection.v1.CollectionFile`. The type is `CollectionInfo`.
- `api.v1.CollectionInfo`, `api/v1/collection.proto`, and
  `api/v1/declaration.proto`.
- CLI `--declaration` and `collection declaration`. Use `--schema` and
  `collection schema`.

## [0.16.3] - 2026-09-19

### Changed

- Bazel crate universe hub is `scheda_cargo`, so another module that names
  its hub `cargo` can depend on this repo as a `bazel_dep`. Transitive
  `bazel_dep` uses a crate_universe `lockfile`.
- Pin `bazel_utils_*` to tag `v0.2.9`.

## [0.16.2] - 2026-09-18

### Changed

- Pin `bazel_utils_*` to tag `v0.2.8`. Local `buf generate` plugins
  (`protoc-gen-buffa`, `protoc-gen-buffa-packaging`,
  `protoc-gen-protovalidate-buffa`) are prebuilt GitHub-release binaries
  (`protoc.plugin`), not a BSR remote plugin or crate_universe fork tags.

## [0.16.1] - 2026-09-17

### Changed

- Update `mimalloc` to `v3.5.3`.

## [0.16.0] - 2026-09-17

### Added

- **`api.v1.Declaration` replaces `api.v1.Schema`.** Collection declaration
  has no `Field`, `Edge`, `Body`, `Object`, or `Text`. Kind is a required
  oneof (`document` as empty `Unit`, `journal`, `singleton`). Perimeter
  flags (`isolated`, `encrypted`, `exclude_lexical_index`,
  `exclude_vector_index`) stay on the message. There is no proto `version`
  (generation lives on the YAML file). `CollectionInfo.declaration` nests
  that message (JSON key `declaration`). `Collection.decl()` returns it.
  `Vault::create_collection` takes `Declaration`. The proto file is
  `api/v1/declaration.proto` (was `schema.proto`). Wire type names stay
  `api.v1.Declaration` / `Journal` / `Singleton` / `Unit`. The on-disk
  loader is `declaration` (`Decls`), not a crate module named `schema`.
- **`api.v1.IndexSchema` / `IndexColumn` / `ColumnKind` (`index.proto`).**
  `Projector.schema` returns that message. The Projector trait is still a
  caller callback, not proto. `IndexDoc` stays a Rust ingest type.
- **Filter leaf `collection`.** Projector-column leaves on Vault `find` /
  `search` must name the collection. On a Collection handle, unset means
  this collection; a qualifier for another collection is `invalid`. Identity
  `id` is not qualified. Unknown Vault qualifier is `not_found`.
- **CLI `--declaration`.** `create-collection --declaration` is Protobuf
  JSON `Declaration`; `--schema` is the same flag. `collection declaration`
  prints the declaration; `collection schema` is the same subcommand.
- **`Projector` callback on `open` / `open_with`.** `find` columns and
  `search` FTS / vector slots come from `Projector.schema` / `project`, not
  collection-declaration fields. No Projector: identity only (`all`,
  `term` on `id`); `search` has no FTS or vector slots and the embedder is
  not called on write. `project` error or empty omits the Record from the
  lexical Index; the file remains. Changing projector schema is an Index
  format change (`index_version` until `reindex`). One vector slot per
  `vector` column; digest skip unchanged.
- **CLI default whole-file Projector.** Every CLI `open` installs one text
  column `body` (marks `find`, `fts`, and `vector`) whose value is the
  entire UTF-8 Record. Non-UTF-8 bytes omit the Record from the lexical
  Index; the file remains. `find` / `search` Filter leaves other than
  identity and `body` are `invalid`. This default is CLI-only; library
  `Vault::open` does not install it.

### Changed

- **Document 5 file is `docs/05-collection-declaration.md`.** Same `05`
  number and `SCH-*` codes. Leftover on-disk `.schema.yaml` is still that
  name (`invalid`).
- **CLI `collection get` writes `Record.raw` bytes** to stdout (no extra
  newline). Empty Record → empty stdout. Errors stay protobuf JSON on
  stderr.
- **On-disk collection file is `{name}/.collection.yaml` without `fields`.**
  `create_collection` writes generation `version: 1` and never a field list.
  `open` / `check` refuse a leftover `{name}/.schema.yaml` (also `.yml` /
  `.json`) and refuse a leftover `fields` / `body` / `layout` key
  (`invalid`). Isolated / encrypted / journal caps / exclude_* flags still
  round-trip.
- **`document` files are opaque UTF-8 at `{collection}/{id}`.** The library
  no longer appends `.md`. An extension is part of `id` (`foo.md`,
  `work/meeting.toml`).   `add` / `replace` take raw bytes (`impl AsRef<[u8]>`
  in Rust); empty is allowed; non-UTF-8 is `not_utf8` and writes no file.
  CLI `add` / `replace` take `--file` or stdin, not Protobuf JSON.
  `get` returns those bytes without parsing YAML. `documents` / `ids`
  list regular files of any extension and skip dotfiles.
- **`documents()` keeps walking after an `Err`.** A bad file does not hide
  later Records. `lines()` still stops on a broken journal envelope.
- **`singleton` file is `{name}/{declared id}`.** Not always `record.yaml`.
  An extension may be part of that id (`inbox.toml`). Contents are opaque
  UTF-8: `add` / `replace` write the caller bytes; Schema fields are not
  applied. Method `id` must equal the declared id (`invalid` otherwise).
  Second `add` is still `duplicate_id`. `get` / `remove` / `history` /
  `documents` / `ids` use that path.
- **`journal` `add` writes `{id}: {payload}\n`.** Colon U+003A then space
  U+0020. `get` / `lines` `raw` is the payload, not the envelope. Identity is
  the envelope `id`, not a JSON key in the payload. Payload with CR or LF
  is `invalid` and writes no line. Non-UTF-8 is `not_utf8`. `replace` /
  `remove` stay `unsupported`. The IDX-5 id map is rebuilt from envelopes.
- **Collection declaration `version` is the collection generation**, required
  on the YAML/JSON file, with no default. Only generation `1` is implemented.
  `create_collection` always writes `version: 1`. Missing, `0`, or any other
  integer is `invalid`.
- Pin Buf CLI to `v1.73.0` and `bazel_utils_*` to tag `v0.2.6`.
- **`check` does not type-check Record bytes.** It still checks
  declaration files, UTF-8, journal envelopes, `id` uniqueness, conflict
  markers, Index freshness, isolation gitignore, and encrypted armor when
  the handle can read it.
- **`reindex` walks Record bytes** and calls `Projector.project` when
  present. Journal id maps are rebuilt from envelopes.

### Removed

- **`api.v1.Schema`, `Field`, `Edge`, `Body`, `Object`, and `Text`.** Field
  lists, edge graphs, and body marks are not Scheda's. `Collection.schema()`
  is gone (`decl()` returns `Declaration`). A leftover `.schema.yaml` or
  `fields` key is `invalid`.
- **Live Index `edges.jsonl`.** The Index no longer writes an edge graph
  file. Leftover `edges.jsonl` is still pruned as a legacy name.
- **`Record.fields`, `Record.body`, and `Record.valid`.** Callers pass UTF-8
  `raw`. Journal `add` writes `{id}: {payload}\n`; `get` / `lines` `raw` is
  the payload.
- **RFC 8785 canonical JSON of journal lines (LIB-18).** The library no
  longer rewrites consumer fields. Payload bytes are stored as given.
- **`api.v1` `RecordAdd` and `RecordReplace`.** Write is method `raw` bytes.
  Proto `Record` is output-only (`get` / `find` / `search`).
- **`CODE_UNSUPPORTED_REMOTE`.** The compile-time remote-git gate is gone; a
  remote the system git cannot speak is `unsupported` or a `git_*` class.
  `CODE_LOCKED` is 302; `CODE_UNSUPPORTED_ENCRYPTION` is 303.
- **Unused `Error.Code` git values** that the git CLI classifier never
  produced (`GIT_AMBIGUOUS`, `GIT_BUF_SIZE`, `GIT_USER`, `GIT_UNMERGED`,
  `GIT_INVALID_SPEC`, `GIT_MODIFIED`, `GIT_APPLIED`, `GIT_PEEL`, `GIT_EOF`,
  `GIT_INVALID`, `GIT_DIRECTORY`, `GIT_HASHSUM_MISMATCH`, `GIT_INDEX_DIRTY`,
  `GIT_APPLY_FAIL`, `GIT_OWNER`). Remaining `git_*` codes are consecutive
  505–514 (`BARE_REPO` through `HEAD`), the classes `git_code` actually maps
  from stderr. Protobuf JSON callers branch on names, not integers. IO codes
  stay: one per stable `std::io::ErrorKind`.
- **`Record.path` and `SearchHits.Hit.path`.** Identity is `(collection, id)`.
  Vault-relative file paths stay on `Error.path` (which file failed) and
  `VaultInfo.path` (the Vault root).
- **`api.v1.Schema.version`.** Generation lives on the collection file, not on
  the proto. `info` therefore has no version field; the caller does not set it.

## [0.15.2] - 2026-09-16

### Added

- **Hierarchical `document` ids.** A `document` `id` may be `/`-separated
  slugs (`work/meeting`, `2026-01-15/standup`). Each component is a
  directory; the last is the file stem. The whole `id` is still at most
  255 bytes. `get` opens that one path; `replace` does not move the file;
  `remove` unlinks it and prunes empty parent directories. `documents()` /
  `ids()` descend slug-named directories and skip others. `journal`,
  `singleton`, and collection names still reject `/`.

## [0.15.1] - 2026-09-15

### Fixed

- **`History.Entry.message` no longer carries a trailing newline.**
  `history` reads the commit body with `git log --format=…%B`, and git
  stores a newline after the body, so every entry came back as
  `"my subject\n"`. That was a silent contract change from `0.14.0`,
  where the `git` executable replaced libgit2: a caller comparing
  `entry.message` with the message it passed to `add`, `remove`, or
  `commit` stopped matching. Only the terminator is stripped — the blank
  line and the body of a multi-line message are preserved. The proto
  field now states the contract, and the tests assert the whole message
  instead of a substring, which is why the regression survived two
  releases.

## [0.15.0] - 2026-09-15

### Removed

- **Crate features `remote-git` and `vendored-openssl`, and the default
  feature set with them.** `remote-git` was a compile-time gate that
  refused `https://`, `http://`, `ssh://`, and `git@` remotes. Since git
  operations invoke the system `git` executable it no longer enabled any
  library, and a build flag cannot enforce the policy anyway: Cargo
  unifies features across the graph, and `git remote set-url` sits
  outside the crate. Remote URLs are now always accepted, and `pull` /
  `push` no longer spawn `git remote get-url` merely to feed the gate.
  `vendored-openssl` was an empty no-op; nothing links OpenSSL. **A
  dependant that names either feature must drop the name**, or Cargo
  fails with an unknown-feature error. `cargo add scheda` still gets git
  plus Records without Tantivy or USearch, which is what the old default
  set amounted to.

## [0.14.0] - 2026-09-15

### Changed

- **Git operations invoke the system `git` executable.** Clone, fetch,
  push, commit, and the rest of the Vault git surface no longer link
  `git2` / libgit2 / libssh2. **`git` must be installed and on
  `PATH`.** Authentication now behaves exactly like a plain `git`
  command, including `~/.ssh/config`, `ProxyCommand`, credential
  helpers, and host-key checking. Scheda does not impose `BatchMode`:
  it does not set `GIT_SSH_COMMAND` and does not read `core.sshCommand`.
  An operator who wants BatchMode sets `GIT_SSH_COMMAND` in the
  environment. Crate feature `remote-git` remains the policy gate that
  refuses `https://` / `ssh://` / `git@` remotes when it is off; it no
  longer enables a library. `vendored-openssl` is kept as an empty
  feature so existing `--features vendored-openssl` builds still
  resolve.

## [0.13.0] - 2026-09-15

### Added

- **`OpenOptions.writer` and `Vault::writer()`.** Each Vault clone has a
  machine-local writer identity, stored in `{vault}/.scheda/writer`
  (gitignored). Unset at `open` generates sixteen lowercase hex digits.
  A set value is a short lowercase token (`invalid` if empty, illegal,
  or if it starts with eight hex digits). Downstream code can reuse the
  same identity for its own per-writer records.

### Changed

- **Journal segments are named per writer.** Sequence files are
  `{seq}-{writer}.jsonl`; daily files are `{date}-{writer}.jsonl` then
  `{date}-{seq}-{writer}.jsonl`. A clone appends only to its own
  segments, so independent clones of one Vault no longer collide on
  segment paths. Writerless names from older Vaults are still iterated,
  found, and checked.
- **Generated writer identity** mixes process id, clock nanoseconds, and
  hashed randomness so two clones do not depend on `RandomState` staying
  non-deterministic.

### Fixed

- **A write from another thread can no longer join an open
  transaction.** While one thread holds the write lock on a shared
  `Vault` (including after `begin`), a mutating call, `commit`, or
  `discard` from another thread is retryable `locked` and does not
  enter that transaction.
- **A writer token that starts with eight hex plus a hyphen
  (`abcdef01-node`) is `invalid`.** It used to pass and then fail to
  match its own journal segments, so every append minted a new file.

## [0.12.0] - 2026-09-15

### Changed

- **`open` of an existing Vault no longer ignores `remote_repo`.** A set
  URL must match the fetch URL of `origin`. No `origin`, or a different
  URL, is `invalid`; `origin` is not added or rewritten. Unset
  `remote_repo` still attaches and leaves `origin` unchanged. Clone of a
  missing path is unchanged.

## [0.11.3] - 2026-09-15

### Changed

- **`protoc-gen-protovalidate-buffa` comes from the
  `protoc-gen-protovalidate-buffa-v0.10.0` fork tag via crate_universe
  `generate_binaries` on the `cargo` hub.** The in-tree dummy crate, short
  `pvb` hub, and `protoc_prefix` PROTOC annotation are gone.
- **`protoc-gen-buffa-packaging` comes from the `v0.9.2-bazel-packaging`
  fork tag via crate_universe `generate_binaries` on the `cargo` hub.** The
  in-tree dummy crate, crates.io tarball `http_archive`, and handwritten
  `rust_binary` BUILD are gone.
- **`aspect_rules_py` comes from the `v1.12.1-windows-fix-backported-3`
  fork tag.** The in-tree Windows uv-host patch is gone;
  `host_platform_repo` reports `msvc` on Windows, and git consumers download
  official `v1.12.1` venv/unpack/shim tools.
- **`cxx` / `cxx-build` come from the `1.0.202-bazel-sandbox-fix-2` fork
  tag.** In-tree crate_universe patches are gone: `make_include_dir`
  writes `rust/cxx.h` in `OUT_DIR`, Bazel runs `build.rs`, and subcrate
  `build.rs` is a no-op so crate_universe can consume git. Workspace `cxx` is
  `1.0.202`.
- **`numkong` comes from the `v7.8.2-bazel-sandbox-fix` fork tag.** The
  in-tree include-to-outdir patch is gone; `build.rs` copies headers into
  `OUT_DIR` so Bazel sandboxes can compile USearch.
- **`usearch` comes from the `v2.26.2-bazel-sandbox-fix` fork tag.** The
  in-tree drop-out-dir-symlinks patch is gone; `build.rs` unlinks
  cxx-build's relative `OUT_DIR` links after compile.
- Update `mimalloc` to `v3.5.2`.
- huggingface-hub 1.31.0 for the vLLM sidecars.

### Fixed

- **`//rust:vuln` no longer fails on rustls 0.23.44
  (RUSTSEC-2026-0285).** The lockfile uses `0.23.45`, which rejects TLS 1.3
  handshake messages at the wrong encryption level. `ureq` stays `3.4.1`.

## [0.11.2] - 2026-09-12

### Fixed

- **vLLM sidecars on WSL2 no longer die at EngineCore with `UVA is not
  available`.** vLLM 0.29's default V2 GPU runner needs pinned host
  memory; on WSL it turns pin_memory off. `python/vllm/serve.py` sets
  `VLLM_WSL2_ENABLE_PIN_MEMORY=1` unless the operator already chose, so
  V2 can start. Embedder, reranker, and the Jina binaries share that
  launcher. Native Linux is unchanged.

## [0.11.1] - 2026-09-10

### Fixed

- **Journal `get` no longer follows a hand-edited `id` map off the
  collection.** A line whose `path` is absolute or walks `..` is skipped
  on load, so `get` does not open that file. `check` reports the map as
  `index_corrupt`.
- **`create_collection` restores the Schema file if a later step fails
  outside a transaction.** A leftover `{name}/.schema.yaml` is no longer
  visible on the next `open` before the next mutating call. An open
  transaction still owns earlier Record paths until `commit` or
  `discard`.

### Changed

- **Documents match shipped P16.** `07` LIB-13 no longer ties
  `OpenOptions.encryption` to `{vault}/.isolated/`. Isolated I/O does
  not require a secret unless the collection is also `encrypted`.
- **Pinned `bazel_utils_*` modules at `v0.2.5`.**
- protovalidate-buffa 0.7.2 and `protoc-gen-protovalidate-buffa` 0.10.0.
  Generated Validate impls sit beside the Buffa types
  (`packaging=false`).
- Update `tantivy` to `0.26.2` and `ureq` to `3.4.1`.
- vLLM sidecars: `vllm==0.29.0` in `uv.lock`.

## [0.11.0] - 2026-09-07

### Added

- **`schema_from_json` / `schema_from_value` refuse unknown Schema keys.**
  Protobuf JSON used to drop a leftover `layout` key and
  `create_collection` would write a file. YAML Schema files already
  denied unknown keys. The CLI `--schema` flag uses this parser.
- **The CLI takes an encryption secret from an environment variable or an
  identity file**, never from argv. `--encryption-passphrase-env-var` and
  `--encryption-identity-file` are mutually exclusive. A missing or empty
  variable, and an unreadable or malformed identity file, are `invalid`
  before a Vault is opened. An `age-keygen` file (comment lines, trailing
  newline) is accepted. A Vault that holds an encrypted collection can
  now be opened from the command line.
- **`push_isolated` / `pull_isolated` / `sync_isolated`** reach a Remote
  on `{vault}/.isolated/` without touching the main tree. Vault `push` /
  `pull` / `sync` still publish the main tree only. `check` no longer
  treats a nested Remote as a defect. The CLI encodes
  `push-isolated` / `pull-isolated` / `sync-isolated`.
- **CLI `collection NAME documents` and `ids`.** Encrypted collections
  are not in the Index, so listing them was library-only. `ids` works
  without a secret; `documents` needs one (`crypto` without it).
  `journal` is `unsupported`.

### Changed

- **`Vault::versions` returns `Result`.** A poisoned schema lock is
  `io_other`, same as the other schema reads, instead of returning the
  last stored versions.

## [0.10.3] - 2026-09-07

### Added

- **`Embedder::embed_query` names the search-query role of a text.** The
  default calls `embed`, so every current implementation stays a one-line
  no-op. `search` uses the query method for the query string and still
  uses `embed` / `embed_many` for record fields. An implementation may
  encode the two roles differently, or do nothing.

## [0.10.2] - 2026-09-06

### Changed

- **With a reranker, each Index leg's top hits always reach the candidate
  pool.** Fusion used Reciprocal Rank Fusion both to score hits and to
  truncate the pool. RRF rewards agreement, so a document only one leg
  ranked first could lose its slot to documents both legs ranked around
  30. The fused order is discarded when a reranker is present, so
  membership is now filled by alternating the vector and lexical ranks
  (skipping duplicates; leftover slots go to the longer leg). RRF scores
  are still reported. Without a reranker the fused page is unchanged:
  weighted RRF still decides order and truncation. `lexical_weight` /
  `vector_weight` still turn a leg off at zero; they no longer decide who
  enters the pool when a reranker is supplied.

## [0.10.1] - 2026-09-06

### Fixed

- **Dropping a `Vault` no longer releases `write.lock` while a
  `Collection` still holds it.** Returning a collection from a function
  that opened the Vault, or dropping the Vault after `begin`, used to
  unlock the flock for a second process. The lock stays until the last
  shared handle is gone.
- **A failed catch-up enqueue no longer splits Index from files.** When
  `replace` / `remove` wrote the Record (and updated live `N`) but could
  not write the queue file during an online `reindex`, the Index forgot
  the id while git restored the file — or the reverse. The mutating call
  now restores the file from git and rebuilds that Index slot from disk,
  so `get` / `find` agree with what is on disk.
- **Journal catch-up no longer scans every segment for each queued id.**
  Catch-up reads the live generation `ids` file. An id the walk already
  mapped is not appended again.

### Changed

- **Pinned `bazel_utils_*` modules at `v0.2.4`.** Lint and generate tools
  stay on the lockstep tag; Scheda's own `rules_rust` / `rules_python` /
  protobuf pins already match that release.
- **`reindex` walks each indexed collection once and streams Records.**
  The search-engine pass used to parse every file again after the stats /
  journal-map walk and hold the collection in a `Vec`. Indexed journals
  write their `id` map in that same pass. Builds without `find` still
  walk once for stats and maps.
- **Vector search depth no longer reshuffles the head of the ranking.**
  HNSW connectivity and insert / search expansion are set by Scheda
  (`M` 16, `ef` construction 128, `ef` search 512), not USearch
  zero-defaults. A request for `N` neighbours is the prefix of a request
  for `2N` (and both match brute-force nearest neighbours on a small
  index). An existing dump keeps the graph it was built with; search
  expansion is re-applied on restore. `reindex` rebuilds the graph with
  the named construction values.
- **Unchanged vector slots are found in one lookup, not a scan of every
  key.** Incremental `add` / `replace` / `reindex` used to walk the whole
  slot table for each Record. The Index now keeps a `(collection, id)`
  map to those keys, so skip / drop / copy of one Record stays
  proportional to that Record's fields.

## [0.10.0] - 2026-09-06

### Changed

- **Online `reindex`.** `reindex` holds `write.lock` only for start and
  publish. `add` / `replace` / `remove` stay allowed during the file walk
  (one writer per mutating call). A second flock,
  `{vault}/.scheda/reindex.lock`, allows one rebuild at a time; a second
  `reindex` is `locked`. Catch-up is a folder of queue files under
  `{vault}/.scheda/reindex/queue/`. `create_collection` / `pull` /
  `sync` / `discard` are `locked` while a rebuild is active; `push` is
  not. Publish retries `write.lock` for 30 seconds at 25 ms; an open
  transaction that holds it throughout fails with `locked` and leaves
  `current` unchanged.
- **A rebuild does not pay the embedder twice.** A write that lands
  during the file walk is caught up by copying its vector slots from the
  live generation's dump when `model_id` and dimensions match, so a
  network embedder is not called again for text it already embedded,
  including when that write came from another handle or another process.
  A Vault opened without an embedder gains no vector data from a rebuild.
- **A Vault opened by an older Scheda must not write during a rebuild.**
  The rebuild marker and the catch-up queue live in
  `{vault}/.scheda/reindex/`. A build without this release ignores both,
  so its `add` / `replace` / `remove` reach the live generation but never
  the one being built, and the published Index silently misses them.
  Upgrade every handle on a Vault together.

## [0.9.12] - 2026-09-06

### Changed

- **Concurrent `find` / `search` on one Vault.** Index queries on one
  handle no longer wait for each other. Writes still take an exclusive
  Index lock; the Vault write lock is unchanged (one writer, `locked` if
  a second writer races it). `get` was already concurrent.
- GitHub Actions CI and Docs workflows no longer run on push or pull
  request. They stay in the tree and can be started by hand
  (`workflow_dispatch`) or re-enabled by restoring those triggers.

## [0.9.11] - 2026-09-05

### Changed

- **Unchanged vector slots skip the embedder.** Each field (and body) slot
  stores a digest of the exact text that was handed to the embedder. `add`,
  `replace`, and `reindex` reuse the stored vector when that digest still
  matches and the embedder's `model_id` and dimensions are unchanged. A
  slot that leaves Schema or the Record is dropped from the vector Index
  so `search` cannot return it. A different model still re-embeds every
  slot. The lexical Index is still rebuilt in full on `reindex`. A rebuild
  that used to re-embed every Record now pays only for what changed. An
  existing Index still loads; the new on-disk digest field defaults to
  empty, so the first rebuild after upgrade re-embeds once, then later
  writes skip the model.

## [0.9.10] - 2026-09-05

### Changed

- Pin Buf `wellknowntypes` to `v36.1`, matching Bazel `protobuf` `36.1.bcr.1`.

## [0.9.9] - 2026-09-05

### Fixed

- **Isolated `find` / `search` docs match the crate.** Naming an isolated
  collection is an ordinary query (`14` collection commands, `13`, A6).
  `replace` / `remove` resolve `id` from the Index when `find` is on, for
  isolated collections too (`07` LIB-9 / LIB-24, `03` MOD-8).
- **Cargo CLI links `encrypt`, same as the Bazel binary.** A Vault that
  holds an encrypted collection fails with `crypto` without a secret, not
  `unsupported_encryption`, whether you built with Cargo or Bazel.
- **Git and HTTP errors no longer echo URL userinfo or path.** `Error.message`
  from libgit2 and from the CLI embedder/reranker keeps the host, not
  `user:token` or a query string.
- **Search `Hit` fallback collection name skips `.isolated/`.** A Record
  path `.isolated/{name}/{id}.md` resolves to `{name}`, not `.isolated`.

### Changed

- README and `10` / `11` / `12` name **P16** as next crate work (CLI secret
  flags and a nested isolated Remote). Collection encryption is not deferred.

## [0.9.8] - 2026-09-05

### Changed

- Update Bazel `protobuf` to `36.1.bcr.1`.

## [0.9.7] - 2026-09-05

### Changed

- Buffa 0.9.2 and protovalidate-buffa 0.7.1 (runtime crates and the
  `protoc-gen-buffa` / `protoc-gen-protovalidate-buffa` pins). Generated
  stubs apply the element-memory decode budget; JSON mapping rejects a
  nonempty `Any` without `@type` and serializes oneof message pointees.
  The plugin stack stays matched.
- Update `cxx` to `1.0.200`.
- huggingface-hub 1.30.0 for the vLLM sidecars.

## [0.9.6] - 2026-09-04

### Fixed

- **`SearchQuery.rerank_limit` above 50 now deepens the fusion pool.** Both
  Index legs retrieve `max(k, 50, N)` so the reranker can score a candidate
  at fused rank N. Unset, or a value at or below the floor, still uses
  `max(k, 50)`. Without a reranker the field is still ignored. A limit
  larger than the corpus scores every hit that exists.

## [0.9.5] - 2026-09-04

### Fixed

- **A second handle can write after `commit` without waiting on the
  lexical Index.** The Tantivy writer is opened for a transaction (or
  one autocommit) and dropped when that work ends, so its directory lock
  is free while the first handle is still alive. A consumer that commits
  in small batches can let another writer in between them. The vector
  Index is an in-memory dump and does not hold a matching lock.

## [0.9.4] - 2026-09-04

### Changed

- Update the Rust toolchain to 1.98.1. 1.98.0 could emit a null function
  pointer in a trait-object vtable.

### Fixed

- **Linux CI and Docs restore the Bazel cache without filling the runner.**
  The debian:13 jobs delete unused GitHub-hosted toolchains (Android,
  .NET, Haskell, and the rest) before extracting `bazel-debian13-cas-*`.
  That archive had grown large enough that restore died with no space
  left on the device.
- **Docs skips GitHub Pages deploy when Pages is not enabled.** rustdoc
  still builds. A private repository without Pages was failing the
  deploy step with 404 after rustdoc succeeded.

## [0.9.3] - 2026-09-04

### Added

- **A collection can stay out of either Index.** Schema
  `exclude_lexical_index` keeps Records out of the Tantivy Index (`find`,
  filters, BM25). `exclude_vector_index` skips the embedder on `add` and
  `replace` and keeps Records out of the vector Index. Either flag, or
  both: a collection that only wants word search can refuse inference
  without leaving Tantivy. `find` that names a lexically excluded
  collection is `unsupported`, not an empty hit list. `search` runs the
  legs the collection still has (`lexical_score` / `vector_score` say
  which) and is `unsupported` only when both are excluded. A `journal`
  may set either flag: keyed `get` uses the journal id map, not Tantivy.

## [0.9.2] - 2026-09-03

### Fixed

- **The Vault write lock is released when the work ends.** `commit` and
  `discard` unlock before they return, a mutating call outside a
  transaction unlocks when that call finishes, and dropping the handle
  unlocks before Index teardown waits on merge threads. A caller that
  wrote, finished, and immediately wrote again on the same path could
  get `locked` ("vault write lock is held") because the lock lived until
  the last handle dropped. A second writer still gets `locked` while
  the first holds it. The error stack names the operation that took the
  lock (`begin`, `commit`, `discard`, `push`, `pull`, `sync`,
  `reindex`, `create_collection`).

## [0.9.1] - 2026-09-03

### Changed

- Update `mimalloc` to `v3.5.1` with the `override` feature enabled.
- Update Bazel `rules_rust` to `0.74.0`.
- Update `usearch` to `2.26.2`.

## [0.9.0] - 2026-09-01

### Added

- **`SearchQuery.rerank_limit` bounds how many fused candidates the reranker
  scores.** Unset still scores the whole fused pool, so existing callers are
  unchanged. Zero and a value below `k` are `invalid`. Truncation happens
  before the library reads the Records, so it saves file reads as well as
  model calls. The CLI flag is `--rerank-limit`.

### Changed

- **Breaking: `Embedder` / `Reranker` require only the batch method.** Implement `embed_many` / `score_many`. `embed` / `score` have default bodies that call the batch method with one element and fail with `invalid` if the result is not exactly one value. An override of the single-item method must stay consistent with the batch path: the same text must yield the same vector whether it travelled alone or in a padded batch, or the vector space splits with no error. `many_limit` and `Embedder::dimensions` are `NonZeroUsize`. A zero dimension no longer silently turns the vector leg off; absence stays `None` for the embedder.

- **Breaking: generational Index.** `{vault}/.scheda/index/current` names the
  live generation; `format`, `lexical/`, `vector/`, edges, ord, and `journal/`
  live under `{vault}/.scheda/index/<generation>/`. `reindex` builds a new
  generation and swaps `current` with one atomic write, so a reader that
  opened before the rebuild keeps answering `find` and `search`. One previous
  generation is kept; a reader older than that may see `index_missing` or
  `index_lexical`. Format version is now `4`. An existing Vault with format
  `3` (files directly in `index/`) is `index_version` until you run `reindex`.

## [0.8.0] - 2026-09-01

### Added

- **`Embedder` / `Reranker` batch methods.** Traits now require
  `many_limit`, `embed_many`, and `score_many`. The library embeds
  several vector fields and scores several rerank candidates in chunks
  of `many_limit`. One text, one Record, or `many_limit` of 1 still
  uses `embed` / `score`. `many_limit` of 0 is `invalid`. CLI OpenAI
  embeddings send an `input` array (up to 2048); Cohere/Jina rerank
  still batches documents (up to 1000); Model2Vec uses `encode` for a
  batch.

## [0.7.1] - 2026-08-31

### Fixed

- **Windows writes encrypted Record files as LF-only age armor.** The
  `age` crate emits PEM with `\r\n` on Windows. Scheda's format is `\n`
  only, and decrypt refuses any `\r`, so a file Scheda had just encrypted
  would not open: `get`, `check`, and `documents()` failed with `crypto`
  ("file is not age armor"). CR is stripped on write; a file that still
  contains `\r` is still refused.

## [0.7.0] - 2026-08-31

### Added

- Isolated collections are searchable. Naming one in `find` or `search`
  is now an ordinary query, on the Vault form and on the collection
  form, and a set mixing isolated with plain names comes back as one page
  with one `total`. Before this, an isolated collection was in no index
  at all and naming it was refused, so the only ways to its Records were
  `get` by a known `id` and the `documents()` walk.
- A query with no collection named still means non-isolated, unchanged.
  Broad scans, and the agent tools that use them, surface nothing sealed
  unless a caller names the collection.

### Changed

- One Index per Vault. `{vault}/.scheda/` now covers every collection the
  build can read, and `{vault}/.isolated/.scheda/` is gone: nothing
  creates it, nothing reads it, and `reindex` deletes a leftover one. An
  older Vault needs no migration — an absent journal `id` map falls back
  to a segment scan, which is slower and never wrong.
- **`{vault}/.scheda/` is owner-only on Unix**: `0700` on directories,
  `0600` on files, applied when it is created and after every write,
  including files Tantivy creates that Scheda never opens. `check`
  reports a wider mode as a problem naming the path. This is the price of
  the change above and the reason it is safe: field values from isolated
  collections are now inside that directory.
- Consequence to weigh if you relied on the opposite: a backup that
  copies the Vault and deliberately skips `.isolated/` will carry those
  field values anyway, in the Index. Deleting `{vault}/.scheda/` stays
  safe and is what removes them.
- Encrypted collections are unaffected and remain in no Index, isolated
  or not. An owner-only mode protects a file from another account, not
  from the account that holds it, which is the whole reason an encrypted
  collection exists.

### Fixed

- **`more_like_this` with a source `id` now looks at the Record's text.**
  It never did. Tantivy builds the term set of that query from the source
  document's **stored** fields, and Scheda's full-text field is indexed
  and not stored, so the query was assembled from Scheda's own
  bookkeeping — `key`, `path`, `collection`. Every Record of the
  collection matched the `collection` term and tied on one score, so
  "similar to this" meant "in the same collection" and the ranking was
  whatever the tie broke to. Scheda now reads the source Record and
  builds the query from the same full-text string the Index holds for it,
  which is what the `fields` source form already did. An unknown source
  `id` is `not_found` instead of an empty page, so a caller can tell a
  missing source from nothing being similar. The `fields` form is
  unchanged.
- **Equal scores no longer return a different page each run.** `find`
  broke a score tie by Tantivy's `DocAddress`, which depends on the
  segment a document happens to land in; segment assignment varies
  between runs of the same inserts. Two identical queries on identical
  content could therefore return different orders, and `offset` / `count`
  could repeat or skip a Record between two calls. Ties now break by the
  Index ordinal ascending, the same counter journal order uses. Two
  limits, stated so they are not discovered later: the ordinal counts per
  collection, so two Records in different collections may share both
  score and ordinal; and it is assigned at index time, not derived from
  content, so `reindex` can reorder within a tie. A page is reproducible
  for a given Index, not a property of the data.

### Documentation

- The cost of the two secret forms is stated where callers will meet it.
  A passphrase runs the age scrypt derivation on **every** Record
  encrypted or decrypted — age tunes that work factor near a second on
  purpose — so a `documents()` walk under a passphrase costs about that
  per Record. An identity is X25519 with no derivation. A one-off command
  may carry a passphrase; a long-running process on a latency budget
  carries an identity. Scheda does not cache a derived key, because
  holding the unwrapped key for the life of the handle is a different
  security decision than the one a caller made by choosing a passphrase.

## [0.6.0] - 2026-08-31

### Added

- Encryption is a property of a collection. A Schema sets `encrypted:
  true` and every Record file of that collection is age v1 in ASCII
  armor on disk, whole-file. It is independent of `isolated`, so all four
  combinations are expressible: ciphertext in a history you fully intend
  to publish — which makes `push` the backup, with no directory to copy
  by hand — or a plain nested tree for something merely local. Offered
  for `document` and `singleton`; an encrypted `journal` is `invalid`,
  because an age file cannot be appended to and every appended line
  would rewrite a whole segment blob in git.
- The `encrypt` Cargo feature, beside `find` and `search` and off by
  default. age brings scrypt, ChaCha20-Poly1305, and X25519 through 24
  crates, and a consumer that encrypts nothing should not compile them.
  A build without the feature does not pretend: a secret at `open`,
  creating an encrypted collection, opening one that already exists on
  disk, and `check` over a Vault holding one each fail with
  `CODE_UNSUPPORTED_ENCRYPTION`, and nothing is created or written. The
  rest of the Vault keeps working, as it does when `find` is off.
- The secret arrives on the handle as `OpenOptions.encryption`, a
  passphrase or an age identity, and never reaches disk: not a Record,
  not the Schema, neither git tree, no Index. Files written under the
  existing temp-and-rename path, so a path holds a whole file or
  nothing. The armor is what the `age` CLI reads: `age -d` recovers
  exactly what `get` returns.
- `CODE_CRYPTO` and `Error::crypto`: the build can decrypt but the
  secret is absent or wrong, or the bytes will not decrypt. Distinct
  from `CODE_UNSUPPORTED_ENCRYPTION` on purpose — a caller that cannot
  tell "recompile" from "give me the passphrase" has to guess. No error
  message carries the secret or any plaintext.
- `CODE_UNSUPPORTED_ENCRYPTION` and `Error::unsupported_encryption`, a
  specific refusal rather than a catch-all `CODE_UNSUPPORTED`, so a
  caller can tell "this build cannot encrypt" from "this operation is
  not offered for this collection kind". Retrying cannot help, so
  `repeat` defaults to forbidden.

### Changed

- `OpenOptions.isolated_encryption` is renamed to `encryption`, because
  encryption is now a property a Schema declares for any one collection
  rather than a property of the isolated tree. Breaking, on purpose: the
  field keeps number 3, so binary protobuf still reads, but the Protobuf
  JSON key changes and a Rust caller gets a compile error. There is no
  alias and no `reserved` entry. A caller still sending the old JSON key
  is not warned, because nothing in this repository parses
  `OpenOptions` from JSON; whoever writes the first such parser must
  reject an unknown key rather than drop it, since a dropped secret key
  means a plaintext Vault and no error.
- No Index holds anything from an encrypted collection, main or nested.
  The Index is plaintext on disk, so indexing one would hand back
  exactly what the encryption was for. `find` and `search` that name an
  encrypted collection fail with `CODE_UNSUPPORTED` rather than
  returning an empty result, which a caller cannot tell from "nothing
  matched". Enumeration is `ids()`, which needs no secret, and
  `documents()`, which decrypts one file at a time.
- `history` on an encrypted collection returns decrypted plaintext in
  `Entry.raw` — the bytes a `get` of that revision would produce — not
  the armor git stores. An entry whose `raw` is empty, a commit that
  deleted the file, is returned as it is.
- `check` never reports an encrypted collection it could not read as
  healthy: with the secret it validates it like any other, without the
  secret it fails with `CODE_CRYPTO`, and in a build without `encrypt`
  with `CODE_UNSUPPORTED_ENCRYPTION`. Both mean "not verified" rather
  than "broken". `check` returns success or one failure, so it cannot
  report per collection; a caller wanting the plain collections verified
  while the secret is absent needs a Vault without an encrypted one.
- Setting or clearing `encrypted` on a collection that already holds
  Records is `invalid` and touches no file: it would mean rewriting
  every one, and the caller may not hold the secret needed to read them
  first. Encrypting an existing collection is an explicit migration.
- `{vault}/.isolated/.encrypted` is gone as a marker. The Schema records
  the choice; the marker file existed only because nothing else did.

### Fixed

- An encryption secret is no longer accepted and ignored. The option was
  declared and validated, and nothing in the library read it: a caller
  who passed a passphrase received plaintext Record files and no error.
  The `0.5.0` note below claimed that the first create of
  `{vault}/.isolated/` encrypts and that later isolated I/O uses the
  secret; that behaviour never existed. This release implements it, on
  the collection instead of the tree.

### Known limitations

- The CLI cannot open a Vault holding an encrypted collection. It builds
  its options from flags and has no flag that carries a secret, so there
  is no command-line way into such a Vault; the library is the only way
  in. The flags are specified — an environment variable name and an
  identity file path, never the secret itself in argv, which every
  process on the machine can read — and not yet built.
- The nested isolated tree still may not have a Remote of its own:
  `check` treats one as a defect. Encryption no longer requires that
  tree, so a consumer wanting ciphertext in a published history does not
  need this.

## [0.5.1] - 2026-08-31

### Added

- `documents()` and `ids()` on the collection handle, for `document` and
  `singleton`; `journal` is `unsupported`, where the ordered read stays
  `lines()`. Both walk the Record files the library named, in `id` order
  (the byte order of `{id}.md`), and neither opens the Index: the answer
  is the same in a build without `find` and is unaffected by a stale
  Index, which makes it the way to enumerate a collection when the Index
  is gone. Until now a caller without `find` could reach a `document`
  Record only by an `id` it already knew.
- `documents()` yields whole Records one at a time, holding no contents
  but the current one, so memory grows with the number of Records rather
  than their size; in Rust it is `CollectionDocuments`, an `Iterator` of
  `Result<Record, Error>`, so a search that stops early stops reading.
  `ids()` opens no Record file and answers in one list.
- A bad file does not end a `documents()` walk. A file that does not
  parse and a Record that parses but fails current Schema are both
  yielded with `valid: false`, exactly as `get` returns them, and the
  next Record still arrives. This is unlike `lines()`, which stops: a
  malformed journal line hides where the next line begins, while
  documents are separate files and stopping would hide every Record
  after the bad one. Names that are not Record files (a leading `.`, no
  `.md` suffix, not a regular file) and subdirectories are skipped
  without error. Reporting bad files remains `check`.

## [0.5.0] - 2026-08-30

### Added

- Optional commit message on `add`, `replace`, `remove`, and
  `create_collection`. Outside a transaction that text is the git
  subject; omitted, the subject is still `scheda`. A present but empty
  message is `invalid` before any file is written. Inside a transaction
  the argument is ignored and `commit` supplies the subject.
- `OpenOptions.author` (`api.v1.Author`): git author and committer for
  every commit on the handle, in the main tree and in
  `{vault}/.isolated/`. Required `name` and `email` when set. Unset:
  local git config, then `Scheda <scheda@invalid>`. CLI `--message` on
  mutating commands and `--author-name` / `--author-email` on `open`.
- `OpenOptions.remote_repo` must be an absolute URI when set. An empty
  string, a filesystem path, or SCP `git@host:path` is `invalid`. Unset
  still means local `git init`.
- `OpenOptions.isolated_encryption` (`api.v1.Encryption`): age secret
  for `{vault}/.isolated/`. Unset: isolated collections are not
  encrypted. When set, required `passphrase` or `identity`. First
  create of that tree encrypts; later isolated Record I/O uses it.
  Vault `open` and main collections do not require it.

### Changed

- Isolated encryption is no longer `OpenOptions.encrypt` plus
  `OpenOptions.secret`. Presence of `isolated_encryption` is the
  encrypt choice at first create of `{vault}/.isolated/`.

### Removed

- `OpenOptions.encrypt` and `OpenOptions.secret`.

## [0.4.4] - 2026-08-30

### Fixed

- `check` validates lexical Index freshness only in a build with `find`.
  A build without it marks the Index stale on every write, holds no
  lexical segments, and cannot rebuild them, so the old behaviour left
  `check` failing `index_lexical` from the first write onward with
  nothing in that build able to clear it. Such a build now passes
  `check` while the marker sits on disk, and a consumer that keeps its
  primary layer on the Cargo default feature set gets Schema, encoding,
  journal-line, `id`-uniqueness, and conflict-marker validation back.
  Nothing is served from an aged Index: writes still set the marker for
  the next build with `find`, whose `check` reports it and whose `find`
  fails closed on it until `reindex`.

## [0.4.3] - 2026-08-29

### Changed

- Bazel `aspect_rules_py` 1.12.1 and `rules_python` 2.3.2. The Windows
  uv-host stub patch stays: `host_platform_repo` still `fail()`s on Windows.

## [0.4.2] - 2026-08-29

### Changed

- protovalidate-buffa 0.7.0 (runtime crate and the `protoc-gen-protovalidate-buffa`
  pin). Generated `Validate` impls cover Buffa borrowed views; the plugin
  stack matches Buffa 0.9.1.
- vLLM sidecar 0.28.0 and huggingface-hub 1.29.0. Extra top-level pins that
  only froze uv's latest of those transitives are gone; the lockfile still
  pins the graph.

### Fixed

- markdownlint ignores `<workspace>/.cache/**` so Hub weights from the
  vLLM sidecars do not fail `//bazel:markdown`.

- USearch / cxx-build writes `rust/cxx.h` into the crate `OUT_DIR` first
  and treats the shared `scratch` cxxbridge dir as best-effort. Bazel
  linux-sandbox on WSL remounts that scratch path read-only (`EROFS`);
  native Linux, macOS, and Windows still compile from `OUT_DIR`.

- vLLM sidecars pin `HF_HOME` before importing Hugging Face Hub or vLLM
  (those libraries freeze the cache path at import) and pass the
  prefetched snapshot directory to `vllm serve`, keeping the Hub id as
  `--served-model-name`. EngineCore no longer downloads a second
  incomplete copy and fails on the tokenizer.

## [0.4.1] - 2026-08-28

### Added

- `object` / `object_list` Schema arm `values`: an open map whose keys
  the author invents and whose values have a declared type. Mutually
  exclusive with `fields`; omitting both is still an open map of
  `string` values. `required`, `default`, and `exclude_find` on the
  `values` entry are `invalid`, and so is an `edge` anywhere beneath
  `values`. `nullable` keeps its meaning. Dynamic keys are not `find`
  keys.

### Fixed

- Dropping or rebuilding the Index waits for Tantivy merge threads
  before the next `IndexWriter`. A later `begin` on the same tree
  retries `LockBusy` instead of failing `index_lexical`.

## [0.4.0] - 2026-08-28

### Added

- `Collection::remove(id)` on `document` and `singleton`. Unlinks the
  Record file, stages the deletion like `add` / `replace` (autocommit
  outside a transaction, batch inside one), and drops the Record from
  the Index. `journal` is `unsupported`. Unknown `id` is `not_found`;
  an illegal slug is `invalid` before any file is touched. `history(id)`
  still answers from git after the file is gone. CLI:
  `collection NAME remove --id ID`. A build without `find` still
  deletes the file and marks the Index stale.
- Crate features `find` (Tantivy) and `search` (hybrid BM25 + USearch).
  `search` turns `find` on. `simd` (was `numkong`) turns `search` on
  and enables USearch SIMD. Cargo `default` is `remote-git` only:
  `cargo add scheda` does not pull Tantivy or USearch. The CLI enables
  `find`, `search`, and `simd`. Bazel CI still links those legs.
  `tantivy`, `usearch`, and `cxx` are optional. A build without `find`
  still writes Records, marks the Index stale, and leaves `check` to
  report it; `reindex` under a full build rebuilds the lexical
  segments. `open` with an embedder without `search` fails
  `index_vector`. `replace` uses the Record file when `find` is off
  (same path isolated collections already used).
- Consumer readiness stages P0–P10 in `12` §4: what the first consumer
  needs before moving a primary layer of text files onto the crate
  (journal line fidelity, ordered journal read, batch commits, isolated
  slice, dead `id` knobs, optional search, acceptance). They do not
  reopen A0–A7; **agent** stays accepted.
- A fixture Vault in test data shaped like a first consumer's primary
  layer: two `document` collections with the same fields, three more
  documents (one a work queue), three journals, two singletons, one
  isolated journal written through the API. `check` and `get` run
  against it; `find` limited to one of the twin documents does not
  return the other. P10 acceptance runs on that fixture: delete the
  Index and `reindex` (counts match a file tally), Cargo default
  `get` / `history` / `lines`, OPS-1 volume, and `remove` then `check`.
- `Collection.lines` walks a `journal` in append order from segment
  files and does not open the Index. `find` still uses Tantivy. A
  missing `{vault}/.scheda/` does not stop the walk.
- Unencrypted isolated collections (ISO-2, ISO-4, ISO-6, IDX-4): Record
  files live under `{vault}/.isolated/` in a nested git with no Remote;
  Schema stubs stay in the main tree. `collection(name)` and
  `create_collection` with `isolated` read and write that tree. Files
  are owner-only (`0600` on Unix). `find` / `search` on the main Index
  skip isolated names (`unsupported` when named). `commit` / `discard`
  split recorded paths across the two repositories. Age encryption
  stays deferred.

### Changed

- CI OPS volume on every OS is 10³ journal and 10² document Records
  (the size Windows already used). Linux and macOS no longer load
  10⁴ / 10³. Each `add` fsyncs, so the full OPS-1 fixture is minutes
  on a real disk.
- README and `10`/`11`/`12` close consumer readiness P0–P10. P0–P5, P8,
  P9, and P10 are in, including the unencrypted isolated nested git
  (`10` §1), the slug as the only `id` rule, `find` / `search` as
  opt-in crate features (Cargo default is `remote-git` only), `remove`
  on `document` and `singleton`, and acceptance on the consumer fixture.
  A crate without those features is the same library with named legs
  off, not a second product (`10` §2). P7 is gone: how a consumer
  reshapes its own files to fit MOD-8, MOD-16, and STO-4 is its
  migration, and what those limits cost every consumer is `12` §6.
  Next crate work is age encryption of `{vault}/.isolated/` (`10` §5).
- STO-2: a `singleton` is YAML 1.2 in `record.yaml`. The old wording
  offered a Schema key that picks JSON or YAML; no such key ever
  existed, and JSON is a subset of YAML 1.2, so an imported JSON file
  parses unchanged under that name. On-disk bytes and behaviour are
  unchanged. The stage that held this question (P6) is gone.
- `12` §6 lists what a second consumer will want and Scheda does not
  have (journal line cannot leave, one writer, flat directories, Struct doubles,
  append-only journal, C++ toolchain unless `search` is off, embedder
  only in the CLI,
  slug-only `id`, JSON accepted for Schema but not for a Record,
  one-consumer acceptance). Those items are not stages.
- GIT-2: a transaction is the only supported batch. There is no
  `OpenOptions` switch that turns autocommit off. A consumer that
  appends many journal lines holds `begin`/`commit`; otherwise each
  line is one commit.
- SCH-1 grades `rust/scheda/src`, not test data. A fixture may name a
  consumer's collections without turning that check red.
- IDX-2 stays **core** for `find` / `search` / `replace`. Sequential
  journal read is `lines` and does not open the Index.
- Journal JSONL is RFC 8785 (STO-2, LIB-18): keys by UTF-16 code unit,
  numbers by ES6 `NumberToString`, strings escaped as the RFC states.
  `RecordAdd.fields` is `google.protobuf.Struct`; an `integer` whose
  magnitude exceeds 2^53 is `invalid`. There is no wider integer
  carrier.
- Schema files and `Collection::version` use `version` (same as
  `api.v1.Schema`). A leftover `schema_version` key is `invalid`.
- The uncommitted-path log is `{vault}/.scheda/tx.log` (next to
  `write.lock`), newline-separated vault-relative paths. It is not
  under `index/`, so `reindex` does not delete it.
- Transactions are in memory on a library handle. There is no
  `{index}/txn` file. `open` does not resume another process's
  transaction. CLI does not expose `begin` / `commit` / `discard`;
  each mutating CLI command autocommits with message `scheda`.
- LIB-5 documents the real `api.v1.Error.Code` values: `duplicate_id` /
  `conflict_markers` instead of coarse `conflict`; `io_*` / `git_*`
  families instead of coarse `io` / `git`; Index `index_missing` /
  `index_version` / `index_lexical` / `index_vector` / `index_corrupt`
  instead of coarse `index_unusable`. Same `id` on `add` is
  `duplicate_id`. Docs `03`/`04`/`06`/`07`/`08`/`09`/`10`/`11`/`12`/`13`/`14`
  match. ISO-2 owner-only mode is `0600` on Unix; other systems leave
  ACLs unchanged. Isolated `collection(name)` / CLI `collection NAME` is
  the Record handle; `unsupported` is only `find` / `search` that name an
  isolated collection (main Index), not get/add/replace/remove/history.

### Removed

- `Collection::id_field()` and the unused `id_pattern` / `id_field`
  slots. Record identity is always `id`; the library slug is the only
  `id` constraint (SCH-6). Schema has no key that renames `id` or adds
  a pattern.

### Fixed

- Windows CI `ops_volume_get_and_add_stay_in_milliseconds` times the
  sample journal `add` inside a transaction. Autocommit fsyncs git on
  every call; that is hundreds of milliseconds on NTFS and is not the
  OPS-1 band.
- Mixed isolated+main `commit`: if the main-tree commit fails after the
  nested git commit succeeded, nested `HEAD` is reset to the commit it
  had before that nested commit (working tree included).
- `open` with `auto_sync` on takes `write.lock` for the auto-pull, then
  releases it. A writer already holding the lock is `locked` (STO-4).
- Mutex poison on Vault/Collection state fails closed (`Result`) instead
  of continuing with `into_inner`. `holds_write_lock` treats poison as
  held. `SearchEngine` drop skips commit if the inner mutex is poisoned.
- On Windows, `add` of a Record whose absolute path is longer than MAX_PATH
  no longer fails with git `path too long`. `init`, `clone`, `commit`, and
  `discard` set `core.longpaths` in the Vault's local git config so libgit2
  will stage a 255-byte filename component.
- An `integer` field on a journal line is stored as `1`, not `1.0`.
- Creating a Vault writes `.gitignore` before `open` takes `write.lock`,
  so a first write right after create is not `CODE_LOCKED`. Repairing
  `.gitignore` on an existing Vault still uses the lock, then
  `File::unlock` before `open` returns.

## [0.3.0] - 2026-08-27

### Added

- ISO-3 on the agent surface: `Vault.info`, Vault `find` / `search`, and
  CLI `collection NAME` for a non-isolated collection omit isolated
  Schema field names and do not return a fixture sealed Record. Isolated
  `collection(name)` stays `unsupported` until nested `{vault}/.isolated/`
  I/O (`10` §1). The main Index does not scan isolated collections.
- **agent** accepted for the Rust binding (`10` §4): `Vault.info` matches
  Schema files, tool writes cannot bypass SCH-12, and library vs CLI JSON
  (and `code`) match on one Vault.

### Changed

- Library API names (`07` LIB-2): public operations are `open`, `info`,
  and the rest of A3. There is no `init`, `clone`, or `catalog` method.
  Creating or cloning a Vault is `open` (GIT-9: existing path ignores
  `remote_repo`). AGT-1 listing is `info`.
- `create_collection` with `schema.isolated` is `unsupported` and writes
  no Schema file until nested `{vault}/.isolated/` I/O exists.
- README opens with what Scheda is, why it exists, and how a program or
  agent uses it without treating the Index as the database.
- Windows CI `ops_test` loads 10³ journal and 10² document Records (one
  tenth of the Unix OPS-1 fixture) so the volume checks finish under
  Bazel `large` on NTFS. All platforms bulk-insert that fixture in one
  transaction. Linux and macOS still load 10⁴ / 10³.

### Fixed

- HTTP embedder/reranker errors report status and host only: they do not
  echo the response body or URL userinfo. Git credential failures do not
  put the remote URL in the message.
- Dropping a Vault waits for Tantivy merge threads so a later `open` on
  the same tree is not `index_lexical` (`LockBusy`).

### Removed

- `Collection::layout()`. Schema has no layout template (SCH-2, MOD-8);
  Record paths stay the kind-fixed names. A leftover `layout` key in a
  Schema file is `invalid`.

## [0.2.0] - 2026-08-27

### Added

- `FindResult.total`: indexed hits matching the Filter (Tantivy `Count`
  in the same `find` pass as the page). Not `records` length: a missing
  file is omitted from the page and does not decrease `total`. Always
  filled; there is no `include_total` flag.
- README: step-by-step `python/cli` (separate LLMs and Model2Vec
  walkthroughs: vLLM sidecars or local Model2Vec, `import` /
  `reindex`, `find` and `search` examples). Model2Vec section opens
  with a disclaimer: static embeddings miss QRY-3 paraphrases; the
  commands exist to contrast quality with Qwen.
- `find` compiles every `api.v1.Filter` kind (`phrase`, `phrase_prefix`,
  `fuzzy`, `regex`, `regex_phrase`, `range`, `more_like_this`). Token
  queries run on per-field positions in the lexical Index. Markdown
  `body` is a find key on `document` unless `exclude_fts`. Format
  version is `3`.
- Optional reranker callback on `search` (`score(query, record)`). Hits
  come back already ordered; `SearchHits.Hit.rerank_score` when set.
  No ranking model inside the crate. Pool is at least `max(k, 50)`.
- `Record.collection` and `SearchHits.Hit.collection` (output only) so
  Vault-wide `find` / `search` can name the collection without parsing
  `path`. First field on those messages (`collection = 1`).
- `--model2vec-embedder DIR`: local Model2Vec files (`config.json`,
  `tokenizer.json`, `model.safetensors`). CLI loads them with
  `model2vec-rs` (`local-only`; no Hub download). Extra files (README)
  are allowed; hidden names are ignored. Missing a required file →
  `invalid`. Not with the HTTP embedder flags.
- `docs/14-cli.md`: target CLI argv (LIB-2 names; drop `scan` / `walk`;
  `--model2vec-embedder` for the local vector leg; HTTP embedder /
  Cohere reranker on `search`).
- Locked vLLM sidecars: `bazel run //python/vllm:embedder` (OpenAI
  `/v1/embeddings` on `:8080`) and `//python/vllm:reranker` (Cohere
  `/v1/rerank` on `:8081`). `vllm==0.27.1` in `uv.lock`; Linux, host
  NVIDIA driver; `tags = ["manual"]` so `bazel build //...` does not
  fetch torch. Hub weights go to `<workspace>/.cache/huggingface`
  (`HF_HOME`; an already-set `HF_HOME` wins). Opt-in Jina pair
  (`//python/vllm:embedder_jina` / `:reranker_jina`, same ports;
  `jinaai/jina-embeddings-v5-text-small` + `jinaai/jina-reranker-v3.5`;
  CC BY-NC 4.0). `cli_jina` uses Vault `.vault-test-jina`.
- `python/cli`: `bazel run //python/cli:cli_LLMs` (Vault
  `.vault-test-LLMs`, Qwen HTTP sidecars), `//python/cli:cli_jina`
  (Vault `.vault-test-jina`, Jina HTTP, CC BY-NC), and
  `//python/cli:cli_model2vec` (Vault `.vault-test-model2vec`, local
  Model2Vec). `import|reindex|find|search` (or `python -m python.cli
  --backend llms|jina|model2vec …`). `import` creates that Vault if needed
  and `get` / `add`s missing `python/cli/testdata/animals` documents.
  `cli_model2vec` downloads `minishlab/potion-multilingual-128M` into
  `.cache/model2vec/potion-multilingual-128M` on `reindex` when files are
  missing (`huggingface_hub` / `hf-xet`; `HF_TOKEN` when set; size and
  Hub hash checked). Bare `search`
  runs fixture `topics.yaml` and checks QRY-3 `expect_ids`.
  `--lexical-only` turns embedder/reranker flags off. `cli_model2vec`
  loads Model2Vec on `reindex` and `search`. `import` / `find` omit
  `--model2vec-embedder`. `cli_LLMs` `add` still passes the HTTP
  embedder.
- `docs/12-agent-plan.md`: A3 wire `07` / `13`; A4 CLI `14` is done; A2 is
  legacy and must not be extended. A5 is HTTP embedder/reranker
  (`--embedder-openai-url` / `--reranker-cohere-url`). Isolation is A6;
  acceptance is A7.
- Thin CLI `scheda` (`rust/cli`, clap 4.6.6): LIB-2 + `catalog` as
  subcommands, Protobuf JSON on stdout, `api.v1.Error` JSON on stderr
  (AGT-2–5). Uncommitted write paths persist at `{index}/written` and the
  in-txn flag at `{index}/txn` so a later process can `commit` them (GIT-2).
- Cargo-only static glibc Linux CLI (`+crt-static`, `static-pie`):
  `--features vendored-openssl` and `.cargo/static-glibc.toml`. Bazel
  stays on the dynamic CRT.
- `api/v1` `Schema` (and nested collection/field/edge/body messages) for
  AGT-1. `Vault::catalog` serializes Schema files as that message (A1);
  isolated collections are listed without `fields` (ISO-3). `07` LIB-20.
  `Vault::create_collection` / CLI `create-collection` write one
  `Collection` (LIB-21) so agents do not hand-write Schema YAML.

### Fixed

- Failed `add` / `replace` no longer leave an unindexed file that the next
  autocommit would publish. The document is discarded; a journal append is
  truncated (or the new segment removed).
- `pull` / `sync` reload Schema, journal id maps, and rebuild the Index after
  a successful pull.
- Journal `find` order survives restart: `next_ord` is persisted under the
  Index. ANN keys are monotonic (no FNV-1a collisions).
- HTTP embedder `POST` body is `{ model, input }` only (`14`). Sending
  OpenAI `dimensions` made vLLM/Qwen return HTTP 400.
- Multi-collection `find` compiles the Filter per collection. Journal scan
  skips a bad JSONL line instead of dropping later records. Windows replace
  uses `MoveFileExW`. `open` writes `.gitignore` under the write lock, then
  releases it. `fast_forward` refuses a dirty worktree. `history` loads blobs
  only for the requested page.
- `python/cli` Model2Vec Hub fetch uses `huggingface_hub` (`hf-xet`)
  instead of one urllib GET through the Xet HTTP bridge, which truncated
  `model.safetensors`.
- vLLM sidecars `snapshot_download` the Qwen repo (tqdm on stderr)
  before `vllm serve`, so EngineCore is not stuck on a silent Hub fetch.
- `cli_model2vec` loads Model2Vec on `reindex` (Hub ensure) and
  `search` (local cache, no Hub). `import` / `find` do not download or
  run the model. `import` no longer trailing-`reindex`es.
  `cli_LLMs` `add` still calls the HTTP embedder.

### Removed

- `--onnx-embedder` and `--reranker-onnx`. The vector leg is HTTP
  embedder flags or `--model2vec-embedder`. Rerank on `search` is HTTP
  Cohere/Jina only (`--reranker-cohere-url` / `--reranker-model`).
- Legacy CLI names: `find-one`, `insert-one`, `replace-one`, `scan`,
  `catalog`, `init` / `clone` as commands, `push-remote` / `pull-remote` /
  `sync-remote`, `walk`, `--text` on `search`. The binary is `14`.
- `walk` / `walk_inverse` from the library API (`07` LIB-2, `13`). Edge
  fields stay in Schema; the caller follows ids with `get`.

### Changed

- Embed of the search query (and per-field vectors on write/rebuild) runs
  outside the search mutex. Poisoned locks fail `CODE_IO_OTHER` instead of
  continuing. Index format mismatch on `upsert` / `has_id` is
  `CODE_INDEX_VERSION`, not a silent no-op. Journal ids map appends are
  fsync'd. CLI rerank batches `score_many` and reuses one HTTP agent.
- `cli_model2vec` Hub default is `minishlab/potion-multilingual-128M`
  (multilingual, ~512 MiB), not `potion-retrieval-32M` or
  `potion-base-8M`. The driver passes `--model2vec-embedder` on
  `reindex` and `search`.
- `find` takes `api.v1.FindQuery` (`filter` required, `include_raw`)
  instead of a bare `Filter` (LIB-10). Unset or false `include_raw`
  omits `Record.raw`; `get` still always returns `raw`.
- `SearchQuery.include_raw` (default false) fills `SearchHits.Hit.raw`.
  CLI `--include-raw` on `find` / `search`.
- `python/cli` `find` / `search` / `reindex` stdout is YAML. Multiple
  records, hits, or topic reports are YAML documents separated by `---`.
  `FindResult.total` is a first document when the page is non-empty.
  `Record.raw` / `Hit.raw` only with `--include-raw`.
- Python `py_*` (`python_pkg`, `python/cli`) skip Windows
  (`target_compatible_with` `NOT_WINDOWS`), same as ruff / pip-audit:
  `bazel build //...` must not fetch aspect_rules_py's uv host
  (`Unsupported platform windows`).
- Write lock is `{vault}/.scheda/write.lock` (STO-4), not under `index/`.
  A leftover `{vault}/.scheda/index/write.lock` is drained on acquire.
- CLI argv is `14`: `--vault`, `open` / `info` / `create-collection`,
  `collection NAME <subcommand>`, Vault `find` / `search` with
  `--collections`. JSON is the Protobuf JSON mapping of `api/v1`.
  `--model2vec-embedder DIR` requires `config.json`, `tokenizer.json`,
  and `model.safetensors` (missing → `invalid`; extra files allowed;
  hidden names ignored; unset → no vector leg unless HTTP embedder flags).
  `--embedder-openai-url` / `--embedder-model` / `--embedder-dimensions`
  (all three or none) and `--reranker-cohere-url` / `--reranker-model`
  (both or none) call OpenAI embeddings and Cohere/Jina rerank over HTTP
  (`14`). `--embedder-api-key-env-var` / `--reranker-api-key-env-var`
  send `Authorization: Bearer` from that env var; unset → no auth.
  Local Qwen sidecars: `bazel run //python/vllm:embedder` and
  `//python/vllm:reranker` (`vllm==0.27.1` in `uv.lock`; not Echo-style
  sandbox hermeticity).
- `rust/scheda/src/api.rs` is `embed.rs` (`Embedder`, `Reranker`). Public
  trait names unchanged. Docs that called the crate surface `api.rs`
  now point at `07`.
- Schema has no `layout`. Record file paths are fixed by kind (MOD-8):
  `{name}/{id}.md`, `{name}/*.jsonl`, `{name}/record.yaml`. Isolated
  Records use `.isolated/{name}/` with the same filenames. The only
  filesystem path the caller supplies is the Vault root (`open`).
  `find` has no `path_prefix`.
- `get` and `history` validate `id` then open or git that file. `add`
  validates, writes, and updates the Index. `find`, `search`,
  and `replace` take `id`s from the Index, then open those files
  (IDX-10). `find` may filter on identity `id`. Schema must not declare a
  field named `id`.
- `api/v1/query.proto` is gone. `find` takes `api.v1.Filter` (`find.proto`):
  a Tantivy 0.26 `Query` tree (`boolean` must / should / must_not, `term`,
  `phrase`, `range`, …), not a Scheda-only AND list. Paging is
  `offset` / `count` on the method (Tantivy collector, not `Filter`):
  `order_by_score()` for `document` / `singleton`; append order for
  `journal` (QRY-7). Returns `api.v1.FindResult` (same file). Every
  indexed Record is `Filter` `all`. There is no `scan` method.
- `search` takes `api.v1.SearchQuery` (`search.proto`): `text`, `k`,
  optional `Filter` (Tantivy first; both legs on that `id` set),
  `lexical_weight` / `vector_weight`, plus an optional library
  `Reranker` (not proto). Returns `api.v1.SearchHits`.
  `Hit.collection` is output only, same as `Record.collection`.

- Schema `find` / search marks are opt-out: `exclude_find`, `exclude_fts`,
  `exclude_vector` (default include). Body is indexed unless excluded.
  Schema has no `field_id`; the vector column is the field `name` (body is
  `body`). Model identity stays in the Index (SCH-10). `Field` spec is a
  `oneof` per type (`string`, `integer`, …) or `edge`. Nested `fields` live
  only on `object` / `object_list`. Edge arity is `one` xor `many`.
- Scheda uses at most one git Remote, named `origin`. `api.v1.VaultInfo`
  has `string remote` (`origin` or empty); `has_remote` and repeated
  remote names are gone (GIT-7). `push` / `pull` / `sync` take no
  remote-name argument (LIB-14). `Vault.info`
  returns `VaultInfo` with `repeated CollectionInfo collections`. Each
  item is `name` plus nested `api.v1.Schema`.
- Isolated collections live under `{vault}/.isolated/` (gitignored nested
  git, ISO-2). Schema stays `{name}/.schema.yaml` in the main tree.
  `Vault.collection` and `reindex` use the same Vault handle. `push`
  does not publish `.isolated/`.
- `add` replaces `insert_one` / `put` (create a Record; same `id` → `conflict`; success is `()`).
- `get` replaces `find_one`.
- `replace` replaces `replace_one` (whole Record; no field patch).
- `api.v1.Record` has no `collection` (the Collection handle is the
  collection). Write is `RecordAdd` (`add`) / `RecordReplace`
  (`replace`); `Record` is the `get` / `find` / `search`
  result (`path` / `valid` / `raw` only there).
- `push` / `pull` / `sync` replace `push_remote` / `pull_remote` /
  `sync_remote`.
- `ReindexResult` lives in `api/v1/index.proto` (`api.v1.ReindexResult`;
  was `ReindexCounts`).
- `Collection.history(id, offset, count)` returns `api.v1.History` with
  `repeated Entry` (paged; `count` 0 is `invalid`). The caller does not
  pass a file path. `HistoryEntry` and `Vault.history` are gone.
- `api/v1` Schema messages match product nouns: `Catalog` is `Schema`,
  `CatalogCollection` is `Collection`, `CatalogField` / `CatalogEdge` /
  `CatalogBody` / `CatalogId` are `Field` / `Edge` / `Body` / `Id`. The
  file is `schema.proto`. The `catalog` operation is unchanged. The Rust
  collection handle is `CollectionHandle` so it does not clash with
  `api/v1` `Collection`.
- On-disk Schema: collection declarations live at
  `{name}/.schema.yaml` (also `.schema.yml` / `.schema.json`). The
  collection directory **is** the collection name (same slug as Record
  `id`; no FNV hash). The central `schema/` directory and root
  `schema.yaml` / `schema_dir` relocation are gone (SCH-1).
  `Vault::create_collection` / CLI `create-collection` replace `put_schema`
  / `put-schema` (LIB-21).
- Record `id` and collection name slugs are checked in the library, not
  protovalidate (`id.proto` is gone). Unicode letter or digit, then
  letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`;
  max 255 bytes; no trailing `.` or whitespace; not a Windows device
  name (`CON`, …). `add(id, record)`: `RecordAdd` is `fields` / `body`.
  Atomic writes use a short `.scheda.{n}.tmp` sibling so a 255-byte
  filename still fits `NAME_MAX`.
- GIT-2: outside a transaction each mutating call autocommits with
  message `scheda`. `begin` / `commit` / `discard` batch writes. `commit`
  without `begin` is `invalid`. Uncommitted write paths and the in-txn
  flag persist under `{index}/written` and `{index}/txn`.
- `api.v1.Error` is a nested `Code` enum. Protobuf JSON names are
  `CODE_NOT_FOUND`, `CODE_IO_NOT_FOUND`, and so on (not the 0.1.0 strings
  `not_found` / `io` / `git` / `index_unusable` / `conflict`). The message
  also carries optional vault-relative `path`, `repeat`, and causal `stack`
  (outermost first). Callers branch on `code`. Coarse `io` / `git` /
  `index_unusable` / `conflict` split into one code per stable
  `std::io::ErrorKind`, one per `git2::ErrorCode`, plus
  `CODE_CONFLICT_MARKERS` / `CODE_DUPLICATE_ID` and Index codes. Rust
  `scheda::Error` is that generated message (no anyhow).
- Cargo `--release` uses `opt-level = 3`, thin LTO, one codegen unit,
  and symbol strip. Bazel rustc flags are unchanged.

## [0.1.0] - 2026-08-24

### Added

- Core development plan in `docs/11-core-plan.md` (stages C0–C10).
- Agent development plan in `docs/12-agent-plan.md` (stages A0–A4).
- LIB-5 error codes on the Rust crate; LIB-2 names including `init` /
  `clone` / `scan` / remote / `check` / `reindex`; `search` returns
  `unsupported`.
- `Vault::init` / `Vault::open` with exclusive write lock (`locked`) and
  local Remote detection without fetch. Index lives at `{vault}/.scheda/`
  (gitignored).
- Language-neutral library messages in `api/v1` (Protobuf edition 2024);
  Rust stubs via hermetic `buf generate` (buffa + protovalidate).
- `init` / `open` / `clone` / `insert_one` / `replace_one` reject
  `api/v1` input messages that fail generated protovalidate rules
  (`invalid`).
- `open` / `check` load Schema from `schema/` (`05` §8 keys); illegal
  declarations are `invalid`.
- Rust crate rustdoc on GitHub Pages (`bazel build //rust/scheda:scheda_doc`);
  crate docs list Cargo features (`remote-git`, `numkong`, `openmp`) and how
  to call the sync API from Tokio (`spawn_blocking`).
- `document` / `singleton` `insert_one` / `replace_one` / `find_one` (C3):
  YAML+Markdown or YAML-only files, atomic sibling-temp writes, Schema
  validation on write, `valid: false` on old files that fail the current
  Schema. `replace_one` on `journal` is `unsupported`.
- `journal` `insert_one` appends RFC 8785 JSONL (C4); size cap rotates
  eight-digit hex segments (`00000001.jsonl`) without rewriting closed
  files; `find_one` / empty `find` / `scan` read by `(collection, id)` in
  append order.
- SCH-8 `find` filters (`equal` / `set` / `exists` / `path_prefix`),
  `scan` as empty `find`, `check` with code+path, and `reindex` that
  writes Index format version `1` and returns `ReindexCounts` (C5).
- `commit(message)` stages only paths this handle wrote and creates one
  git commit; `history(path)` returns `HistoryEntry` snapshots that match
  `git show` for `document` / `singleton` / journal segment files (C6).
- `clone` / `remote_repo`, `auto_sync` fast-forward only, and
  `pull_remote` / `push_remote` / `sync_remote` (C7). Diverged histories
  are `not_fast_forward`; a failed push does not roll back the local commit.
- Journal `id` map under `{vault}/.scheda/index/journal/` so `find_one` does
  not scan unrelated segments; writer updates it on `insert_one`, `reindex`
  rebuilds it (C8, IDX-5, OPS-1, OPS-3).
- Hybrid `search` (C9): Tantivy BM25 over Schema `fts` fields and body,
  in-process USearch ANN over vector-marked slots, RRF fusion, SCH-8
  `filter` before ranking, one-step `walk` / `walk_inverse`. Inject an
  `Embedder` with `Vault::with_embedder`; without one the vector leg is off
  and writes still work. `api/v1` `SearchQuery` / `SearchHits`.
- CI runs `bazel test //...` on Debian 13, macOS, and Windows amd64.
- USearch 2.26.0 as the in-process HNSW engine (`AnnIndex`). Crate
  feature `numkong` (default) enables SIMD kernels (`numkong` 7.8.0);
  `openmp` is opt-in and needs libomp. Tantivy 0.26.1 is the BM25
  lexical engine for `search`. The C++ bridge uses `/usr/bin/c++` on Linux. macOS
  keeps Bazel's Apple clang wrapper so `__BAZEL_XCODE_SDKROOT__` expands;
  Windows uses MSVC. NumKong headers are copied into `OUT_DIR` so Bazel
  sandboxes can compile the bridge.
- Crate feature `remote-git` (default) enables `git2` `https` and `ssh` so
  `clone` / `pull_remote` / `push_remote` can talk to `https://` and `git@`
  remotes (libgit2 credential helper / ssh-agent). Without the feature those
  URL schemes are `unsupported`; file remotes still work. TLS is platform
  OpenSSL / Secure Transport / WinHTTP; Linux `+crt-static` is dropped so
  the native crates link.

### Changed

- Collection files live under a 16-hex FNV-1a-64 directory of the collection
  name (any characters in `name` are allowed). Layout templates are relative
  to that directory.
- Index is stored inside the Vault at `.scheda/` and listed in `.gitignore`.
  `OpenOptions` has no Index path field.
- Target crates use the platform CRT on Linux, macOS, and Windows (no
  `+crt-static`). Proc-macros and build scripts stay dynamically linked.
- crate_universe annotations keep only USearch/cxx patches and a Linux
  `CXX=/usr/bin/c++` pin. macOS must not override `CXX` (Apple sysroot
  placeholders). libgit2/OpenSSL crates already see host PATH and vendored
  sources from rules_rust defaults. On Windows the cxx-build patch writes
  `rust/cxx.h` as a regular file so MSVC can compile USearch. Directory
  symlinks cxx-build leaves in USearch `OUT_DIR` are removed with
  `remove_dir` (Windows `remove_file` cannot unlink them). Windows CI uses
  `--output_user_root=C:/b` and a short crate_universe hub `pvb` so
  `CARGO_MANIFEST_DIR` stays under MAX_PATH.
- Pinned `bazel_utils_*` modules at `v0.2.3` (`buf_generate` installs local
  plugins as `name.exe` so Windows `buf.exe` LookPath can find them; Scheda
  CI still runs Windows amd64 only).
- Git operations go through `git2` (libgit2), not the `git` CLI.
- Generated Rust stubs live in `rust/generated` (crate `scheda-generated`):
  one buffa file per `.proto`, packaging `mod.rs`, and protovalidate
  `Validate` impls.
- The `scheda` crate uses generated `api/v1` messages (`Error`,
  `OpenOptions`, `Record`) instead of parallel hand-written structs.
- Schema tests load YAML/JSON fixtures from `rust/scheda/testdata/schema/`
  instead of inlined strings.
- `atomic_write` replaces the destination on Windows; native `rename`
  cannot overwrite an existing file.
- `Collection::find_one` returns `Result<Record, Error>`; a missing `id` is
  `not_found` (LIB-8).
- `SearchQuery` weights and `SearchHit` scores, when set, must be finite
  and `>= 0` (`api/v1` protovalidate, including `+Inf` / `-Inf` via field
  CEL until codegen emits `double.finite`). `search` rejects a failing
  message as `invalid` before retrieval; it does not re-check those numbers.
- `Record.id` / `SearchHit.id` reject `/` and `\\` in protovalidate
  (layout path component). `HistoryEntry.time` is required. `find` walks
  the validated `Filter` oneof; it does not re-check clause shape.
- Rust **core** is accepted (C10): the five gates in `10` §2 pass in
  `scheda` tests, including deleting `.scheda/` without losing Records.
- `docs/10-roadmap.md` (v0.12): next `scheda` work is the unencrypted
  isolated slice; **agent** (catalog, CLI, ISO-3) follows (`12`); age
  encryption of that root stays deferred. `01`, `11`, and the README
  status match.
- Call the JSON encoding of `api/v1` the Protobuf JSON mapping (edition
  2024), not "proto3 JSON". The `.proto` files were already edition 2024.

### Removed

- Unused workspace pins `thiserror` and `tempfile`.

### Fixed

- Windows CI: GIT-6 inspects only the Vault's local git config, so a
  Git-for-Windows global `filter.lfs` does not fail `init`. Fixture
  checkout stays LF (`.gitattributes`). `scheda_doc_test` is `large` so
  rustdoc on MSVC is not a 60s timeout. bazel_utils `.bash` lint/audit
  tests are skipped on Windows (`CreateProcess` error 193); Linux and
  macOS still run them.
- `search` returns `index_unusable` when a lexical Index is on disk but
  unreadable, instead of an empty hit list. `find_one` still reads files.

## [0.0.1] - 2026-08-23

### Added

- Language-neutral design package in `docs/` (`01`–`10`): Vault model, git
  rules, Schema, Index, library API, agent surface, isolation (later), and
  phases.
- Rust crate skeleton `scheda` (`todo!` bodies; workspace lints deny warnings).
- Hermetic Bazel on the host architecture: library build and tests, Clippy,
  cargo-audit, buildifier, markdownlint-cli2, and GitHub Actions CI.

## Links

- [Unreleased]
- [0.23.0]
- [0.22.1]
- [0.22.0]
- [0.21.0]
- [0.20.1]
- [0.20.0]
- [0.19.0]
- [0.18.1]
- [0.18.0]
- [0.17.1]
- [0.17.0]
- [0.16.3]
- [0.16.2]
- [0.16.1]
- [0.16.0]
- [0.15.2]
- [0.15.1]
- [0.15.0]
- [0.14.0]
- [0.13.0]
- [0.12.0]
- [0.11.3]
- [0.11.2]
- [0.11.1]
- [0.11.0]
- [0.10.3]
- [0.10.2]
- [0.10.1]
- [0.10.0]
- [0.9.12]
- [0.9.11]
- [0.9.10]
- [0.9.9]
- [0.9.8]
- [0.9.7]
- [0.9.6]
- [0.9.5]
- [0.9.4]
- [0.9.3]
- [0.9.2]
- [0.9.1]
- [0.9.0]
- [0.8.0]
- [0.7.1]
- [0.7.0]
- [0.6.0]
- [0.5.1]
- [0.5.0]
- [0.4.4]
- [0.4.3]
- [0.4.2]
- [0.4.1]
- [0.4.0]
- [0.3.0]
- [0.2.0]
- [0.1.0]
- [0.0.1]

[Unreleased]: https://github.com/sonalect/scheda/compare/v0.23.0...HEAD
[0.23.0]: https://github.com/sonalect/scheda/releases/tag/v0.23.0
[0.22.1]: https://github.com/sonalect/scheda/releases/tag/v0.22.1
[0.22.0]: https://github.com/sonalect/scheda/releases/tag/v0.22.0
[0.21.0]: https://github.com/sonalect/scheda/releases/tag/v0.21.0
[0.20.1]: https://github.com/sonalect/scheda/releases/tag/v0.20.1
[0.20.0]: https://github.com/sonalect/scheda/releases/tag/v0.20.0
[0.19.0]: https://github.com/sonalect/scheda/releases/tag/v0.19.0
[0.18.6]: https://github.com/sonalect/scheda/releases/tag/v0.18.6
[0.18.5]: https://github.com/sonalect/scheda/releases/tag/v0.18.5
[0.18.4]: https://github.com/sonalect/scheda/releases/tag/v0.18.4
[0.18.3]: https://github.com/sonalect/scheda/releases/tag/v0.18.3
[0.18.2]: https://github.com/sonalect/scheda/releases/tag/v0.18.2
[0.18.1]: https://github.com/sonalect/scheda/releases/tag/v0.18.1
[0.18.0]: https://github.com/sonalect/scheda/releases/tag/v0.18.0
[0.17.1]: https://github.com/sonalect/scheda/releases/tag/v0.17.1
[0.17.0]: https://github.com/sonalect/scheda/releases/tag/v0.17.0
[0.16.3]: https://github.com/sonalect/scheda/releases/tag/v0.16.3
[0.16.2]: https://github.com/sonalect/scheda/releases/tag/v0.16.2
[0.16.1]: https://github.com/sonalect/scheda/releases/tag/v0.16.1
[0.16.0]: https://github.com/sonalect/scheda/releases/tag/v0.16.0
[0.15.2]: https://github.com/sonalect/scheda/releases/tag/v0.15.2
[0.15.1]: https://github.com/sonalect/scheda/releases/tag/v0.15.1
[0.15.0]: https://github.com/sonalect/scheda/releases/tag/v0.15.0
[0.14.0]: https://github.com/sonalect/scheda/releases/tag/v0.14.0
[0.13.0]: https://github.com/sonalect/scheda/releases/tag/v0.13.0
[0.12.0]: https://github.com/sonalect/scheda/releases/tag/v0.12.0
[0.11.3]: https://github.com/sonalect/scheda/releases/tag/v0.11.3
[0.11.2]: https://github.com/sonalect/scheda/releases/tag/v0.11.2
[0.11.1]: https://github.com/sonalect/scheda/releases/tag/v0.11.1
[0.11.0]: https://github.com/sonalect/scheda/releases/tag/v0.11.0
[0.10.3]: https://github.com/sonalect/scheda/releases/tag/v0.10.3
[0.10.2]: https://github.com/sonalect/scheda/releases/tag/v0.10.2
[0.10.1]: https://github.com/sonalect/scheda/releases/tag/v0.10.1
[0.10.0]: https://github.com/sonalect/scheda/releases/tag/v0.10.0
[0.9.12]: https://github.com/sonalect/scheda/releases/tag/v0.9.12
[0.9.11]: https://github.com/sonalect/scheda/releases/tag/v0.9.11
[0.9.10]: https://github.com/sonalect/scheda/releases/tag/v0.9.10
[0.9.9]: https://github.com/sonalect/scheda/releases/tag/v0.9.9
[0.9.8]: https://github.com/sonalect/scheda/releases/tag/v0.9.8
[0.9.7]: https://github.com/sonalect/scheda/releases/tag/v0.9.7
[0.9.6]: https://github.com/sonalect/scheda/releases/tag/v0.9.6
[0.9.5]: https://github.com/sonalect/scheda/releases/tag/v0.9.5
[0.9.4]: https://github.com/sonalect/scheda/releases/tag/v0.9.4
[0.9.3]: https://github.com/sonalect/scheda/releases/tag/v0.9.3
[0.9.2]: https://github.com/sonalect/scheda/releases/tag/v0.9.2
[0.9.1]: https://github.com/sonalect/scheda/releases/tag/v0.9.1
[0.9.0]: https://github.com/sonalect/scheda/releases/tag/v0.9.0
[0.8.0]: https://github.com/sonalect/scheda/releases/tag/v0.8.0
[0.7.1]: https://github.com/sonalect/scheda/releases/tag/v0.7.1
[0.7.0]: https://github.com/sonalect/scheda/releases/tag/v0.7.0
[0.6.0]: https://github.com/sonalect/scheda/releases/tag/v0.6.0
[0.5.1]: https://github.com/sonalect/scheda/releases/tag/v0.5.1
[0.5.0]: https://github.com/sonalect/scheda/releases/tag/v0.5.0
[0.4.4]: https://github.com/sonalect/scheda/releases/tag/v0.4.4
[0.4.3]: https://github.com/sonalect/scheda/releases/tag/v0.4.3
[0.4.2]: https://github.com/sonalect/scheda/releases/tag/v0.4.2
[0.4.1]: https://github.com/sonalect/scheda/releases/tag/v0.4.1
[0.4.0]: https://github.com/sonalect/scheda/releases/tag/v0.4.0
[0.3.0]: https://github.com/sonalect/scheda/releases/tag/v0.3.0
[0.2.0]: https://github.com/sonalect/scheda/releases/tag/v0.2.0
[0.1.0]: https://github.com/sonalect/scheda/releases/tag/v0.1.0
[0.0.1]: https://github.com/sonalect/scheda/releases/tag/v0.0.1
