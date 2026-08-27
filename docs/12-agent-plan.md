# 12. Agent development plan

Document 12 of the Scheda package. Version 0.30, 27 August 2026.

Work order for the Rust binding of **agent** (`10` §4, `08`). It does not
add product rules. Codes and *Check:* lines in `03`–`09` stay normative.
Target surface: `07`, `13`, `14`. This file assigns them to stages
and records whether the first crate has passed them.

The first implementation is the crate `scheda`. Other language bindings are
out of this plan (`10` §1). MCP is an example transport, not a stage here
(`08` AGT-4).

A0–A5b are done. A2 was the
**legacy** CLI; A4 replaced `rust/cli` with argv `14`. Do not revive A2
names. The unencrypted `{vault}/.isolated/` tree in `10` §1 (SCH-15,
ISO-2, ISO-4, ISO-6, IDX-4) is not this checklist. Age encryption
(ISO-5, ISO-7–11) stays out.

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

If `03`–`10`, `13`, or `14` do not decide something the stage needs, stop.
Amend the spec; do not invent a silent default in code. Do not copy
`rust/cli` names that are not on `07`.

Suggested crate order: A3 (`07` / `13`) → A4 (CLI `14`) → A5 (HTTP
embedder `14`) → A5b (dead `layout` parts) → A6 (ISO-3 on that surface)
→ A7. A5b does not depend on A5; it is numbered `b` so that A6 and A7
keep the names `10` and the CHANGELOG already use.

---

## 2. Dashboard

