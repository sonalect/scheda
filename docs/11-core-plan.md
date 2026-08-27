# 11. Core development plan

Document 11 of the Scheda package. Version 0.24, 27 August 2026.

Work order for the Rust binding of **core** (`10` §2) and the **search**
stage that runs after C8 (`10` §3). It does not add product rules. Codes and
*Check:* lines in `03`–`09` stay normative. This file only assigns them to
stages and records whether the first crate has passed them. What `scheda`
codes after C10 is `10` §1, not extra stages here.

The first implementation is the crate `scheda`. Other language bindings are
out of this plan (`10` §1).

---

## 1. How to track progress

Tick a box only when the cited *Check:* passes under `bazel test //...` (or the
named `#[test]`). Do not tick from a verbal claim that the code is ready.

| Mark | Meaning |
|---|---|
| `[ ]` | Not done |
| `[x]` | Done; the check lives in the crate tests |

Each stage has **Status:** `not started` | `in progress` | `done`. Set `done`
only when every box in that stage is `[x]` and Clippy is clean.

If `03`–`10` do not decide something the stage needs (for example Schema YAML
key names), stop. Amend the spec; do not invent a silent default in
code.

Suggested crate order is `10` §2–3: working tree + lock → Schema + `document` /
`singleton` → `journal` segments → `commit` / Remote → journal `id` map →
hybrid `search` → core acceptance.

---

## 2. Dashboard

