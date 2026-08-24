# 12. Agent development plan

Document 12 of the Scheda package. Version 0.2, 24 August 2026.

Work order for the Rust binding of **agent** (`10` §4, `08`). It does not
add product rules. Codes and *Check:* lines in `03`–`09` stay normative.
This file only assigns them to stages and records whether the first crate
has passed them.

The first implementation is the crate `scheda`. Other language bindings are
out of this plan (`10` §1). MCP is an example transport, not a stage here
(`08` AGT-4).

Do not start A0 until the unencrypted isolated slice in `10` §1 is in the
crate (SCH-15, ISO-2, ISO-4, ISO-6, IDX-4). That slice is not this
checklist. Age encryption (ISO-5, ISO-7–11) stays out.

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

If `03`–`10` do not decide something the stage needs (for example catalog
message fields in `api/v1`), stop. Amend the spec; do not invent a silent
default in code.

Suggested crate order after the unencrypted isolated slice (`10` §1):
library `catalog` → thin CLI of `07` + `catalog` → ISO-3 on the main Vault
→ agent acceptance.

---

## 2. Dashboard

| Stage | Status | What closes |
|---|---|---|
| [A0](#a0-catalog-messages) | not started | `catalog` in `api/v1` and on the library surface |
| [A1](#a1-catalog-from-schema) | not started | AGT-1; isolated listed without sealed fields |
| [A2](#a2-thin-cli) | not started | AGT-2, AGT-3, AGT-4, AGT-5 |
| [A3](#a3-isolation-at-the-agent-surface) | not started | ISO-3 on CLI + library tools |
| [A4](#a4-agent-acceptance) | not started | `10` §4 acceptance |

**Agent in:** AGT-1–5. ISO-3 when isolated collections exist (`09`, with
**agent**).

---

## 3. Out of this plan

These must not block A0–A4. Writes must not depend on them (`10` §4–5).

- MCP, HTTP, or a second transport. AGT-4 needs one thin encoding; `scheda`
  chooses CLI (`10` §4).
- ISO-5, ISO-7–11, LIB-5 `crypto`. AGT-5's *Check (**later**):* `crypto`
  waits on encryption.
- File watcher, `delete_one`, REL-4, extra language bindings (`10` §5).
- Collection-named tools, scoring, or product rules in the CLI (`08`).

---

## A0. Catalog messages

**Status:** not started

**Codes:** AGT-1 (name exists), `07` (catalog is **agent**, not LIB-2
**core**), `api/v1` Protobuf JSON mapping (`08` AGT-4).

`catalog` is not a **core** LIB-2 name. Agent transport still uses the
Protobuf JSON mapping of `api/v1`, not a Rust-only struct (`07` intro,
`08` AGT-4). If `07` and `api/v1` lack a catalog message that can carry
AGT-1, amend those files in this stage. Catalog content is Schema
serialized (`05` SCH-1, §8 keys): collections, kinds, fields, index marks,
edges, layout templates. Do not infer names from directories. Do not
invent JSON keys that are not in `api/v1`.

- [ ] `07` names `catalog` as an **agent** operation (same mapping rules as
      LIB-2).
- [ ] `api/v1` has a catalog message (or messages) whose Protobuf JSON
      mapping can hold AGT-1. Generate stubs (`bazel run //api/v1:generate`).
- [ ] Library surface has `catalog` (body may be `todo!` until A1).
- [ ] Caller-supplied catalog request messages, if any, fail protovalidate
      as `invalid` before Schema (`07` intro).
- [ ] Library path still has no `panic!` / `unwrap` / `expect` / `assert!`
      (`07` §5).

---

## A1. Catalog from Schema

**Status:** not started

**Codes:** AGT-1, SCH-1, ISO-3 (catalog half).

Close only after A0. Isolated collections already exist in Schema (`10`
§1). `catalog` on the main Vault lists them as isolated and does not
include their field values (ISO-3). Field lists and marks come from Schema
files for collections that are not isolated.

- [ ] Fixture Schema exported as JSON; collection names, kinds, layout
      templates, fields, `fts` / `vector` / `filter` marks, and edges match
      the Schema files (AGT-1, SCH-1).
- [ ] Catalog is not built by walking Record directories (SCH-1, AGT-1).
- [ ] Two Vaults with different collection names: each `catalog` matches
      that Vault; grep of `rust/scheda/src` does not contain those names
      (MOD-1, SCH-1).
- [ ] Isolated collection: `catalog` on the main Vault marks it isolated
      and omits its field values (ISO-3). Opening the isolated handle is
      how a caller reads those Records (`09` ISO-2).

---

## A2. Thin CLI

**Status:** not started

**Codes:** AGT-2, AGT-3, AGT-4, AGT-5, SCH-12, LIB-5.

Close only after A1. The CLI is a thin encoding of `07` + `catalog`. Exact
argv and binary path are this binding's choice (`08` §3) as long as the
boxes below pass. JSON of Records and errors is the Protobuf JSON mapping
of `api/v1`.
There is no tool that writes raw bytes at a path.

Include when a Remote exists: `push_remote`, `pull_remote`, `sync_remote`,
and `auto_sync` as an `open` option — same meaning as `04`. Do not add a
second remote product tool (AGT-2).

`walk` / `walk_inverse` are **search** LIB-2 names; expose them if the CLI
covers `07`. The AGT-2 scripted run does not require them. `history` stays
optional (LIB-2).

- [ ] Tool set is `07` operations that mutate or query Records and git,
      plus `catalog`, `check`, `reindex`. No family of methods named after
      consumer types (AGT-2, SCH-1).
- [ ] Scripted run `scan` / `search` → `find_one` → `insert_one` on a
      `journal` → `commit` uses only those tools (AGT-2).
- [ ] `insert_one` / `replace_one` with an extra field or a path outside
      the layout → `invalid`; the file is not created at a rogue path
      (AGT-3, SCH-12). No “write these bytes at this path” tool.
- [ ] Same Vault, same operation: library call vs CLI — JSON of the Record
      (and error `code`) match (AGT-4).
- [ ] Each result includes, as applicable: `id`, collection, path, `valid`,
      search scores; on failure a LIB-5 `code`. A human message is optional
      and does not replace `code` (AGT-5).
- [ ] Contract tests for `not_found`, `invalid`, `locked`,
      `index_unusable` (AGT-5). Not `crypto` (later).
- [ ] CLI with `auto_sync` off does not require the network for local
      `catalog` / `find_one` / `insert_one` / `commit` (ISO-1, GIT-8).

---

## A3. Isolation at the agent surface

**Status:** not started

**Codes:** ISO-3, AGT-1.

Close only after A2. Unencrypted isolated is already in (`10` §1). This
stage is the agent half of isolation, not encryption.

- [ ] Agent tools bound to the main Vault never return a fixture sealed
      value: `catalog`, `find_one`, `find`, `scan`, `search` (ISO-3).
- [ ] `catalog` on the main Vault still lists isolated collections as
      isolated and does not include their field values (ISO-3, A1).
- [ ] Reaching isolated Records requires opening that root on purpose
      (ISO-2). CLI on the main Vault path must not silently open it.

---

## A4. Agent acceptance

**Status:** not started

Close only after A0–A3. Re-run the three gates in `10` §4:

- [ ] Catalog JSON matches Schema files (AGT-1).
- [ ] Tool writes cannot bypass SCH-12 (AGT-3).
- [ ] Library vs transport JSON (and `code`) match on one Vault (AGT-4).

When A4 is `done`, **agent** is accepted for the Rust binding. Encryption
of an isolated root stays `10` §5.

---

## 4. What this document does not decide

Calendar dates or crate version numbers (`10` §8).

Exact CLI argument names, binary crate layout, and whether the agent
process is the writer (`08` §3). Lock rules stay STO-4.

Exact MCP tool names — out of this plan.

Catalog proto field names — `api/v1` after A0 amends it. Pretty-print of
Schema YAML stays `05` §9.

Whether `open` refuses a dirty Index or only warns via `check` (`10` §7).

Vault hooks: already the consumer's (`04`). A CLI note must not recurse
into the same write lock (`10` §7); this plan does not design hooks.

ISO-5 `encrypt` / `secret` / `crypto`.
