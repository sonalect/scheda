# 12. Agent development plan

Document 12 of the Scheda package. Version 0.66, 31 August 2026.

Work order for the Rust binding of **agent** (`10` §4, `08`). It does not
add product rules. Codes and *Check:* lines in `03`–`09` stay normative.
Target surface: `07`, `13`, `14`. This file assigns them to stages
and records whether the first crate has passed them.

The first implementation is the crate `scheda`. Other language bindings are
out of this plan (`10` §1). MCP is an example transport, not a stage here
(`08` AGT-4).

A0–A7 are done. **agent** is accepted for this binding. A2 was the
**legacy** CLI; A4 replaced `rust/cli` with argv `14`. Do not revive A2
names. Schema already parses `isolated`. Age encryption (ISO-5,
ISO-7–11) stays out.

Stages P0–P13 in §4 are the second half of this file: what the **first
consumer** needs before it can move its primary layer onto the crate.
They are not **agent**, and they do not reopen A0–A7. Their codes cross
**core**, **search**, and **later**, so the unencrypted
`{vault}/.isolated/` tree of `10` §1 (ISO-2 nested git, ISO-4, ISO-6,
IDX-4) is P4 here instead of being out of scope. Accepting **agent**
never waited on a P stage and still does not.

§6 lists what a *second* consumer will want and Scheda does not have.
Those are open questions, not stages: nothing there blocks A0–A7 or
P0–P13.

---

## 1. How to track progress

Tick a box only when the cited *Check:* passes under `bazel test //...` (or the
named `#[test]`). Do not tick from a verbal claim that the code is ready.

| Mark  | Meaning                                  |
| ----- | ---------------------------------------- |
| `[ ]` | Not done                                 |
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

P order is in §4. It starts after A7 and does not interleave with it.

---

## 2. Dashboard

