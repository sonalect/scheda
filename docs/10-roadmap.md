# 10. Phases, acceptance, and deferrals

Document 10 of the Scheda package. Version 0.21, 27 August 2026.

This document does not add product rules. It assigns the checkable requirements in `03`–`09` to a phase, states when a binding is accepted, lists size targets, open questions, and what is deliberately out. It also records which phases the first binding has passed and what it codes next. Terms are in `02`. Codes: `OPS`.

An assertion without a check is not a requirement.

---

## 1. Phases

Four labels appear on requirements:

| Phase | Meaning |
|---|---|
| **core** | Files are truth, writes are safe, one writer, autocommit or `begin`/`commit`/`discard`, `get` / `find`. No network unless opted in. A consumer can store and retrieve Records with no embedder, no agent transport, and no isolated collections. |
| **search** | Hybrid `search` that an agent can recall from: ranked BM25, in-process ANN, RRF fusion, Schema-marked fields only, one-step and inverse edge walks. Must not be required to `add` a journal line or `replace` a document. Grep is not this phase. |
| **agent** | Schema listing (`info`) and a thin encoding of the same API (`08`). Does not block **core**. |
| **later** | Isolated collections and their encryption, scale, extra writers, extra walks, conveniences. Must not leak into **core** as hidden dependencies. |

A binding may ship **search** before **agent**. Isolated collections are **later**: they must not be required to pass **core**, **search**, or **agent**. Nested `{vault}/.isolated/` I/O is not a gate of **agent**.

The first implementation is the Rust crate `scheda`. Other languages (Python, Java, Go, TypeScript, and others) are separate bindings of the same Vault layout and the same operations (`07`). They are accepted on their own after **core** (at least) is true of that binding. They are not a prerequisite for accepting Rust **core**.

**First binding (`scheda`).** **core** is accepted (`11` C10). **search** is accepted (`11` C9). **agent** is accepted (`12` A7). No other language binding exists yet. Stage ticks and *Check:* tests live in `11`, `12`, `rust/scheda/tests/`, and `rust/cli/tests/`.

The journal `id` map is **core** (IDX-5). After it exists, `get` on `journal` must not scan unrelated segments (`03` §6). `scheda` has that map.

Schema already parses `isolated: true` (SCH-15). `Vault.info` lists those collections as isolated and omits `schema.fields` (ISO-3). `find` / `search` / `check` / Index skip them. `collection(name)` and `create_collection` with `isolated` are `unsupported` (no Schema file written). There is no nested git yet.

**Next in `scheda`.** Unencrypted isolated slice of **later**: ISO-2, ISO-4 (nested Index), ISO-6, IDX-4. A crate consumer needs `{vault}/.isolated/` (gitignored nested git, owner-only files, no Remote) so `collection(name)` can read and write those Records. Do not start age encryption of that tree, other language bindings, or the rest of §5 until this slice is in the crate.

Order inside that slice (not a second spec): nested git under `{vault}/.isolated/` → `create_collection` / `add` on an isolated name write there → `find` / `search` on non-isolated collections still return no isolated Records and do not require `.isolated/` → main Index has no fixture sealed string → `check` covers the nested tree.

**Still deferred after that slice:** ISO-5, ISO-7–11; file watcher; `delete_one`; REL-4; extra language bindings.

---

## 2. Core

**In:**

- Model: MOD-1–18
- Files and git: STO-1–5, GIT-1–13
- Schema: SCH-1–8, SCH-12–14 (SCH-7 stores the body in **core**; indexing the body is **search**; SCH-15 is **later**)
- Index needed for truth and lookup: IDX-1, IDX-2, IDX-5
- Point read and filter: QRY-1, QRY-2, QRY-7
- Library: LIB-1–10, LIB-12–18 (`search` itself is LIB-11; isolated/`encrypt` clauses on those ops are **later**)
- Perimeter: ISO-1 only
- Size: OPS-1, OPS-3

**Out of core** (do not block **core** acceptance; a binding may leave them as stubs or `unsupported` until **search** / **agent** / **later**, and writes must not depend on them): LIB-11, IDX-3, IDX-4, IDX-6–9, QRY-3–6, REL-1–3, SCH-9–11, SCH-15, AGT-1–5, ISO-2–11. In `scheda`, the **search** and **agent** codes in that list are implemented (C9, A7), not stubs. SCH-15 parse and ISO-3 on the agent surface are in. Remaining unencrypted isolated work is ISO-2 nested git, ISO-4 nested Index, ISO-6, IDX-4 (`§1`). ISO-5, ISO-7–11 stay deferred.

**Core is accepted** when:

1. Every **core** code above has a passing check in the first binding.
2. A fixture Vault is readable without the library (STO-2) and through `get` (QRY-1).
3. Deleting the Index does not lose Records (STO-1, IDX-1).
4. Implementation source does not name a consumer's collections (SCH-1, MOD-1).
5. `auto_sync` off: writes and `commit` succeed with the network disabled (GIT-8, ISO-1).

**Rust `scheda`:** those five gates pass (`11` C10; tests in `rust/scheda/tests/core.rs` and the C0–C8 suites). **core** is accepted for this binding.

Order used in `scheda` (not a second spec): working tree + lock → Schema + `document` / `singleton` → `journal` segments → `commit` / Remote → journal `id` map → hybrid `search` → core acceptance → agent → unencrypted isolated nested git. Remote operations are **core** but landed after local `commit`. The stage checklist is `11`; it does not add requirements. **search** codes stay out of this section; `11` ran C9 before C10.

---

## 3. Search

**In:** SCH-9–11, IDX-3, IDX-6–9, QRY-3–6, REL-1–3, LIB-11, OPS-2, OPS-4.

Quality class is `06` §3–4: BM25 (or equivalent ranked FTS), in-process ANN (HNSW or equivalent) at OPS-1, RRF hybrid when both legs are on, injected embedder, no LLM or ranking model in the library (optional caller reranker on `search` is `13`). Substring search and brute-force vector scan at OPS-1 do not accept this phase.

**Search is accepted** when those codes pass and:

1. `add` / `replace` / `get` still work with no embedder (IDX-8).
2. `exclude_fts` / `exclude_vector` fields are absent from the corresponding `search` leg (SCH-9).
3. Corrupt Index fails closed (QRY-4).
4. One-step and inverse walks match fixtures (REL-2, REL-3). Multi-step (REL-4) is not required.
5. Token, synonym, and hybrid fixtures put expected ids in `k` with per-leg scores (QRY-3, IDX-6, IDX-7).
6. Hybrid `search` on the OPS-1 fixture finishes in tens of milliseconds (OPS-2).

**Rust `scheda`:** **search** is accepted (`11` C9). Lexical ranking is Tantivy BM25; ANN is in-process USearch HNSW; fusion is RRF. No embedder → vector leg off; writes still work (IDX-8). Corrupt lexical Index → `index_unusable`; `get` still reads files (QRY-4). Crate names are this binding's choice (`§8`), not a requirement on other languages.

---

## 4. Agent

**In:** AGT-1–5.

ISO-3 applies when isolated collections exist and **agent** is on. Nested `{vault}/.isolated/` I/O is not required to accept **agent**.

**Agent is accepted** when `info` JSON matches Schema files, `create_collection` writes declarations without a raw path tool, tool writes cannot bypass SCH-12, and library vs transport JSON (and `code`) match on one Vault.

Which transport a binding ships (CLI, MCP, HTTP) is that binding's choice. Shipping none does not fail **core** or **search**. **Agent** acceptance needs at least one thin encoding of `07` (including `info` and `create_collection`). MCP stays an example, not a requirement (`08`).

**Rust `scheda`:** **agent** is accepted (`12` A7). Catalog is `Vault.info`.
The thin encoding is CLI argv `14`. ISO-3 holds on that surface when an
isolated collection exists; nested `{vault}/.isolated/` I/O stays `§1`.
Age encryption stays `§5`.

---

## 5. Later

### Isolated collections

**In:** SCH-15, IDX-4, ISO-2–11 (including age encryption of `{vault}/.isolated/`). Library clauses that mention isolated/`encrypt`/`secret`/`crypto` (LIB-4, LIB-5, LIB-8, LIB-9, LIB-13–15). Isolated Records are `collection(name)` on the same Vault handle, not a second `open`. Until nested I/O exists, that call is `unsupported`.

ISO-1 stays **core**. The main Vault is never encrypted.

Unencrypted isolated (`encrypt: false`, `0600`, no Remote) is valid; encryption is optional on top.

**Already in `scheda`:** SCH-15 parse; ISO-3 listing (`info` omits isolated fields); main `find` / `search` / Index skip isolated collections; `create_collection` with `isolated` and `collection(name)` for an isolated name are `unsupported` (no Schema file written).

**Next in `scheda` (unencrypted only):** ISO-2 nested git, ISO-4 nested Index, ISO-6, IDX-4. `{vault}/.isolated/` nested git; owner-only files; no Remote; `collection(name)` reads and writes those Records; `find` / `search` on non-isolated collections return no isolated Records and do not require `.isolated/`; main Index has no fixture sealed string; `check` covers the nested tree.

**Still later — encryption:** ISO-5, ISO-7–11. `OpenOptions.encrypt` and `secret` wait until the unencrypted nested tree is in.

**Isolated is accepted** when:

1. A fixture with gitignored `{vault}/.isolated/`: queries on non-isolated collections return no isolated Records and do not require `.isolated/` (ISO-2, SCH-15).
2. Nested git: local `commit` / `history` of isolated paths; `push` does not upload `.isolated/`; `check` fails if the nested repo has a Remote or the parent tracks `.isolated/` (ISO-6).
3. `encrypt=true` on first creation of `.isolated/` round-trips with `secret`; isolated I/O without `secret` is `crypto`; main `open` still succeeds; files decrypt with `age -d` (ISO-5–9, ISO-11).
4. Main Index contains no fixture sealed string (ISO-4, IDX-4).
5. If **agent** is on: default tools never return that sealed value (ISO-3).

Gate 3 is encryption; it does not block the unencrypted slice. Gate 5 holds in `scheda` today. `scheda` has no `{vault}/.isolated/` nested git yet.

### Everything else later

Not required of **core**, **search**, or **agent**:

- File watcher for human edits (default remains `reindex`, IDX-5).
- Line-level journal blame (`history` of a JSONL line). `history(id)` of the segment that holds that `id` stays **core** (GIT-3, LIB-17).
- `delete_one` for `document` (not for `journal` lines).
- REL-4 multi-step walk.
- Two vector generations at once on embedder change (SCH-10: rebuild is enough).
- Server-side Index for tens of millions of Records: a new Index implementation behind `07`, not a new primary layer. Callers still must not speak SQL.
- Multi-master without STO-4. CRDTs. Custom YAML merge. Git LFS.
- Schema-named `[[wiki-link]]` extractor if an implementation never ships one (SCH-3: silence is not an extractor).
- Argon2id instead of age scrypt (ISO-8).
- Encrypting the **main** Vault. KMS / Tink / AWS Encryption SDK as the on-disk format.
- Second and further language bindings (they follow this package; they are not **later** in the CRDT sense — they are extra implementations).

---

## 6. Size targets

These are orders of magnitude for an interactive caller on one machine, not SLAs. Codes `OPS`.

**OPS-1. Core volume.** At least 10⁴ `journal` Records and 10³ `document` Records in one Vault. `get` and `add` stay in milliseconds, not hundreds of milliseconds, on that fixture. *Check (**core**):* load the fixture; sample `get` / journal `add` stay in that band.

**OPS-2. Search budget.** Hybrid `search` without a pair reranker on the OPS-1 fixture finishes in tens of milliseconds. *Check (**search**):* timed fixture queries.

**OPS-3. `reindex` is measurable.** Full `reindex` on OPS-1 completes and reports Records read, skipped invalid, segments seen (LIB-16). *Check (**core**):* counts match a hand tally of files and journal lines.

**OPS-4. Offline embed.** After embedder weights are already on disk, **search** does not need the network. Download of weights is the consumer's install, not the library hot path. *Check (**search**):* a full cycle with the network disabled.

`scheda` meets OPS-1 and OPS-3 (`11` C8) and OPS-2 and OPS-4 (`11` C9). Linux and macOS CI load those sizes. Windows CI runs the same C8 checks on 10³ journal and 10² document Records (one tenth) so every commit still measures NTFS I/O inside Bazel `large` (900s).

Vaults on NFS/SMB, and volumes far above OPS-1, are outside these checks. A future Index backend (`§5`) is how huge Vaults stay on the same API.

---

## 7. Open questions

**When a human edits a file.** Default is `reindex` / `check`. A watcher is **later**. Not decided: whether `open` should refuse a dirty Index (mtime/hash mismatch) or only warn via `check`.

**Exact Schema YAML key names.** Closed in `05` §8. Pretty-print is still free.

**Marker path for `encrypt` (**later**).** ISO-11 allows a binding path (example `.scheda/encrypted`). A shared path across languages would ease mixed-binding Vaults; not fixed yet.

**Passphrase vs identity in fixtures (**later**).** Both are required by ISO-7. Tests should cover each; not decided which the README example leads with.

**Hooks.** `04` already: Vault hooks are the consumer's and must not recurse into the same write lock. Worth a binding note when a CLI lands.

Already closed in this package (not open): SQL as caller API (no); merge on `open` (no); isolated Remote (no); homemade ciphertext (no); Git LFS (no); library API field names invented per binding (no — `api/v1`).

---

## 8. What this document does not decide

Calendar dates or crate version numbers for each phase.

Which FTS or vector crate a binding pins — `06` requires BM25-class lexical and HNSW-class ANN in-process; the crate names are a binding choice. `scheda` pins Tantivy and USearch. SQL is not the caller API.

Which age library a binding vendors — ISO-8.

A consumer's collection names and whether that consumer enables `encrypt`.
