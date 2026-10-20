# 01. Concept

Document 1 of the Scheda package. Version 0.53, 20 October 2026.

This is the entry point. It is written so a reader can decide whether Scheda is the right kind of store before meeting collection declarations, git lock details, or the library API. Technical contracts live in later documents. Terms used here are defined in `02. Glossary`.

Scheda is a **library**. The thing on disk is a **Vault**: a directory that is a git working tree, not a proprietary database file.

---

## 1. The gap

Work that matters still lands as text: notes, configs, journals, tickets exported to files, Markdown, YAML, TOML. Git already versions that text. A person can open a file in an editor, `diff` the change, and recover last week's bytes without a vendor tool.

The moment those files need to be **queried** — by field, by words, by meaning — the usual move is to leave git. Put the records in a document database. Put the prose in a vector database. Put the schema in application code. The files either disappear or become an export. The index becomes the thing people trust.

That split is expensive.

- The human and the AI agent no longer share one tree. The agent searches a store the human cannot `git log`. The human edits files the index does not see until someone remembers to sync.
- Backup, review, and merge become product features instead of `git`.
- A crash or a bad migration can lose meaning that still existed as files five minutes earlier — or the opposite: files remain, but nobody treats them as truth anymore.

The other usual move is a **product**: a second brain, a wiki, a knowledge app. Those can be excellent. They are not a library another program calls on its hot path. They bring users, a UI, and someone else's model of notes.

Scheda exists for the case in between: **the files stay the source of truth**, git stays the history, and a library in the caller's process gives access and search without inventing a second store, and without owning the format of those files.

---

## 2. Challenges

### Challenge 1. Search must not become truth

An index that is fast to query will always be more convenient than walking files. If the API lets callers treat that index as the database — SQL over a binary file, updates that never hit the working tree — the files rot. After that, deleting the index is data loss.

The store has to make the opposite accident cheap: wipe the Index, open the Vault with another copy of the library, rebuild, and get the same Documents back.

### Challenge 2. A person must be able to ignore the library

If the only honest way to read a Document is through Scheda, the Vault is already a database with extra steps. A Document is a file in the tree, at a path the layout fixes (`03`), that a person can open, grep, and commit. Scheda keeps its bytes as they were given; for text, `git diff` on a change shows that text, not a binary snapshot.

A Document is bytes: text, an image, protobuf wire. Scheda does not ask for UTF-8, since git keeps any file (`20` D20). Encrypted collections are a separate mark: whole-file age armor of the bytes (`09`).

The library is the disciplined path for programs and agents. It is not a gate in front of the tree.

### Challenge 3. The engine must not know the consumer's types

A library that names the caller's collections in source is a framework, not a store. The next consumer then forks the library or lives with the wrong nouns.

The engine also must not own the consumer's document shape. Document bytes are opaque: YAML, Markdown, TOML, prototext, or a mix in one collection. Field names, types, and which columns enter the queries and searches live in the consumer — in code, as a `Projector` port given to the Vault, not as a field list in the Vault. Grep over library source (`rust/scheda/src`) must not find the first consumer's type names. Test data is not that grep.

What **does** sit in the Vault, as data next to the Documents, is the **collection declaration**: name, kind (write mode), isolation, encryption, journal caps. Changing those is a commit. `Scheda.Open` discovers collections from those files. That is not a schema of Document contents.

### Challenge 4. Git is a history tool, not a row lock

Commits can publish a consistent tree. Branches are not database transactions. Two writers on one working tree produce conflict markers a person must resolve. A journal is append-only lines across segments, not a row store: git has no lock on a single line.

Scheda does not invent a merge engine or a multi-master story to paper over that. Writers on one Vault take turns on a short lock; each write call is one commit, and several writes sent as one batch are one commit. No call opens a transaction the caller holds while it thinks. When two clones changed one Document differently, `Mirror.Pull` fails with `MIRROR_CONFLICT` and changes nothing, for a person to resolve.

### Challenge 5. A small surface is enough

Callers should not learn a dialect of “write these bytes at this filesystem path”. `Create`, `Get`, `Update`, `Upsert`, `Delete`, `Query`, and `Search` on a collection are enough. The caller supplies the Document `id` and bytes; the library places the file. The Index may use an embedded database internally; that is not the caller API.

---

## 3. The answer

A **Vault** is a git working tree. Inside it, the consumer declares **collections**. Each collection has one write mode. Kind is how Scheda writes, not a codec for contents:

