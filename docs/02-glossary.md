# 02. Glossary

Document 2 of the Scheda package. Version 0.73, 20 October 2026.

This document is normative. Terms defined here bind the rest of the package, the on-disk Vault, and every language implementation. If another document uses a word differently, that document is wrong.

Each article is: the term, a short gloss, the definition, and — when it matters — what not to confuse it with.

---

## Writing rules

**Product nouns are capitalized:** Vault, Collection, Document, Index, Projector, Scheda.

**Collection kinds and API names are monospace literals:** `standard`, `journal`, `singleton`, the calls by their rpc names (`Scheda.Open`, `Vault.Close`, `Vault.CreateCollection`, `Collection.Create`, `Collection.Get`, `Collection.Update`, `Collection.Upsert`, `Collection.Delete`, `Collection.History`, `Collection.Query`, `Collection.Search`, `Mirror.Pull`, `Mirror.Push`, `Mirror.Sync`, `Index.Reindex`), and the options `auto_sync`, `author`, `encryption`, `encrypted`, `passphrase`, `identity`.

**Index in this package is never git's staging area.** Git's index, if mentioned, is written “git index” or “staging area”. Scheda's Index is the derived search layer.

**Language-neutral contracts.** Requirements talk about error, failure, missing, and empty, and name a failure by its `VaultErrorCode` (`NOT_FOUND`, `ALREADY_EXISTS`, …; in Protobuf JSON `VAULT_ERROR_CODE_NOT_FOUND`). Idioms of one language (`Err`, `None`, `error` return, `throw`) appear only in a sentence that names that language.

---

## 1. Product and process

**Scheda** — the store.

A library in the caller's process: it reads and writes a Vault, rebuilds an Index, and answers queries. It is not a daemon, not a hosted database, and not a knowledge product. The first implementation is the Rust crate `scheda`. Other languages (Python, Java, Go, TypeScript, and more) may follow. Every implementation must use these terms and the same Vault layout.

Do not confuse with the Vault. Scheda is the code. The Vault is the directory.

**Implementation** — one language binding of Scheda (the Rust crate today; Python, Java, Go, TypeScript, or another package later). Implementations share the Vault and the operations. They do not each invent a Vault layout.

**Consumer** — the program that links or imports an implementation and declares collections. The consumer owns type names, Document bytes, scores, and product protocol. Scheda does not. Field names and Index columns live in the consumer's `Projector`, not as a field list in the Vault.

**Caller** — the code that invoked the library in this process. Often the consumer; for the agent surface, an adapter in front of the same API.

---

## 2. Units on disk

**Vault** — a git working tree that holds collections and the marker file `.scheda.yaml` (`version: 4`, the generation of the Vault format, committed to git).

The Vault root is a git repository. Documents are ordinary files in that tree. A mirror is optional. A person opens the Vault in an editor and in `git` without the library.

Do not confuse with a database file, with the Index, or with “a Scheda file”. There is no single-file Scheda database.

**Collection** — a named set of Documents with one collection declaration and one write mode.

The consumer declares collections. Scheda does not ship a built-in collection for a domain. Search does not cross Vaults unless the caller asks. The directory at the Vault root is the collection name (same rules as one Document `id` component).

**Document** — one member of a collection.

`(collection, id)` plus bytes at the path the library names. A `standard` file, a journal line payload, or a singleton file. Any bytes: text, an image, protobuf wire. Format and extension are the caller's. Scheda does not parse the bytes. A Document is output of a read or a write; a write request carries the `id` and the bytes.

**Collection declaration** — data in the Vault that names a collection's write mode and perimeter.

Which collections exist, their kind, isolation, encryption, journal caps, singleton `id`. Not a list of Document fields, types, required keys, search marks, or edges. Document file paths are fixed by kind (`03`), not chosen in the declaration. The declaration contains no executable code. Changing it is a git commit like any other file. On disk: `{name}/.collection.yaml` (also `.yml` / `.json`); in the contract, the message `CollectionDeclaration`. The file has no version of its own: the generation is the marker's.

Do not confuse with types in the library source, or with a consumer's document schema. Consumer collection names must not appear in `rust/scheda/src`. Test data may name them.

**Schema** — the part of a collection declaration that names the kind and the perimeter (`CollectionDeclaration.schema`: the kind arm, `isolated`, `encrypted`, `index_scope`).

Not a field list. The crate once used `{name}/.schema.yaml` with a field list. That is gone. A leftover `.schema.yaml` is `INVALID_ARGUMENT`. Use **collection declaration**.

