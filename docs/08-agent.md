# 08. Agent surface

Document 8 of the Scheda package. Version 0.7, 24 August 2026.

How an AI agent uses Scheda: the same operations as `07`, plus a Schema catalog, over a thin transport. The agent is not a second store and does not get a looser write path. Codes: `AGT`. Phase **agent** unless marked otherwise. Terms are in `02`.

An assertion without a check is not a requirement. This phase does not block **core**.

---

## 1. Same API, extra catalog

A person may read Vault files without Scheda (STO-2). That is not a substitute for tools: lexical/vector `search`, Schema filters, and edge walks are not available by grepping the tree.

**AGT-1. Catalog from Schema.** The agent (or the caller wrapping it) can list collections, kinds, fields, index marks, edges, and layout templates — serialized from Schema as `api/v1` `Catalog`, not inferred from directory names. *Check:* a fixture Schema exported as JSON; names match the Schema files.

**AGT-2. Few tools, not one per collection.** The tool set is the `07` operations that mutate or query Records and git, plus `catalog`, `check`, `reindex`. There is no family of methods named after consumer types. *Check:* a scripted run “`scan` / `search` → `find_one` → `insert_one` on a `journal` → `commit`” uses only those tools.

Include when a Remote exists: `push_remote`, `pull_remote`, `sync_remote`, and `auto_sync` as an `open` option — same meaning as `04`. Do not add a second “GitHub tool”.

**AGT-3. Writes go through Schema.** Tool writes run SCH-12. There is no tool “write these bytes at this path”. A human may still edit files in git; that path is not offered to the agent. *Check:* `insert_one` / `replace_one` with an extra field or a path outside the layout → `invalid`; the file is not created at a rogue path.

**AGT-4. Transport is a shell.** CLI, MCP, HTTP, or another agent protocol is a thin encoding of `07` + `catalog`. It must not add product rules, scoring, or collection-specific behaviour. JSON of Records and errors is the Protobuf JSON mapping of `api/v1` messages. *Check:* the same Vault, same operation, library call vs CLI/MCP: JSON of the Record (and error `code`) match.

Which transports a binding ships is that binding's choice. MCP is an example, not a requirement of Scheda.

**AGT-5. Machine-readable results.** Each tool result includes, as applicable: `id`, collection, path, `valid`, search scores, and on failure a `code` from LIB-5 (plus `not_fast_forward` where `04` says; **later:** `crypto` where `09` says). A human message is optional and must not replace `code`. *Check:* contract tests for `not_found`, `invalid`, `locked`, `index_unusable`. *Check (**later**):* `crypto`.

**Later:** opening an encrypted isolated root for the agent uses the same `secret` as LIB-13. The main Vault's tools still must not return sealed values (ISO-3).

---

## 2. What the agent must not assume

- That unmarked fields are searchable (SCH-9).
- That `search` is a language-model answer (QRY-3).
- That `commit` happened because it inserted (GIT-2).
- That `open` pulled unless `auto_sync` was on and the pull was a fast-forward (GIT-10).
- That it holds the write lock if it opened for read (STO-4).

---

## 3. What this document does not decide

Exact MCP tool names and wrapping — a binding appendix, as long as AGT-4 and AGT-5 hold. Field names of Records and errors are `api/v1`, not that appendix.

Whether the agent process is the same as the writer's — deployment (`10` / consumer). The lock rules stay STO-4.

Isolation of secrets from the agent — `09` (**later**).
