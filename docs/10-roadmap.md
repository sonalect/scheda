# 10. Phases, acceptance, and deferrals

Document 10 of the Scheda package. Version 0.57, 20 October 2026.

This document does not add product rules. It assigns the checkable requirements in `03`–`09` to a phase, states when a binding is accepted, lists size targets, open questions, and what is deliberately out. It also records which phases the first binding has passed and what it codes next. Terms are in `02`. Codes: `OPS`.

An assertion without a check is not a requirement.

---

## 1. Phases

Four labels appear on requirements:

| Phase | Meaning |
|---|---|
| **core** | Files are truth, writes are safe, one commit per write call, `Collection.Get` / `Collection.Query`. No network unless opted in. A consumer can store and retrieve Documents with no embedder, no agent transport, and no isolated collections. |
| **search** | Hybrid `Collection.Search` that an agent can recall from: ranked BM25, in-process ANN, RRF fusion, Projector columns only. Must not be required to `Collection.Create` a journal line or `Collection.Update` a Document. Grep is not this phase. |
| **agent** | Collection listing (`Vault.ListCollections`) and a thin encoding of the same API (`08`). Does not block **core**. |
| **later** | Isolated collections and their encryption, scale, extra writers, extra walks, conveniences. Must not leak into **core** as hidden dependencies. |

A binding may ship **search** before **agent**. Isolated collections are **later**: they must not be required to pass **core**, **search**, or **agent**. Nested `{vault}/.isolated/` I/O is not a gate of **agent**.

The first implementation is the Rust crate `scheda`. Other languages (Python, Java, Go, TypeScript, and others) are separate bindings of the same Vault layout and the same calls (`07`). They are accepted on their own after **core** (at least) is true of that binding. They are not a prerequisite for accepting Rust **core**.

**First binding (`scheda`).** **core** is accepted (`11` C10). **search** is accepted (`11` C9). **agent** is accepted (`12` A7). No other language binding exists yet. Stage ticks and *Check:* tests live in `11`, `12`, `rust/scheda/tests/`, and `rust/cli/tests/`.

The journal `id` map is **core** (IDX-5). After it exists, `Collection.Get` on a `journal` must not scan unrelated segments (`03` §6). `scheda` has that map.

The collection declaration parses `isolated: true` (SCH-15). `Vault.ListCollections` lists those collections as isolated and has no field list (ISO-3). Queries and searches of the Vault skip them unless they are named. `Vault.GetCollection` and `Vault.CreateCollection` with `isolated` write and read `{vault}/.isolated/` (ISO-2, ISO-4, ISO-6).

**Next in `scheda`.** The 0.22 contract (`20`) is built: the whole API is protobuf services in `api/scheda/v1`, reached through the objects `Scheda`, `Vault`, `Collection`, `Mirror`, and `Index`, with a sync and an async form. Consumer readiness P0–P16 (`12` §4) and opaque documents (`16`) came before it; age encryption (`12` P14) is a declaration key on any one collection behind the `encrypt` feature; the Index follows `HEAD` in a background task (`17` Part B, `20` D23), so writes do not wait for it, queries are eventually consistent (`Index.Status`, `Index.Watch`), and a damaged or old Index rebuilds itself. The CLI is one subcommand per rpc (`14`). Extra language bindings stay deferred.

**First consumer.** A first consumer is moving a primary layer of text files under git onto the crate. The P checklist in `12` §4 is in; its stages are not a phase, and their codes cross **core**, **search**, and **later**. Two things they decided still hold: the slug is the whole `id` rule, and the Index is a crate feature (`index`, on by default), so a consumer that wants none builds without it. The consumer's own move to 0.22 is its repository's work (the list above).

**Deferred** (decided out of the 0.22 contract, `20` §6; none blocks a phase):

- A remote transport (connect-rust). The services allow it later; a port would then be a service the caller serves, or a bidirectional stream.
- An interactive transaction, and a read of one's own writes before the call returns (`20` D6).
- A priority among writers (`20` D6, "Order").
- Signing commits with the key `OpenOptions.signing` names (`20` D22).
- `Listen`, a watch on changes (`20` D7).
- A `bytes_type` option in `protoc-gen-buffa` (`20` D10); `bytes` stays `Vec<u8>`.
- The anchor consumer's move to `0.22.0` (its call sites, error mapping, and ports change), and moving its Vaults to generation 2 (`20` D1), by hand, in its own repository after the release.
- A file watcher (the indexer polls `HEAD`; an uncommitted edit is not indexed); journal line delete; REL-4; extra language bindings.

