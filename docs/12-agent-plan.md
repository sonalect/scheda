# 12. Agent development plan

Document 12 of the Scheda package. Version 0.40, 27 August 2026.

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

Stages P0–P9 in §4 are the second half of this file: what the **first
consumer** needs before it can move its primary layer onto the crate.
They are not **agent**, and they do not reopen A0–A7. Their codes cross
**core**, **search**, and **later**, so the unencrypted
`{vault}/.isolated/` tree of `10` §1 (ISO-2 nested git, ISO-4, ISO-6,
IDX-4) is P4 here instead of being out of scope. Accepting **agent**
never waited on a P stage and still does not.

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

P order is in §4. It starts after A7 and does not interleave with it.

---

## 2. Dashboard

| Stage | Status | What closes |
|---|---|---|
| [A0](#a0-catalog-messages) | done | `VaultInfo` in `api/v1`; listing is `info` |
| [A1](#a1-catalog-from-schema) | done | AGT-1; isolated listed without sealed fields |
| [A2](#a2-thin-cli-legacy) | done | Legacy CLI; do not extend; replaced by A4 |
| [A3](#a3-target-library) | done | Public API = `07` / `13` |
| [A4](#a4-target-cli) | done | CLI argv = `14` (AGT-2–5) |
| [A5](#a5-http-embedder-and-reranker) | done | CLI OpenAI embeddings + Cohere rerank (`14`) |
| [A5b](#a5b-layout-clean-up) | done | Dead `layout` parts out; the `layout` key check exists |
| [A6](#a6-isolation-at-the-agent-surface) | done | ISO-3 on CLI + library tools |
| [A7](#a7-agent-acceptance) | done | `10` §4 acceptance |

**Agent in:** AGT-1–5. ISO-3 when isolated collections exist (`09`, with
**agent**). AGT-1 listing is `Vault.info` (`VaultInfo.collections`), not a
`catalog` command.

Consumer readiness has its own dashboard in §4. A green A0–A7 does not
imply a green P0–P9, and the reverse is also true.

---

## 3. Out of A0–A7

These must not block A0–A7. Writes must not depend on them (`10` §4–5).
Some of them are consumer requirements and come back as P stages in §4;
that is noted on the line. Coming back does not restore an A2 name.

- MCP, HTTP, or a second transport. AGT-4 needs one thin encoding; `scheda`
  chooses CLI (`10` §4).
- ISO-5, ISO-7–11, LIB-5 `crypto`. AGT-5's *Check (**later**):* `crypto`
  waits on encryption.
- File watcher, `delete_one`, REL-4, extra language bindings (`10` §5).
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
      and omits its field values (ISO-3). Until ISO-2, `collection(name)`
      for that name is `unsupported`.

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
      `index_unusable` (AGT-5). Not `crypto` (later).
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

**Status:** done

**Codes:** ISO-3, AGT-1.

Close only after A5. Nested `{vault}/.isolated/` I/O (`10` §1) is not this
stage. This is the agent half of isolation, not encryption. Isolated
`collection(name)` stays `unsupported` until that slice (A3). Crate
Check: `isolated_sealed_values_stay_off_the_main_vault` in
`rust/scheda/tests/catalog.rs` and `cli_omits_isolated_sealed_values` in
`rust/cli/tests/cli.rs`.

- [x] Agent tools bound to the main Vault never return a fixture sealed
      value: `info`, `get`, `find`, `search` (ISO-3).
- [x] `Vault.info` still lists isolated collections as isolated and does
      not include their field values (ISO-3, A1 / A3).
- [x] Reaching isolated Records will be `collection(name)` after `10` §1
      (ISO-2). Until then that call is `unsupported`. CLI `collection NAME`
      for a non-isolated collection must not return isolated Record values.

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

**Status of the part:** in progress. P0, P1, and P2 are done; no other
stage is started.

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

| Stage | Status | What closes |
|---|---|---|
| [P0](#p0-consumer-fixture) | done | A fixture Vault shaped like the consumer's primary layer |
| [P1](#p1-journal-line-fidelity) | done | An integer stays an integer; the line is RFC 8785 |
| [P2](#p2-ordered-journal-read-without-the-lexical-index) | done | Reading a journal in order without Tantivy |
| [P3](#p3-many-writes-one-commit) | done | Append per event without a commit per event |
| [P4](#p4-unencrypted-isolated-slice) | not started | ISO-2, ISO-4, ISO-6, IDX-4 (`10` §1) |
| [P5](#p5-id-pattern-in-schema) | not started | `id` pattern reaches `CollectionDecl` |
| [P6](#p6-singleton-file-format) | not started | STO-2 and SCH-6 agree on the singleton file |
| [P7](#p7-a-consumer-that-does-not-search) | not started | The search stack is not forced on every consumer |
| [P8](#p8-decisions-without-code) | not started | Layout answers the consumer needs before it moves |
| [P9](#p9-consumer-acceptance) | not started | The consumer's acceptance list on the P0 fixture |

**Order.** P0 first, and its grep box before its fixture box: a fixture
shaped like a consumer carries that consumer's collection names, and
until SCH-1 says it grades library source, adding one turns SCH-1 red.
An implementer who meets that failure will rename the fixture into
something meaningless instead of fixing the check.

Then P1 and P2: without them a consumer can neither write nor read its
journals honestly. Then P3, P5, P6 — each small, each removing one
migration blocker. P7 next, because it decides whether a consumer pays
for Tantivy and USearch it does not use. P4 is the large one and is
already `10` §1 Next. P9 closes the part.

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
them one at a time. P5, P6, and P7 touch Schema keys, the singleton
file, and the build graph, and do not overlap each other. P8 is prose
and blocks nothing.

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
sentence decides whether this stage writes code at all, and P7 waits on
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
- [x] Fixture of 10⁴ lines across rotated segments: every line once, in
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
- [x] 10⁴ appends in one batch produce one commit and stay inside the
      OPS-1 band.
- [x] A process killed mid-batch leaves the last complete line and no
      partial line; on reopen the accepted set is what the segment holds
      (STO-5, MOD-16).

---

## P4. Unencrypted isolated slice

**Status:** not started

**Codes:** ISO-2, ISO-4, ISO-6, IDX-4, LIB-21, `10` §1.

**Needs:** P1 and P3. The isolated tree is a `journal` under its own git,
so a wrong line form or a commit per line is the same defect in a
smaller repository.

**Decide first:** nothing. `10` §1 already fixes the order and `09` the
rules; this stage implements them.

This is `10` §1 Next, recorded here because a consumer waits on it and
because the consumer's shape adds requirements the roadmap order does not
mention. It needs exactly one isolated collection, and that collection is
a `journal`: its Records must stay out of the published history and be
readable only by their owner. Encryption stays out (ISO-5, ISO-7–11).

- [ ] The `10` §1 order holds: nested git under `{vault}/.isolated/` →
      `create_collection` / `add` on an isolated name write there →
      non-isolated `find` / `search` return nothing isolated and do not
      require `.isolated/` → the main Index holds no sealed fixture
      string → `check` covers the nested tree.
- [ ] `journal` works isolated, not only `document`: segments, rotation,
      and the IDX-5 map inside `{vault}/.isolated/.scheda/` (ISO-4,
      IDX-4).
- [ ] Files are `0600`; the parent repository does not track
      `.isolated/`; the nested repository has no Remote and `check` fails
      if one appears (ISO-6).
- [ ] A batch of isolated appends is one nested commit, not one per line
      (P3).
- [ ] `collection(name)` stops being `unsupported` for an isolated name;
      the **Later** clause of `07` LIB-21 becomes the rule.

---

## P5. `id` pattern in Schema

**Status:** not started

**Codes:** SCH-2, SCH-6, MOD-13, MOD-8.

**Needs:** nothing.

**Decide first:** nothing. Whether the pattern is anchored is a box of
this stage, answered in `05`, not a question held open before it.

`CollectionDecl.id_pattern` is already honoured on write, but nothing
ever sets it: there is no Schema key, so the field is always unset. The
consumer wants a ULID pattern on two journals and a slug pattern on its
document collections, and wants the library to refuse a wrong `id`
instead of trusting the caller.

- [ ] A Schema key for the pattern, listed in `05` §8 keys and present in
      `api/v1` `Schema`. A value that is not a valid expression is
      `invalid` on load and on `create_collection`.
- [ ] The pattern applies to `add` on every kind, not only `journal`.
- [ ] The pattern is anchored, or `05` states plainly that it is not. An
      unanchored pattern that matches a substring is the silent default
      this plan forbids.
- [ ] The library slug rule runs first. A pattern narrows it and can
      never widen it: a pattern that would allow `a/b` still does not
      create a path (MOD-8).

---

## P6. Singleton file format

**Status:** not started

**Codes:** STO-2, SCH-6, MOD-8, QRY-1.

**Needs:** nothing.

**Decide first:** which of the two documents is wrong — `04` STO-2, which
lets Schema choose JSON or YAML, or `05` SCH-6, which fixes the file at
`record.yaml`. Code follows SCH-6 today, so choosing STO-2 is a change of
behaviour and not only of prose.

`04` STO-2 says a `singleton` is "JSON or YAML 1.2 as Schema says". `05`
SCH-6 says the file is always `record.yaml`. The code follows SCH-6. Two
documents of this package disagree, and the consumer keeps two singletons
that are JSON today.

- [ ] One of the two changes. If the file stays `record.yaml`, STO-2
      drops "JSON or"; if Schema chooses, `05` §8 gains the key and MOD-8
      gains the second file name.
- [ ] Either way `get` returns the file bytes and the format is taken
      from Schema, never guessed from content (QRY-1, IDX-10).

---

## P7. A consumer that does not search

**Status:** not started

**Codes:** IDX-2, IDX-3, IDX-8, QRY-4, LIB-1, `10` §2.

**Needs:** P2. Dropping the lexical leg is only possible once a
sequential journal read does not go through it.

**Decide first:** the same `10` §2 sentence as P2. P2 wrote it: IDX-2
stays **core** for methods that resolve `id` through the Index; `lines`
walks files. If a **core** build must still link Tantivy for `find`,
this stage shrinks to the vector leg alone.

The crate links Tantivy, USearch, and a bundled libgit2 unconditionally.
A consumer whose first stage is only writing and reading files pays for
all three on every build and every test link. That is not a theoretical
cost; it is what slows the consumer's own iteration today.

- [ ] Cargo features let a build drop the vector leg, and with P2 the
      lexical leg. The default keeps both, so existing consumers see no
      change.
- [ ] `10` §2 says which of IDX-2 / IDX-3 a **core** binding must ship.
      It puts IDX-2 in **core** today; if a feature can remove it, that
      sentence changes.
- [ ] With the features off, `open`, `add`, `get`, `history`, `commit`,
      and `check` work. `find` / `search` return a `code` naming the
      absent leg, not an empty result that reads as "no match" (QRY-4).
- [ ] `bazel test //...` covers the reduced build. An untested feature
      combination rots.

---

## P8. Decisions without code

**Status:** not started

**Codes:** MOD-8, MOD-16, STO-4.

**Needs:** nothing.

**Decide first:** all three boxes are the decision. This stage writes no
code and its result is prose the consumer can act on.

Three shapes in the consumer's current layout have no direct translation.
Deciding them here costs a paragraph; discovering them mid-migration
costs a rewrite. None needs crate work.

- [ ] One of its collections is stored a directory level deeper today,
      keyed by a field before the `id`. MOD-8 gives flat collection
      directories with no path template (A5b). Record the answer: that
      field becomes an ordinary field of one collection, or each of its
      values becomes its own collection.
- [ ] One of its files is a work queue whose entries change status in
      place. A `journal` cannot do that: `replace` is `unsupported` and
      there is no delete (MOD-16). Record whether such a queue stays the
      consumer's own file or becomes a `document` collection with one
      Record per entry.
- [ ] Its capture path is a second process that writes a buffer of its
      own. STO-4 allows one writer per working tree. Record that such a
      buffer stays outside the Vault.

---

## P9. Consumer acceptance

**Status:** not started

**Codes:** STO-1, STO-2, SCH-1, QRY-1, OPS-1, OPS-3.

**Needs:** P0 for the fixture, and every other stage either `done` or
recorded as unmet in its last box.

**Decide first:** nothing.

Run the consumer's acceptance list against the P0 fixture. Passing it is
what "migrate without friction" means; anything short of it is a promise.

- [ ] The fixture opens and reads in an editor and in `git` with no crate
      (STO-2).
- [ ] Delete the Index and `reindex`: same Records, same content, and the
      counts match a hand tally of files and lines (STO-1, OPS-3).
- [ ] `rust/scheda/src` holds no consumer collection name (SCH-1, MOD-1,
      P0).
- [ ] OPS-1 volume on the fixture: 10⁴ journal Records and 10³
      documents, with `get` and `add` in milliseconds.
- [ ] Every P stage is `done`, or its entry names the consumer
      requirement that stays unmet and why.

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

ISO-5 `encrypt` / `secret` / `crypto`.

The consumer's schemas, field names, and identifier formats. §4 says what
the engine must be able to express; the shapes themselves stay in the
consumer's own documents and must not be copied into this package
(SCH-1).

The order in which a consumer moves its data, and what it keeps outside a
Vault. §4 P8 records the answers a stage depends on; the migration itself
is the consumer's plan, not a stage here.