| Stage | Status | What closes |
|---|---|---|
| [A0](#a0-catalog-messages) | done | `catalog` in `api/v1` and on the library surface |
| [A1](#a1-catalog-from-schema) | done | AGT-1; isolated listed without sealed fields |
| [A2](#a2-thin-cli-legacy) | done | Legacy CLI; do not extend; replaced by A4 |
| [A3](#a3-target-library) | done | Public API = `07` / `13` |
| [A4](#a4-target-cli) | done | CLI argv = `14` (AGT-2–5) |
| [A5](#a5-http-embedder-and-reranker) | done | CLI OpenAI embeddings + Cohere rerank (`14`) |
| [A5b](#a5b-layout-clean-up) | done | Dead `layout` parts out; the `layout` key check exists |
| [A6](#a6-isolation-at-the-agent-surface) | not started | ISO-3 on CLI + library tools |
| [A7](#a7-agent-acceptance) | not started | `10` §4 acceptance |

**Agent in:** AGT-1–5. ISO-3 when isolated collections exist (`09`, with
**agent**). AGT-1 listing is `Vault.info` (`VaultInfo.collections`), not a
`catalog` command.

---

## 3. Out of this plan

These must not block A0–A7. Writes must not depend on them (`10` §4–5).

- MCP, HTTP, or a second transport. AGT-4 needs one thin encoding; `scheda`
  chooses CLI (`10` §4).
- ISO-5, ISO-7–11, LIB-5 `crypto`. AGT-5's *Check (**later**):* `crypto`
  waits on encryption.
- File watcher, `delete_one`, REL-4, extra language bindings (`10` §5).
- Collection-named tools or product rules in the CLI (`08`).
- Weights in this repository. `--model2vec-embedder` still accepts an
  operator folder (`14`). The crate does not ship a model (QRY-5). Prefer
  `--embedder-openai-url` / `--reranker-cohere-url` for Qwen3-class
  models. Local sidecars: `bazel run //python/vllm:embedder` and
  `//python/vllm:reranker` (locked `vllm` in `uv.lock`; GPU is the
  host's; Hub cache is `<workspace>/.cache/huggingface`).
- `walk` / `walk_inverse` / `scan` / `init` / `clone` / `catalog` as
  library or CLI names (`07`, `13`, `14`).
- `Collection.info`. Listing is `Vault.info`; Schema on a handle is
  `schema()`.

---

## A0. Catalog messages

**Status:** done

**Codes:** AGT-1 (name exists), `07` (catalog is **agent**, not LIB-2
**core**), `api/v1` Protobuf JSON mapping (`08` AGT-4).

`catalog` is not a **core** LIB-2 name. Agent transport still uses the
Protobuf JSON mapping of `api/v1`, not a Rust-only struct (`07` intro,
`08` AGT-4). If `07` and `api/v1` lack a catalog message that can carry
AGT-1, amend those files in this stage. Catalog content is Schema
serialized (`05` SCH-1, §8 keys): collections, kinds, fields, index marks,
edges. Do not infer names from directories. Do not
invent JSON keys that are not in `api/v1`.

A3 replaces the **library method name** `catalog` with `info`
(`VaultInfo`). A0 boxes stay as the message work that already shipped.

- [x] `07` names `catalog` as an **agent** operation (same mapping rules as
      LIB-2).
- [x] `api/v1` has a catalog message (or messages) whose Protobuf JSON
      mapping can hold AGT-1. Generate stubs (`bazel run //api/v1:generate`).
- [x] Library surface has `catalog` (body may be `todo!` until A1).
- [x] Caller-supplied catalog request messages, if any, fail protovalidate
      as `invalid` before Schema (`07` intro). There is no request envelope
      (LIB-20); constructed `Schema` / `Collection` / `Field`
      still fail `require_valid` as `invalid`.
- [x] Library path still has no `panic!` / `unwrap` / `expect` / `assert!`
      (`07` §5).

---

## A1. Catalog from Schema

**Status:** done

**Codes:** AGT-1, SCH-1, ISO-3 (catalog half).

Close only after A0. Schema may mark a collection `isolated: true` (`05`
§8). `catalog` on the main Vault lists those collections as isolated and
does not include their field values (ISO-3). Field lists and marks come
from Schema files for collections that are not isolated. `{vault}/.isolated/`
(ISO-2) is `10` §1 / `09`, not this stage.

A3 moves this listing to `Vault.info`.

- [x] Fixture Schema exported as JSON; collection names, kinds, fields,
      `fts` / `vector` / `filter` marks, and edges match the Schema files
      (AGT-1, SCH-1).
- [x] Catalog is not built by walking Record directories (SCH-1, AGT-1).
- [x] Two Vaults with different collection names: each `catalog` matches
      that Vault; grep of `rust/scheda/src` does not contain those names
      (MOD-1, SCH-1).
- [x] Isolated collection: `catalog` on the main Vault marks it isolated
      and omits its field values (ISO-3). `collection(name)` on this handle
      is how a caller reads those Records (`09` ISO-2).

---

## A2. Thin CLI (legacy)

**Status:** done

**Codes:** AGT-2, AGT-3, AGT-4, AGT-5, SCH-12, LIB-5.

The CLI that shipped under A2 (`find-one`, `insert-one`, `catalog`,
`init` / `clone`, `walk`, `scan`, `push-remote`). Leave it until A4
replaces `rust/cli`. Do not add flags or commands here.

- [x] Tool set is `07` operations that mutate or query Records and git,
      plus `catalog`, `create_collection`, `check`, `reindex`. No family of methods
      named after consumer types (AGT-2, SCH-1).
- [x] Scripted run `find` / `search` → `get` → `add` on a
      `journal` → `commit` uses only those tools (AGT-2).
- [x] `create_collection` then `add` without a hand-written Schema file
      (LIB-21, AGT-3).
- [x] `add` / `replace` with an extra field → `invalid`; the file
      is not created at a rogue path (AGT-3, SCH-12). No “write these
      bytes at a path” tool.
- [x] Same Vault, same operation: library call vs CLI — JSON of the Record
      (and error `code`) match (AGT-4).
- [x] Each result includes, as applicable: `id`, path, `valid`,
      search scores; on failure a LIB-5 `code`. A human message is optional
      and does not replace `code` (AGT-5).
- [x] Contract tests for `not_found`, `invalid`, `locked`,
      `index_unusable` (AGT-5). Not `crypto` (later).
- [x] CLI with `auto_sync` off does not require the network for local
      `catalog` / `get` / `add` / `commit` (ISO-1, GIT-8).

---

## A3. Target library

**Status:** done

**Codes:** LIB-2, LIB-4, LIB-7–11, LIB-13–17, `13`, QRY-5.

Close only after A1. Public crate API is `07` / `13`. `Embedder` /
`Reranker` live in `rust/scheda/src/embed.rs` (not `api/v1`).

- [x] `lib.rs` exports `Vault`, `Collection`, `Embedder`, `Reranker`.
      `Embedder` / `Reranker` are in `embed.rs`. Public names `init`, `clone`, `catalog`, `walk`,
      `walk_inverse`, `scan`, `find_one`, `insert_one`, `replace_one`,
      `push_remote` / `pull_remote` / `sync_remote` are gone.
- [x] `Vault::open(path, OpenOptions, Option<Arc<dyn Embedder>>)` as in
      `07`. Path exists → open (`remote_repo` ignored). Path missing +
      `remote_repo` → clone. Path missing → `git init`. Path exists but is
      not a Vault → `invalid`.
- [x] `Vault::info` returns `api.v1.VaultInfo` (collections + nested
      Schema). No `catalog()` method. AGT-1 listing is this message.
- [x] `create_collection(name, schema)`. Schema has no `name` / `layout`.
      `collection(name)` returns the handle.
- [x] Collection: `name`, `schema`, `add(id, RecordAdd)`, `get(id)`,
      `replace(id, RecordReplace)`, `history(id, offset, count)`,
      `find(query, offset, count)`, `search(query, reranker?)`.
      No `Collection::info`.
- [x] `Vault::find(collections, query, offset, count)` and
      `Vault::search(collections, query, reranker?)` (`07` LIB-10, LIB-11,
      `13`). Empty names = every non-isolated collection on this handle.
      Unknown name → `not_found`; isolated → `unsupported`. Collection
      `find` / `search` call those with `&[self.name]`. `Record.collection`
      / `Hit.collection` are output only. `FindQuery.include_raw` /
      `SearchQuery.include_raw` default off.
- [x] `find`: Tantivy Query tree (`13` §3). `count` `0` → `invalid`. No
      `scan`. `document` / `singleton` score order; `journal` append order.
- [x] `search`: `SearchQuery` (`k` only; no offset). RRF. Pool at least
      `max(k, 50)`. Optional `Reranker`; `Hit.rerank_score` only when set
      (`13` §6). No paging.
- [x] Embedder at `open` only (`13` §4). One vector per field (`13` §5).
      No embedder → vector leg off. Library does not load a ranking model
      (QRY-5).
- [x] Tests and Clippy clean. No `panic!` / `unwrap` / `expect` / `assert!`
      in `src/` (`07` §5).

---

## A4. Target CLI

**Status:** done

**Codes:** AGT-2, AGT-3, AGT-4, AGT-5, SCH-12, LIB-5, `14`.

Close only after A3. Replace `rust/cli` with a thin encoding of `07`.
Argv is `14`. JSON is the Protobuf JSON mapping of `api/v1`. Operator
Model2Vec dirs are checked here (missing required files → `invalid`). HTTP
embedder / reranker is A5.

- [x] Globals: `--vault PATH` (required), `--auto-sync`,
      `--model2vec-embedder DIR` (`config.json` + `tokenizer.json` +
      `model.safetensors`; missing → `invalid`; unset → no vector leg).
- [x] `open [--remote-repo URL]`. No `init` / `clone`. `--remote-repo` is
      not global.
- [x] Vault commands: `info`, `create-collection NAME --schema`,
      `collection NAME <subcommand>`, `begin`, `commit -m`, `discard`,
      `check`, `reindex`, `find`, `search`, `push`, `pull`, `sync`. No `catalog`.
- [x] Vault `find [--collections NAME,…] --filter --offset --count` and
      Vault `search [--collections NAME,…] --query` (`14`). Unset
      `--collections` → every non-isolated collection.
      Unknown name → `not_found`; isolated → `unsupported`. Collection
      `find` / `search` have no `--collections`. JSON includes
      `Record.collection` / `Hit.collection`.
- [x] `collection NAME` subcommands: `schema`, `add`, `get`, `history`,
      `replace`, `find`, `search`. No `--collection` flag.
- [x] `search --query JSON` (or stdin) [`--reranker-cohere-url` /
      `--reranker-model`]. No `--text` / `--query` dual path. Reranker
      flags only on `search` (unset → RRF).
- [x] Gone: `find-one`, `insert-one`, `replace-one`, `scan`, `walk`,
      `walk-inverse`, `push-remote`, `pull-remote`, `sync-remote`,
      `history --path`, Record path from the caller.
- [x] Scripted run `find` / `search` → `get` → `add` on a `journal` →
      `commit` uses only those tools (AGT-2).
- [x] `create-collection` then `add` without a hand-written Schema file
      (LIB-21, AGT-3). Extra field → `invalid` (SCH-12).
- [x] Same Vault, same operation: library vs CLI JSON and error `code`
      match (AGT-4, AGT-5).
- [x] `auto_sync` off does not require the network for local `info` /
      `get` / `add` / `commit` (ISO-1, GIT-8).

---

## A5. HTTP embedder and reranker

**Status:** done

**Codes:** IDX-8, QRY-5, `13` §4–6, `14`.

Close only after A4. A4 argv and Model2Vec folder checks stay. This stage
wires `--embedder-openai-url` / `--reranker-cohere-url`. The **library**
still does not call HTTP (QRY-5); only `rust/cli` does. ISO-1: those
flags are opt-in; `auto_sync` off still does not call git remotes. Local
Qwen sidecars are `bazel run //python/vllm:embedder` and `:reranker`
(locked `vllm` in `uv.lock`, host NVIDIA driver; not Echo sandbox
hermeticity). Hub weights go to `<workspace>/.cache/huggingface`
(`HF_HOME`). Opt-in Jina sidecars (`:embedder_jina` / `:reranker_jina`,
CC BY-NC 4.0) share those ports. Testdata driver:
`bazel run //python/cli:cli_LLMs` (Qwen HTTP), `//python/cli:cli_jina`
(Jina HTTP), and `//python/cli:cli_model2vec` (local Model2Vec cache).
Not a library check. Crate Check:
`http_embedder_writes_vectors_and_scores_match` in `rust/cli/tests/cli.rs`
(loopback OpenAI/Cohere stub).

- [x] `--embedder-openai-url` + `--embedder-model` +
      `--embedder-dimensions`: `POST` OpenAI embeddings JSON; implement
      `Embedder`; pass it to `open`. All three or none (`invalid` if
      partial). `http`/`https` only. `--embedder-api-key-env-var` when
      set (Bearer from that env; missing/empty → `invalid`); unset → no
      auth. Unset HTTP flags still turn the vector leg off. Not with
      `--model2vec-embedder`.
- [x] `add` / `replace` / `reindex` with the embedder set write per-field
      vectors (`13` §5). `search` vector leg retrieves those ids (QRY-3
      synonym fixture: hit from the vector leg).
- [x] `--reranker-cohere-url` + `--reranker-model` on `search` only:
      implement `Reranker`; `POST` Cohere/Jina `{ query, documents,
      model }`. Both flags or none (`invalid` if partial). Hits come
      back with `rerank_score`. Unset → RRF, no `rerank_score`.
      `--reranker-api-key-env-var` when set (same Bearer rule as the
      embedder).
- [x] Same endpoint, library `Embedder` / `Reranker` vs CLI: vector /
      rerank scores match (AGT-4). `auto_sync` off still does not call
      git remotes (ISO-1).
- [x] Tests and Clippy clean. No `panic!` / `unwrap` / `expect` / `assert!`
      in `rust/cli/src` (`07` §5).

---

## A5b. Layout clean-up

**Status:** done

**Codes:** SCH-2, MOD-8, LIB-21, ISO-2 (`09` §2).

Schema has no `layout`. A path template in a file a human edits is
data-driven path construction: `../`, an absolute path, `.git/`,
`.scheda/`, `.isolated/`, a symlink escape, a Windows device name, or two
Records deriving one path. Record paths are fixed by kind (MOD-8), and
that decision stands. What is left is the wreckage of the removal: a dead
field on a public struct is an invitation to fill it again, and the
negative check three documents promise has no test. Deletion plus that
test, not a feature.

Independent of A5 — this stage touches Schema and tests, not the CLI.
Close it before A6 so isolation lands on a Schema with no dead path
machinery.

- [x] `CollectionDecl.layout` and `Collection::layout()` are gone from
      `rust/scheda/src`. Removing a public accessor is a CHANGELOG entry;
      the version number stays `10` §8, not this plan.
- [x] Fixtures carrying the field are gone: `layout: Vec::new()` in
      `schema.rs` and `store.rs`, `layout: vec!["c/{id}.md"]` in the
      `vault.rs` unit test.
- [x] Tests asserting the accessor are gone: `catalog.rs`
      (`written.layout().is_empty()`), `schema.rs` (`collection.layout()`).
      `tests/document.rs::extra_layout_slots_write_derived_path` is
      deleted or renamed to what it now asserts — a flat `{name}/{id}.md`
      path. A test whose name promises the opposite of its body is worse
      than no test.
- [x] New negative test, the *Check:* SCH-2, MOD-8, and LIB-21 already
      state: a Schema file with a `layout` key → `invalid` on `open` /
      `check`; `create_collection` with a leftover `layout` key →
      `invalid` and no file written. `RawCollection` denies unknown keys
      today, so this pins existing behaviour rather than changing it.
- [x] `09` ISO-2 drops “Layout templates are relative to that directory”.
      Isolated Records use the same fixed names under `.isolated/{name}/`.
- [x] Tests and Clippy clean. `bazel test //...` green.

---

## A6. Isolation at the agent surface

**Status:** not started

**Codes:** ISO-3, AGT-1.

Close only after A5. Unencrypted isolated is already in (`10` §1). This
stage is the agent half of isolation, not encryption.

- [ ] Agent tools bound to the main Vault never return a fixture sealed
      value: `info`, `get`, `find`, `search` (ISO-3).
- [ ] `Vault.info` still lists isolated collections as isolated and does
      not include their field values (ISO-3, A1 / A3).
- [ ] Reaching isolated Records requires `collection(name)` on this handle
      (ISO-2). CLI `collection NAME` for a non-isolated collection must not
      return isolated Record values.

---

## A7. Agent acceptance

**Status:** not started

Close only after A0–A6. Re-run the three gates in `10` §4:

- [ ] `Vault.info` JSON matches Schema files (AGT-1).
- [ ] Tool writes cannot bypass SCH-12 (AGT-3).
- [ ] Library vs transport JSON (and `code`) match on one Vault (AGT-4).

When A7 is `done`, **agent** is accepted for the Rust binding. Encryption
of `{vault}/.isolated/` stays `10` §5.

---

## 4. What this document does not decide

Calendar dates or crate version numbers (`10` §8).

CLI argv is `14`. Binary crate layout and whether the agent process is the
writer stay `08` §3. Lock rules stay STO-4.

Exact MCP tool names — out of this plan.

Schema proto field names — `api/v1` `schema.proto` (`05` §8 keys). Pretty-print of
Schema YAML stays `05` §9.

Whether `open` refuses a dirty Index or only warns via `check` (`10` §7).

Vault hooks: already the consumer's (`04`). A CLI note must not recurse
into the same write lock (`10` §7); this plan does not design hooks.

ISO-5 `encrypt` / `secret` / `crypto`.