---

## 2. Core

**In:**

- Model: MOD-1–18
- Files and git: STO-1–5, GIT-1–13
- Collection declaration: SCH-1–8, SCH-12–14 (SCH-7 body marks are retired; Projector columns are **search**)
- Index needed for truth and lookup: IDX-1, IDX-2, IDX-5
- Point read and filter: QRY-1, QRY-2, QRY-7
- Library: the rpcs of `Scheda`, `Vault`, `Collection`, and `Mirror` in `api/scheda/v1` (`07`), and the cross-method rules of `07` (isolated/`encryption` clauses on those calls are **later**)
- Perimeter: ISO-1 only
- Size: OPS-1, OPS-3

IDX-2 stays **core** for the Index format and for the calls that take `id`s from the Index (`Collection.Query`, `Collection.Search`); `Collection.Update` and `Collection.Delete` use the Document file. Reading a journal in order is not one of those calls: `Collection.GetMany` walks a collection's files without the Index. QRY-7 remains the order of a query when the Index is present. The crate's default feature is `index`, the Index on Lance: queries, counts, searches, and the ports. `encrypt` is opt-in; Bazel CI links both. A crate without `index` is the same library with the Index off, not a second product: the calls that need it are `UNIMPLEMENTED`, and the journal `id` maps are still kept. `encrypt` degrades further and the contract says so: without it an encrypted collection cannot be read at all, so the handle refuses it with `UNIMPLEMENTED` and `Vault.Check` reports it, rather than returning ciphertext as a Document or reporting health it never verified (SCH-16, ISO-5). Phases grade a build with `index` on.

**Out of core** (do not block **core** acceptance; a binding may leave them as stubs or `UNIMPLEMENTED` until **search** / **agent** / **later**, and writes must not depend on them): the Index and query rpcs, IDX-3, IDX-4, IDX-6–9, QRY-3–6, REL-1–3, SCH-9–11, SCH-15, AGT-1–5, ISO-2–11. In `scheda`, the **search** and **agent** codes in that list are implemented (C9, A7), not stubs. SCH-15 parse, ISO-3 listing, and the unencrypted isolated slice (ISO-2 nested git, IDX-4) are in (`12` P4), with ISO-4 and IDX-4 in their P15 form: one owner-only Index per Vault, ISO-10 retired. ISO-5, ISO-7–9, and ISO-11 are in with encryption (`12` P14). ISO-6 is rewritten (`20` D14): isolated collections never leave the machine.

**Core is accepted** when:

1. Every **core** code above has a passing check in the first binding.
2. A fixture Vault is readable without the library (STO-2) and through `Collection.Get` (QRY-1).
3. Deleting the Index does not lose Documents (STO-1, IDX-1).
4. Library source (`rust/scheda/src`) does not name a consumer's collections (SCH-1, MOD-1). Test data is not that grep.
5. `auto_sync` off: writes succeed with the network disabled (GIT-8, ISO-1).

**Rust `scheda`:** those five gates pass (`11` C10; tests in `rust/scheda/tests/` and the C0–C8 suites). **core** is accepted for this binding.

Order used in `scheda` (not a second spec): working tree + lock → collection declaration + `standard` / `singleton` → `journal` segments → write and mirror → journal `id` map → hybrid search → core acceptance → agent → unencrypted isolated nested git. Mirror operations are **core** but landed after the local write. The stage checklist is `11`; it does not add requirements. **search** codes stay out of this section; `11` ran C9 before C10.

---

## 3. Search

**In:** IDX-3, IDX-6–9, QRY-3–6, OPS-2, OPS-4, and the search rpcs (`Collection.Search`, `Vault.SearchDocuments`). SCH-9–11 and REL-* were field-Schema rules; Projector column marks replace them.

Quality class is `06` §3–4: BM25 (or equivalent ranked FTS), in-process ANN (HNSW or equivalent) at OPS-1, RRF hybrid when both legs are on, injected `Embedder`, no LLM or ranking model in the library (an optional caller `Reranker` on a search is `13`). Substring search and brute-force vector scan at OPS-1 do not accept this phase.

**Search is accepted** when those codes pass and:

