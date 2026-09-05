# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
While the major version is 0, compatible additions bump the patch; breaking
API or on-disk format changes bump the minor.

## [Unreleased]

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

[Unreleased]: https://github.com/amsokol/scheda/compare/v0.9.7...HEAD
[0.9.7]: https://github.com/amsokol/scheda/releases/tag/v0.9.7
[0.9.6]: https://github.com/amsokol/scheda/releases/tag/v0.9.6
[0.9.5]: https://github.com/amsokol/scheda/releases/tag/v0.9.5
[0.9.4]: https://github.com/amsokol/scheda/releases/tag/v0.9.4
[0.9.3]: https://github.com/amsokol/scheda/releases/tag/v0.9.3
[0.9.2]: https://github.com/amsokol/scheda/releases/tag/v0.9.2
[0.9.1]: https://github.com/amsokol/scheda/releases/tag/v0.9.1
[0.9.0]: https://github.com/amsokol/scheda/releases/tag/v0.9.0
[0.8.0]: https://github.com/amsokol/scheda/releases/tag/v0.8.0
[0.7.1]: https://github.com/amsokol/scheda/releases/tag/v0.7.1
[0.7.0]: https://github.com/amsokol/scheda/releases/tag/v0.7.0
[0.6.0]: https://github.com/amsokol/scheda/releases/tag/v0.6.0
[0.5.1]: https://github.com/amsokol/scheda/releases/tag/v0.5.1
[0.5.0]: https://github.com/amsokol/scheda/releases/tag/v0.5.0
[0.4.4]: https://github.com/amsokol/scheda/releases/tag/v0.4.4
[0.4.3]: https://github.com/amsokol/scheda/releases/tag/v0.4.3
[0.4.2]: https://github.com/amsokol/scheda/releases/tag/v0.4.2
[0.4.1]: https://github.com/amsokol/scheda/releases/tag/v0.4.1
[0.4.0]: https://github.com/amsokol/scheda/releases/tag/v0.4.0
[0.3.0]: https://github.com/amsokol/scheda/releases/tag/v0.3.0
[0.2.0]: https://github.com/amsokol/scheda/releases/tag/v0.2.0
[0.1.0]: https://github.com/amsokol/scheda/releases/tag/v0.1.0
[0.0.1]: https://github.com/amsokol/scheda/releases/tag/v0.0.1
