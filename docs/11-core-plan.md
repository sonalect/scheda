# 11. Core development plan

Document 11 of the Scheda package. Version 0.7, 23 August 2026.

Work order for the Rust binding of **core** (`10` §2). It does not add product
rules. Codes and *Check:* lines in `03`–`09` stay normative. This file only
assigns them to stages and records whether the first crate has passed them.

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

Suggested crate order is `10` §2: working tree + lock → Schema + `document` /
`singleton` → `journal` segments → `commit` / Remote → journal `id` map.

---

## 2. Dashboard

| Stage | Status | What closes |
|---|---|---|
| [C0](#c0-skeleton) | done | Names, error codes, stubs; crate builds |
| [C1](#c1-working-tree-and-lock) | done | `init` / `open`, lock, gitignored Index, ISO-1 local |
| [C2](#c2-schema) | done | Load and reject Schema |
| [C3](#c3-document-and-singleton) | done | Whole-file writes and `find_one` |
| [C4](#c4-journal-segments) | done | Append, rotate, scan `find_one` |
| [C5](#c5-find-check-reindex) | not started | Filters, `check`, disposable Index |
| [C6](#c6-local-commit-and-history) | not started | `commit`, `history` of a path |
| [C7](#c7-remote) | not started | `clone`, `auto_sync`, push/pull/sync |
| [C8](#c8-journal-id-map-and-size) | not started | IDX-5, OPS-1, OPS-3 |
| [C9](#c9-core-acceptance) | not started | `10` §2 acceptance |

**Core in:** MOD-1–18; STO-1–5; GIT-1–13; SCH-1–8, SCH-12–14; IDX-1, IDX-2,
IDX-5; QRY-1, QRY-2, QRY-7; LIB-1–10, LIB-12–18; ISO-1; OPS-1, OPS-3.

---

## 3. Out of core (stubs)

These must **compile**. They must not be required to pass C0–C9. Writes must
not depend on them (`10` §2).

Return `unsupported` (or a documented stub that does not write) for: `search`
(LIB-11), walks (REL-1–3), isolated/`encrypt` clauses (ISO-2–11, SCH-15),
vector/lexical Index (IDX-3, IDX-4, IDX-6–9), hybrid query (QRY-3–6), agent
catalog (AGT-1–5), SCH-9–11 as **search** marks (store still ignores them for
ranking).

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
- [x] `insert_one` / `replace_one` reject a `Record` that fails `api/v1`
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
- [x] Write lock: second write `open` → `locked`; a reader `find_one` is not
      blocked once C3 exists (STO-4). Until C3, lock vs second writer is
      enough.
- [x] `open` with network disabled still reports configured `origin` (GIT-7).
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
- [x] Filter-key marks parse; unmarked fields cannot be declared as filter
      keys in a way that C5 would honour (SCH-8).
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

- [x] `insert_one` / `replace_one` / `find_one` round-trip fields (QRY-1,
      LIB-7, LIB-8). Missing `id` → `not_found`, not an empty Record.
- [x] `document` Markdown+YAML and YAML-only; empty body allowed (MOD-11,
      MOD-14, STO-2). Body is stored; no search Index yet (SCH-7).
- [x] Header `id` matches path-derived `id` (MOD-13, SCH-6).
- [x] Path template with extra slots; `find` path-prefix is C5, but
      `insert_one` must write the derived path (MOD-8).
- [x] Rename of a `document` file so path ≠ `id` → `check` fails; `find_one`
      does not follow the rename (MOD-5). May land with C5 `check`.
- [x] Missing optional field is absent on read-back (MOD-12). Stored `null`
      stays `null` (MOD-18, SCH-5).
- [x] `id` pattern from Schema; library does not invent a consumer scheme
      (MOD-6).
- [x] `singleton`: Schema-fixed `id`; second `insert_one` rejected; then
      `replace_one` only (MOD-7, MOD-17).
- [x] `replace_one` is whole-file; no field patch (MOD-15, LIB-3).
- [x] Failed / interrupted write does not become the new file (STO-5).
- [x] Invalid Record vs current Schema: write rejected; old file `find_one`
      with `valid: false` (SCH-12, LIB-8).
- [x] Schema N+1 that only adds an optional field still reads N files
      (SCH-13).
- [x] A fixture Vault opens in an editor and in `git` with no Scheda (STO-2).

---

## C4. `journal` segments

**Status:** done

**Codes:** MOD-2–4, MOD-9–11, MOD-16, STO-3, STO-5, SCH-14, LIB-3, LIB-7,
LIB-9, LIB-18, QRY-7 (empty `find` / `scan`).

`find_one` **may scan segments** until C8 (IDX-5, `03` §6).

- [x] `insert_one` appends one RFC 8785 canonical JSON object plus `\n`
      (MOD-16, LIB-18).
- [x] `replace_one` → `unsupported` (MOD-2, MOD-16, LIB-3, LIB-9).
- [x] Duplicate `id` across segments is rejected (MOD-4).
- [x] After the size cap, the next `insert_one` does not grow the closed
      file; previous bytes unchanged (MOD-10, SCH-14).
- [x] `find_one` uses `(collection, id)`, not a segment name (MOD-3, MOD-9).
- [x] Empty `find` / `scan` after insert A, B, rotate, insert C → A, B, C
      (QRY-7, LIB-10).
- [x] Mid-segment corruption → `check` fails; Scheda does not rewrite the
      file to “fix” it (STO-3, MOD-16).
- [x] A tail without `\n` is not a Record (STO-3, STO-5).

---

## C5. `find`, `check`, `reindex`

**Status:** not started

**Codes:** QRY-2, LIB-10, LIB-15, LIB-16, IDX-1, IDX-2, STO-1, GIT-4, SCH-8,
SCH-12.

- [ ] SCH-8 filters: equality (including `null`), set, existence, `document`
      path-prefix. Unmarked field → error. Empty filter returns every Record
      (QRY-2, LIB-10).
- [ ] `scan` is empty `find` or a documented alias (LIB-10).
- [ ] `check` reports Schema, encoding, journal lines, `id` uniqueness,
      conflict markers — with a code and path; it does not write Records
      (LIB-15, GIT-4, STO-2, STO-3).
- [ ] Conflict markers in a `document` → unreadable; `check` fails (GIT-4).
- [ ] `reindex` rebuilds the Index from files; returns Records read, skipped
      invalid, segments seen (IDX-1, LIB-16).
- [ ] Delete the Index, `reindex`; Records match the files (STO-1, IDX-1).
- [ ] Index format version: unknown/corrupt → `reindex` or `index_unusable`;
      never another Vault's ids (IDX-2). `find_one` still reads files.
- [ ] Invalid old files stay `find_one`-able with `valid: false` (SCH-12).

---

## C6. Local `commit` and `history`

**Status:** not started

**Codes:** GIT-2, GIT-3, LIB-12, LIB-17.

- [ ] Several `insert_one` without `commit` → `git log` unchanged (GIT-2).
- [ ] One `commit(message)` → one commit whose paths are those writes, not
      `git add` of junk at the Vault root (GIT-2, LIB-12).
- [ ] `history` of a `document` / `singleton` / segment path matches
      `git show` (GIT-3, LIB-17). Not line-level journal blame.

---

## C7. Remote

**Status:** not started

**Codes:** GIT-8–12, LIB-13, LIB-14, ISO-1.

- [ ] `clone(url, path)` produces a Vault whose `origin` is that URL;
      `open`+`remote_repo` on an existing repo fails (GIT-9).
- [ ] `auto_sync` default off: `open` does not change `HEAD` when Remote has
      new commits (GIT-10).
- [ ] `auto_sync` on + fast-forward → `HEAD` matches Remote after `open`;
      diverged → `not_fast_forward`, no conflict markers from Scheda
      (GIT-10).
- [ ] `pull_remote` / `push_remote` / `sync_remote` with `auto_sync` off
      (GIT-11, LIB-14). Diverged `sync_remote` fails before push.
- [ ] Unreachable Remote after `commit`: `HEAD` is the new commit; later
      `push_remote` publishes it (GIT-12).
- [ ] Writes and local `commit` with `auto_sync` off succeed with the
      network disabled (GIT-8, ISO-1, `10` §2 item 5).

---

## C8. Journal `id` map and size

**Status:** not started

**Codes:** IDX-5, OPS-1, OPS-3.

- [ ] `find_one` on `journal` does not scan every unrelated segment (IDX-5).
      Writer updates the map under the lock; human editor edits still need
      `reindex`.
- [ ] Fixture ≥ 10⁴ journal Records and ≥ 10³ `document` Records: sampled
      `find_one` / journal `insert_one` stay in milliseconds, not hundreds
      (OPS-1).
- [ ] Full `reindex` on that fixture completes; counts match a hand tally
      (OPS-3, LIB-16).

---

## C9. Core acceptance

**Status:** not started

Close only after C0–C8. Re-run the five gates in `10` §2:

- [ ] Every **core** code in §2 of this file has a passing check in `scheda`.
- [ ] Fixture Vault readable without the library (STO-2) and through
      `find_one` (QRY-1).
- [ ] Deleting the Index does not lose Records (STO-1, IDX-1).
- [ ] Implementation source does not name a consumer's collections (SCH-1,
      MOD-1).
- [ ] `auto_sync` off: writes and `commit` succeed with the network disabled
      (GIT-8, ISO-1).

When C9 is `done`, **core** is accepted for the Rust binding. **search**,
**agent**, and **later** stay out.

---

## 4. What this document does not decide

Calendar dates or crate version numbers (`10` §8).

Which git or YAML crate the Rust binding pins.

Exact Schema YAML key names — `05` §8.

Whether `open` refuses a dirty Index or only warns via `check` (`10` §7).

**search**, **agent**, isolated roots, and `encrypt`.