| Kind | On disk | Write |
|---|---|---|
| `standard` | one file per Document; path is the `id` (extension, if any, is part of `id`) | `Update` and `Upsert` replace the file; `Delete` removes it |
| `journal` | directory of segment files; one Document per line | `Create` appends to the open segment; rotate when the declaration says; no `Delete` |
| `singleton` | one file, named by the declaration | `Update` and `Upsert` replace the file; `Delete` removes it |

A **Document** is `(collection, id)` plus bytes. The library places the file. The caller never supplies a Document path; the only filesystem path they pass is the Vault root (`Scheda.Open`). Format and extension are the caller's. Scheda does not parse the bytes. For `standard` and `singleton`, renaming the file is changing `id`.

A **collection declaration** sits in the Vault next to the Documents: which collections exist, their kind, isolation, encryption, journal caps. There is no field list, no required keys inside the Document, and no executable code in the declaration.

**Index** is derived, lives outside git, is not part of a Vault backup, and can be deleted. Rebuild reads the commits. Columns beyond identity come from a caller-supplied **Projector** (same injection idea as an embedder): not Vault data, not a field of the contract. A query or search that needs the Index fails closed if the Index is junk: an error, not stale ids from a corrupt file. Without a Projector, writes and `Collection.Get` still work; `Query` by field and content `Search` do not.

**Scheda** is a library in the caller's process. The first implementation is the Rust crate `scheda`; others (Python, Java, Go, TypeScript, and more) may follow. Every implementation must speak the same Vault layout and the same operations. The contract is the protos under `api/scheda/v1/` (package `scheda.v1`, Protobuf edition 2024); Vault Document files are the bytes the caller gave. A Scheda daemon or HTTP server is not required to read and write. Opening a Vault, writing, and lexical search do not call the network unless `auto_sync` or an explicit `Mirror` operation is on. An embedder, if any, is supplied by the caller; without it, vector search is off and the rest still works.

