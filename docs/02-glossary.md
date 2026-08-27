# 02. Glossary

Document 2 of the Scheda package. Version 0.41, 27 August 2026.

This document is normative. Terms defined here bind the rest of the package, the on-disk Vault, and every language implementation. If another document uses a word differently, that document is wrong.

Each article is: the term, a short gloss, the definition, and — when it matters — what not to confuse it with.

---

## Writing rules

**Product nouns are capitalized:** Vault, Collection, Record, Schema, Index, Scheda.

**Collection kinds and API names are monospace literals:** `document`, `journal`, `singleton`, `add`, `get`, `replace`, `history`, `lines`, `find`, `search`, `create_collection`, `begin`, `commit`, `discard`, `open`, `info`, `auto_sync`, `push`, `pull`, `sync`, `encrypt`, `secret`.

**Index in this package is never git's staging area.** Git's index, if mentioned, is written “git index” or “staging area”. Scheda's Index is the derived search layer.

**Language-neutral contracts.** Requirements talk about error, failure, missing, and empty. Idioms of one language (`Err`, `None`, `error` return, `throw`) appear only in a sentence that names that language.

---

## 1. Product and process

**Scheda** — the store.

A library in the caller's process: it reads and writes a Vault, rebuilds an Index, and answers queries. It is not a daemon, not a hosted database, and not a knowledge product. The first implementation is the Rust crate `scheda`. Other languages (Python, Java, Go, TypeScript, and more) may follow. Every implementation must use these terms and the same Vault layout.

Do not confuse with the Vault. Scheda is the code. The Vault is the directory.

**Implementation** — one language binding of Scheda (the Rust crate today; Python, Java, Go, TypeScript, or another package later). Implementations share the Vault and the operations. They do not each invent a file format.

**Consumer** — the program that links or imports an implementation and declares collections through Schema. The consumer owns type names, scores, and product protocol. Scheda does not.

**Caller** — the code that invoked the library in this process. Often the consumer; for the agent surface, an adapter in front of the same API.

---

## 2. Units on disk

**Vault** — a git working tree that holds collections.

The Vault root is a git repository. Records are ordinary files in that tree. A remote is optional. A person opens the Vault in an editor and in `git` without the library.

Do not confuse with a database file, with the Index, or with “a Scheda file”. There is no single-file Scheda database.

**Collection** — a named set of records with one Schema and one write mode.

The consumer declares collections. Scheda does not ship a built-in collection for a domain. Search and links do not cross Vaults unless the caller asks. The directory at the Vault root is the collection name (same rules as Record `id`).

**Record** — one member of a collection.

A document file, a journal line, or a singleton file. It has a stable `id` in its collection. The library derives the file path from kind and `id` (MOD-8). Not “whatever the file is currently named”.

**Schema** — data in the Vault that describes collections.

Which collections exist, their kind, fields, what is required, what is indexed for words or vectors, what is an edge. Record file paths are fixed by kind (MOD-8), not chosen in Schema. Schema contains no executable code. Changing Schema is a git commit like any other file.

Do not confuse with types in the library source. Consumer collection names must not appear in `rust/scheda/src`. Test data may name them.

**Working tree** — git's checkout at the Vault root. Scheda writes files here. Uncommitted writes are visible as ordinary git status.

---

## 3. Collection kinds

**Kind** — the write mode of a collection. Values: `document`, `journal`, `singleton`. A collection has exactly one kind.

**`document`** — one file per Record.

YAML header plus optional Markdown body, or YAML throughout. The body may be empty. Replace updates the file as a whole. History of the Record is git history of its file (`Collection.history(id)`), not a Scheda revision type.

**`journal`** — append-only JSONL records in a collection directory.

Each line is one Record (one JSON object, `\n`, no comments). Append only: there is no library operation to update or delete a line. On disk this is **not** one unbounded file. The collection is a directory of **segments**. Append writes to the single open segment. Schema closes that file when it hits a size cap (and optionally a line cap or calendar boundary). A closed segment is never rewritten.

Git does not use LFS in this design. Hosts commonly warn near 50 MiB and reject near 100 MiB per file; a JSONL that grows in place also makes every commit rewrite a large blob. Segments stay small enough that ordinary git remains usable. The default cap belongs in `05`; it must sit well below those host limits.

Do not confuse with `document`. Many journal files are still one collection and one `id` space.

**Segment** — one JSONL file inside a `journal` collection.