1. `Collection.Create` / `Collection.Update` / `Collection.Get` still work with no embedder (IDX-8).
2. Columns without `fts` / `vector` marks are absent from the corresponding search leg (Projector `Columns`).
3. Corrupt Index fails closed (QRY-4).
4. Edge walks (REL-*) are retired with field Schema; search acceptance does not require them.
5. Token, synonym, and hybrid fixtures put expected ids in `k` with per-leg scores (QRY-3, IDX-6, IDX-7).
6. Hybrid search on the OPS-1 fixture finishes in tens of milliseconds (OPS-2).

**Rust `scheda`:** **search** is accepted (`11` C9). Lexical ranking is BM25 over the full-text index of Lance; ANN is Lance's IVF_PQ index on cosine distance, with an exact scan below its size and for the rows no index covers yet; fusion is RRF. No embedder → vector leg off; writes still work (IDX-8). Corrupt Index → `INDEX_BROKEN`; `Collection.Get` still reads files (QRY-4). Crate names are this binding's choice (`§8`), not a requirement on other languages.

---

## 4. Agent

**In:** AGT-1–5.

ISO-3 applies when isolated collections exist and **agent** is on. Nested `{vault}/.isolated/` I/O is not required to accept **agent**.

**Agent is accepted** when `Vault.ListCollections` JSON matches collection declaration files, `Vault.CreateCollection` writes declarations without a raw path tool, tool writes cannot bypass SCH-12, and library vs transport JSON (and `code`) match on one Vault.

Which transport a binding ships (CLI, MCP, HTTP) is that binding's choice. Shipping none does not fail **core** or **search**. **Agent** acceptance needs at least one thin encoding of `07` (including `Vault.ListCollections` and `Vault.CreateCollection`). MCP stays an example, not a requirement (`08`).

**Rust `scheda`:** **agent** is accepted (`12` A7). Catalog is `Vault.ListCollections`. The thin encoding is CLI argv `14`, one subcommand per rpc. ISO-3 holds on that surface when an isolated collection exists. Unencrypted nested `{vault}/.isolated/` I/O is in (`12` P4). Collection encryption shipped as `12` P14, and the CLI carries the secret by environment variable or file (`14`).

---

## 5. Later

### Isolated collections

**In:** SCH-15, IDX-4, ISO-2–11 (including collection encryption, independent of `{vault}/.isolated/`). Calls and clauses that mention isolated/`encryption`/`CRYPTO` (`07`). Isolated Documents are `Vault.GetCollection` on the same Vault object, not a second open.

ISO-1 stays **core**. The main Vault is never encrypted.

Unencrypted isolated (`encryption` unset, `0600`) is valid; encryption is optional on top. Isolated data never leaves the machine (`20` D14, ISO-6): what is private and must survive belongs in an encrypted collection of the main tree, which reaches the mirror as ciphertext only.

**Already in `scheda`:** SCH-15 parse; ISO-3 listing; ISO-2 nested git; ISO-4 and IDX-4 in their P15 form — one owner-only Index per Vault that holds isolated collections and no encrypted one, and no nested Index (ISO-10 retired). `Vault.GetCollection` and `Vault.CreateCollection` with `isolated` read and write `{vault}/.isolated/`. A Vault-wide query or search with no collection named returns no isolated Documents and does not require `.isolated/`; naming an isolated collection is an ordinary query. `Vault.Check` covers the nested tree. ISO-5, ISO-7–9, and ISO-11 shipped as collection encryption (`12` P14): `OpenOptions.encryption` is read, `encrypted: true` is a declaration key on any one collection, and the declaration is the marker (no `.encrypted` file).

Online `reindex` (writers during the build) is `Index.Reindex` (`20` D18, D23): it starts the rebuild and `Index.Watch` shows it.

**Isolated is accepted** when:

1. A fixture with gitignored `{vault}/.isolated/`: queries on non-isolated collections return no isolated Documents and do not require `.isolated/` (ISO-2, SCH-15).
2. Nested git: local write and `Collection.History` of isolated paths; `Mirror.Push` does not upload `.isolated/`; `Vault.Check` fails if the parent tracks `.isolated/`; Scheda never contacts a remote for `.isolated/` (ISO-6).
3. Encryption of a collection (P14): `Collection.Create` / `Collection.Get` round-trip with the handle secret; I/O without it is `CRYPTO`; files decrypt with `age -d`; flipping `encrypted` on a non-empty collection is `INVALID_ARGUMENT` (ISO-5–9, ISO-11). Independent of `isolated`.
4. No Index holds a fixture string from an encrypted collection (ISO-4, IDX-4). Isolated Projector-derived values sit in the owner-only Index.
5. If **agent** is on: default tools never return that sealed value (ISO-3).

