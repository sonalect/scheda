# 08. Agent surface

Document 8 of the Scheda package. Version 0.33, 19 October 2026.

How an AI agent uses Scheda: the same calls as `07`, over a thin
transport. Collection listing is `Vault.ListCollections` (AGT-1). The
agent is not a second store and does not get a looser write path. Codes:
`AGT`. Phase **agent** unless marked otherwise. Terms are in `02`.

An assertion without a check is not a requirement. This phase does not block **core**.

---

## 1. Same API, listing from the declaration

A person may read Vault files without Scheda (STO-2). That is not a substitute for tools: lexical/vector `Collection.Search` and Projector `Filter`s are not available by grepping the tree.

**AGT-1. Catalog from the collection file.** The agent (or the caller wrapping it) can list collections, kinds, isolation, encryption, journal caps, and Index-exclusion flags with `Vault.ListCollections`: the `CollectionDeclaration`s (`scheda.v1.Collections`) serialized from collection files, not inferred from directory names. There is no field catalog: no field names, type marks, or edges. Queryable column names live in the caller's `Projector`, not in the listing. *Check:* a fixture Vault listed as JSON; names, kinds, and perimeter marks match the `.collection.yaml` files (`rust/cli/tests/agent.rs`: `list_collections_matches_the_declaration_files`).

**AGT-2. Few tools, not one per collection.** The tool set is the rpcs of `07` that write or query Documents (`Collection.Create` … `Collection.Search`, `Vault.QueryDocuments`, `Vault.SearchDocuments`, the batches), plus `Vault.CreateCollection`, `Vault.UpdateCollection`, and `Vault.DeleteCollection`. `Vault.ListCollections` is the listing (AGT-1). There is no family of methods named after consumer types. The CLI is one subcommand per rpc (`14`); each write is one call and one commit, with its `--note`, and no default note is supplied. Writes that must happen together use the batch subcommands (`batch-documents`, `batch-collections`, `collection NAME batch`), which read their operations as JSON Lines. *Check:* a scripted CLI run `query-documents` / `search-documents` → `collection NAME get` → `collection NAME create` uses only those tools (`rust/cli/tests/cli.rs`: `writes_round_trip_with_a_note`, `queries_and_searches_run_across_collections`, `batch_documents_and_collection_batch_are_all_or_nothing`).

Include when a mirror exists: `Mirror.Push`, `Mirror.Pull`, `Mirror.Sync`, and `auto_sync` as an open option — same meaning as `04`. Do not add a second “GitHub tool”.

**AGT-3. Writes go through the library.** Tool writes store bytes (SCH-12). Collection declarations are written with `Vault.CreateCollection` and `Vault.UpdateCollection`, not by offering a path. The JSON for those calls is the declaration (`05`); a key the message does not know is `INVALID_ARGUMENT` and nothing is written. There is no tool “write these bytes at this path”. Document files land where MOD-8 says. A human may still edit files in git; that path is not offered to the agent. *Check:* `create-collection` with an unknown schema key → `VAULT_ERROR_CODE_INVALID_ARGUMENT` and no file (`rust/cli/tests/cli.rs`: `create_collection_rejects_an_unknown_schema_key_and_writes_nothing`). *Check:* `create-collection` then `collection NAME create` without a hand-written declaration file; a write the declaration forbids is refused (`rust/cli/tests/agent.rs`: `writes_cannot_bypass_the_schema`). *Check:* `Collection.Create` and `Collection.Update` take bytes (`--file` or stdin), not a JSON object of consumer fields (the subcommands have no such flag).

**AGT-4. Transport is a shell.** CLI, MCP, HTTP, or another agent protocol is a thin encoding of `07` (including `Vault.ListCollections` and `Vault.CreateCollection`). It must not add product rules, scoring, or collection-specific behaviour. JSON of Documents, listing, and errors is the Protobuf JSON mapping of the `scheda.v1` messages, with no exception: the CLI `collection NAME get` prints a `Document`, with `raw` as Base64 (`14`). A failure is an `ErrorReport`. *Check:* the same Vault, same operation, library call vs CLI/MCP: JSON of `Vault.Info`, `Collection.Query`, and one `Collection.Search` hit, and error `code`s match (`rust/cli/tests/cli.rs`: `the_cli_and_the_library_give_the_same_json_and_code`; `rust/cli/tests/agent.rs`: `the_cli_and_the_library_create_collections_with_the_same_json_and_code`).

**AGT-5. Machine-readable results.** Each tool result includes, as applicable: `id`, `collection`, `revision`, `raw` (when `include_raw` asks, and on `Collection.Get`), search scores, and on failure an `ErrorReport`: `code` (the name of a `VaultErrorCode`, as `VAULT_ERROR_CODE_NOT_FOUND`, or the code of a port), `retryable`, `stack`, and `path` when the failure is about one Vault file. `MIRROR_AHEAD` is where `04` says; `CRYPTO` where `09` says. A human `message` is optional and must not replace `code`. `Vault.Check` reports its `Problem`s with the same codes. There is no Document path field and no `valid` flag (it meant Schema match). *Check:* contract tests for `NOT_FOUND`, `INVALID_ARGUMENT`, `ALREADY_EXISTS`, `FAILED_PRECONDITION` (`rust/cli/tests/cli.rs`: `writes_round_trip_with_a_note`, `usage_errors_and_a_missing_vault_are_invalid_argument`), `UNIMPLEMENTED` (`rust/cli/tests/agent.rs`: `writes_cannot_bypass_the_schema`), and `CRYPTO` (`rust/cli/tests/cli.rs`: `encryption_flags_round_trip_and_the_secret_stays_off_the_output`). Not tied to a CLI test: `ABORTED`, `INDEX_REBUILDING`, `INDEX_STALE`, `INDEX_BROKEN` (library tests, `06`).

`Vault.GetCollection` for an isolated collection reaches those Documents. Default Vault queries and searches (no `collections` named) must not return isolated or encrypted Documents (ISO-3). An encrypted collection is listed as encrypted by `Vault.ListCollections`, and the agent's I/O on it uses the same `encryption` secret as `OpenOptions` (the CLI takes it by environment variable or file, `14`); without it those tools fail with `CRYPTO` rather than returning armor bytes as a Document.

---

## 2. What the agent must not assume

- That `Vault.ListCollections` lists queryable field names. Columns are the `Projector`'s (`13`). Collection-level `exclude_lexical_index` / `exclude_vector_index` still apply (SCH-9).
- That `Collection.Search` is a language-model answer (QRY-3).
- That a CLI write without `--note` is accepted, or that a batch is split into commits: a batch is one write and one commit (GIT-2).
- That `open` pulled unless `auto_sync` was on and the pull was a fast-forward (GIT-10).
- That it holds the write lock between calls: a write takes it for that one write (STO-4), and a write that waits past `write_timeout_ms` is `ABORTED`, retryable.
- That a query sees a write at once: reads see the stored state and the Index follows (`06` IDX-9). The CLI waits for the Index before a query unless `--no-wait` (`14`).
- That the CLI's default whole-file Projector (`14`) is installed when a program opens the library with no Projector.

---

## 3. What this document does not decide

Exact MCP tool names and wrapping — a binding appendix, as long as AGT-4 and AGT-5 hold. Field names of Documents and errors are `api/scheda/v1`, not that appendix.

Whether the agent process is the same as the writer's — deployment (`10` / consumer). The lock rules stay STO-4.

Isolation of secrets from the agent — `09`.