**Generation** — the format generation of the Vault: the shape of its files and their layout. Recorded in one place, the marker `.scheda.yaml`, as `version` (field 1; the current generation is 2).

`Scheda.Open` and `Vault.Check` read it before any other file; another generation fails with `GENERATION`. Not the version of the library. A collection has no generation of its own. A field added to a stored file needs no new generation: an earlier Scheda reads past it.

**Working tree** — git's checkout at the Vault root. Scheda writes the files of a write here after its commit. Everything works on `HEAD`; no call reads these files. They equal `HEAD`, or hold a change a person has not committed (ordinary git status), which Scheda leaves as it is.

---

## 3. Collection kinds

**Kind** — the write mode of a collection. Values: `standard`, `journal`, `singleton`. A collection has exactly one kind. Kind is not a codec for Document contents. (`standard` was `document` before generation 2; `Document` now names the object.)

**`standard`** — one file per Document.

The file is `{collection}/{id}` (`/` in `id` is a directory). Extension, if any, is part of `id`. `Collection.Update` and `Upsert` replace the file as a whole. `Collection.Delete` removes it. History of the Document is git history of its file (`Collection.History`), not a Scheda revision type.

**`journal`** — append-only lines in a collection directory.

Each line is one Document. Append only: there is no library operation to update or delete a line (`Update`, `Upsert`, and `Delete` are `UNIMPLEMENTED`). On disk this is **not** one unbounded file. The collection is a directory of **segments**. `Create` appends to the single open segment. The collection declaration closes that file when it hits a size cap (and optionally a line cap or calendar boundary). A closed segment is never rewritten.

The line is a Scheda-owned envelope `{id}: {payload}\n` (colon, then space). `id` is the request's `id` (a single slug). `payload` is the `raw` bytes `Create` received; it holds no `\n` and no `\r`. `Document.raw` is the payload, not the envelope. Scheda splits on the first `:` (colon, then space); it does not parse the payload. A segment of these lines is not a YAML document.

Git does not use LFS in this design. Hosts commonly warn near 50 MiB and reject near 100 MiB per file; a segment that grows in place also makes every commit rewrite a large blob. Segments stay small enough that ordinary git remains usable. The default cap belongs in `05`; it must sit well below those host limits.

Do not confuse with `standard`. Many journal files are still one collection and one `id` space.

**Segment** — one file of envelope lines inside a `journal` collection.

Named by the collection declaration (sequence number, UTC date, or both) and the writer identity of the clone. At most one segment is open for append. Closed segments are immutable, so git history adds new small files instead of a growing blob. Merging two histories that appended to the same open segment is still a line-level git merge; rotation does not make multi-writer safe.

**`singleton`** — one file for the whole collection.

The file is `{collection}/{declared id}` (the `id` in the collection declaration; it may include an extension). `Update` and `Upsert` replace that file as a whole. There is not a set of ids in the usual sense; the collection *is* the file. The file must stay small for the same git reasons as a segment; a singleton is not a place to dump a journal.

---

## 4. Document identity

**`id`** — stable identifier of a Document inside its collection.

For `standard` and `singleton`, `id` is a vault-relative POSIX path under the collection. The library does not append `.md`. An extension, if any, is part of `id` (`note.yaml`, `work/meeting.toml`). Scheda does not interpret the extension. There is no identity key inside the file. Renaming the file is changing `id`.

The `id` must stay inside the collection directory. Empty, NUL, `\`, a leading or trailing `/`, `//`, a `.` or `..` component, and a Windows device stem (`CON`, `NUL`, …) are `INVALID_ARGUMENT`. Each path component follows the slug character set (Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`; max 255 bytes for the whole `id`; no leading/trailing whitespace; no trailing `.`). `journal` and collection names stay a single slug (no `/`). Scheda does not mint domain identifiers for the consumer.

**Path** — location of the Document in the Vault.

For `standard`, `{collection}/{id}` (`/` in `id` is a directory). For `journal`, the segment file that contains the envelope line (the Document is still addressed by `(collection, id)`, not by segment name). For `singleton`, `{collection}/{declared id}`. The caller never chooses these paths.

---

## 5. Derived layer

**Index** — the disposable search layer, on Lance.

