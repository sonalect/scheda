# 10. Phases, acceptance, and deferrals

Document 10 of the Scheda package. Version 0.4, 23 August 2026.

This document does not add product rules. It assigns the checkable requirements in `03`–`09` to a phase, states when a binding is accepted, lists size targets, open questions, and what is deliberately out. Terms are in `02`. Codes: `OPS`.

An assertion without a check is not a requirement.

---

## 1. Phases

Four labels appear on requirements:

| Phase | Meaning |
|---|---|
| **core** | Files are truth, writes are safe, one writer, explicit `commit`, `find_one` / `find`. No network unless opted in. A consumer can store and retrieve Records with no embedder, no agent transport, and no isolated root. |
| **search** | Hybrid `search`, lexical and vector Index, one-step and inverse edge walks. Must not be required to `insert_one` a journal line or `replace_one` a document. |
| **agent** | Catalog and a thin encoding of the same API (`08`). Does not block **core**. |
| **later** | Isolated collections and their encryption, scale, extra writers, extra walks, conveniences. Must not leak into **core** as hidden dependencies. |

A binding may ship **search** before **agent**. Isolated collections are **later**: they must not be required to pass **core**, **search**, or **agent**.

The first implementation is the Rust crate `scheda`. Other languages (Python, Java, Go, TypeScript, and others) are separate bindings of the same Vault layout and the same operations (`07`). They are accepted on their own after **core** (at least) is true of that binding. They are not a prerequisite for accepting Rust **core**.

Until the journal `id` map exists, **core** `find_one` on `journal` may scan segments (`03` §6). IDX-5's cost check applies once that map is in.

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

**Out of core (must still compile as stubs or `unsupported` without breaking writes):** LIB-11, IDX-3, IDX-4, IDX-6–9, QRY-3–6, REL-1–3, SCH-9–11, SCH-15, AGT-1–5, ISO-2–11.

**Core is accepted** when:

1. Every **core** code above has a passing check in the first binding.
2. A fixture Vault is readable without the library (STO-2) and through `find_one` (QRY-1).
3. Deleting the Index does not lose Records (STO-1, IDX-1).
4. Implementation source does not name a consumer's collections (SCH-1, MOD-1).
5. `auto_sync` off: writes and `commit` succeed with the network disabled (GIT-8, ISO-1).

Suggested order inside the crate (not a second spec): working tree + lock → Schema + `document` / `singleton` → `journal` segments → `commit` / Remote → journal `id` map. Remote operations are **core** but may land after local `commit`. Isolated roots and `encrypt` wait for **later**. The stage checklist for the Rust binding is `11`; it does not add requirements.

---

## 3. Search

**In:** SCH-9–11, IDX-3, IDX-6–9, QRY-3–6, REL-1–3, LIB-11, OPS-2, OPS-4.

**Search is accepted** when those codes pass and:

1. `insert_one` / `replace_one` / `find_one` still work with no embedder (IDX-8).
2. Unmarked fields are absent from `search` (SCH-9).
3. Corrupt Index fails closed (QRY-4).
4. One-step and inverse walks match fixtures (REL-2, REL-3). Multi-step (REL-4) is not required.

---

## 4. Agent

**In:** AGT-1–5.

ISO-3 applies when isolated collections exist (**later**); it does not block this phase.

**Agent is accepted** when catalog JSON matches Schema files, tool writes cannot bypass SCH-12, and library vs transport JSON (and `code`) match on one Vault.

Which transport a binding ships (CLI, MCP, HTTP) is that binding's choice. Shipping none does not fail **core** or **search**.

---

## 5. Later

### Isolated collections

**In:** SCH-15, IDX-4, ISO-2–11 (including age encryption of that root). Library clauses that mention isolated/`encrypt`/`secret`/`crypto` (LIB-4, LIB-5, LIB-8, LIB-9, LIB-13–15). Combined `search` across the main Vault and an isolated handle (GIT-5).

ISO-1 stays **core**. The main Vault is never encrypted.

**Isolated is accepted** when:

1. A fixture with two roots: queries on the main Vault return no isolated Records and do not require that root (ISO-2, SCH-15).
2. Isolated handle: local `commit` / `history`; `push_remote` / `clone(url)` / `auto_sync` fail; `check` fails if a Remote exists (ISO-6).
3. `init(..., encrypt=true)` round-trips with `secret`; `open` without `secret` is `crypto`; files decrypt with `age -d` (ISO-5–9, ISO-11).
4. Main Index contains no fixture sealed string (ISO-4, IDX-4).
5. If **agent** is on: tools on the main Vault never return that sealed value (ISO-3).

Unencrypted isolated (`encrypt: false`, `0600`, no Remote) is valid in this phase; encryption is optional on top.

### Everything else later

Not required of **core**, **search**, or **agent**:

- File watcher for human edits (default remains `reindex`, IDX-5).
- Line-level journal blame (`history` of a JSONL line). Segment-file `history` stays **core** (GIT-3, LIB-17).
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

**OPS-1. Core volume.** At least 10⁴ `journal` Records and 10³ `document` Records in one Vault. `find_one` and `insert_one` stay in milliseconds, not hundreds of milliseconds, on that fixture. *Check (**core**):* load the fixture; sample `find_one` / journal `insert_one` stay in that band.

**OPS-2. Search budget.** Hybrid `search` without a pair reranker on the OPS-1 fixture finishes in tens of milliseconds. *Check (**search**):* timed fixture queries.

**OPS-3. `reindex` is measurable.** Full `reindex` on OPS-1 completes and reports Records read, skipped invalid, segments seen (LIB-16). *Check (**core**):* counts match a hand tally of files and journal lines.

**OPS-4. Offline embed.** After embedder weights are already on disk, **search** does not need the network. Download of weights is the consumer's install, not the library hot path. *Check (**search**):* a full cycle with the network disabled.

Vaults on NFS/SMB, and volumes far above OPS-1, are outside these checks. A future Index backend (`§5`) is how huge Vaults stay on the same API.

---

## 7. Open questions

**Default Index path.** `open` may derive a directory next to the Vault or require an explicit path. Either is fine if GIT-13 holds. Prefer explicit in tests.

**When a human edits a file.** Default is `reindex` / `check`. A watcher is **later**. Not decided: whether `open` should refuse a dirty Index (mtime/hash mismatch) or only warn via `check`.

**Exact Schema YAML key names.** Round-trip of SCH-2–SCH-14 is **core**; SCH-15 with them is **later**. Pretty-print names may be an appendix.

**Marker path for `encrypt` (**later**).** ISO-11 allows a binding path (example `.scheda/encrypted`). A shared path across languages would ease mixed-binding Vaults; not fixed yet.

**Passphrase vs identity in fixtures (**later**).** Both are required by ISO-7. Tests should cover each; not decided which the README example leads with.

**Hooks.** `04` already: Vault hooks are the consumer's and must not recurse into the same write lock. Worth a binding note when a CLI lands.

Already closed in this package (not open): SQL as caller API (no); merge on `open` (no); isolated Remote (no); homemade ciphertext (no); Git LFS (no).

---

## 8. What this document does not decide

Calendar dates or crate version numbers for each phase.

Which FTS or vector crate the Rust binding pins — `06`, as long as the Index stays disposable and SQL is not the caller API.

Which age library a binding vendors — ISO-8.

A consumer's collection names and whether that consumer enables `encrypt`.
