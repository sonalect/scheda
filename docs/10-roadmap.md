# 10. Phases, acceptance, and deferrals

Document 10 of the Scheda package. Version 0.54, 5 October 2026.

This document does not add product rules. It assigns the checkable requirements in `03`–`09` to a phase, states when a binding is accepted, lists size targets, open questions, and what is deliberately out. It also records which phases the first binding has passed and what it codes next. Terms are in `02`. Codes: `OPS`.

An assertion without a check is not a requirement.

---

## 1. Phases

Four labels appear on requirements:

| Phase | Meaning |
|---|---|
| **core** | Files are truth, writes are safe, one writer, autocommit or `begin`/`commit`/`discard`, `get` / `find`. No network unless opted in. A consumer can store and retrieve Records with no embedder, no agent transport, and no isolated collections. |
| **search** | Hybrid `search` that an agent can recall from: ranked BM25, in-process ANN, RRF fusion, Projector columns only. Must not be required to `add` a journal line or `replace` a document. Grep is not this phase. |
| **agent** | Collection listing (`info`) and a thin encoding of the same API (`08`). Does not block **core**. |
| **later** | Isolated collections and their encryption, scale, extra writers, extra walks, conveniences. Must not leak into **core** as hidden dependencies. |

A binding may ship **search** before **agent**. Isolated collections are **later**: they must not be required to pass **core**, **search**, or **agent**. Nested `{vault}/.isolated/` I/O is not a gate of **agent**.

The first implementation is the Rust crate `scheda`. Other languages (Python, Java, Go, TypeScript, and others) are separate bindings of the same Vault layout and the same operations (`07`). They are accepted on their own after **core** (at least) is true of that binding. They are not a prerequisite for accepting Rust **core**.

**First binding (`scheda`).** **core** is accepted (`11` C10). **search** is accepted (`11` C9). **agent** is accepted (`12` A7). No other language binding exists yet. Stage ticks and *Check:* tests live in `11`, `12`, `rust/scheda/tests/`, and `rust/cli/tests/`.

The journal `id` map is **core** (IDX-5). After it exists, `get` on `journal` must not scan unrelated segments (`03` §6). `scheda` has that map.

The collection declaration already parses `isolated: true` (SCH-15). `Vault.info` lists those collections as isolated and has no field list (ISO-3). `find` / `search` on non-isolated collections skip them. `collection(name)` and `create_collection` with `isolated` write and read `{vault}/.isolated/` (ISO-2, ISO-4, ISO-6).

**Next in `scheda`.** Consumer readiness P0–P16 is in (`12` §4). Age encryption shipped as `0.6.0` (`12` P14): it is a declaration key on any one collection, behind the `encrypt` feature. The CLI can open an encrypted Vault from an environment variable or an identity file, and the nested isolated tree may have a Remote of its own (`12` P16). Opaque Records (`16` F0–F19) are in. The Index follows `HEAD` in a background thread (`17` Part B, B1–B10): writes no longer update it, `find` / `search` are eventually consistent with `wait_indexed` / `index_status` to wait and to look, several processes share one indexer per role, a model change is the caller's `reindex`, and a damaged or old Index is rebuilt by itself. `17` Part T added the opt-in `tokio` feature: an async API whose batch of writes is one `transaction` on one blocking thread (LIB-29). Extra language bindings stay deferred.

**First consumer.** A first consumer is moving a primary layer of text files under git onto the crate. The P checklist in `12` §4 is in. Those stages are not a phase: their codes cross **core**, **search**, and **later**, and three of them amend requirements this document already assigns (P1 on STO-2 and MOD-12, P2 on IDX-2 in **core** §2, P3 on GIT-2). P0–P5, P8, P9, P10, P11, P12, and P13 of that list are in, and P6 and P7 are gone: the slug is the whole `id` rule (P5); the unencrypted isolated slice (`{vault}/.isolated/` nested git) is P4; listing a `document` collection without the Index is P13. P14 took encryption out of the isolation flag and made it a declaration key on any one collection, behind the `encrypt` feature; it shipped as `0.6.0`, and the two things it designed without building — a CLI that can carry a secret, and a Remote for the nested tree — shipped as P16. P15 replaced the nested isolated Index with one owner-only Index per Vault, which is what makes an isolated collection searchable at all. `find` and `search` are opt-in crate features (P8); the crate has no default features. `remove` deletes a `document` or `singleton` (P9); a journal line still cannot leave. P10 is acceptance on that fixture. P11's open-map field types were a field-Schema rule and are gone with `16`. P12 is an optional commit message on each mutating call and a commit identity at `open`.