Built from the commits of the Vault, not the work tree, and kept at git `HEAD` by the indexer of an open Vault that has every port the Index needs (`06` IDX-9), one round at a time under `index.lock` (eventually consistent; `Index.Status` and `Index.Watch` report the state it reached, and a call that names a **state token** waits for it). Stored under `{vault}/.scheda/index/` (gitignored), not committed with the Vault, safe to delete. It holds, per indexed collection, a table of the Projector's typed columns with the full-text and vector indices on them, and a table of the files the rows came from; one `commit` file per **Index generation** names the state and the version of every table, so a reader sees one state across all collections. The journal `id` maps live beside it with their own `commit`. A round writes changed rows in place, not the whole Index. A rebuild (`Index.Reindex`, or the first build) is made beside the live Index, which keeps answering until the switch. A damaged Index must not serve stale ids: queries fail with an Index code. `Collection.Get` and `Collection.History` do not use it: they read the commit at `HEAD` (IDX-10).

Do not confuse with git's staging area, with the Vault, or with source of truth. If the Index and the commit at `HEAD` disagree, the commit wins for `Collection.Get` and `Collection.History`. Queries and searches trust the Index for which `id`s exist, then read those Documents.

Identity columns (`id`, `file`, `order`) are the library's. Every other column comes from a **Projector**.

**Index generation** — one build of the tables of the Index: the folder `{vault}/.scheda/index/g{n}/`. `current` names the live one. `Index.Reindex` makes another beside it and switches by one atomic write; the old one is removed an hour later. Not the **generation** of the Vault format.

**State token** — the opaque text that names a stored state of the Vault: the commit of the main repository and, when there is one, of the nested repository. A write returns the token of the state it made (`DocumentWriteResult.version`), `Vault.Info` the token of the stored state now, and `IndexState` two: `version`, the state the Index reflects, and `stored_version`, the stored state the indexer last saw (the Index is behind while they differ). Equal tokens are the same state; the caller compares them and passes them, nothing more.

**`not_before`** — a request field of a query, a count, or a search that holds a state token. The call waits until the Index reflects a state that contains it, up to `OpenOptions.index_wait_timeout_ms`; past it, `DEADLINE_EXCEEDED`. A token whose commit left the history of `HEAD` does not wait. Unset: the call answers at once from what the Index holds.

**Filter** — the condition of a query or the prefilter of a search: a typed tree the caller fills (`all_of`, `any_of`, `none_of`; the leaves `term`, `terms`, `range`, `exists`, `prefix`) over `id` and the Projector's columns. The library checks it against the columns and runs it in the Index; no caller writes the query language of the engine. Two-valued: a Document without a value in a column fails a condition on it, and `none_of` it holds.

**Projector** — a caller-supplied port that declares Index columns and fills them from Document bytes.

Not Vault data. Not a field of a declaration. A port, like Embedder and Reranker. `Projector.Describe` names the projection (`ProjectorDescriptor.projection_id`, for example `notes@4`: the projection and its version together), read once when the Projector is given to a Vault and recorded with the Index; a Projector of another projection, or one that gives other columns, makes the Index `INDEX_STALE` for that Vault until an `Index.Reindex` from a Vault with the new Projector. `Projector.Columns` names the typed columns of a collection (`scalar_index` / `fts` / `vector` marks). `Projector.Project` returns values for those columns of one Document. A write does not call `Project` to accept or reject the Document. A `Project` that refuses the Document leaves it out of the Index, and one that gives no values leaves it with its identity only; `Collection.Get` still returns the file. Without a Projector, a query that names only `id` still works; any other column is `INVALID_ARGUMENT`; a search has no content to rank.

Do not confuse with Embedder (text → vector) or with the Index (the files under `.scheda/`).

**Port** — a service the caller implements and Scheda calls: `Embedder`, `Reranker`, `Projector`. A port reaches a Vault as a reference (`EmbedderRef`, `RerankerRef`, `ProjectorRef`) in `Scheda.Open` or later (`Vault.SetEmbedder`, `SetReranker`, `SetProjector`, and a `Remove…` for each), in the form its implementation has (blocking or async). A port is optional; Scheda ships no model and does not know what a Document holds.

**Lexical search** — ranked full-text (BM25) over Projector columns marked `fts`, by Lance's inverted index (lower case, no stemming, no phrases). Not grep, SQL `LIKE`, or a file scan at query time.

**Vector search** — in-process nearest neighbours by cosine distance (an IVF_PQ index ranked again with the full vectors, and an exact scan of the rows no index covers yet) over embeddings of Projector columns marked `vector`, one per text (a `text list` column gives several); a Document is ranked once, by its text closest to the query. Embedding weights are not stored in the Vault. An external vector database is not the Index.