Named by Schema (sequence number, UTC date, or both). At most one segment is open for append. Closed segments are immutable, so git history adds new small files instead of a growing blob. Merging two histories that appended to the same open segment is still a line-level git merge; rotation does not make multi-writer safe.

**`singleton`** — one JSON or YAML file for the whole collection.

Replace replaces that file after Schema checks. There is not a set of ids in the usual sense; the collection *is* the file. The file must stay small for the same git reasons as a segment; a singleton is not a place to dump a journal.

---

## 4. Record shape

**`id`** — stable identifier of a Record inside its collection.

The library checks `id` as a slug: starts with a Unicode letter or digit, then letters, digits, combining marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@` (max 255 bytes; not a Windows device name; no leading/trailing whitespace; no trailing `.`). Schema may require a tighter pattern (ULID, `kebab-case`, or another). Scheda checks the pattern. It does not mint domain identifiers for the consumer. Renaming a file so that it no longer matches the derived path is rejected: `id` is not the current basename “until someone renames it”.

**Path** — location of the Record in the Vault.

For `document`, `{collection}/{id}.md`. For `journal`, the segment file that contains the line (the Record is still addressed by `(collection, id)`, not by segment name). For `singleton`, `{collection}/record.yaml`. The caller never chooses these paths.

**Fields** — structured values.

In a `document` or `singleton`, fields live in the YAML (or JSON) header or file. In a `journal`, fields are the RFC 8785 JSON object on the line (`04` STO-2). Types include `string`, `integer`, `number`, `boolean`, `string_list`, `object`, `object_list` (`05`). Missing is missing: Scheda does not insert `false`, `0`, `""`, `[]`, or `null` unless Schema states a default. A stored JSON `null` is a value, not omission (`03`).

**Body** — free text after the header, typically Markdown. Allowed to be empty. In the lexical Index (`find` field `body` and BM25) unless Schema sets `exclude_fts`; in vectors unless `exclude_vector`.

**Header** — the YAML (or JSON) envelope of a `document` that carries fields. Not used on `journal` lines.

**Edge** — a field Schema declares as a typed reference to a Record `id` in a target collection. Edges are data, not links parsed from Markdown unless Schema names that extractor.

---

## 5. Derived layer

**Index** — the disposable search layer.

Built from Vault files. Stored under `{vault}/.scheda/` (gitignored), not committed with the Vault, safe to delete. Rebuild (`reindex`) reads files. A corrupt Index must not serve stale ids: `find` / `search` return empty or an error. `get` and `history` still read files (IDX-10).

Do not confuse with git's staging area, with the Vault, or with source of truth. If the Index and the files disagree, the files win for `get` and `history`. Other methods trust the Index for which `id`s exist, then read those files.

**Lexical search** — ranked full-text (BM25 or equivalent) over `string` / `string_list` fields and body that Schema does not mark `exclude_fts`. Not grep, SQL `LIKE`, or a file scan at query time.

**Vector search** — in-process ANN nearest neighbours (HNSW or equivalent) over embeddings of fields and body that Schema marked for vectors. Embedding weights are not stored in the Vault. An external vector database is not the Index.

**Embedder** — a caller-supplied function (or equivalent) that turns marked text into a vector. Optional. Without it, vector search is off; the rest of Scheda still works. An embedder that uses the network is passed in explicitly; core open/write/lexical rebuild with `auto_sync` off do not call the network.

**Hybrid search** — one `search` call that runs the lexical and vector legs when both are available, then fuses ranks with Reciprocal Rank Fusion (default) or caller merge weights. Optional reranker callback then reorders. The result is ordered ids and scores, not a model-written answer.

**Reranker** — optional caller function on `search`: `score(query text, Record) →` number. The library sorts by it and returns `k` hits. No model in Scheda. Without it, order is RRF.

---

## 6. Access

**Library API** — the operations every implementation must provide, named in `07`. Message fields are `api/v1` (Protobuf edition 2024). Language-neutral as a contract. The first binding is Rust.

**`open`** — attach to a Vault working tree, or create one. Path exists as a Vault → open (`remote_repo` ignored). Path missing and `remote_repo` set → `git clone`. Path missing → `git init`, no Remote. Reads the Remote locally on an existing tree (no fetch unless `auto_sync`). `open` does not take the write lock. More than one Vault may be open in one process. Roots do not leak into each other's search.

**`info`** — snapshot of the open Vault as `api.v1.VaultInfo` (path, Index path, `remote`, `auto_sync`, collections). Not a live handle.

**Write lock** — the right of exactly one process to mutate a given Vault. Readers may proceed without it and may see a state slightly older than the writer.

**`add`** — add a Record: a new `document` file, a new journal line, or the first write of a `singleton` as Schema allows.

**`get`** — read one Record by `(collection, id)`. Validate the slug, then open that file. Does not ask the Index which ids exist (`06` IDX-10). Absence is an error or empty according to `07`, not a blank document pretending to exist.

**`replace`** — replace a whole `document` or `singleton`. Resolve `id` in the Index first, then write that file. Error on `journal`.

**`history`** — git history of one Record by `id`, newest first. Same identity path as `get` (validate slug, then that file). Not an Index lookup. Not journal `find` append order.

**`find`** — select records with a Tantivy query tree (`api.v1.FindQuery`: required `Filter` — boolean must / should / must_not, term, phrase, range, `all`, … — plus `include_raw`; default omits `Record.raw`), including identity `id`. On Vault: a set of collection names (empty = this Vault, non-isolated). On Collection: that collection only. Query the Index, then open the files for the `(collection, id)`s it returns. Paged (`offset`, `count`); one page is `api.v1.FindResult` (`records` plus `total`: indexed hits, not page length). One `journal` collection: append order; otherwise score order. Every indexed Record in the selected collections is `all`. Not by meaning.

**`search`** — hybrid retrieval (BM25 + ANN, RRF). Same collection-name set as `find`. Optional `Filter` pre-cuts both legs. `SearchQuery.include_raw` fills `Hit.raw`. Not the same as `find`.

**`create_collection`** — write one collection declaration (`name` plus `api/v1` `Schema`) as `{name}/.schema.yaml`. Not a raw path write. Autocommits unless a transaction is open. Until nested `{vault}/.isolated/` I/O, `schema.isolated` is `unsupported` and no file is written.

**`begin`** — start a transaction. Later writes do not create a git commit until `commit` or `discard`.

**`commit`** — end a transaction and create one git commit for paths Scheda recorded. Message comes from the caller. Outside a transaction, mutating calls autocommit with message `scheda`.

**`discard`** — restore Scheda-recorded paths to HEAD, end the transaction if any, reload Schema, rebuild the Index.

**Remote** — the one optional git remote Scheda uses on the Vault repository, named `origin`. Empty `VaultInfo.remote` means none. Extra remotes in git config are ignored. Detected on `open` from git config (no fetch). GitHub is one host, not a Scheda type.

**`auto_sync`** — option on `open`, default off. When on and a Remote exists: fast-forward `pull` on open, and `push` after a local commit (including autocommit). Not a merge. Details in `04`.

**`pull`** — fetch and fast-forward only. Uses the network.

**`push`** — send local commits to a Remote. Uses the network. Mutating calls inside a transaction never push.

**`sync`** — `pull` then `push`. Independent of `auto_sync`.

**`reindex`** — rebuild the Index from files.

**`check`** — validate the Vault against Schema without necessarily writing.

**Agent surface** — the same operations as the library API, over a thin transport (CLI, MCP, or other). Schema listing is `info`. The transport contains no product rules.

---

## 7. Isolation (**later**)

**Isolated collection** — a collection whose Record files live under `{vault}/.isolated/{name}/`: a gitignored nested git working tree, never the main Remote. Schema stays `{vault}/{name}/.schema.yaml` in the main tree. Search over non-isolated collections does not read it and must not require `.isolated/` to exist. `push` does not publish it. Optional `encrypt` (`09`).

**Isolated tree** — `{vault}/.isolated/`. Local `commit` and `Collection.history` of isolated Records only. Optional `encrypt`: Record files are age armor. The main Index must not copy its sealed field values.

**`encrypt`** — `OpenOptions` flag used when Scheda first creates `{vault}/.isolated/`. Default false. When true, isolated Record I/O needs `secret` (`09`). Main `open` does not.

**`secret`** — passphrase or age identity for encrypted `.isolated/`. Never stored in that nested git or the main tree.

**Sealed field** — a field that must not appear in the main Index. Isolated encryption (`encrypt`, age armor) is `09`; the name here only marks the Index rule.

---

## 8. Words this package does not treat as Scheda types

**Revision** — if a consumer stores a `revision` field, that is Schema. Git commit hashes are addresses of bytes, not that field.

**Score, rating, utility** — consumer concerns. Scheda does not persist them as first-class columns of the engine.

**Transaction, branch-as-transaction** — not Scheda operations. A git branch is not a database transaction.
