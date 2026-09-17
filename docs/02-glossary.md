# 02. Glossary

Document 2 of the Scheda package. Version 0.57, 17 September 2026.

This document is normative. Terms defined here bind the rest of the package, the on-disk Vault, and every language implementation. If another document uses a word differently, that document is wrong.

Each article is: the term, a short gloss, the definition, and — when it matters — what not to confuse it with.

---

## Writing rules

**Product nouns are capitalized:** Vault, Collection, Record, Index, Projector, Scheda.

**Collection kinds and API names are monospace literals:** `document`, `journal`, `singleton`, `add`, `get`, `replace`, `remove`, `history`, `lines`, `find`, `search`, `create_collection`, `begin`, `commit`, `discard`, `open`, `info`, `auto_sync`, `author`, `push`, `pull`, `sync`, `encryption`, `encrypted`, `passphrase`, `identity`.

**Index in this package is never git's staging area.** Git's index, if mentioned, is written “git index” or “staging area”. Scheda's Index is the derived search layer.

**Language-neutral contracts.** Requirements talk about error, failure, missing, and empty. Idioms of one language (`Err`, `None`, `error` return, `throw`) appear only in a sentence that names that language.

---

## 1. Product and process

**Scheda** — the store.

A library in the caller's process: it reads and writes a Vault, rebuilds an Index, and answers queries. It is not a daemon, not a hosted database, and not a knowledge product. The first implementation is the Rust crate `scheda`. Other languages (Python, Java, Go, TypeScript, and more) may follow. Every implementation must use these terms and the same Vault layout.

Do not confuse with the Vault. Scheda is the code. The Vault is the directory.

**Implementation** — one language binding of Scheda (the Rust crate today; Python, Java, Go, TypeScript, or another package later). Implementations share the Vault and the operations. They do not each invent a Vault layout.

**Consumer** — the program that links or imports an implementation and declares collections. The consumer owns type names, Record bytes, scores, and product protocol. Scheda does not. Field names and Index columns live in the consumer's `Projector`, not as a field list in the Vault.

**Caller** — the code that invoked the library in this process. Often the consumer; for the agent surface, an adapter in front of the same API.

---

## 2. Units on disk

**Vault** — a git working tree that holds collections.

The Vault root is a git repository. Records are ordinary UTF-8 files in that tree. A remote is optional. A person opens the Vault in an editor and in `git` without the library.

Do not confuse with a database file, with the Index, or with “a Scheda file”. There is no single-file Scheda database.

**Collection** — a named set of records with one collection declaration and one write mode.

The consumer declares collections. Scheda does not ship a built-in collection for a domain. Search does not cross Vaults unless the caller asks. The directory at the Vault root is the collection name (same rules as one Record `id` component).

**Record** — one member of a collection.

`(collection, id)` plus opaque UTF-8 bytes at the path the library names. A `document` file, a journal line payload, or a singleton file. Format and extension are the caller's. Scheda does not parse the bytes.

**Collection declaration** — data in the Vault that names a collection's write mode and perimeter.

Which collections exist, their kind, isolation, encryption, journal caps, singleton `id`. Not a list of Record fields, types, required keys, search marks, or edges. Record file paths are fixed by kind (`03`), not chosen in the declaration. The declaration contains no executable code. Changing it is a git commit like any other file. On disk: `{name}/.collection.yaml` (also `.yml` / `.json`).

Do not confuse with types in the library source, or with a consumer's document schema. Consumer collection names must not appear in `rust/scheda/src`. Test data may name them.

**Schema** — retired as a product noun for field types.

The crate used `{name}/.schema.yaml` with a field list. That is gone. A leftover `.schema.yaml` is `invalid`. Use **collection declaration**.

**Working tree** — git's checkout at the Vault root. Scheda writes files here. Uncommitted writes are visible as ordinary git status.

---

## 3. Collection kinds

**Kind** — the write mode of a collection. Values: `document`, `journal`, `singleton`. A collection has exactly one kind. Kind is not a codec for Record contents.

**`document`** — one UTF-8 file per Record.

The file is `{collection}/{id}` (`/` in `id` is a directory). Extension, if any, is part of `id`. Replace updates the file as a whole. `remove` unlinks it. History of the Record is git history of its file (`Collection.history(id)`), not a Scheda revision type.

**`journal`** — append-only UTF-8 lines in a collection directory.