Gates 1–5 hold in `scheda` today.

### Everything else later

Not required of **core**, **search**, or **agent**:

- File watcher for human edits: the indexer polls `HEAD`, so a committed edit is indexed and an uncommitted one is not (IDX-5, IDX-9).
- Line-level journal blame (`Collection.History` of a JSONL line). `Collection.History` of the segment that holds that `id` stays **core** (GIT-3).
- Journal line delete (not for `standard` / `singleton`: `Collection.Delete`).
- REL-4 multi-step walk.
- Two vector generations at once on embedder change (SCH-10: rebuild is enough).
- Server-side Index for tens of millions of Documents: a new Index implementation behind `07`, not a new primary layer. Callers still must not speak SQL.
- Multi-master without STO-4. CRDTs. Custom YAML merge. Git LFS.
- A `[[wiki-link]]` extractor if an implementation never ships one (silence is not an extractor).
- Argon2id instead of age scrypt (ISO-8).
- Encrypting the **main** Vault. KMS / Tink / AWS Encryption SDK as the on-disk format.
- Second and further language bindings (they follow this package; they are not **later** in the CRDT sense — they are extra implementations).

---

## 6. Size targets

These are orders of magnitude for an interactive caller on one machine, not SLAs. Codes `OPS`.

**OPS-1. Core volume.** At least 10⁴ `journal` Documents and 10³ `standard` Documents in one Vault. `Collection.Get` and `Collection.Create` stay in milliseconds, not hundreds of milliseconds, on that fixture. *Check (**core**):* load the fixture; sample `Collection.Get` / journal `Collection.Create` return the expected Documents. The millisecond band is a benchmark, not a gate check.

**OPS-2. Search budget.** Hybrid search without a `Reranker` on the OPS-1 fixture finishes in tens of milliseconds. *Check (**search**):* fixture queries put the expected id in `k`; timing them is a benchmark, not a gate check.

**OPS-3. `Index.Reindex` is measurable.** A full rebuild on OPS-1 completes and `IndexState.rebuild` reports Documents read, Documents skipped, and segments read. Those counts are what the walk saw. A Document written or deleted during the walk may fall either side; the hand tally holds on a quiet tree. *Check (**core**):* counts match a hand tally of files and journal lines (`rust/scheda/tests/index.rs`: `reindex_returns_before_the_rebuild_a_second_is_aborted_and_the_maps_match`).

**OPS-4. Offline embed.** After embedder weights are already on disk, search does not need the network. Download of weights is the consumer's install, not the library hot path. *Check (**search**):* a full cycle with the network disabled.

`scheda` meets OPS-1 and OPS-3 (`11` C8) and OPS-2 and OPS-4 (`11` C9). The gate on every OS loads 10³ journal and 10² `standard` Documents (one tenth of OPS-1) and checks that `Collection.Get`, `Collection.Create`, `Index.Reindex`, and a search work on that volume; it does not time them.

Vaults on NFS/SMB, and volumes far above OPS-1, are outside these checks. A future Index backend (`§5`) is how huge Vaults stay on the same API.

---

## 7. Open questions

**When a human edits a file.** Default is `Index.Reindex` / `Vault.Check`. A watcher is **later**. Not decided: whether `Scheda.Open` should refuse a dirty Index (mtime/hash mismatch) or only warn via `Vault.Check`.

**Exact declaration YAML key names.** Closed in `05` §8. Pretty-print is still free.

**Passphrase vs identity in fixtures (**later**).** Both are required by ISO-7. Tests should cover each; not decided which the README example leads with.

**Hooks.** `04` already: Vault hooks are the consumer's and must not recurse into the same write lock. Worth a binding note when a CLI lands.

Already closed in this package (not open): SQL as caller API (no); merge on `open` (no); homemade ciphertext (no); Git LFS (no); library API field names invented per binding (no — `api/scheda/v1`); encryption marker path (no — ISO-11 is the declaration key `encrypted`, not a file). `Mirror.Push`, `Mirror.Pull`, and `Mirror.Sync` never publish `.isolated/`, and no call reaches a remote for it (`20` D14, ISO-6).

---

## 8. What this document does not decide

Calendar dates or crate version numbers for each phase.

Which FTS or vector crate a binding pins — `06` requires BM25-class lexical and HNSW-class ANN in-process; the crate names are a binding choice. `scheda` uses Lance. SQL is not the caller API.

Which age library a binding vendors — ISO-8.

A consumer's collection names and whether that consumer sets `encryption`.