A collection may be **isolated** (`09`): Document files under `{vault}/.isolated/` (a gitignored nested git that never leaves the machine: no `Mirror` call contacts it). It is for what may be lost or rebuilt; what is private and must survive belongs in an encrypted collection. The collection declaration stays in the main tree. Separately, a collection may be **encrypted**: the declaration sets `encrypted: true`, its Document files are [age](https://age-encryption.org/) ASCII armor, and I/O on it takes the Vault's `passphrase` or `identity` (`OpenOptions.encryption`). The two marks are independent — ciphertext is worth having for material a consumer fully intends to publish to the mirror, and a merely local collection has no need of it — so all four combinations are valid. Only the collections that declare it are encrypted; the rest of the Vault stays plaintext.

An **AI agent** does not get a second, looser API. It gets the same operations, plus `Vault.ListCollections` taken from collection declarations so it does not guess paths, and `Vault.CreateCollection` so it does not write the declaration file by hand. The listing shows collections and write-mode marks, not Document fields. A person may still edit files in an editor; that path is not offered to the agent as “write any file at any path”.

The library does not score Documents, does not decide what to show a human, does not call a language model, and does not know a profession. Those belong to the consumer.

---

## 4. What this is not

**Not a general-purpose database you adopt instead of git.** History, remotes, and conflict markers stay git's. Scheda makes one git commit for each write call (or batch) and never exposes git's commit, branch, or transaction in the API; the caller gives a `note` that history shows. `Mirror.Info` reports the mirror from local git config. With `auto_sync`, `Scheda.Open` pulls from the mirror and a write also pushes. `Mirror.Pull`, `Push`, and `Sync` always exist. `Scheda.Open` with `OpenOptions.mirror` on a missing path clones instead of making a new local Vault.

**Not a second brain, wiki, or knowledge product.** No users, roles, or web UI in the library. No “tools for thought” model of pages.

**Not a vector database as source of truth.** Vectors live in the Index. Embedding weights live in the consumer's install, not in the Vault.

**Not a replacement for git.** There is no Scheda-specific versioning of Documents. `Document.revision` is an opaque token (the id of the stored blob) for a write that must find the Document unchanged; a field such as `revision` inside a consumer's file is that consumer's document, not a substitute for `git log`.

**Not a consumer's domain model.** Collection names, scores, product protocol, and the bytes of each Document belong to the caller. Mapping those bytes onto `standard` / `journal` / `singleton` is a choice of write mode, not a Scheda content schema.

Ready-made experiments that keep text files under git and hang a search index on the side are useful as prior art. They are not an acceptable implementation of this package if they are not a library a program can call on its hot path, if they bake in a foreign knowledge model or a single file format, or if their index is not a disposable layer over files.

---

## 5. What works on day one

A store that only searches is a demo. A store that cannot be opened as files is a database. Day one is the core: files are truth, Documents are bytes, the library writes them safely, writers take turns, each write call or batch is one commit, `Get` by id, `Query` (`journal` in append order; other kinds in `id` order). Isolated collections under `{vault}/.isolated/` and encrypted collections stay a **later** phase (`09`, `10`); neither is required to accept **core**, **search**, or **agent**.

The crate `scheda` accepted **core**, **search**, and **agent** first on YAML/Markdown files and field Schema. Opaque Records (`16` F0–F19) came next: collection declaration without a field list, Projector, mixed-format acceptance fixture. The proto contract (`20`, version 0.22) then replaced the API; a Document became bytes of any kind. The on-disk store and `api/scheda/v1` follow `03`–`09` as they read today.

Hybrid `Search` is accepted in `scheda` (`10` §3, `11` C9). It must be **strong retrieval**, not a grep demo: ranked BM25, in-process ANN (HNSW-class), Reciprocal Rank Fusion of both legs, injected embedder, no LLM in this library (`06`). After `16`, BM25 and vector slots come from the Projector, not from field marks in a Schema file. It must not be required to create a journal line or replace a Document.

Age encryption as a property of a collection, behind the opt-in `encrypt` feature, shipped as `0.6.0` (`09`, `12` P14). After it, one Index for the Vault (`12` P15): isolated collections became searchable by name, the nested Index went away, and `{vault}/.scheda/` became owner-only because it now holds their indexed values. A CLI that can carry a secret shipped as `12` P16; the contract of `20` ended the Remote of the nested isolated tree (`09` ISO-6). Consumer readiness P0–P16 is in (`12` §4). Isolated I/O (`{vault}/.isolated/` nested git, owner-only files) is in. The slug is the whole `id` rule. The queries and `Search` are opt-in crate features (`find`, `search`); the crate has no default features. **agent** is accepted (`12`).

What is deliberately later after those slices: field-level encryption inside an otherwise plaintext Document, and an encrypted `journal` — both refused on purpose (`09`, SCH-16); a server-side index for tens of millions of Documents (a new Index implementation behind the same API, not a new primary layer); multi-master writes without one history; CRDTs; a custom merge of a consumer's format.

---

## 6. How to read the package

| File | Contents |
|---|---|
| `01` (this document) | Why Scheda exists, what it is, what it refuses to be |
| `02` | Glossary |
| `03` | Documents, ids, paths, collection kinds |
| `04` | Files, git, locks, writes |
| `05` | Collection declaration |
| `06` | Disposable Index, `Collection.Get` |
| `07` | Library API: what spans methods (the per-method contract is the proto comments in `api/scheda/v1/`) |
| `08` | Agent surface |
| `09` | Isolation and perimeter |
| `10` | Phases, acceptance, what `scheda` codes next, open questions, deferrals |
| `11` | Rust stage checklist (core, then hybrid `search`; both done) |
| `12` | Rust stage checklist (**agent**; done) |
| `13` | `Query` and `Search` (Tantivy query, hybrid BM25 + USearch, Projector columns) |
| `14` | CLI |
| `15` | Online reindex |
| `16` | Opaque Records (work order; F0–F19 done) |
| `20` | The proto contract (decisions D1–D24 and the stages of the 0.22 API) |

Checkable requirements live in `03`–`09` and `13`, and are gathered with phases in `10`. This document does not list them.

---

## 7. Status

The repository currently holds this design package and the first Rust binding. In `scheda`, **core** (C0–C8, C10), **search** (C9), and **agent** (A0–A7) are accepted (`10`, `11`, `12`). Isolated I/O is in (`12` P4). The slug is the only `id` rule (`12` P5). `find` and `search` are opt-in crate features (`12` P8); the crate has no default features. Consumer readiness P0–P16 is in (`12` §4). Age encryption of a collection shipped as `0.6.0` (`12` P14), one Index for the Vault is in (`12` P15), and a secret in the CLI shipped as `12` P16. Extra language bindings and the rest of `10` §5 stay deferred. Opaque Records (`16` F0–F19) are in, and the proto contract of `20` (0.22, Vault generation 2; generation 3 replaced the two Index flags by `index_scope`) is the API. That crate is the first implementation, not the definition of the product.

The product name is Scheda. The first package name is the Rust crate `scheda`. Neither name claims compatibility with another database.