**Still deferred:** file watcher (the indexer polls `HEAD`; an uncommitted edit is not indexed); journal line delete; REL-4; extra language bindings.

---

## 2. Core

**In:**

- Model: MOD-1–18
- Files and git: STO-1–5, GIT-1–13
- Collection declaration: SCH-1–8, SCH-12–14 (SCH-7 body marks are retired; Projector columns are **search**)
- Index needed for truth and lookup: IDX-1, IDX-2, IDX-5
- Point read and filter: QRY-1, QRY-2, QRY-7
- Library: LIB-1–10, LIB-12–19, LIB-24 (`search` itself is LIB-11; isolated/`encryption` clauses on those ops are **later**)
- Perimeter: ISO-1 only
- Size: OPS-1, OPS-3

IDX-2 stays **core** for the Index format and for the methods that take `id`s from the Index (`find`, `search`); `replace` and `remove` use the Record file. A sequential journal read is not one of those methods. **Core** also has `lines` (LIB-19): it walks journal segments in declaration sequence and does not open the Index. QRY-7 remains the order of `find` when the Index is present. The crate has no default features. `find`, `search`, `simd`, and `encrypt` are opt-in; Bazel CI still links them. A crate without `find`, `search`, or `simd` is the same library with those legs off, not a second product. `encrypt` degrades further and the contract says so: without it an encrypted collection cannot be read at all, so the handle refuses it by code and `check` fails with `unsupported_encryption`, rather than returning ciphertext as a Record or reporting health it never verified (SCH-16, ISO-5). Phases grade a build with `find` and `search` on.

**Out of core** (do not block **core** acceptance; a binding may leave them as stubs or `unsupported` until **search** / **agent** / **later**, and writes must not depend on them): LIB-11, IDX-3, IDX-4, IDX-6–9, QRY-3–6, REL-1–3, SCH-9–11, SCH-15, AGT-1–5, ISO-2–11. In `scheda`, the **search** and **agent** codes in that list are implemented (C9, A7), not stubs. SCH-15 parse, ISO-3 listing, and the unencrypted isolated slice (ISO-2 nested git, ISO-6, IDX-4) are in (`12` P4), with ISO-4 and IDX-4 in their P15 form: one owner-only Index per Vault, ISO-10 retired. ISO-5, ISO-7–9, and ISO-11 are in with encryption (`12` P14). The ISO-6 nested Remote relaxation shipped as P16.

**Core is accepted** when:

1. Every **core** code above has a passing check in the first binding.
2. A fixture Vault is readable without the library (STO-2) and through `get` (QRY-1).
3. Deleting the Index does not lose Records (STO-1, IDX-1).
4. Library source (`rust/scheda/src`) does not name a consumer's collections (SCH-1, MOD-1). Test data is not that grep.
5. `auto_sync` off: writes and `commit` succeed with the network disabled (GIT-8, ISO-1).

**Rust `scheda`:** those five gates pass (`11` C10; tests in `rust/scheda/tests/core.rs` and the C0–C8 suites). **core** is accepted for this binding.

Order used in `scheda` (not a second spec): working tree + lock → collection declaration + `document` / `singleton` → `journal` segments → `commit` / Remote → journal `id` map → hybrid `search` → core acceptance → agent → unencrypted isolated nested git. Remote operations are **core** but landed after local `commit`. The stage checklist is `11`; it does not add requirements. **search** codes stay out of this section; `11` ran C9 before C10.

---

## 3. Search

**In:** IDX-3, IDX-6–9, QRY-3–6, LIB-11, OPS-2, OPS-4. SCH-9–11 and REL-* were field-Schema rules; Projector column marks replace them.

Quality class is `06` §3–4: BM25 (or equivalent ranked FTS), in-process ANN (HNSW or equivalent) at OPS-1, RRF hybrid when both legs are on, injected embedder, no LLM or ranking model in the library (optional caller reranker on `search` is `13`). Substring search and brute-force vector scan at OPS-1 do not accept this phase.

**Search is accepted** when those codes pass and:

1. `add` / `replace` / `get` still work with no embedder (IDX-8).
2. Columns without `fts` / `vector` marks are absent from the corresponding `search` leg (Projector `schema`).
3. Corrupt Index fails closed (QRY-4).
4. Edge walks (REL-*) are retired with field Schema; search acceptance does not require them.
5. Token, synonym, and hybrid fixtures put expected ids in `k` with per-leg scores (QRY-3, IDX-6, IDX-7).
6. Hybrid `search` on the OPS-1 fixture finishes in tens of milliseconds (OPS-2).

**Rust `scheda`:** **search** is accepted (`11` C9). Lexical ranking is Tantivy BM25; ANN is in-process USearch HNSW; fusion is RRF. No embedder → vector leg off; writes still work (IDX-8). Corrupt lexical Index → `index_lexical`; `get` still reads files (QRY-4). Crate names are this binding's choice (`§8`), not a requirement on other languages.

---

## 4. Agent

**In:** AGT-1–5.

ISO-3 applies when isolated collections exist and **agent** is on. Nested `{vault}/.isolated/` I/O is not required to accept **agent**.

**Agent is accepted** when `info` JSON matches collection declaration files, `create_collection` writes declarations without a raw path tool, tool writes cannot bypass SCH-12, and library vs transport JSON (and `code`) match on one Vault.

Which transport a binding ships (CLI, MCP, HTTP) is that binding's choice. Shipping none does not fail **core** or **search**. **Agent** acceptance needs at least one thin encoding of `07` (including `info` and `create_collection`). MCP stays an example, not a requirement (`08`).

**Rust `scheda`:** **agent** is accepted (`12` A7). Catalog is `Vault.info`.
The thin encoding is CLI argv `14`. ISO-3 holds on that surface when an
isolated collection exists. Unencrypted nested `{vault}/.isolated/` I/O
is in (`12` P4). Collection encryption shipped as `12` P14. A CLI secret
and a nested Remote shipped as `12` P16.

---

## 5. Later

### Isolated collections

**In:** SCH-15, IDX-4, ISO-2–11 (including collection encryption, independent of `{vault}/.isolated/`). Library clauses that mention isolated/`encryption`/`crypto` (LIB-4, LIB-5, LIB-8, LIB-9, LIB-13–15). Isolated Records are `collection(name)` on the same Vault handle, not a second `open`.

ISO-1 stays **core**. The main Vault is never encrypted.

Unencrypted isolated (`encryption` unset, `0600`, nested Remote optional) is valid; encryption is optional on top.

**Already in `scheda`:** SCH-15 parse; ISO-3 listing; ISO-2 nested git; ISO-6 with a nested Remote of its own (`12` P16); ISO-4 and IDX-4 in their P15 form — one owner-only Index per Vault that holds isolated collections and no encrypted one, and no nested Index (ISO-10 retired). `collection(name)` and `create_collection` with `isolated` read and write `{vault}/.isolated/`. `find` / `search` on non-isolated collections return no isolated Records and do not require `.isolated/`; naming an isolated collection is an ordinary query. `check` covers the nested tree. ISO-5, ISO-7–9, and ISO-11 shipped as collection encryption (`12` P14): `OpenOptions.encryption` is read, `encrypted: true` is a declaration key on any one collection, and the declaration is the marker (no `.encrypted` file). The CLI can carry the encryption secret (`14`), and the nested tree may have a Remote of its own (`12` P16).

Online `reindex` (writers during the build) is not P16. The work order was `15`; `17` replaced its queue with marks.

**Isolated is accepted** when:

1. A fixture with gitignored `{vault}/.isolated/`: queries on non-isolated collections return no isolated Records and do not require `.isolated/` (ISO-2, SCH-15).
2. Nested git: local `commit` / `history` of isolated paths; `push` does not upload `.isolated/`; `check` fails if the parent tracks `.isolated/`; a nested Remote of its own is not a defect (ISO-6).
3. Encryption of a collection (P14): `add` / `get` round-trip with the handle secret; I/O without it is `crypto`; files decrypt with `age -d`; flipping `encrypted` on a non-empty collection is `invalid` (ISO-5–9, ISO-11). Independent of `isolated`.
4. No Index holds a fixture string from an encrypted collection (ISO-4, IDX-4). Isolated Projector-derived values sit in the owner-only Index.
5. If **agent** is on: default tools never return that sealed value (ISO-3).