Each line is one Record. Append only: there is no library operation to update or delete a line (`replace` and `remove` are `unsupported`). On disk this is **not** one unbounded file. The collection is a directory of **segments**. Append writes to the single open segment. The collection declaration closes that file when it hits a size cap (and optionally a line cap or calendar boundary). A closed segment is never rewritten.

The line is a Scheda-owned envelope `{id}: {payload}\n` (colon, then space). `id` is the method argument (a single slug). `payload` is the bytes `add` received. `Record.raw` is the payload, not the envelope. Scheda splits on the first `:` (colon, then space); it does not parse the payload. A segment of these lines is not a YAML document.

Git does not use LFS in this design. Hosts commonly warn near 50 MiB and reject near 100 MiB per file; a segment that grows in place also makes every commit rewrite a large blob. Segments stay small enough that ordinary git remains usable. The default cap belongs in `05`; it must sit well below those host limits.

Do not confuse with `document`. Many journal files are still one collection and one `id` space.

**Segment** — one file of envelope lines inside a `journal` collection.

Named by the collection declaration (sequence number, UTC date, or both). At most one segment is open for append. Closed segments are immutable, so git history adds new small files instead of a growing blob. Merging two histories that appended to the same open segment is still a line-level git merge; rotation does not make multi-writer safe.

**`singleton`** — one UTF-8 file for the whole collection.

The file is `{collection}/{declared id}` (the `id` in the collection declaration; it may include an extension). Replace replaces that file as a whole. There is not a set of ids in the usual sense; the collection *is* the file. The file must stay small for the same git reasons as a segment; a singleton is not a place to dump a journal.

---

## 4. Record identity

**`id`** — stable identifier of a Record inside its collection.

For `document` and `singleton`, `id` is a vault-relative POSIX path under the collection. The library does not append `.md`. An extension, if any, is part of `id` (`note.yaml`, `work/meeting.toml`). Scheda does not interpret the extension. There is no identity key inside the file. Renaming the file is changing `id`.

The `id` must stay inside the collection directory. Empty, NUL, `\`, a leading or trailing `/`, `//`, a `.` or `..` component, and a Windows device stem (`CON`, `NUL`, …) are `invalid`. Each path component follows the slug character set (Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`; max 255 bytes for the whole `id`; no leading/trailing whitespace; no trailing `.`). `journal` and collection names stay a single slug (no `/`). Scheda does not mint domain identifiers for the consumer.

**Path** — location of the Record in the Vault.

For `document`, `{collection}/{id}` (`/` in `id` is a directory). For `journal`, the segment file that contains the envelope line (the Record is still addressed by `(collection, id)`, not by segment name). For `singleton`, `{collection}/{declared id}`. The caller never chooses these paths.

---

## 5. Derived layer

**Index** — the disposable search layer.

Built from Vault files. Stored under `{vault}/.scheda/` (gitignored), not committed with the Vault, safe to delete. Rebuild (`reindex`) reads files. A corrupt Index must not serve stale ids: `find` / `search` return empty or an error. `get` and `history` still read files (IDX-10).

Do not confuse with git's staging area, with the Vault, or with source of truth. If the Index and the files disagree, the files win for `get` and `history`. Other methods trust the Index for which `id`s exist, then read those files.

Identity columns (`collection`, `id`, ordinal) are the library's. Every other column comes from a **Projector**.

**Projector** — a caller-supplied callback at `open` that declares Index columns and fills them from Record bytes.

Not Vault data. Not an `api/v1` field. Same injection class as Embedder and Reranker. `schema()` names typed columns (find / FTS / vector marks). `project(collection, id, raw)` returns values for those columns. `add` does not call `project` to accept or reject the write. Failure or empty `project` omits the Record from lexical and vector indexes; `get` still returns the file. Without a Projector, `find` on identity (`all`, `term` on `id`) still works; any other field is `invalid`; `search` has no content slots.

Do not confuse with Embedder (text → vector) or with the Index (the files under `.scheda/`).

**Lexical search** — ranked full-text (BM25 or equivalent) over Projector columns marked FTS. Not grep, SQL `LIKE`, or a file scan at query time.

**Vector search** — in-process ANN nearest neighbours (HNSW or equivalent) over embeddings of Projector columns marked for vectors. Embedding weights are not stored in the Vault. An external vector database is not the Index.

