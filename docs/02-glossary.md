# 02. Glossary

Document 2 of the Scheda package. Version 0.11, 23 August 2026.

This document is normative. Terms defined here bind the rest of the package, the on-disk Vault, and every language implementation. If another document uses a word differently, that document is wrong.

Each article is: the term, a short gloss, the definition, and — when it matters — what not to confuse it with.

---

## Writing rules

**Product nouns are capitalized:** Vault, Collection, Record, Schema, Index, Scheda.

**Collection kinds and API names are monospace literals:** `document`, `journal`, `singleton`, `insert_one`, `find_one`, `replace_one`, `find`, `scan`, `search`, `commit`, `open`, `init`, `clone`, `auto_sync`, `push_remote`, `pull_remote`, `sync_remote`, `encrypt`, `secret`.

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

The consumer declares collections. Scheda does not ship a built-in collection for a domain. Search and links do not cross Vaults unless the caller asks.

**Record** — one member of a collection.

A document file, a journal line, or a singleton file. It has a stable `id` in its collection and a path derived from Schema, not “whatever the file is currently named”.

**Schema** — data in the Vault that describes collections.

Which collections exist, their kind, path templates, fields, what is required, what is indexed for words or vectors, what is an edge. Schema contains no executable code. Changing Schema is a git commit like any other file.

Do not confuse with types in the library source. Consumer collection names must not appear in Scheda code.

**Working tree** — git's checkout at the Vault root. Scheda writes files here. Uncommitted writes are visible as ordinary git status.

---

## 3. Collection kinds

**Kind** — the write mode of a collection. Values: `document`, `journal`, `singleton`. A collection has exactly one kind.

**`document`** — one file per Record.

YAML header plus optional Markdown body, or YAML throughout. The body may be empty. Replace updates the file as a whole. History of the path is git history, not a Scheda revision type.

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

Schema may require a pattern (ULID, `kebab-case`, or another). Scheda checks the pattern. It does not mint domain identifiers for the consumer. Renaming a file so that it no longer matches the derived path is rejected: `id` is not the current basename “until someone renames it”.

**Path** — location of the Record in the Vault.

For `document`, derived from the FNV-1a-64 collection directory plus the layout template plus `id`. For `journal`, the segment file that contains the line (the Record is still addressed by `(collection, id)`, not by segment name). For `singleton`, the single file named by Schema, under the same collection directory.

**Fields** — structured values.

In a `document` or `singleton`, fields live in the YAML (or JSON) header or file. In a `journal`, fields are the JSON object on the line. Types include `string`, `integer`, `number`, `boolean`, `string_list`, `object`, `object_list` (`05`). Missing is missing: Scheda does not insert `false`, `0`, `""`, `[]`, or `null` unless Schema states a default. A stored JSON `null` is a value, not omission (`03`).

**`scan`** — `find` with an empty filter: every Record in the collection. On `journal`, order is append order (closed segments in sequence, then the open file, lines top to bottom).

**Body** — free text after the header, typically Markdown. Allowed to be empty. Indexed only if Schema marks the body for search.

**Header** — the YAML (or JSON) envelope of a `document` that carries fields. Not used on `journal` lines.

**Edge** — a field Schema declares as a typed reference to a Record `id` in a target collection. Edges are data, not links parsed from Markdown unless Schema names that extractor.

---

## 5. Derived layer

**Index** — the disposable search layer.

Built from Vault files. Stored under `{vault}/.scheda/` (gitignored), not committed with the Vault, safe to delete. Rebuild (`reindex`) reads files. A corrupt Index must not serve stale ids: search returns empty or an error.

Do not confuse with git's staging area, with the Vault, or with source of truth. If the Index and the files disagree, the files win.

**Lexical search** — full-text search over fields and body that Schema marked for words.

**Vector search** — nearest-neighbour search over embeddings of fields and body that Schema marked for vectors. Embedding weights are not stored in the Vault.