Gates 1–5 hold in `scheda` today. The nested Remote and the CLI secret shipped as P16; they do not reopen these gates.

### Everything else later

Not required of **core**, **search**, or **agent**:

- File watcher for human edits: the indexer polls `HEAD`, so a committed edit is indexed and an uncommitted one is not (IDX-5, IDX-9).
- Line-level journal blame (`history` of a JSONL line). `history(id)` of the segment that holds that `id` stays **core** (GIT-3, LIB-17).
- Journal line `remove` (not for `document` / `singleton`: those have LIB-24).
- REL-4 multi-step walk.
- Two vector generations at once on embedder change (SCH-10: rebuild is enough).
- Server-side Index for tens of millions of Records: a new Index implementation behind `07`, not a new primary layer. Callers still must not speak SQL.
- Multi-master without STO-4. CRDTs. Custom YAML merge. Git LFS.
- A `[[wiki-link]]` extractor if an implementation never ships one (silence is not an extractor).
- Argon2id instead of age scrypt (ISO-8).
- Encrypting the **main** Vault. KMS / Tink / AWS Encryption SDK as the on-disk format.
- Second and further language bindings (they follow this package; they are not **later** in the CRDT sense — they are extra implementations).

---

## 6. Size targets

These are orders of magnitude for an interactive caller on one machine, not SLAs. Codes `OPS`.

**OPS-1. Core volume.** At least 10⁴ `journal` Records and 10³ `document` Records in one Vault. `get` and `add` stay in milliseconds, not hundreds of milliseconds, on that fixture. *Check (**core**):* load the fixture; sample `get` / journal `add` stay in that band.

**OPS-2. Search budget.** Hybrid `search` without a pair reranker on the OPS-1 fixture finishes in tens of milliseconds. *Check (**search**):* timed fixture queries.

**OPS-3. `reindex` is measurable.** Full `reindex` on OPS-1 completes and reports Records read, skipped invalid, segments seen (LIB-16). Those counts are what the walk saw. A Record written or removed during the walk may fall either side; the hand tally holds on a quiet tree. *Check (**core**):* counts match a hand tally of files and journal lines.

**OPS-4. Offline embed.** After embedder weights are already on disk, **search** does not need the network. Download of weights is the consumer's install, not the library hot path. *Check (**search**):* a full cycle with the network disabled.

`scheda` meets OPS-1 and OPS-3 (`11` C8) and OPS-2 and OPS-4 (`11` C9). CI on every OS loads 10³ journal and 10² document Records (one tenth of OPS-1) so the same C8 checks finish under Bazel `large` (900s) when the Vault is on a real disk: each `add` fsyncs.

Vaults on NFS/SMB, and volumes far above OPS-1, are outside these checks. A future Index backend (`§5`) is how huge Vaults stay on the same API.

---

## 7. Open questions

**When a human edits a file.** Default is `reindex` / `check`. A watcher is **later**. Not decided: whether `open` should refuse a dirty Index (mtime/hash mismatch) or only warn via `check`.

**Exact declaration YAML key names.** Closed in `05` §8. Pretty-print is still free.

**Passphrase vs identity in fixtures (**later**).** Both are required by ISO-7. Tests should cover each; not decided which the README example leads with.

**Hooks.** `04` already: Vault hooks are the consumer's and must not recurse into the same write lock. Worth a binding note when a CLI lands.

Already closed in this package (not open): SQL as caller API (no); merge on `open` (no); homemade ciphertext (no); Git LFS (no); library API field names invented per binding (no — `api/v1`); encryption marker path (no — ISO-11 is the declaration key `encrypted`, not a file). Main-tree `push` / `pull` / `sync` never publish `.isolated/`; a nested Remote is reached only by `push_isolated` / `pull_isolated` / `sync_isolated` (ISO-6).

---

## 8. What this document does not decide

Calendar dates or crate version numbers for each phase.

Which FTS or vector crate a binding pins — `06` requires BM25-class lexical and HNSW-class ANN in-process; the crate names are a binding choice. `scheda` pins Tantivy and USearch. SQL is not the caller API.

Which age library a binding vendors — ISO-8.

A consumer's collection names and whether that consumer sets `encryption`.
