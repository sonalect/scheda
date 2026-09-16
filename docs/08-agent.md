# 08. Agent surface

Document 8 of the Scheda package. Version 0.30, 16 September 2026.

How an AI agent uses Scheda: the same operations as `07`, over a thin
transport. Collection listing is `info` (AGT-1). The agent is not a
second store and does not get a looser write path. Codes: `AGT`. Phase
**agent** unless marked otherwise. Terms are in `02`.

An assertion without a check is not a requirement. This phase does not block **core**.

---

## 1. Same API, listing from the declaration

A person may read Vault files without Scheda (STO-2). That is not a substitute for tools: lexical/vector `search` and Projector `Filter`s are not available by grepping the tree.

**AGT-1. Catalog from the collection declaration.** The agent (or the caller wrapping it) can list collections, kinds, isolation, encryption, journal caps, and Index-exclusion flags — serialized from declaration files as nested `api.v1.Schema` on `Vault.info` (`api.v1.VaultInfo`; rename is later), not inferred from directory names. There is no field catalog: no field names, type marks, or edges. There is no `catalog()` method. Queryable column names live in the caller's `Projector`, not in `info`. *Check:* a fixture declaration exported as JSON; names, kinds, and perimeter marks match the `.collection.yaml` files; the JSON has no `fields` key.

**AGT-2. Few tools, not one per collection.** The tool set is the `07` operations that mutate or query Records and git, plus `create_collection`. `info` is the listing (AGT-1). There is no family of methods named after consumer types. CLI transport omits `begin` / `commit` / `discard` (GIT-2); each mutating CLI command autocommits (`--message` when set, otherwise `scheda`). An in-process agent that holds a library handle may still batch with those names, and a message passed on a mutating call inside that batch is ignored. *Check:* a scripted CLI run “`find` / `search` → `get` → `add` on a `journal`” uses only those tools.

Include when a Remote exists: `push`, `pull`, `sync`, and `auto_sync` as an `open` option — same meaning as `04`. Do not add a second “GitHub tool”.

**AGT-3. Writes go through the library.** Tool writes store opaque UTF-8 bytes (SCH-12). Collection declarations are written with `create_collection` (LIB-21), not by offering a path. The JSON for that call is the thin declaration (`05`); a `fields` or `version` key is `invalid`. There is no tool “write these bytes at this path”. Record files land where MOD-8 says. A human may still edit files in git; that path is not offered to the agent. *Check:* `create_collection` with a `fields` key → `invalid` and no file. *Check:* `create_collection` then `add` without a hand-written declaration file. *Check:* `add` / `replace` do not take a JSON object of consumer fields.

**AGT-4. Transport is a shell.** CLI, MCP, HTTP, or another agent protocol is a thin encoding of `07` (including `info` and `create_collection`). It must not add product rules, scoring, or collection-specific behaviour. JSON of Records, listing, and errors is the Protobuf JSON mapping of `api/v1` messages, except CLI `get`, which writes `Record.raw` bytes (`14`). *Check:* the same Vault, same operation, library call vs CLI/MCP: JSON of `info`, `find`, and one `search` match, and error `code`s match. CLI `get` stdout bytes equal library `Record.raw`.

**AGT-5. Machine-readable results.** Each tool result includes, as applicable: `id`, collection, path, `raw` (when requested or when the transport is CLI `get`), search scores, and on failure a `code` from LIB-5 (plus `not_fast_forward` where `04` says; **later:** `crypto` where `09` says). There is no `valid` flag (it meant Schema match). A human message is optional and must not replace `code`. *Check:* contract tests for `not_found`, `invalid`, `locked`, `index_version`. *Check (**later**):* `crypto`.

`collection(name)` for an isolated collection reaches those Records. Default tools on the Vault must not return sealed values (ISO-3). **Later:** an encrypted collection is listed as encrypted by `info`, and the agent's I/O on it uses the same `encryption` secret as `OpenOptions` (LIB-13); without it those tools fail with `crypto` rather than returning armor bytes as a Record.

---

## 2. What the agent must not assume

- That `info` lists queryable field names. Columns are the `Projector`'s (`13`). Collection-level `exclude_lexical_index` / `exclude_vector_index` still apply (SCH-9).
- That `search` is a language-model answer (QRY-3).
- That a mutating CLI command skipped a git commit because a transaction was open, or that it used a caller message without `begin` (GIT-2). Batch writes need an in-process library handle. A CLI `--message` is the autocommit subject of that one command.
- That `open` pulled unless `auto_sync` was on and the pull was a fast-forward (GIT-10).
- That it holds the write lock if it opened for read (STO-4).
- That the CLI's default whole-file Projector (`14`) is installed when a program opens the library with no Projector.

---

## 3. What this document does not decide

Exact MCP tool names and wrapping — a binding appendix, as long as AGT-4 and AGT-5 hold. Field names of Records and errors are `api/v1`, not that appendix.

Whether the agent process is the same as the writer's — deployment (`10` / consumer). The lock rules stay STO-4.

Isolation of secrets from the agent — `09` (**later**).
