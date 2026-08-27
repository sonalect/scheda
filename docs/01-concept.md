# 01. Concept

Document 1 of the Scheda package. Version 0.29, 27 August 2026.

This is the entry point. It is written so a reader can decide whether Scheda is the right kind of store before meeting schemas, git lock details, or the library API. Technical contracts live in later documents. Terms used here are defined in `02. Glossary`.

Scheda is a **library**. The thing on disk is a **Vault**: a directory that is a git working tree, not a proprietary database file.

---

## 1. The gap

Work that matters still lands as text: notes, configs, journals, tickets exported to files, Markdown with a YAML header. Git already versions that text. A person can open a file in an editor, `diff` a field, and recover last week's bytes without a vendor tool.

The moment those files need to be **queried** — by field, by words, by meaning — the usual move is to leave git. Put the records in a document database. Put the prose in a vector database. Put the schema in application code. The files either disappear or become an export. The index becomes the thing people trust.

That split is expensive.

- The human and the AI agent no longer share one tree. The agent searches a store the human cannot `git log`. The human edits files the index does not see until someone remembers to sync.
- Backup, review, and merge become product features instead of `git`.
- A crash or a bad migration can lose meaning that still existed as files five minutes earlier — or the opposite: files remain, but nobody treats them as truth anymore.

The other usual move is a **product**: a second brain, a wiki, a knowledge app. Those can be excellent. They are not a library another program calls on its hot path. They bring users, a UI, and someone else's model of notes.

Scheda exists for the case in between: **the files stay the source of truth**, git stays the history, and a library in the caller's process gives structured access and search without inventing a second store.

---

## 2. Challenges

### Challenge 1. Search must not become truth

An index that is fast to query will always be more convenient than walking Markdown. If the API lets callers treat that index as the database — SQL over a binary file, updates that never hit the working tree — the files rot. After that, deleting the index is data loss.

The store has to make the opposite accident cheap: wipe the Index, open the Vault with another copy of the library, rebuild, and get the same records back.

### Challenge 2. A person must be able to ignore the library

If the only honest way to read a record is through Scheda, the Vault is already a database with extra steps. A file must be UTF-8 text a person can open, grep, and commit. `git diff` on a field change must show the field, not a binary snapshot.

The library is the disciplined path for programs and agents. It is not a gate in front of the tree.

### Challenge 3. The engine must not know the consumer's types

A library that names the caller's collections in source is a framework, not a store. The next consumer then forks the library or lives with the wrong nouns.

Collections, field names, what is searchable, and what is a link are **data in the Vault**. Changing them is a commit. Grep over an implementation must not find the first consumer's type names.

### Challenge 4. Git is a history tool, not a row lock

Commits can publish a consistent tree. Branches are not database transactions. Two writers on one working tree produce conflict markers a person must resolve. A journal is append-only lines across segments, not a row store: git has no lock on a single line.

Scheda does not invent a merge engine or a multi-master story to paper over that. One writer holds the Vault. Commit is an explicit call, not a side effect of every insert. Conflicts on a document file are git's, shown to a human.

### Challenge 5. A small surface is enough

Callers should not learn a new dialect of “write these bytes at this path”. `add`, `get`, `replace`, `find`, `search`, and `commit` are enough. The Index may use an embedded database internally; that is not the caller API.

---

## 3. The answer

A **Vault** is a git working tree. Inside it, the consumer declares **collections**. Each collection has one write mode:

| Kind | On disk | Write |
|---|---|---|
| `document` | one file per Record (YAML header plus optional Markdown body, or YAML throughout) | replace the file |
| `journal` | directory of JSONL segments; one object per line | append to the open segment; rotate when Schema says |
| `singleton` | one JSON or YAML file | replace the file |

A **Record** has a stable `id` in its collection. The library places the file (MOD-8). The caller never supplies a Record path; the only filesystem path they pass is the Vault root (`open`). The `id` is not “whatever the file is currently named”.

**Schema** sits in the Vault next to the records: which collections exist, which fields are required, which fields and which body participate in word search or vectors, which fields are edges to another collection. There is no executable code in the schema.

**Index** is derived, lives outside git, is not part of a Vault backup, and can be deleted. Rebuild reads files. Search that needs the Index fails closed if the Index is junk: empty or error, not stale ids from a corrupt file.

**Scheda** is a library in the caller's process. The first implementation is the Rust crate `scheda`; others (Python, Java, Go, TypeScript, and more) may follow. Every implementation must speak the same Vault layout and the same operations. Shared message fields live in `api/v1` (Protobuf edition 2024); Vault files stay text. A Scheda daemon or HTTP server is not required to read and write. Opening a Vault, writing, and lexical search do not call the network unless `auto_sync` or an explicit remote operation is on. An embedder, if any, is supplied by the caller; without it, vector search is off and the rest still works.