**Embedder** — a port that turns Projector text into vectors. Optional. `Describe` gives the model id and the vector length (`EmbedderDescriptor`; both are recorded with the Index, so an embedder of another model finds it `INDEX_STALE` and is never mixed in). `Embed` takes a stream of texts and returns a stream of vectors in the same order; the implementation forms the portions its model wants. `EmbedQuery` embeds one search text. Without an embedder, vector search is off; the rest of Scheda still works. An embedder that uses the network is passed in explicitly; core open/write/lexical rebuild with `auto_sync` off do not call the network. Its codes are `EmbedderErrorCode` (`UNAVAILABLE`, retryable; `REFUSED`).

**Hybrid search** — one `Collection.Search` (or `Vault.SearchDocuments`) call that runs the lexical and vector legs when both are available, then fuses ranks with Reciprocal Rank Fusion, weighted by `lexical_weight` and `vector_weight`. An optional reranker then reorders. The result is a summary of the legs, then ordered Documents with scores, not a model-written answer.

**Reranker** — an optional port: `Score` takes a stream whose first item is the query text and the rest the Documents, and returns one score per Document, in order. The library sorts by it. No model in Scheda. Without a reranker, order is RRF; `DocumentSearch.rerank_limit` set without one is `INVALID_ARGUMENT`. `rerank_limit` is how many of the first fused hits reach it; unset means no rerank. Its codes are `RerankerErrorCode`.

---

## 6. Access

**Async only** — in Rust, the objects are async on the caller's tokio runtime, a multi-thread or a current-thread one, and have no blocking form of their own. A caller without async code builds a multi-thread runtime and takes `to_sync()` of a reference inside it (protocontract's `Blocking`); on a current-thread runtime that form fails with `CANNOT_BLOCK`. A call that is dropped stops at its next `await`.

**Library API** — the operations every implementation must provide: the rpcs of `api/scheda/v1/*.proto`, whose comments are the per-method contract (`07` keeps what spans methods). Language-neutral as a contract. The first binding is Rust.

**Object** — the contract is a set of objects, each a service, reached from the one before it: `Scheda` opens a `Vault`; a `Vault` gives a `Collection`, its `Mirror`, and its `Index`.

**Reference** — an object (or a port) passed as a message: `VaultRef`, `CollectionRef`, `MirrorRef`, `IndexRef`, `EmbedderRef`, `RerankerRef`, `ProjectorRef`. It holds the implementation itself, within one process; a decoded reference holds nothing, and a reference has no Protobuf JSON form.

**`Scheda.Open`** — attach to a Vault, or create one, as `OpenOptions.mode` says: `OPEN_MODE_OPEN_OR_CREATE` (the default), `OPEN_MODE_OPEN_EXISTING`, `OPEN_MODE_CREATE_NEW`. Returns a `VaultRef`. Path exists as a Vault → open. A set `OpenOptions.mirror` must match the URL of `origin` (`INVALID_ARGUMENT` otherwise); unset attaches and does not change `origin`. Path missing and `mirror` set → clone. Path missing → new local Vault with no mirror. Reads the mirror locally on an existing tree (no fetch unless `auto_sync`). `Scheda.Open` takes no lock of its own: its writes (the marker of a new Vault, the `auto_sync` pull) take the write lock as any write does. More than one Vault may be open in one process; two on one path are two Vaults, as in two processes. Roots do not leak into each other's search. Ports are caller code, given as references.

**`Scheda.Info` / `Vault.Info`** — the version and features of the build (`SchedaInfo`); the path, Index path, writer identity, and state token (`version`) of one Vault (`VaultInfo`). Not a live handle. Collections are listed by `Vault.ListCollections` (declarations: kind, isolation, encryption, caps), not Document fields.

**`Vault.Close`** — end the Vault: new calls of it and of every object reached from it fail with `FAILED_PRECONDITION`, as does a wait for `not_before`; it returns when the running calls have ended, the indexer task has stopped, and no lock is held. A second `Close` is nothing to do. A Vault whose references are all dropped closes by itself, without waiting: it tells its indexer to stop, and that ends after its current round.

**Write** — a call that changes the Vault: `Collection.Create`, `Update`, `Upsert`, `Delete`; `Vault.CreateCollection`, `UpdateCollection`, `DeleteCollection`. It is atomic: when it returns `Ok` the write is stored, and an error means nothing was written. One write is one git commit and returns the state token of the Vault it made; the API shows no commit, no transaction, and no `HEAD`.