| Stage                                    | Status | What closes                                            |
| ---------------------------------------- | ------ | ------------------------------------------------------ |
| [A0](#a0-catalog-messages)               | done   | `VaultInfo` in `api/v1`; listing is `info`             |
| [A1](#a1-catalog-from-schema)            | done   | AGT-1; isolated listed without sealed fields           |
| [A2](#a2-thin-cli-legacy)                | done   | Legacy CLI; do not extend; replaced by A4              |
| [A3](#a3-target-library)                 | done   | Public API = `07` / `13`                               |
| [A4](#a4-target-cli)                     | done   | CLI argv = `14` (AGT-2–5)                              |
| [A5](#a5-http-embedder-and-reranker)     | done   | CLI OpenAI embeddings + Cohere rerank (`14`)           |
| [A5b](#a5b-layout-clean-up)              | done   | Dead `layout` parts out; the `layout` key check exists |
| [A6](#a6-isolation-at-the-agent-surface) | done   | ISO-3 on CLI + library tools                           |
| [A7](#a7-agent-acceptance)               | done   | `10` §4 acceptance                                     |

**Agent in:** AGT-1–5. ISO-3 when isolated collections exist (`09`, with
**agent**). AGT-1 listing is `Vault.info` (`VaultInfo.collections`), not a
`catalog` command.

Consumer readiness has its own dashboard in §4. A green A0–A7 does not
imply a green P0–P13, and the reverse is also true.

---

## 3. Out of A0–A7

These must not block A0–A7. Writes must not depend on them (`10` §4–5).
Some of them are consumer requirements and come back as P stages in §4;
that is noted on the line. Coming back does not restore an A2 name.

- MCP, HTTP, or a second transport. AGT-4 needs one thin encoding; `scheda`
  chooses CLI (`10` §4).
- ISO-5, ISO-7–11, LIB-5 `crypto`. AGT-5's *Check (**later**):* `crypto`
  waits on encryption.
- File watcher, removing a Record, REL-4, extra language bindings
  (`10` §5). Removal comes back as P9, under the name `remove`; the A2
  name `delete_one` does not.
  The watcher question is open in the consumer's requirements too; no P
  stage takes it, so `reindex` after a human edit stays the answer.
- Collection-named tools or product rules in the CLI (`08`).
- Weights in this repository. `--model2vec-embedder` still accepts an
  operator folder (`14`). The crate does not ship a model (QRY-5). Prefer
  `--embedder-openai-url` / `--reranker-cohere-url` for Qwen3-class
  models. Local sidecars: `bazel run //python/vllm:embedder` and
  `//python/vllm:reranker` (locked `vllm` in `uv.lock`; GPU is the
  host's; Hub cache is `<workspace>/.cache/huggingface`).
- `walk` / `walk_inverse` / `scan` / `init` / `clone` / `catalog` as
  library or CLI names (`07`, `13`, `14`). Ordered journal read without
  the Index is `lines` (`07` LIB-19), not the A2 `scan`.
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
(`VaultInfo`). A0 boxes below record the message work that shipped; they
do not restore a `catalog()` method.

- [x] `07` names AGT-1 listing as `info` (LIB-4, LIB-20), not a `catalog()`
      method.
- [x] `api/v1` has a listing message (`VaultInfo` with nested `Schema`) whose
      Protobuf JSON mapping can hold AGT-1. Generate stubs (`bazel run //api/v1:generate`).
- [x] Library surface has `Vault::info` (supersedes A0's `catalog()` name).
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
§8). `info` on the main Vault lists those collections as isolated and
does not include their field values (ISO-3). Field lists and marks come
from Schema files for collections that are not isolated. `{vault}/.isolated/`
(ISO-2) is `10` §1 / `09`, not this stage.

A3 moves this listing to `Vault.info`.

- [x] Fixture Schema exported as JSON; collection names, kinds, fields,
      `fts` / `vector` / `filter` marks, and edges match the Schema files
      (AGT-1, SCH-1).
- [x] Catalog is not built by walking Record directories (SCH-1, AGT-1).
- [x] Two Vaults with different collection names: each `info` matches
      that Vault; grep of `rust/scheda/src` does not contain those names
      (MOD-1, SCH-1).
- [x] Isolated collection: `info` on the main Vault marks it isolated
      and omits its field values (ISO-3). Record I/O for that name is
      `collection(name)` (ISO-2 / P4). This stage only required the
      listing. `find` / `search` that name an isolated collection stay
      `unsupported` (main Index).

---

## A2. Thin CLI (legacy)

**Status:** done

**Codes:** AGT-2, AGT-3, AGT-4, AGT-5, SCH-12, LIB-5.

The CLI that shipped under A2 (`find-one`, `insert-one`, `catalog`,
`init` / `clone`, `walk`, `scan`, `push-remote`). Leave it until A4
replaces `rust/cli`. Do not add flags or commands here.

- [x] Tool set is `07` operations that mutate or query Records and git,
      plus `create_collection`. No family of methods
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
      `index_version` (AGT-5). Not `crypto` (later).
- [x] CLI with `auto_sync` off does not require the network for local
      `info` / `get` / `add` / `commit` (ISO-1, GIT-8).

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
      Unknown name → `not_found`; isolated name on `find` / `search` →
      `unsupported`. `collection(name)` for an isolated name is still the
      Record handle. Collection `find` / `search` call those with
      `&[self.name]`. `Record.collection` / `Hit.collection` are output
      only. `FindQuery.include_raw` / `SearchQuery.include_raw` default
      off.
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
      `collection NAME <subcommand>`, `check`, `reindex`, `find`,
      `search`, `push`, `pull`, `sync`. No `catalog`. No `begin` /
      `commit` / `discard` (GIT-2: CLI autocommits each mutation).
- [x] Vault `find [--collections NAME,…] --filter --offset --count` and
      Vault `search [--collections NAME,…] --query` (`14`). Unset
      `--collections` → every non-isolated collection.
      Unknown name → `not_found`; isolated name on `find` / `search` →
      `unsupported`. CLI `collection NAME` get/add/replace/remove/history
      on an isolated name still uses that handle. Collection
      `find` / `search` have no `--collections`. JSON includes
      `Record.collection` / `Hit.collection`.
- [x] `collection NAME` subcommands: `schema`, `add`, `get`, `history`,
      `replace`, `find`, `search`. No `--collection` flag.
- [x] `search --query JSON` (or stdin) [`--reranker-cohere-url` /
      `--reranker-model`]. No `--text` / `--query` dual path. Reranker
      flags only on `search` (unset → RRF).
- [x] Gone: `find-one`, `insert-one`, `replace-one`, `scan`, `walk`,
      `walk-inverse`, `push-remote`, `pull-remote`, `sync-remote`,
      `history --path`, Record path from the caller, `begin`, `commit`,
      `discard`.
- [x] Scripted run `find` / `search` → `get` → `add` on a `journal`
      uses only those tools (AGT-2). CLI `add` autocommits.
- [x] `create-collection` then `add` without a hand-written Schema file
      (LIB-21, AGT-3). Extra field → `invalid` (SCH-12).
- [x] Same Vault, same operation: library vs CLI JSON and error `code`
      match (AGT-4, AGT-5).
- [x] `auto_sync` off does not require the network for local `info` /
      `get` / `add` (ISO-1, GIT-8). CLI `add` autocommits.

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

**Status:** done

**Codes:** ISO-3, AGT-1.

Close only after A5. This is the agent half of isolation, not encryption.
Nested `{vault}/.isolated/` I/O is `10` §1 / P4. Isolated Records are
`collection(name)` on this handle. `find` / `search` that name an
isolated collection are `unsupported` (main Index). Crate Check:
`isolated_sealed_values_stay_off_the_main_vault` in
`rust/scheda/tests/catalog.rs` and `cli_omits_isolated_sealed_values` in
`rust/cli/tests/cli.rs`.

- [x] Agent tools bound to the main Vault never return a fixture sealed
      value: `info`, `get`, `find`, `search` (ISO-3).
- [x] `Vault.info` still lists isolated collections as isolated and does
      not include their field values (ISO-3, A1 / A3).
- [x] Reaching isolated Records is `collection(name)` (ISO-2 / P4).
      `find` / `search` that name an isolated collection are `unsupported`.
      CLI `collection NAME` `schema` / `get` / `add` / `replace` /
      `remove` / `history` on an isolated name use that handle. CLI
      `collection NAME` for a non-isolated collection must not return
      isolated Record values.

---

## A7. Agent acceptance

**Status:** done

Close only after A0–A6. Re-run the three gates in `10` §4. Crate Check:
`info_json_matches_schema_files` (info nested Schema equals
`collection.schema()` parsed from files), `tool_writes_cannot_bypass_schema`,
and `library_and_cli_json_and_code_match` (`info`, `get`, `find`, `search`,
`not_found`, illegal slug `invalid`) in `rust/cli/tests/agent.rs`.

- [x] `Vault.info` JSON matches Schema files (AGT-1).
- [x] Tool writes cannot bypass SCH-12 (AGT-3).
- [x] Library vs transport JSON (and `code`) match on one Vault (AGT-4).

When A7 is `done`, **agent** is accepted for the Rust binding. Encryption
of `{vault}/.isolated/` stays `10` §5.

---

## 4. First-consumer readiness

**Status of the part:** P0–P5, P8, P9, P10, P11, and P12 are done. P13 is
open: the consumer found that a `document` collection cannot be listed at
all in a build without `find`.

Two numbers are empty and stay empty, so the rest keep the names other
documents already use. P6 asked which of `04` STO-2 and `05` SCH-6 was
wrong about the `singleton` file; STO-2 was, since it promised a Schema
key that picks JSON or YAML and no such key has ever existed, and it now
states the file plainly. P7 recorded how one consumer would reshape its
own files to fit MOD-8, MOD-16, and STO-4. That is the consumer's
migration, not work on this crate (§5); what those three limits cost
*any* consumer is §6.

The first consumer moves a primary layer of text files under git onto the
crate: `document` collections it revises in place, and `journal`
collections it appends to and never rewrites. It keeps its own
requirements document for a storage engine, written with its own code
names. Those names collide with ours in spelling and not in number, so
this part does not cite them, and it does not name that consumer or its
collections (SCH-1, MOD-1). Every box below states behaviour and cites
**Scheda** codes (`03`–`10`).

A P stage closes the way an A stage does: the cited *Check:* passes under
`bazel test //...`. Two ground rules:

- A P stage may amend `03`–`10`, and several must. Where a box does, it
  names the document and the code. Amending the spec is the work; a
  silent default in `src/` is not.
- A P stage must not put a consumer's collection name in the crate.
  SCH-1 and MOD-1 stand: grep over `rust/scheda/src` does not contain
  those names.

### Dashboard

| Stage                                                    | Status      | What closes                                                         |
| -------------------------------------------------------- | ----------- | ------------------------------------------------------------------- |
| [P0](#p0-consumer-fixture)                               | done        | A fixture Vault shaped like the consumer's primary layer            |
| [P1](#p1-journal-line-fidelity)                          | done        | An integer stays an integer; the line is RFC 8785                   |
| [P2](#p2-ordered-journal-read-without-the-lexical-index) | done        | Reading a journal in order without Tantivy                          |
| [P3](#p3-many-writes-one-commit)                         | done        | Append per event without a commit per event                         |
| [P4](#p4-unencrypted-isolated-slice)                     | done        | ISO-2, ISO-4, ISO-6, IDX-4 (`10` §1)                                |
| [P5](#p5-dead-id-knobs)                                  | done        | Dead `id_pattern` / `id_field` out; the slug rule is the whole rule |
| [P8](#p8-find-and-search-as-features)                    | done        | The search stack is not forced on every consumer                    |
| [P9](#p9-remove-a-record)                                | done        | A Record can leave, on `document` and `singleton`                   |
| [P10](#p10-consumer-acceptance)                          | done        | The consumer's acceptance list on the P0 fixture                    |
| [P11](#p11-open-maps-with-a-typed-value)                 | done        | A map whose keys the author invents and whose values are not strings |
| [P12](#p12-commit-message-and-author)                    | done        | A readable commit subject, and an identity the caller chooses        |
| [P13](#p13-listing-a-collection-without-the-index)       | not started | Asking a `document` collection what it holds, with `find` off       |

**Order.** P0 first, and its grep box before its fixture box: a fixture
shaped like a consumer carries that consumer's collection names, and
until SCH-1 says it grades library source, adding one turns SCH-1 red.
An implementer who meets that failure will rename the fixture into
something meaningless instead of fixing the check.

Then P1 and P2: without them a consumer can neither write nor read its
journals honestly. Then P3, small and removing one migration blocker, and
P5, which removes dead machinery rather than a blocker. P4 is the large
one and is done (`10` §1 unencrypted slice). P8 is in: `find` and
`search` are crate features, optional on a reduced build. P9 is in:
`remove` on `document` and `singleton`. P10 is in: acceptance on the
fixture.

P11 is in: an `object` takes listed keys, a typed open map (`values`), or
an open map of strings when both arms are omitted.

P11 came after that acceptance and is last for a reason: nothing already
written stops working without it, and it blocks only the part of a
primary layer that is configuration rather than content. It needs no
other stage and overlaps none of them, so it can run whenever there is
room.

P12 is in: a mutating call may name the commit, and `open` may name who
it is by. It came after enough autocommits existed to read: the history
was already written, just unreadable, and an identity nobody chose was
already in it.

Each stage carries **Needs** (stages that must be `done` first) and
**Decide first** (a question `03`–`10` leaves open, which has to be
answered before any code). Those two lines are the running order; the
paragraph above is only its reasoning.

### Running one stage

Hand an implementer one stage, never the part. A stage is the unit
because it carries its own spec amendment, its own tests, and its own
CHANGELOG line. Two stages in one session lose that separation and the
review cannot tell which change closed which box.

For any P stage, in this order:

1. Read the stage. If it has **Decide first**, that answer is not the
   implementer's to invent: it changes `03`–`10` for every consumer.
   Settle it, write it into the named document, then start.
2. Amend the documents the boxes name. A box citing `04` STO-2 or
   `10` §2 means that file changes in this stage. Code that contradicts
   an unamended document is the failure this plan exists to prevent.
3. Set **Status:** `in progress` in the stage and in the dashboard.
4. Implement in `rust/scheda`. `src/` returns `Result`: no `panic!`,
   `unwrap`, `expect`, or `assert!` (`07` §5). After editing
   `api/v1/*.proto`, run `bazel run //api/v1:generate` before the tests,
   or the build fails on stale stubs.
5. Write the check each box names as a real test. A box with no test
   stays `[ ]`, whatever the code does.
6. Run `bazel test //... --keep_going` from the repository root. Green,
   or the stage is not done. `cargo test` alone is not that gate.
7. Tick the boxes a passing test covers, set **Status:** `done` in the
   stage and in the dashboard, and add the CHANGELOG entry under
   `[Unreleased]`.

Stop and ask rather than guess. If a box needs something `03`–`10` does
not decide, that is a gap in the specification, and a silent default in
`src/` hides it instead of closing it (§1).

### Working in parallel

P1, P2, and P3 all touch the journal write path and the commit rule; run
them one at a time. P5 removed dead `CollectionDecl` fields. P8 touches
the build graph and overlaps nothing. P12 touches the same commit rule
P3 settled; both are done. P13 touches only the read path and no commit
rule at all, so it can run at any time and conflicts with nothing above.

---

## P0. Consumer fixture

**Status:** done

**Codes:** SCH-1, MOD-1, MOD-8, STO-2, QRY-1.

**Needs:** nothing.

**Decide first:** nothing. The grep box settles the scope question
itself, in `05`.

One fixture Vault shaped like the consumer's primary layer, so the later
stages have something honest to run against: two `document` collections
with identical field lists, two further `document` collections, three
`journal` collections, one `singleton`, and one isolated `journal`.

A fixture of that shape carries collection names that belong to a
consumer, which is what the SCH-1 grep forbids. That check is about
library source, not test data, and today's wording does not say which it
means. Settle that before the fixture exists, not after it fails.

The isolated `journal` is declared in Schema and its files are placed by
hand, the way the A6 fixture already does it. Writing a Record there is
P4, and `check` does not reach the nested tree until then.

The consumer's shape moved after this stage closed: its work queue became
a `document` (an entry has to leave when it is handled, which a `journal`
forbids), and a second `singleton` joined. The fixture is therefore one
`document` and one `singleton` short of that shape. P10 grows the fixture
rather than reopening this stage.

- [x] Fixture lives in test data. Collection names come from its Schema
      files; no name is compiled into `rust/scheda/src` (SCH-1).
- [x] The MOD-1 / SCH-1 grep check names `rust/scheda/src` as its scope,
      in the test and in `05`. Test data must not be able to turn SCH-1
      red.
- [x] `check` passes on the fixture; `get` of each kind returns bytes
      equal to the file (STO-2, QRY-1).
- [x] Two `document` collections with identical field lists: a Record
      accepted by one is accepted by the other; `find` limited to one
      returns none of the other's Records (MOD-8, QRY-2).

---

## P1. Journal line fidelity

**Status:** done

**Codes:** STO-2, STO-3, MOD-12, MOD-16, QRY-1, SCH-4.

**Needs:** nothing.

**Decide first:** nothing; the boxes name the answers. What that limit
costs a caller is now `07` LIB-18.

The consumer chains its own checksum over each journal line, so the bytes
it reads back must be the bytes it meant to write. Two things stand in
the way, and neither has failed loudly yet.

Every number entering the library is a `double`: `RecordAdd.fields` is
`google.protobuf.Struct`, and the bridge builds `serde_json::Number::from_f64`.
The line is then printed with `Number::to_string`. A field Schema calls
`integer` lands on disk as `1.0`. A person reading the JSONL sees `1.0`,
and a consumer that canonicalises `1` never matches the file.

The line is also sorted-key JSON rather than RFC 8785: keys sort in
`serde_json::Map` order, which is UTF-8 byte order, not UTF-16 code unit
order; numbers print through Rust's shortest round-trip, not ES6
`NumberToString`. For ASCII keys and small integers the two agree, which
is why this has been invisible.

- [x] An `integer` field round-trips as an integer. `add`, then read the
      segment: the line holds `1`, not `1.0`. Same after `reindex` and
      through `get` (MOD-12, STO-2).
- [x] Canonical form is RFC 8785 and `04` STO-2 says so: keys by UTF-16
      code unit, numbers by ES6 `NumberToString`, strings escaped as the
      RFC states.
- [x] `get` and `find` with `include_raw` on a `journal` return the exact
      bytes of that line, byte for byte equal to the segment (QRY-1).
- [x] Round-trip fixture: integers, negative integers, finite fractions,
      nested objects, a list, and a non-ASCII key. Bytes are stable
      across `add` → `get` → re-encode.
- [x] A caller value on an `integer` field that is not integral is
      `invalid`, not truncated (SCH-4).
- [x] An integer the `Struct` bridge cannot carry exactly (magnitude above
      2^53) is `invalid` on write, not silently rounded. If that limit is
      unacceptable, `07` says what carries the value instead.

---

## P2. Ordered journal read without the lexical Index

**Status:** done

**Codes:** QRY-7, QRY-2, IDX-2, IDX-5, MOD-16, `10` §2.

**Needs:** P1, whose line bytes this read hands back.

**Decide first:** `10` §2 — does IDX-2 stay a **core** requirement? That
sentence decides whether this stage writes code at all, and P8 waits on
the same answer. Do not open an editor on `src/` before it is written.

To verify its chain the consumer reads every line of a journal in append
order, and it wants that in a build with no search. Today append order is
`find` (QRY-7), `find` runs a Tantivy query (QRY-2), and `get` on a
`journal` goes through the IDX-5 map. The whole lexical Index sits on the
path of a pure sequential read. `10` §2 puts IDX-2 in **core**, so this is
a specification question before it is an implementation one.

- [x] `10` §2 decides it, and only one answer survives: either the
      lexical Index stays **core** and a consumer accepts Tantivy on a
      read, or an ordered journal read is defined that walks segments in
      Schema sequence without opening the Index.
- [x] If that read lands: a name decided in `07` — not the A2 `scan`
      (§3) — append order identical to QRY-7 including rotation,
      streaming rather than a materialised list, and raw line bytes (P1).
- [x] A missing `.scheda/` does not stop it. `get` and `history` already
      survive a deleted Index (GIT-3); this read matches them.
- [x] Fixture of 10³ lines across rotated segments: every line once, in
      order, and memory does not grow with the line count (OPS-1).

---

## P3. Many writes, one commit

**Status:** done

**Codes:** GIT-2, GIT-8, GIT-10, LIB-7, STO-5, OPS-1.

**Needs:** nothing.

**Decide first:** `04` GIT-2 — an `OpenOptions` switch, or a statement
that a transaction is the only supported batch. The boxes below split on
that answer.

GIT-2 is deliberate: outside a transaction each mutating call autocommits
with the message `scheda`. The consumer appends one journal Record per
observed event, so on an OPS-1 fixture that rule means 10⁴ commits.
`begin` / `commit` already answers it, at the price of making a
transaction the only correct way to write and of discarding accepted
writes if the process dies before `commit`.

Decided: a transaction is the only supported batch. There is no
`OpenOptions` switch that turns autocommit off. A consumer that appends
many journal lines holds `begin` / `commit`. `auto_sync` still pushes
only after a real commit. The switch did not land; `OpenOptions` and
`info` are unchanged.

- [x] `04` GIT-2 decides it: a transaction is the only supported batch
      and a consumer must hold one. There is no `OpenOptions` switch
      that turns autocommit off.
- [x] The switch did not land. `OpenOptions` and `info` are unchanged.
      `auto_sync` still pushes only after a real commit (GIT-8, GIT-10).
- [x] 10³ appends in one batch produce one commit and stay inside the
      OPS-1 band.
- [x] A process killed mid-batch leaves the last complete line and no
      partial line; on reopen the accepted set is what the segment holds
      (STO-5, MOD-16).

---

## P4. Unencrypted isolated slice

**Status:** done

**Codes:** ISO-2, ISO-4, ISO-6, IDX-4, LIB-21, `10` §1.

**Needs:** P1 and P3. The isolated tree is a `journal` under its own git,
so a wrong line form or a commit per line is the same defect in a
smaller repository.

**Decide first:** nothing. `10` §1 already fixes the order and `09` the
rules; this stage implements them.

This is the unencrypted slice of `10` §1, recorded here because a
consumer waits on it and because the consumer's shape adds requirements
the roadmap order does not mention. It needs exactly one isolated
collection, and that collection is a `journal`: its Records must stay
out of the published history and be readable only by their owner.
Encryption stays out (ISO-5, ISO-7–11).

- [x] The `10` §1 order holds: nested git under `{vault}/.isolated/` →
      `create_collection` / `add` on an isolated name write there →
      non-isolated `find` / `search` return nothing isolated and do not
      require `.isolated/` → the main Index holds no sealed fixture
      string → `check` covers the nested tree.
- [x] `journal` works isolated, not only `document`: segments, rotation,
      and the IDX-5 map inside `{vault}/.isolated/.scheda/` (ISO-4,
      IDX-4).
- [x] Files are `0600`; the parent repository does not track
      `.isolated/`; the nested repository has no Remote and `check` fails
      if one appears (ISO-6).
- [x] A batch of isolated appends is one nested commit, not one per line
      (P3).
- [x] `collection(name)` stops being `unsupported` for an isolated name;
      the **Later** clause of `07` LIB-21 becomes the rule.

---

## P5. Dead `id` knobs

**Status:** done

**Codes:** SCH-6.

**Needs:** nothing.

**Decide first:** nothing.

Schema has no `id_pattern` key and no `id_field` key, so nothing can
ever set the two fields the code carries. Both are dead. Delete them.
The slug rule is the whole `id` rule, and the identity field is `id`.

- [x] `id_pattern` is gone from `rust/scheda/src` and from
      `tests/document.rs`. `regex` stays; `id.rs` needs it.
- [x] `id_field` is gone: the field, its per-collection `String`, and
      `Collection::id_field()`, whose rustdoc promises a default that
      cannot vary. Call sites take the `id` constant.
- [x] `05` says the slug rule is the only constraint on `id`, and `id`
      is the identity field (SCH-6).
- [x] `bazel test //...` green.

---

## P8. `find` and `search` as features

**Status:** done

**Codes:** IDX-2, IDX-3, IDX-8, QRY-4, LIB-1, `10` §2.

**Needs:** P2, which moved a sequential journal read off the Index.

**Decide first:** nothing. The shape below is decided: two features named
after the two methods, `search` pulls `find` in, generated types stay
compiled and stop being public.

Not urgent: nothing waits on it. Cargo `default` is `remote-git` only.
`tantivy`, `usearch`, and `cxx` are `optional = true` behind `find` and
`search`. `git2` is not part of this — git is the product — and its
network legs are already `remote-git` and `vendored-openssl`.

`search` cannot stand without `find`. `search.proto` imports `find.proto`
for `SearchQuery.filter`, and the hybrid's lexical half is BM25 on the
same Tantivy Index under `SearchQuery.lexical_weight`. Without `find`
what remains is ANN, not `search`.

Neither is only a read path. `add` and `replace` write into the Index,
and `replace` asks it whether the Record exists at all.

```toml
find   = ["dep:tantivy"]
search = ["find", "dep:usearch", "dep:cxx"]
simd   = ["search", "usearch/numkong"]
```

- [x] `tantivy`, `usearch`, and `cxx` are `optional = true`. Cargo
      `default` is `remote-git` only. `find`, `search`, and `simd` are
      opt-in; turning `search` on turns `find` on; turning `simd` on
      turns `search` on. The fourth combination (`search` without
      `find`) does not exist. Bazel CI still links those three legs.
- [x] `Vault::find` / `Collection::find` compile only under `find`, and
      `Vault::search` / `Collection::search` only under `search`. Calls
      into Tantivy and USearch sit behind `#[cfg(feature = …)]`; nothing
      else in `src/` is conditional.
- [x] The generated types stay compiled and stop being public. `lib.rs`
      re-exports `Filter`, `FindQuery`, `FindResult`, and `filter` only
      under `find`, and `SearchQuery`, `SearchHits`, `search_hits`, and
      `AnnIndex` only under `search`. `scheda-generated` and
      `bazel run //api/v1:generate` do not change.
- [x] `replace` decides existence from the Record file, not from the
      Index. Isolated collections already do exactly that; the same path
      serves every collection when `find` is off.
- [x] A build without `find` writes Records and cannot update the Tantivy
      segments, so it marks the Index stale. A build with `find` reports
      that marker, from `find` and from `check`, and `reindex` under a
      full build is the cure. A build without `find` does not report it:
      it holds no lexical segments and its `reindex` cannot rebuild them,
      so a `check` that reported it would stay red from the first write
      on. Narrowed after this stage shipped; the contract now lives in
      `07` LIB-15. A full build never answers `find` from an Index a thin
      build silently aged (STO-1: the Index is derived, so a stale one is
      a reported state, not a wrong result).
- [x] `open` with an embedder in a build without `search` fails with the
      `code` for the absent leg (IDX-8, QRY-4). Not ignored, and not a
      second `code` that tells the caller how the crate was compiled.
      The `open` signature and the `Embedder` / `Reranker` traits are the
      same in every build.
- [x] Vault bytes and the `.scheda/index` format do not depend on the
      feature set. A thin build opens a Vault a full build wrote and the
      reverse.
- [x] `10` §2 says the phases grade a build with `find` and `search`
      on (Bazel CI). Cargo default is `remote-git` only: the same
      library with those legs off, not a second product.
- [x] CI builds full.

---

## P9. Remove a Record

**Status:** done

**Codes:** MOD-16, STO-5, GIT-2, IDX-10, LIB-5, `07` §later.

**Needs:** nothing.

**Decide first:** nothing. The kinds are decided: `document` and
`singleton` only.

Today a Record can be written and revised but never removed. The only
way out is to delete the file by hand and `reindex`, which is not an API
and is not safe under a transaction. A consumer with a work queue needs
an entry to leave when it is handled, and a store that cannot forget
cannot answer an erasure request either.

A `journal` keeps none of this: `add` appends and nothing rewrites a
line, so a caller's checksum chain over the segment stays sound
(MOD-16). Removing a line stays out, later and otherwise.

- [x] `Collection::remove(id)` on `document` and `singleton`; `journal`
      → `unsupported`. The name is `remove`; `delete_one` was an A2 name
      and does not come back (§3).
- [x] Unknown `id` → `not_found`. An illegal slug → `invalid`, before
      any file is touched.
- [x] The Record file is deleted and staged like any other mutation:
      autocommit outside a transaction, part of the batch inside one
      (GIT-2). Isolated collections use the nested git.
- [x] The Record leaves the Index, so `find` and `search` stop returning
      it and `get` answers `not_found`. Under a build without `find` the
      file still goes and the Index is marked stale (P8).
- [x] An interrupted `remove` leaves either the whole file or no file,
      never a partial one, and the accepted set on reopen matches
      (STO-5).
- [x] `history(id)` still answers from git after the file is gone: the
      commits that touched it are the record of what was there.
- [x] `07` gains the method and drops it from its **later** list; `02`
      gains the term; `14` gains `collection NAME remove --id ID`; `03`
      MOD-16 says plainly that a `journal` has no remove.
- [x] `bazel test //...` green.

---

## P10. Consumer acceptance

**Status:** done

**Codes:** STO-1, STO-2, SCH-1, QRY-1, LIB-19, ISO-2, OPS-1, OPS-3.

**Needs:** P0 for the fixture, and every other stage either `done` or
recorded as unmet in its last box.

**Decide first:** nothing.

Run the consumer's acceptance list against the P0 fixture. Passing it is
what "migrate without friction" means; anything short of it is a promise.

The first box grows the fixture to the shape the consumer settled on
after P0 closed. Run it first: the boxes below expect those collections
to be there.

Crate Check: `rust/scheda/tests/consumer.rs` (full build and Cargo
default / `consumer_thin_test`).

- [x] The fixture gains a third non-paired `document` (a work queue: an
      entry is revised in place and then removed) and a second
      `singleton`. Its isolated `journal` is written through the API, not
      placed by hand (P4).
- [x] The fixture opens and reads in an editor and in `git` with no crate
      (STO-2), and `get` of every kind through the crate returns bytes
      equal to the file (QRY-1).
- [x] Delete the Index and `reindex`: same Records, same content, and the
      counts match a hand tally of files and lines (STO-1, OPS-3).
- [x] `rust/scheda/src` holds no consumer collection name (SCH-1, MOD-1,
      P0).
- [x] The Cargo default build, which has neither `find` nor `search`,
      writes every kind and reads it back: `get`, `history`, and `lines`
      on the fixture, with `.scheda/` deleted for the `lines` case (P8,
      P2, LIB-19).
- [x] The isolated collection round-trips on the same handle: `add` and
      `get` under `{vault}/.isolated/`, `find` on the main tree returns
      none of it, and `check` covers the nested tree (ISO-2, ISO-4, P4).
- [x] OPS-1 volume on the fixture: 10³ journal Records and 10²
      documents (one tenth of OPS-1, every OS), with `get` and `add` in
      milliseconds.
- [x] Every P stage is `done`, or its entry names the consumer
      requirement that stays unmet and why.
- [x] A Record removed with `remove` is gone from `get`, `find`, and the
      files, and `check` passes after it (P9).

## P11. Open maps with a typed value

**Status:** done

**Codes:** SCH-4, SCH-5, SCH-8, SCH-9, MOD-12.

**Needs:** none. It touches Schema parsing and field validation, nothing
else.

**Decide first:** settled. The key is `values`. The value declaration
keeps the required `name`, which labels one entry in error messages and
is never a key on disk. An `edge` under `values` is refused, and §6 item
11 records why that refusal is not final.

An `object` today is one of two things: a map whose keys Schema lists, or
an open map whose values must be strings (`05` SCH-4).
Configuration-shaped data is neither. Its keys are invented by whoever
writes the file — an axis name, a category name, a registry name — and
its values are objects or numbers, not strings. Such a caller gives up
either the keys (every map rewritten as a list of objects, changing a
file format that already exists outside this library) or the schema (the
map kept in a body and checked by hand). Both are losses the library can
avoid.

`object` and `object_list` gain a second arm. `fields` lists keys and
stays exactly as it is. `values` declares the type of every value under a
key the Schema does not name. The two are mutually exclusive, and
omitting both keeps today's meaning, so no existing Schema file changes
what it says.

```yaml
- name: env
  object:
    values:
      name: axis        # labels one entry in error messages; never a key on disk
      object:
        fields:
          - name: significant
            integer: {}
```

`values` reuses the field entry, so nesting is recursive: a map of maps
of `integer` is two `values` deep, and the value of a map may itself be
an `object_list`. Three keys on that entry are meaningless and are
refused rather than ignored — `required` (whether a key is present is the
writer's choice, MOD-12), `default` (there is no key to apply it to), and
`exclude_find` (a dynamic key is not a find key to begin with, SCH-8).
`nullable` keeps its meaning. Because that entry names no key, `id` and
`body` are free for its label, while a field nested under it is a key
again and refuses both. `edge` is refused anywhere beneath `values`,
not only as the value type itself: an edge is a filter key and a dangling
check, and a key no Schema names can be neither. Half of an edge under a
deeper nesting would be the same loss wearing a longer path.

The Index does not change. `object` and `object_list` are never FTS or
vector (SCH-9), and filter keys are collected from declared names
(SCH-8), so a typed open map adds no column, no query key, and no
rebuild.

- [x] `05` SCH-4 and `05` §8 state `fields` **xor** `values` on `object`
      and `object_list`, and that omitting both is still the open map of
      `string` values.
- [x] `api/v1` `schema.proto` carries the arm. Generated `Validate`
      refuses `required` / `default` / `exclude_find` on `values`. `edge`
      under `values`, and both `fields` and `values`, are `invalid` at
      Schema load (YAML and `create_collection`); protovalidate-buffa
      cannot transpile that xor over a repeated message or `has(edge)`
      on a oneof. Run
      `bazel run //api/v1:generate` before the tests.
- [x] A Record whose values under unnamed keys all match the declared
      type round-trips through `add` and `get`.
- [x] A value of the wrong type is `invalid`, and the message names the
      field, the `values` label, and the offending key.
- [x] Recursion holds: a map whose values are maps of `integer` accepts
      `2` and rejects `"2"`.
- [x] `required`, `default`, or `exclude_find` on the `values` entry is
      `invalid` at Schema load, not silently dropped, and so is an `edge`
      anywhere beneath `values`, at any depth.
- [x] `nullable` on the `values` entry keeps its meaning: `null` under a
      key passes with it and is `invalid` without it.
- [x] `object_list` takes `values` as well, and a wrong-typed value in one
      item of the list is `invalid`.
- [x] An `object` with neither arm still accepts a map of strings and
      still rejects a non-string value: the P0 fixture and
      `testdata/schema/types.yaml` pass unchanged.
- [x] The `values` label names no key, so `id` and `body` are accepted
      there while a nested field under it still refuses both.
- [x] Schema round-trips through proto and back with the arm intact, and
      the A1 catalog prints it.
- [x] `find` gains no key: filter keys for a collection carrying such a
      field are what they were.
- [x] CHANGELOG under `[Unreleased]`; `05` version bumped.

---

## P12. Commit message and author

**Status:** done

**Codes:** GIT-2, GIT-3, LIB-7, LIB-9, LIB-13, LIB-17, LIB-21, LIB-22.

**Needs:** nothing.

**Decide first:** nothing. Both questions this stage exists for are
answered below: an optional message per mutating call, and the commit
identity at `open`.

Today every autocommit is signed `scheda` and nothing else is possible.
GIT-2 calls that "the default message", but no override exists, so a
consumer that writes through autocommit gets a history where every entry
reads the same word. The count makes it worse than dull: one appended
line plus the Record that tracks it is two commits, so a few thousand
writes bury whatever a person wanted to find. A history that cannot be
read is not a history, and GIT-3 and LIB-17 hand both the message and the
author to callers through `history`, so this is not only `git log` — it
reaches whatever a consumer builds on top.

The identity has the same shape of problem from the other side. There is
no way to say who a commit is by: `signature` reads the local git config
and otherwise falls back to `Scheda <scheda@invalid>`. A consumer that
keeps the people in its Records pseudonymous gets the opposite of what it
asked for, because the config holds a real name and address, and a commit
author cannot be redacted later without rewriting history. Such a
consumer needs to pass its own pseudonym and never touch the config.

Two rules stay untouched. Autocommit itself is not up for negotiation:
LIB-7 says there is no `OpenOptions` switch that turns it off, and a
consumer writing many lines must `begin`. This stage adds a name for the
commit, not a way to avoid it. And a message is still required to say
something: `commit` rejects an empty one, and an explicitly empty
override is the same mistake.

A per-call message and a transaction have to agree on who wins. Inside a
transaction the mutating calls do not commit at all, so a message passed
there has nowhere to go. It is ignored, and every method that takes one
says so in its own rustdoc. Failing instead would be worse: code that
wraps arbitrary writes in `begin` … `commit` cannot know whether a
callee passed a message, and would start breaking on someone else's
argument.

The identity belongs at `open` rather than on each call. It does not vary
between two writes in one process, and threading it through every
mutation would repeat it everywhere for nothing. `OpenOptions` is an
`api/v1` message, so its constraints are protovalidate rules on the proto
and not checks in `src/`; after editing it, generate before the tests.

- [x] Each mutating call (`add`, `replace`, `remove`,
      `create_collection`) takes an optional commit message. Outside a
      transaction that text is the commit subject; omitted, the subject
      is `scheda` as before. `04` GIT-2 and the `07` entries say so.
- [x] A message passed inside a transaction is ignored, and `commit`'s
      own message is what lands. The rustdoc of each of those methods
      states it, and `07` states it once.
- [x] A message that is present but empty is `invalid`, before any file
      is written, matching `commit`.
- [x] `OpenOptions` carries `author` (`api.v1.Author`): required `name`
      and `email`. Unset `author` is the fallback. A partial Author is
      `invalid` at `open`, from protovalidate on `api/v1`, and the
      generated `Validate` contains it.
- [x] Characters git cannot carry in a name (`<`, `>`, a newline) and an
      address that is not an HTML email are `invalid` at `open`, not at
      the first commit.
- [x] The identity falls back in one order: `OpenOptions`, then the local
      git config, then `Scheda <scheda@invalid>`. An existing caller that
      sets nothing sees exactly today's behaviour.
- [x] The identity is the git author and the git committer, in the main
      repository and in the `{vault}/.isolated/` nested one. A Vault does
      not end up with two identities across its two trees.
- [x] `history(id, …)` reports the message and the author that were set,
      for both an autocommit and a transaction commit (GIT-3, LIB-17).
- [x] No switch to disable autocommit appears anywhere: LIB-7 stands, and
      `grep` over `rust/scheda/src` and `api/v1` finds no such option.
- [x] Documents amended: `04` GIT-2 (override and identity) and GIT-9
      (`remote_repo` URI), `07` LIB-7, LIB-9, LIB-13, LIB-21 and the
      `remove` entry, `08` where the agent surface encodes a mutating
      call, `14` where a mutating command gains the message and `open`
      takes `--author-name` / `--author-email`.
- [x] `bazel test //...` green; CHANGELOG under `[Unreleased]`.

---

## P13. Listing a collection without the Index

**Status:** not started

**Codes:** LIB-2, LIB-25, LIB-26.

**Needs:** nothing.

**Decide first:** nothing. The two questions this stage could have opened
are answered below: paging is out, and naming is separate from reading.

A consumer can ask for one Record by `id`, and can walk a `journal` in
append order. It cannot ask a `document` collection what it holds. LIB-19
says `document` / `singleton` → `unsupported`, so the only listing in the
library is `find` — and `find` is an opt-in crate feature whose Cargo
default leaves it out. A consumer built without it cannot answer "which
Records exist" through the library at all. The one way left is to read the
collection directory behind the library's back, which is exactly what
MOD-8 and `07` §4 forbid: the caller never handles a Record path, because
the layout belongs to the library and may change. Today that promise
cannot be kept for a document collection, and the first real consumer hit
it immediately.

The machinery is already written and merely private. `store::list_files`
names the Record files of a collection and sorts them, skipping dotfiles,
non-`.md` names, and anything that is not a regular file;
`store::scan_documents` reads them into a `Vec<Record>` behind
`#[cfg(feature = "find")]`. This stage turns that into a contract: usable
without `find`, streaming instead of a `Vec`, and split in two so a caller
who wants names does not pay for bodies.

Order is `id` order — the byte order of the file name — and it is a
promise, not a side effect of `read_dir`. A caller that walks a collection
twice, or walks two of them together, needs the same sequence both times.

Paging is deliberately out of scope. An iterator already bounds memory:
`lines` yields one Record at a time and stops wherever the caller stops.
What a page would add is a cursor that outlives the call, and no consumer
needs to resume a walk yet. A cursor also cannot have one shape for both
kinds — append position for a journal, last `id` for documents — so
designing it now would fix the wrong shape before a caller exists to
judge it. When one needs to resume, that is its own stage.

Scope is the Rust library. The agent envelope (`08`) and the CLI (`14`)
are not extended here: the consumer that needs this calls the Rust API,
and a listing command is a separate decision about those surfaces.

- [ ] `documents()` on the collection handle yields every Record of a
      `document` collection in `id` order, one at a time (`07` LIB-25).
- [ ] `ids()` gives the same order without opening a single Record file
      (`07` LIB-26).
- [ ] Both refuse `journal` with `unsupported`, and both accept
      `singleton`: nothing before the first write, one Record after.
- [ ] Neither sits behind `find`. The Cargo default build has both, and a
      deleted or deliberately stale `{vault}/.scheda/` changes neither
      answer.
- [ ] `.schema.yaml`, any dotfile, a name not ending in `.md`, and a
      subdirectory are skipped, and none of them is an error.
- [ ] A Record that fails current Schema and a `.md` file that is not a
      Record at all are both yielded with `valid: false`, as `get` does,
      and the walk continues past them. Unlike LIB-19 it does not stop:
      separate files, so one bad file must not hide the rest, and `find`
      rebuild must not start failing closed because of this stage.
- [ ] An isolated collection walks `{vault}/.isolated/{name}/`.
- [ ] On a 10³ document fixture the walk holds no Record contents but the
      current one, and `scan_documents` either becomes this iterator or
      is deleted — two ways to read a collection is one too many.
- [ ] Documents amended: `07` LIB-2 name list, LIB-25, LIB-26, and §4.
- [ ] `bazel test //...` green; CHANGELOG under `[Unreleased]`.

---

## 5. What this document does not decide

Calendar dates or crate version numbers (`10` §8).

CLI argv is `14`. Binary crate layout and whether the agent process is the
writer stay `08` §3. Lock rules stay STO-4.

Exact MCP tool names — out of this plan.

Schema proto field names — `api/v1` `schema.proto` (`05` §8 keys). Pretty-print of
Schema YAML stays `05` §9.

Whether `open` refuses a dirty Index or only warns via `check` (`10` §7).

Vault hooks: already the consumer's (`04`). A CLI note must not recurse
into the same write lock (`10` §7); this plan does not design hooks.

ISO-5 `isolated_encryption` / `crypto`.

The consumer's schemas, field names, and identifier formats. §4 says what
the engine must be able to express; the shapes themselves stay in the
consumer's own documents and must not be copied into this package
(SCH-1).

The order in which a consumer moves its data, and what it keeps outside a
Vault. That is the consumer's plan, not a stage here. What the limits it
runs into cost every consumer is §6.

---

## 6. Open past the first consumer

§4 asks what one consumer needs. This section asks what a second one will
need and Scheda does not have. None of these is a stage: no box, no
status, no *Check:*. They are recorded so that a later plan starts from a
list instead of from a surprise, and so that a decision taken for the
first consumer is not mistaken for a decision taken for everybody.

1. **A journal line cannot leave.** P9 gives `document` and `singleton` a
   `remove`, and stops there on purpose: rewriting a segment would break
   the append-only guarantee a caller's checksum chain rests on
   (MOD-16). So an erasure request that reaches a journal still has no
   answer, and neither has retention (item 5).

2. **One writer per working tree (STO-4).** A consumer whose capture path
   is a second process keeps that buffer outside the Vault. A
   service with several workers, or a daemon beside a CLI, has no answer
   at all: there is no cross-process wait, only `locked`.

3. **Flat collection directories (MOD-8).** A5b removed the caller's
   path template and was right to; nothing replaced it. The workaround —
   let each value of a field become its own collection — does not scale
   past a handful of values, and one collection of 10⁵–10⁶ Records is
   that many files in one directory. What is missing is a fan-out the
   library owns and derives, never one a Schema file supplies.

4. **Fields carry only `double` (MOD-12, `api/v1`).** `RecordAdd.fields`
   is `google.protobuf.Struct`, so P1 had to make a magnitude above 2^53
   `invalid`. There is no int64, no bytes, and no exact decimal.
   Nanosecond timestamps, file sizes, and foreign identifiers do not fit.

5. **A journal only grows (MOD-16).** No `replace`, no line delete, no
   retention, no compaction. Rotation cuts segments and discards nothing,
   so an append-only collection is unbounded by design.

6. **A C++ toolchain is mandatory unless `search` is off.** USearch
   arrives through `cxx`. P8 puts `find` and `search` behind Cargo
   features; a build without `search` does not need C++. Cargo default
   does not enable `search`. Bazel CI still does.

7. **The HTTP embedder lives in the CLI, not the crate (QRY-5, A5).**
   Anyone embedding the library writes `Embedder` again, next to an
   OpenAI client this repository already has.

8. **`id` is a slug and nothing else.** After P5 that rule is the whole
   rule. A URL, an email address, or a foreign key cannot be a Record
   `id`; the caller has to derive a slug and keep the mapping.

9. **JSON is accepted for Schema and refused for a Record.** A Schema
   file may be `.schema.yaml`, `.schema.yml`, or `.schema.json`, and the
   loader branches on it. A `singleton` Record has one name,
   `record.yaml`. Both cannot be the right rule. The fixed Record name is
   the cheaper one to keep — one on-disk form, no format guessed from
   content — so the question is whether the three Schema names should
   become one.

10. **Acceptance is one consumer's list (P10).** A second consumer needs
    acceptance stated against `03`–`10`, not against §4's fixture.

11. **An edge cannot be the value of an open map (P11).** `values` takes
    any type but `edge`, because an edge is two promises — the dangling
    check and a `find` key — and a key no Schema names can keep only the
    first. The same relation written as an `object_list` keeps both, so
    nothing is lost today. What would change that is a caller whose file
    format is already a map of identifiers and cannot be reshaped; then
    the question is whether a half-edge under a clearly different name is
    better than no edge at all.