A collection may be **isolated** (**later**, `09`): Record files under `{vault}/.isolated/` (gitignored nested git, never a Remote). Schema stays in the main tree. `OpenOptions.encrypt` (default off) applies when that nested tree is first created. Encrypted isolated files are [age](https://age-encryption.org/) ASCII armor; isolated I/O takes one `secret` (passphrase or age identity). The main Vault is not encrypted.

An **AI agent** does not get a second, looser API. It gets the same operations, plus a catalog taken from schema so it does not guess paths, and `create_collection` so it does not write Schema YAML by hand. A person may still edit files in an editor; that path is not offered to the agent as “write any Markdown anywhere”.

The library does not score records, does not decide what to show a human, does not call a language model, and does not know a profession. Those belong to the consumer.

---

## 4. What this is not

**Not a general-purpose database you adopt instead of git.** History, remotes, and conflict markers stay git's. Scheda writes the working tree and, unless a transaction is open, commits each mutating call (message `scheda`). `open` reports the Remote from local git config (`VaultInfo.remote` is `origin` or empty). With `auto_sync`, open fast-forward-pulls and a local commit also `push`. Explicit `sync` / `push` / `pull` always exist. Clone uses a URL instead of `git init`.

**Not a second brain, wiki, or knowledge product.** No users, roles, or web UI in the library. No “tools for thought” model of pages.

**Not a vector database as source of truth.** Vectors live in the Index. Embedding weights live in the consumer's install, not in the Vault.

**Not a replacement for git.** There is no Scheda-specific versioning of records. A field such as `revision` on a document is the consumer's schema, not a substitute for `git log`.

**Not a consumer's domain model.** Collection names, scores, and product protocol belong to the caller. Mapping that caller's records onto `document` / `journal` / `singleton` is that consumer's schema, not a Scheda feature.

Ready-made experiments that keep Markdown under git and hang a search index on the side are useful as prior art. They are not an acceptable implementation of this package if they are not a library a program can call on its hot path, if they bake in a foreign knowledge model, or if their index is not a disposable layer over files.

---

## 5. What works on day one

A store that only searches is a demo. A store that cannot be opened as files is a database. Day one is the core: files are truth, the library writes them safely, one writer, autocommit or `begin`/`commit`/`discard`, `get` by id, `find` (`journal` in append order; other kinds by Tantivy score). Isolated collections under `{vault}/.isolated/` and `encrypt` stay a **later** phase (`09`, `10`); they are not required to accept **core**, **search**, or **agent**.

Hybrid `search` is accepted in `scheda` (`10` §3, `11` C9). It must be **strong retrieval**, not a grep demo: ranked BM25 over Schema `fts` marks, in-process ANN (HNSW-class) over vector marks, Reciprocal Rank Fusion of both legs, injected embedder, no LLM in this library (`06`). It must not be required to insert a journal line or replace a document.

Next in `scheda` is the unencrypted isolated slice: `{vault}/.isolated/` nested git, owner-only files, no Remote (`10` §1). **agent** is accepted (`12`). Age encryption of `.isolated/` stays deferred.

What is deliberately later after those slices: optional age encryption of `{vault}/.isolated/` (`09`); a server-side index for tens of millions of records (a new Index implementation behind the same API, not a new primary layer); multi-master without the single-writer lock; CRDTs; a custom YAML merge; extracting edges from `[[wiki-links]]` unless the schema names that extractor.

---

## 6. How to read the package

| File | Contents |
|---|---|
| `01` (this document) | Why Scheda exists, what it is, what it refuses to be |
| `02` | Glossary |
| `03` | Records, ids, paths, collection kinds |
| `04` | Files, git, locks, commit |
| `05` | Schema as data |
| `06` | Disposable Index, `get` |
| `07` | Library API (language-neutral contract; first binding is Rust) |
| `08` | Agent surface |
| `09` | Isolation and perimeter |
| `10` | Phases, acceptance, what `scheda` codes next, open questions, deferrals |
| `11` | Rust stage checklist (core, then hybrid `search`; both done) |
| `12` | Rust stage checklist (**agent**; done) |
| `13` | `find` and `search` (target: Tantivy Query, hybrid BM25 + USearch, per-field vectors) |

Checkable requirements live in `03`–`09` and `13`, and are gathered with phases in `10`. This document does not list them.

---

## 7. Status

The repository currently holds this design package and the first Rust binding. In `scheda`, **core** (C0–C8, C10), **search** (C9), and **agent** (A0–A7) are accepted (`10`, `11`, `12`). Next work is the unencrypted isolated slice of **later** (`10` §1). Age encryption of `{vault}/.isolated/`, extra language bindings, and the rest of `10` §5 stay deferred. That crate is the first implementation, not the definition of the product.

The product name is Scheda. The first package name is the Rust crate `scheda`. Neither name claims compatibility with another database.