**Write stream** (batch) — several writes sent as one stream of operations: `Collection.Batch`, `Vault.BatchDocuments`, `Vault.BatchCollections`. All happen, or none, in one commit; an error names the operation by its index. A batch holds Document writes or collection writes, never both. Nothing is held while the caller builds the stream.

**Write lock** — the right to make one commit at a time on a Vault: `flock` on `{vault}/.scheda/commit.lock`, of any handle in any process, held for that commit only. A write waits for it at most `OpenOptions.write_timeout_ms`; past it the call is `ABORTED` and writes nothing. Reads never take it and see stored state only.

**Note** — the reason a caller gives for a write of a Document (required, non-empty). History shows it. A collection write takes no note.

**Revision** — `Document.revision`: an opaque token that changes whenever the stored bytes change (the id of the Document's blob in `HEAD`). A write that must find the Document unchanged passes it back (`DocumentUpdate.revision`, `DocumentDeletion.revision`); a stale one is `FAILED_PRECONDITION`. Unset on a journal line.

**`Collection.Get`** — read one Document by `(collection, id)`: validate the `id`, then read that file from `HEAD`. Returns bytes, not parsed fields. Does not ask the Index which ids exist (`06` IDX-10). Absence is `NOT_FOUND`, not a blank Document pretending to exist. `Collection.GetMany` reads the given ids, or the collection page by page, as a stream.

**`Create`** — add a Document: a new `standard` file, a new journal line, or the first write of a `singleton`. Present is `ALREADY_EXISTS`, or, with `keep_existing`, the Document already stored is returned and nothing is written.

**`Update`** — replace a whole existing `standard` or `singleton` Document. Absent is `NOT_FOUND`. `UNIMPLEMENTED` on a `journal`.

**`Upsert`** — create the Document, or replace it when present. `UNIMPLEMENTED` on a `journal`.

**`Delete`** — delete a `standard` or `singleton` Document. Unlinks the file; absent is `NOT_FOUND`, or nothing with `ignore_missing`. `UNIMPLEMENTED` on a `journal`. After the file is gone, `Collection.History` still answers from git.

**`Collection.History`** — git history of one Document by `id`, newest first, as a stream of versions (revision, time, author, note, bytes, and the `write_id` that every version one write made shares). Same identity path as `Collection.Get` (validate `id`, then that file). Not an Index lookup. After `Delete`, the commits that stored the file remain. `UNIMPLEMENTED` on a `journal`.

**`Collection.Query`** — select Documents with a `Filter` (a typed tree: `all_of`, `any_of`, `none_of`, `term`, `terms`, `range`, `exists`, `prefix`), over `id` and the Projector's columns. On `Vault.QueryDocuments`: a set of collection names (empty = this Vault, neither isolated nor encrypted; a named isolated collection is an ordinary query), and one filter applies to every collection in the set. On Collection: that collection only. The exact set of matches, without scores, in a stable order (`order_by` keys, then the collection, then `id`; one `journal` in append order). A stream: first a `QuerySummary` with the `total` of matches, counted in the same snapshot as the page, then the Documents; paged by `offset` / `limit` / `start_after`. `Count` gives the number without opening a file. Not by meaning: that is `Search`.

**`Collection.Search`** — hybrid retrieval (BM25 + nearest neighbours, RRF) by relevance. Same collection-name set as `Query` (`Vault.SearchDocuments`). An optional `Filter` pre-cuts both legs. `include_raw` fills the Documents' `raw`. Ranks; does not select.

**`Vault.CreateCollection`** — write one collection declaration (`CollectionDeclaration`) as `{name}/.collection.yaml`, and return it as stored, defaults filled in. Not a raw path write. `UpdateCollection`, `DeleteCollection` (an empty collection only), and `BatchCollections` complete the set. An isolated collection's Document files go under `{vault}/.isolated/`, whose nested git is made if needed.

**Mirror** — the one optional git remote of the Vault repository (`origin` is git's name for it and stays inside Scheda). `Mirror.Info` gives its `url` (empty: none), `auto_sync`, and `last_auto_sync_error`, read locally. `Mirror.Set` and `Remove` attach and detach it; the Vault then works with a mirror or locally only. Extra remotes in git config are ignored. GitHub is one host, not a Scheda type. Isolated collections never leave the machine.

**`auto_sync`** — option on `Scheda.Open`, default off. When on and a mirror exists: pull when the Vault opens, and push after a write. A failure of either neither fails the open nor undoes the write; `Mirror.Info.last_auto_sync_error` shows it. Details in `04`.

**`author`** — option on `Scheda.Open` (`Author`). Git author and committer for every commit of this Vault (main tree and `{vault}/.isolated/`). Required `name` and `email` when set. Unset: local git config, then `Scheda <scheda@invalid>`.

**`Mirror.Pull`** — fetch and join: a mirror that only added moves `HEAD` forward; histories that both moved are joined Document by Document, and a Document changed differently on both sides is `MIRROR_CONFLICT` with nothing changed. Uses the network. Takes the write lock.

**`Mirror.Push`** — send the stored writes to the mirror. Uses the network. The mirror holds writes this Vault has not pulled: `MIRROR_AHEAD`.

**`Mirror.Sync`** — `Pull`, then `Push`. Against a mirror that holds nothing yet it pushes. Independent of `auto_sync`.

**`reindex`** (`Index.Reindex`) — ask the indexer to build a new Index generation from the commits at `HEAD` with this Vault's ports, and return at once; the live Index keeps answering queries and searches, frozen at its state, until the switch, and the writes made meanwhile appear at the switch. With the embedder model of the live Index, only a text it holds no vector of for that Document is embedded; with another model, every text. The only way the projection, the columns, or the embedder model of the Index change, and the way to repair an `INDEX_BROKEN` one.

**`Vault.Check`** — validate the Vault file by file and return a report of every problem (code, path, message): collection declarations, journal lines, duplicate ids, an unfinished merge (from git's own state), the gitignore of isolated collections, owner-only modes, and files a person changed and did not commit. Does not type-check Document contents. The call fails only when it cannot run.

**Agent surface** — the same operations as the library API, over a thin transport (CLI, MCP, or other). Collection listing is `Vault.ListCollections`. The transport contains no product rules.

---

## 7. Isolation (**later**)

**Isolated collection** — a collection whose Document files live under `{vault}/.isolated/{name}/`: a gitignored nested git working tree that never leaves the machine (no `Mirror` call contacts it, whatever it has configured). It is for what may be lost or rebuilt. What is private and must survive belongs in an encrypted collection. The collection declaration stays `{vault}/{name}/.collection.yaml` in the main tree. Search over non-isolated collections does not read it and must not require `.isolated/` to exist. Whether its files are ciphertext is a separate question — see **Encrypted collection** (`09`).

**Isolated tree** — `{vault}/.isolated/`. Its writes are commits of the nested repository, and `Collection.History` of isolated Documents reads it. Its indexed values are in the one Vault Index, which is owner-only for that reason; it has no Index of its own.

**Encrypted collection** — a collection whose declaration sets `encrypted: true` (SCH-16). Every Document file of it is age v1 armor on disk, whole-file; I/O needs the Vault secret (`encryption`) and fails with `CRYPTO` without it, while a listing without `include_raw` still gives the ids. Independent of **isolated**: all four combinations are valid. `standard` and `singleton` only — an encrypted `journal` is `INVALID_ARGUMENT`. No Index holds it, so `Query` and `Search` naming it are `UNIMPLEMENTED` and the ways in are `Collection.Get` and `GetMany`. In a build without the `encrypt` feature the collection is refused with `UNIMPLEMENTED`, never returned as ciphertext dressed as a Document (`09`).

**`encryption`** — `OpenOptions` field (`Encryption`) carrying this Vault's one secret, used for every encrypted collection it touches. When set, `passphrase` or `identity` is required. Setting it encrypts nothing by itself: what is ciphertext is each collection's declaration (SCH-16). `Scheda.Open` never requires it, and plain collections work without it (`09`).

**`passphrase`** — symmetric arm of `Encryption`. Age scrypt wrapping.

**`identity`** — age X25519 identity arm of `Encryption` (`AGE-SECRET-KEY-…` file).

**Sealed value** — a Document value that must not appear in the Index. Encryption of a collection's files (`encrypted`, age armor) is `09`; the name here only marks the Index rule. Encrypted collections enter no Index.

---

## 8. Words this package does not treat as Scheda types

**Record, transaction, autocommit, Remote** — names of 0.21, replaced. A Document is what was a Record. A write call or a write stream replaces `begin` / `commit` / `discard` and autocommit: there is no transaction in the API. The mirror is what was the Remote. Git's own branch is not a database transaction either.

**Score, rating, utility** — consumer concerns. Scheda does not persist them as first-class columns of the engine.

**Fields, body, header, edge** — not Scheda Document concepts. They may exist inside a consumer's bytes. The Index sees only what a Projector `project`s.