**Embedder** — a caller-supplied function (or equivalent) that turns marked text into a vector. Optional. Without it, vector search is off; the rest of Scheda still works. An embedder that uses the network is passed in explicitly; core open/write/lexical rebuild with `auto_sync` off do not call the network.

**Hybrid search** — one `search` call that may combine lexical hits, vector hits, and (when declared) edges, then merge ranks. Weights are a request or Vault parameter, not constants in library source. The result is ordered ids and scores, not a model-written answer.

**Cross-encoder / pair reranker** — out of Scheda. The library returns candidates and raw leg scores. The consumer may rerank.

---

## 6. Access

**Library API** — the operations every implementation must provide, named in `07`. Message fields are `api/v1` (Protobuf edition 2024). Language-neutral as a contract. The first binding is Rust.

**`open`** — attach to an existing Vault working tree. Reads remotes locally (no fetch). Takes the write lock if opened for write. More than one Vault may be open in one process. Roots do not leak into each other's search.

**Write lock** — the right of exactly one process to mutate a given Vault. Readers may proceed without it and may see a state slightly older than the writer.

**`insert_one`** — add a Record: a new `document` file, a new journal line, or the first write of a `singleton` as Schema allows.

**`find_one`** — read one Record by `(collection, id)`. Absence is an error or empty according to `07`, not a blank document pretending to exist.

**`replace_one`** — replace a whole `document` or `singleton`. Error on `journal`.

**`find`** — select records by field filters (equality, set, existence, path prefix), not by meaning.

**`search`** — hybrid retrieval. Not the same as `find`.

**`init`** — create a new git repository at a path, no Remote.

**`clone`** — `git clone` from a URL into a path (network). Not `git init`.

**`commit`** — create one git commit for changes already in the working tree. Message comes from the caller. Inserts do not commit by themselves.

**Remote** — a git remote on the Vault repository (`origin` or another). Optional. Detected on `open` from git config. GitHub is one host, not a Scheda type.

**`auto_sync`** — option on `open` / `clone`, default off. When on and a Remote exists: fast-forward `pull_remote` on open, and `push_remote` after `commit`. Not a merge. Details in `04`.

**`pull_remote`** — fetch and fast-forward only. Uses the network.

**`push_remote`** — send local commits to a Remote. Uses the network. `insert_one` / `replace_one` never push.

**`sync_remote`** — `pull_remote` then `push_remote`. Independent of `auto_sync`.

**`reindex`** — rebuild the Index from files.

**`check`** — validate the Vault against Schema without necessarily writing.

**Catalog** — Schema exported so an agent (or the caller) lists collections, fields, index marks, and edges without guessing from the directory tree.

**Agent surface** — the same operations as the library API, plus catalog, over a thin transport (CLI, MCP, or other). The transport contains no product rules.

---

## 7. Isolation (**later**)

**Isolated collection** — a collection whose files live in another **git** root, not in the main Vault working tree, with tighter file permissions. Search over the main Vault does not read it and must not require it to exist. That root has no Remote; `push_remote` is forbidden. Optional `encrypt` (`09`).

**Isolated root** — that other git working tree. Local `commit` and `history` only. Optional `encrypt`: Record files are age armor. The main Index must not copy its sealed field values.

**`encrypt`** — `init` flag on an isolated root. Default false. When true, `open` needs `secret` (`09`).

**`secret`** — passphrase or age identity for an encrypted isolated root. Never stored in that git tree.

**Sealed field** — a field that must not appear in the main Index. Isolated encryption (`encrypt`, age armor) is `09`; the name here only marks the Index rule.

---

## 8. Words this package does not treat as Scheda types

**Revision** — if a consumer stores a `revision` field, that is Schema. Git commit hashes are addresses of bytes, not that field.

**Score, rating, utility** — consumer concerns. Scheda does not persist them as first-class columns of the engine.

**Transaction, branch-as-transaction** — not Scheda operations. A git branch is not a database transaction.