**Embedder** — a caller-supplied function (or equivalent) that turns Projector text into a vector. Optional. Without it, vector search is off; the rest of Scheda still works. An embedder that uses the network is passed in explicitly; core open/write/lexical rebuild with `auto_sync` off do not call the network. `embed_many` is the required function; `embed` is provided as the batch call with one text. `many_limit` and `dimensions` are non-zero sizes, so a zero cannot be passed: no vector search means no embedder, not a zero dimension.

**Hybrid search** — one `search` call that runs the lexical and vector legs when both are available, then fuses ranks with Reciprocal Rank Fusion (default) or caller merge weights. Optional reranker callback then reorders. The result is ordered ids and scores, not a model-written answer.

**Reranker** — optional caller function on `search`:`score_many(query text, Records) →` numbers for a batch of at most `many_limit`, with `score` provided as that call with one Record. The library sorts by it and returns `k` hits. No model in Scheda. Without it, order is RRF. `many_limit` is a non-zero size. How many fused candidates reach it is the caller's choice (`SearchQuery.rerank_limit`); unset means the fusion floor `max(k, 50)`; a larger N deepens both Index legs so a candidate at fused rank N can be scored.

---

## 6. Access

**Library API** — the operations every implementation must provide, named in `07`. Message fields are `api/v1` (Protobuf edition 2024). Language-neutral as a contract. The first binding is Rust.

**`open`** — attach to a Vault working tree, or create one. Path exists as a Vault → open. A set `remote_repo` must match `origin` (`invalid` if there is no `origin` or the URL differs). Unset `remote_repo` attaches and does not change `origin`. Path missing and `remote_repo` set → `git clone`. Path missing → `git init`, no Remote. Reads the Remote locally on an existing tree (no fetch unless `auto_sync`). `open` does not take the write lock. More than one Vault may be open in one process. Roots do not leak into each other's search. Optional Embedder and optional Projector are caller code, not proto.

**`info`** — snapshot of the open Vault as `api.v1.VaultInfo` (path, Index path, `remote`, `auto_sync`, collections). Lists collection declarations (kind, isolation, encryption, caps), not Record fields. Not a live handle.

**Write lock** — the right of exactly one process to mutate a given Vault. Readers may proceed without it and may see a state slightly older than the writer.

**`add`** — add a Record: a new `document` file, a new journal line, or the first write of a `singleton`. The caller supplies `id` and UTF-8 bytes.

**`get`** — read one Record by `(collection, id)`. Validate the `id`, then open that file. Returns bytes, not parsed fields. Does not ask the Index which ids exist (`06` IDX-10). Absence is an error or empty according to `07`, not a blank document pretending to exist.

**`replace`** — replace a whole `document` or `singleton`. Resolve `id` in the Index first, then write that file. Error on `journal`.

**`remove`** — delete a `document` or `singleton` Record. Unlinks the file and drops it from the Index. Autocommits unless a transaction is open. Error on `journal`. After the file is gone, `history(id)` still answers from git.

**`history`** — git history of one Record by `id`, newest first. Same identity path as `get` (validate `id`, then that file). Not an Index lookup. Not journal `find` append order. After `remove`, the commits that stored the file remain.

**`find`** — select records with a Tantivy query tree (`api.v1.FindQuery`: required `Filter` — boolean must / should / must_not, term, phrase, range, `all`, … — plus `include_raw`; default omits `Record.raw`), including identity `id`. Field names other than identity come from the Projector. On Vault: a set of collection names (empty = this Vault, non-isolated; a named isolated collection is an ordinary query). On Collection: that collection only. Query the Index, then open the files for the `(collection, id)`s it returns. Paged (`offset`, `count`); one page is `api.v1.FindResult` (`records` plus `total`: indexed hits, not page length). One `journal` collection: append order; otherwise score order. Every indexed Record in the selected collections is `all`. Not by meaning.

**`search`** — hybrid retrieval (BM25 + ANN, RRF). Same collection-name set as `find`. Optional `Filter` pre-cuts both legs. `SearchQuery.include_raw` fills `Hit.raw`. Not the same as `find`.

**`create_collection`** — write one collection declaration (`name` plus the declaration message) as `{name}/.collection.yaml`. Not a raw path write. Autocommits unless a transaction is open. Optional commit message on the call; omitted → `scheda`. Isolated writes Record files under `{vault}/.isolated/` and creates the nested git if needed.