| Stage | Status | What closes |
|---|---|---|
| [C0](#c0-skeleton) | done | Names, error codes, stubs; crate builds |
| [C1](#c1-working-tree-and-lock) | done | `init` / `open`, lock, gitignored Index, ISO-1 local |
| [C2](#c2-schema) | done | Load and reject Schema |
| [C3](#c3-document-and-singleton) | done | Whole-file writes and `get` |
| [C4](#c4-journal-segments) | done | Append, rotate, scan `get` |
| [C5](#c5-find-check-reindex) | done | Filters, `check`, disposable Index |
| [C6](#c6-local-commit-and-history) | done | `commit`, `history` of a path |
| [C7](#c7-remote) | done | `clone`, `auto_sync`, push/pull/sync |
| [C8](#c8-journal-id-map-and-size) | done | IDX-5, OPS-1, OPS-3 |
| [C9](#c9-hybrid-search) | done | BM25 + ANN + RRF (`10` §3) |
| [C10](#c10-core-acceptance) | done | `10` §2 acceptance |

**Core in:** MOD-1–18; STO-1–5; GIT-1–13; SCH-1–8, SCH-12–14; IDX-1, IDX-2,
IDX-5; QRY-1, QRY-2, QRY-7; LIB-1–10, LIB-12–18; ISO-1; OPS-1, OPS-3.

Next crate work after this checklist is `10` §1 (unencrypted isolated).
**agent** follows that slice (`12`). This file does not add those stages.

---

## 3. Out of core (stubs)

These must **compile**. They must not be required to pass C0–C8 or C10.
Writes must not depend on them (`10` §2). **C9** implements **search**
(`10` §3). After C10, `scheda` codes unencrypted isolated (SCH-15, ISO-2,
ISO-4, ISO-6, IDX-4) as in `10` §1 (`{vault}/.isolated/`). **agent** (AGT-1–5) and ISO-3 follow
(`12`). Age encryption (ISO-5, ISO-7–11) stays deferred.

`delete_one`, line-level journal blame, file watcher: not in this plan (`10`
§5).

---

## C0. Skeleton

**Status:** done

**Codes:** LIB-2 (required names exist), LIB-4 / LIB-7 / LIB-9 (`api/v1`
envelope rules), LIB-5 (typed codes exist), `07` §5.

- [x] `bazel test //...` is green on the host.
- [x] Library path has no `panic!` / `unwrap` / `expect` / `assert!` (`07` §5).
- [x] Every LIB-2 required name exists on the public surface (`search` may be
      `unsupported`).
- [x] `Error` carries LIB-5 **core** codes: `not_found`, `invalid`, `conflict`,
      `not_fast_forward`, `locked`, `io`, `git`, `index_unusable`,
      `unsupported`. Negative tests assert **codes**, not only message text.
- [x] `search` (LIB-11) is `unsupported` or an empty stub that does not write.
- [x] `init` / `open` / `clone` reject `OpenOptions` that fail `api/v1` field
      rules (`invalid`) before creating a tree or taking the lock (`07` LIB-4).
- [x] `add` / `replace` reject a `Record` that fails `api/v1`
      envelope rules (`invalid`) before Schema writes (`07` LIB-7, LIB-9).

---

## C1. Working tree and lock

**Status:** done

**Codes:** LIB-1, LIB-4, LIB-6, LIB-13 (`init` / `open` only), STO-4, GIT-1,
GIT-5, GIT-6, GIT-7, GIT-8 (local writes once they exist), GIT-9 (`init` /
`open`; `clone` is C7), GIT-13, ISO-1 (no network on `open` / `init`).

- [x] `init(path)` creates a git working tree with no Remote (GIT-9).
- [x] `open(path)` on that tree succeeds; Index is `{vault}/.scheda/index`,
      gitignored, not committed (GIT-1, GIT-13). `init` writes `.gitignore`.
- [x] Two `open`s on different roots in one process (GIT-5, LIB-4).
- [x] Write lock: second write `open` → `locked`; a reader `get` is not
      blocked once C3 exists (STO-4). Until C3, lock vs second writer is
      enough.
- [x] `open` with network disabled still reports configured `origin` on
      `info.remote` (GIT-7).
- [x] Core tests run without an async executor (LIB-6).
- [x] The library never configures `filter.lfs` (GIT-6).
- [x] `init` / `open` with `auto_sync` off open no outbound connections
      (ISO-1, GIT-8).

---

## C2. Schema

**Status:** done

**Codes:** SCH-1–6, SCH-8 (declaration only), SCH-12–14 (load / reject),
SCH-3, MOD-1, MOD-2, MOD-6.

YAML/JSON keys used in fixtures must already appear in `05` §8. Do not
hard-code a consumer's collection names (SCH-1, MOD-1).

- [x] Schema lives under git; `open` / `check` reads it (SCH-1).
- [x] Missing kind or `schema_version` → rejected (SCH-2).
- [x] Executable blob in Schema → rejected; unmarked Markdown is not an
      extractor (SCH-3).
- [x] Core field types round-trip: `string`, `integer`, `number`, `boolean`,
      `string_list`, `object`, `object_list` (SCH-4).
- [x] Required / optional / `nullable` matrix (SCH-5) — write checks may wait
      for C3 if `open` already rejects illegal declarations.
- [x] Identity field name default `id`, overridable (SCH-6).
- [x] Filter-key marks parse; `exclude_find` fields cannot be declared as
      filter keys in a way that C5 would honour (SCH-8).
- [x] Default `segment_max_bytes` is 8 MiB; a cap `> 32 MiB` is rejected
      (SCH-14, GIT-6).
- [x] Two Vaults with different collection names work on the same build;
      grep of `rust/scheda/src` does not contain those names (MOD-1, SCH-1).

---

## C3. `document` and `singleton`

**Status:** done

**Codes:** MOD-2, MOD-5–8, MOD-11–15, MOD-17, MOD-18, STO-2, STO-5, SCH-5,
SCH-6, SCH-7 (store body; do not index), SCH-12, SCH-13, LIB-3, LIB-7–9,
QRY-1.

- [x] `add` / `replace` / `get` round-trip fields (QRY-1,
      LIB-7, LIB-8). Missing `id` → `not_found`, not an empty Record.
- [x] `document` Markdown+YAML and YAML-only; empty body allowed (MOD-11,
      MOD-14, STO-2). Body is stored; no search Index yet (SCH-7).
- [x] Header `id` matches path-derived `id` (MOD-13, SCH-6).
- [x] Path template with extra slots; `find` path-prefix is C5, but
      `add` must write the derived path (MOD-8).
- [x] Rename of a `document` file so path ≠ `id` → `check` fails; `get`
      does not follow the rename (MOD-5). May land with C5 `check`.
- [x] Missing optional field is absent on read-back (MOD-12). Stored `null`
      stays `null` (MOD-18, SCH-5).
- [x] `id` pattern from Schema; library does not invent a consumer scheme
      (MOD-6).
- [x] `singleton`: Schema-fixed `id`; second `add` rejected; then
      `replace` only (MOD-7, MOD-17).
- [x] `replace` is whole-file; no field patch (MOD-15, LIB-3).
- [x] Failed / interrupted write does not become the new file (STO-5).
- [x] Invalid Record vs current Schema: write rejected; old file `get`
      with `valid: false` (SCH-12, LIB-8).
- [x] Schema N+1 that only adds an optional field still reads N files
      (SCH-13).
- [x] A fixture Vault opens in an editor and in `git` with no Scheda (STO-2).

---

## C4. `journal` segments

**Status:** done

**Codes:** MOD-2–4, MOD-9–11, MOD-16, STO-3, STO-5, SCH-14, LIB-3, LIB-7,
LIB-9, LIB-18, QRY-7 (`find` with `all`).

`get` **may scan segments** until C8 (IDX-5, `03` §6).

- [x] `add` appends one RFC 8785 canonical JSON object plus `\n`
      (MOD-16, LIB-18).
- [x] `replace` → `unsupported` (MOD-2, MOD-16, LIB-3, LIB-9).
- [x] Duplicate `id` across segments is rejected (MOD-4).
- [x] After the size cap, the next `add` does not grow the closed
      file; previous bytes unchanged (MOD-10, SCH-14).
- [x] `get` uses `(collection, id)`, not a segment name (MOD-3, MOD-9).
- [x] `find` with `all` after insert A, B, rotate, insert C → A, B, C
      (QRY-7, LIB-10).
- [x] Mid-segment corruption → `check` fails; Scheda does not rewrite the
      file to “fix” it (STO-3, MOD-16).
- [x] A tail without `\n` is not a Record (STO-3, STO-5).

---

## C5. `find`, `check`, `reindex`

**Status:** done

**Codes:** QRY-2, LIB-10, LIB-15, LIB-16, IDX-1, IDX-2, STO-1, GIT-4, SCH-8,
SCH-12.

- [x] SCH-8 filters: equality (including `null`), set, existence, `document`
      path-prefix. `exclude_find` field → error. Empty filter returns every
      Record (QRY-2, LIB-10).
- [x] `find` with `Filter` `all` returns every indexed Record (LIB-10).
- [x] `check` reports Schema, encoding, journal lines, `id` uniqueness,
      conflict markers — with a code and path; it does not write Records
      (LIB-15, GIT-4, STO-2, STO-3).
- [x] Conflict markers in a `document` → unreadable; `check` fails (GIT-4).
- [x] `reindex` rebuilds the Index from files; returns Records read, skipped
      invalid, segments seen (IDX-1, LIB-16).
- [x] Delete the Index, `reindex`; Records match the files (STO-1, IDX-1).
- [x] Index format version: unknown/corrupt → `reindex` or `index_unusable`;
      never another Vault's ids (IDX-2). `get` still reads files.
- [x] Invalid old files stay readable via `get` with `valid: false` (SCH-12).

---

## C6. Local `commit` and `history`

**Status:** done

**Codes:** GIT-2, GIT-3, LIB-12, LIB-17.

- [x] Several `add` after `begin` without `commit` → `git log` unchanged (GIT-2).
- [x] `add` without `begin` → one new commit with message `scheda` (GIT-2).
- [x] One `commit(message)` → one commit whose paths are those writes, not
      `git add` of junk at the Vault root (GIT-2, LIB-12).
- [x] `history(id)` of a `document` / `singleton` (and `journal` via its
      segment file) matches `git show` (GIT-3, LIB-17). Not line-level
      journal blame.

---

## C7. Remote

**Status:** done

**Codes:** GIT-8–12, LIB-13, LIB-14, ISO-1.

- [x] `clone(url, path)` produces a Vault whose `origin` is that URL;
      `open`+`remote_repo` on an existing repo fails (GIT-9).
- [x] `auto_sync` default off: `open` does not change `HEAD` when Remote has
      new commits (GIT-10).
- [x] `auto_sync` on + fast-forward → `HEAD` matches Remote after `open`;
      diverged → `not_fast_forward`, no conflict markers from Scheda
      (GIT-10).
- [x] `pull` / `push` / `sync` with `auto_sync` off
      (GIT-11, LIB-14). Diverged `sync` fails before push.
- [x] Unreachable Remote after `commit`: `HEAD` is the new commit; later
      `push` publishes it (GIT-12).
- [x] Writes and local `commit` with `auto_sync` off succeed with the
      network disabled (GIT-8, ISO-1, `10` §2 item 5).

---

## C8. Journal `id` map and size

**Status:** done

**Codes:** IDX-5, OPS-1, OPS-3.

- [x] `get` on `journal` does not scan every unrelated segment (IDX-5).
      Writer updates the map under the lock; human editor edits still need
      `reindex`.
- [x] Fixture ≥ 10⁴ journal Records and ≥ 10³ `document` Records: sampled
      `get` / journal `add` stay in milliseconds, not hundreds
      (OPS-1).
- [x] Full `reindex` on that fixture completes; counts match a hand tally
      (OPS-3, LIB-16).

---

## C9. Hybrid search

**Status:** done

**Codes:** SCH-9–11, IDX-3, IDX-6–9, QRY-3–6, REL-1–3, LIB-11, OPS-2, OPS-4.

This is the **search** phase (`10` §3), not **core**. It runs before C10 so
the crate returns candidates an agent can recall from, not `unsupported`.
Writes must still succeed with no embedder (IDX-8). Quality is `06` §3–4:
BM25-class lexical, HNSW-class ANN in-process, RRF hybrid, Schema-marked
fields only, injected embedder, no LLM or pair reranker in this crate
(QRY-5). Grep, SQL `LIKE`, and brute-force vector scan at OPS-1 are not
this stage.

- [x] `search` is no longer `unsupported` (LIB-11).
- [x] Lexical Index: BM25 (or equivalent ranked FTS) over SCH-9 fields and
      body that are not `exclude_fts`; excluded text is absent (IDX-6,
      SCH-9). Token fixture: expected id in `k` among distractors.
- [x] Vector Index: in-process ANN (HNSW or equivalent) over SCH-9/10 slots;
      no external vector database (IDX-7). No embedder → vector leg off;
      writes and lexical still work (IDX-8). Synonym fixture with no shared
      tokens: expected id in `k`.
- [x] At OPS-1, vector queries use the ANN structure, not a scan of every
      stored vector (IDX-7, OPS-2).
- [x] After `add` / `replace`, lexical (and vector if an embedder
      is present) update without a full `reindex` (IDX-9).
- [x] Invalid Records stay out of `search`; `get` still returns them
      (IDX-3, SCH-12).
- [x] Hybrid `search`: both legs when available; default fusion is RRF;
      per-leg scores on the hit; caller may pass `k` and merge weights
      (QRY-3, LIB-11).
- [x] Missing or corrupt Index: `search` fails closed (QRY-4).
- [x] No pair reranker or LLM in `scheda` (QRY-5).
- [x] SCH-8 filters apply before ranking (QRY-6).
- [x] One-step and inverse walks match fixtures (REL-1–3). Multi-step is
      **later**.
- [x] OPS-2: hybrid `search` on the OPS-1 fixture finishes in tens of
      milliseconds.
- [x] OPS-4: with embedder weights already on disk, `search` needs no
      network.

---

## C10. Core acceptance

**Status:** done

Close only after C0–C8. C9 is **search** (`10` §3) and is not a core gate.
Re-run the five gates in `10` §2:

- [x] Every **core** code in §2 of this file has a passing check in `scheda`.
- [x] Fixture Vault readable without the library (STO-2) and through
      `get` (QRY-1).
- [x] Deleting the Index does not lose Records (STO-1, IDX-1).
- [x] Implementation source does not name a consumer's collections (SCH-1,
      MOD-1).
- [x] `auto_sync` off: writes and `commit` succeed with the network disabled
      (GIT-8, ISO-1).

When C10 is `done`, **core** is accepted for the Rust binding. **search** is
C9. This checklist stops there. **agent** is accepted in `12`. Next crate
work is `10` §1.

---

## 4. What this document does not decide

Calendar dates or crate version numbers (`10` §8).

Which git, YAML, FTS, or ANN crate the Rust binding pins. Quality class is
`06` §3–4 (BM25, HNSW-class ANN, RRF); the pin is a binding choice (`10` §8).

Exact Schema YAML key names — `05` §8.

Whether `open` refuses a dirty Index or only warns via `check` (`10` §7).

**agent** — `12`. Isolated roots and `encrypt` — order and slice are `10` §1 / §5.