**`begin`** — start a transaction on a library handle. Later writes on that handle do not create a git commit until `commit` or `discard`. A transaction is the only supported batch of mutating calls. There is no option that turns autocommit off. The CLI does not expose this name.

**`commit`** — end a transaction on this handle and create one git commit per repository that has recorded paths (main and/or `{vault}/.isolated/`). Message comes from the caller. Outside a transaction, mutating calls autocommit with an optional message (default `scheda`). The CLI does not expose this name.

**`discard`** — restore Scheda-recorded paths to HEAD, end the transaction if any on this handle, reload collection declarations, rebuild the Index. The CLI does not expose this name.

**Remote** — the one optional git remote Scheda uses on the Vault repository, named `origin`. Empty `VaultInfo.remote` means none. Extra remotes in git config are ignored. Detected on `open` from git config (no fetch). GitHub is one host, not a Scheda type.

**`auto_sync`** — option on `open`, default off. When on and a Remote exists: fast-forward `pull` on open, and `push` after a local commit (including autocommit). Not a merge. Details in `04`.

**`author`** — option on `open` (`api.v1.Author`). Git author and committer for every commit on this handle (main tree and `{vault}/.isolated/`). Required `name` and `email` when the message is set. Unset: local git config, then `Scheda <scheda@invalid>`.

**`pull`** — fetch and fast-forward only. Uses the network.

**`push`** — send local commits to a Remote. Uses the network. Mutating calls inside a transaction never push.

**`sync`** — `pull` then `push`. Independent of `auto_sync`.

**`reindex`** — rebuild the Index from files (and the Projector, if any).

**`check`** — validate the Vault against collection declarations, encodings, and envelopes without necessarily writing. Does not type-check Record contents.

**Agent surface** — the same operations as the library API, over a thin transport (CLI, MCP, or other). Collection listing is `info`. The transport contains no product rules.

---

## 7. Isolation (**later**)

**Isolated collection** — a collection whose Record files live under `{vault}/.isolated/{name}/`: a gitignored nested git working tree that the main Vault's `push` / `pull` / `sync` never touch. The collection declaration stays `{vault}/{name}/.collection.yaml` in the main tree. Search over non-isolated collections does not read it and must not require `.isolated/` to exist. Whether its files are ciphertext is a separate question — see **Encrypted collection** (`09`).

**Isolated tree** — `{vault}/.isolated/`. Local `commit` and `Collection.history` of isolated Records only; it may carry a Remote of its own, which the main Vault's `push` / `pull` / `sync` never use. Its indexed values are in the one Vault Index, which is owner-only for that reason; it has no Index of its own.

**Encrypted collection** — a collection whose declaration sets `encrypted: true` (SCH-16). Every Record file of it is age v1 armor on disk, whole-file; I/O needs the handle secret (`encryption`) and fails with `crypto` without it, while `ids()` still works. Independent of **isolated**: all four combinations are valid. `document` and `singleton` only — an encrypted `journal` is `invalid`. No Index holds it, so `find` and `search` naming it are `unsupported` and the ways in are `get` and the `documents()` walk. In a build without the `encrypt` feature the collection is refused with `unsupported_encryption`, never returned as ciphertext dressed as a Record (`09`).

**`encryption`** — `OpenOptions` field (`api.v1.Encryption`) carrying this handle's one secret, used for every encrypted collection it touches. When set, `passphrase` or `identity` is required. Setting it encrypts nothing by itself: what is ciphertext is each collection's declaration (SCH-16). `open` of the Vault never requires it, and plain collections work without it (`09`).

**`passphrase`** — symmetric arm of `Encryption`. Age scrypt wrapping.

**`identity`** — age X25519 identity arm of `Encryption` (`AGE-SECRET-KEY-…` file).

**Sealed value** — a Record value that must not appear in the Index. Encryption of a collection's files (`encrypted`, age armor) is `09`; the name here only marks the Index rule. Encrypted collections enter no Index.

---

## 8. Words this package does not treat as Scheda types

**Revision** — if a consumer stores a `revision` key in a file, that is the consumer's document. Git commit hashes are addresses of bytes, not that key.

**Score, rating, utility** — consumer concerns. Scheda does not persist them as first-class columns of the engine.

**Transaction, branch-as-transaction** — not Scheda operations. A git branch is not a database transaction.

**Fields, body, header, edge** — not Scheda Record concepts. They may exist inside a consumer's bytes. The Index sees only what a Projector `project`s.
