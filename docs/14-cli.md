# 14. CLI

Document 14 of the Scheda package. Version 0.73, 19 October 2026.

The `scheda` binary: a thin encoding of `07`. Each subcommand is one rpc
of the contract (`api/scheda/v1`). The process opens the Vault from the
global flags, makes the one call, prints the reply, closes the Vault, and
exits. The reply is the Protobuf JSON of the contract; a failure is an
`ErrorReport` on stderr.

An unset embedder means no vector leg: a search is lexical only (`13`).
An unset reranker means RRF order.

The CLI gives every Vault it opens a **whole-file Projector**
(`projection_id` `scheda-cli/whole-file@1`): one text column `body`
(marks `find`, `fts`, and `vector`) whose value is the whole Document when
it is UTF-8 text. A Filter leaf on a Projector column other than `body`
is `INVALID_ARGUMENT`; `id` is always allowed. A leaf in a query of the
Vault (`query-documents`, `count-documents`, `search-documents`) names its
`collection` (`13`). The Projector is the CLI's: the library installs none
when a program passes none.

Because `body` carries the `vector` mark, a CLI process indexes only when
it has an embedder flag, unless the collection sets `exclude_vector_index`.
The Index follows `HEAD` in the background of whichever process has the
ports (`06` IDX-9); a short CLI process indexes what it finds and stops
when it closes.

---

## Output and exit status

| Stream | Content |
|---|---|
| stdout | The Protobuf JSON of the reply: lowerCamelCase keys, unset fields left out, 64-bit numbers as strings, `bytes` (`raw`) as Base64. One line. `google.protobuf.Empty` is `{}`. |
| stdout, a stream reply | JSON Lines: one item per line, written as it comes (`collection NAME query`, `get-many`, `history`, `search`, the `…-documents` queries, `index watch`). |
| stderr | One line: the `ErrorReport` of the failure (`code` as `VAULT_ERROR_CODE_NOT_FOUND` or the code of a port, `message`, `retryable`, `stack`, `path`). Never replaces `code` with `message`. |
| exit status | `0` on success, `1` on any failure, including a usage error (which is `VAULT_ERROR_CODE_INVALID_ARGUMENT`). `--help` and `--version` exit `0`. |

The items of a stream before a failure stay written. A reader that closes
stdout ends the command quietly with status `0`. Any other failure to
write stdout is `STORAGE`.

`collection NAME get` prints a `Document` like every other call: `raw` is
Base64. There is no raw-bytes mode.

---

## Global flags

All of them go before the subcommand or after it.

| Flag | Meaning |
|---|---|
| `--vault PATH` | The Vault folder. Required by every subcommand but `scheda-info`. The only filesystem path the user supplies for the Vault. |
| `--mode MODE` | `open-or-create`, `open-existing`, or `create-new` (`OpenOptions.mode`). Unset: the library default (open, or make when the folder holds no Vault). |
| `--mirror URL` | `OpenOptions.mirror`: the git remote URL of the mirror, set at open. |
| `--auto-sync` | `OpenOptions.auto_sync` (default off): pull on open, push after a local commit, when a mirror exists. |
| `--author-name NAME` | `OpenOptions.author.name`. With `--author-email`, or neither; one alone is `INVALID_ARGUMENT`. |
| `--author-email EMAIL` | `OpenOptions.author.email`. With `--author-name`, or neither. |
| `--writer ID` | `OpenOptions.writer`: the writer id of this process. |
| `--write-timeout-ms N` | `OpenOptions.write_timeout_ms`: how long a write waits for the write lock; past it the write is `ABORTED`. |
| `--encryption-passphrase-env-var NAME` | Read `NAME` and use it as `OpenOptions.encryption.passphrase`. Missing or empty variable → `INVALID_ARGUMENT` before the Vault is opened. Not with `--encryption-identity-file`. |
| `--encryption-identity-file PATH` | An age identity file (`AGE-SECRET-KEY-…`; comments and a trailing newline as `age-keygen` writes) as `OpenOptions.encryption.identity`. Unreadable or malformed → `INVALID_ARGUMENT`. Not with `--encryption-passphrase-env-var`. |
| `--embedder-openai-url URL` | OpenAI-compatible API root (`http`/`https`). The CLI `POST`s `{URL}embeddings`. With `--embedder-model` and `--embedder-dimensions`, or none of the three; a partial set is `INVALID_ARGUMENT`. Implements the `Embedder` port. Not with `--model2vec-embedder`. |
| `--embedder-model NAME` | The JSON `model` of the embeddings body, and the model id of the embedder. |
| `--embedder-dimensions N` | A positive integer: the length of the vectors (no probe). A reply of another length fails the call. |
| `--embedder-api-key-env-var NAME` | Read `NAME` and send `Authorization: Bearer`. Only with the HTTP embedder flags. Unset: no auth. Missing or empty variable → `INVALID_ARGUMENT`. |
| `--model2vec-embedder DIR` | A folder with a Model2Vec model (`config.json`, `tokenizer.json`, `model.safetensors`). The CLI loads it (`model2vec-rs`, no Hub download) as the `Embedder`. A missing required file, or a path that is not a folder → `INVALID_ARGUMENT`. Extra files are allowed. Not with the HTTP embedder flags. |
| `--reranker-cohere-url URL` | Cohere or Jina rerank API root (`http`/`https`). The CLI `POST`s `{URL}rerank`. With `--reranker-model`, or neither. Implements the `Reranker` port, used by a search with `rerankLimit`. |
| `--reranker-model NAME` | The JSON `model` of the rerank body. |
| `--reranker-api-key-env-var NAME` | Read `NAME` and send `Authorization: Bearer`. Only with the HTTP reranker flags. |
| `--no-wait` | Do not wait for the Index before a query, a count, or a search, and do not wait for the rebuild after `index reindex`. |

Every wrong or contradictory set of these flags is `INVALID_ARGUMENT`
before the Vault is touched. There is no flag that takes the encryption
secret as its value, and adding one would be a defect: argv is readable
by every process on the machine, so a passphrase on the command line is
a passphrase published to the host and left in the shell history. The
secret arrives by environment variable or by file path, the same way an
API key does. With neither flag the handle has no secret: plain
collections work, and an encrypted one fails with `CRYPTO` (ISO-5). The
CLI does not parse `OpenOptions` from JSON; a key such as
`isolatedEncryption` has no input to land on. In a build without the
`encrypt` feature either flag is `UNIMPLEMENTED` and nothing is created
(SCH-16).

`--help` and `--version` are the clap ones; `{binary} SUBCOMMAND --help`
prints the flags of one subcommand.

### The HTTP ports

The HTTP embedder's model id is exactly `--embedder-model`: no URL, query,
or userinfo is stored in the Index, and a host change alone is not a
model change. The body is `{ "model": <embedder-model>, "input": "<text>" }`
for one text, or `"input": ["…", …]` for a batch of at most 2048. The
rerank body is `{ "model": <reranker-model>, "query": "…", "documents":
["…", …] }` for one Document or a batch of at most 1000.

HTTP status is checked before the body is read; the message is `HTTP
{status} from {host}` and never the body (it may echo the key). A port
fails with the code of its port (`EmbedderErrorCode`, `RerankerErrorCode`):

| Case | Code | `retryable` |
|---|---|---|
| Transport failure, 408, 429, 5xx | `…_UNAVAILABLE` | yes |
| 401, 403 | `…_UNAVAILABLE` | no |
| Any other non-2xx status | `…_REFUSED` | no |
| A 2xx body of the wrong shape or the wrong vector length | `…_UNAVAILABLE` | no |

Code names are `EMBEDDER_ERROR_CODE_UNAVAILABLE`,
`EMBEDDER_ERROR_CODE_REFUSED`, `RERANKER_ERROR_CODE_UNAVAILABLE`, and
`RERANKER_ERROR_CODE_REFUSED`. A failure of the embedder in the
background indexer is not the failure of a command: it shows as
`lastError` of the Index state (`index status`, `index watch`).

The Model2Vec model id is `model2vec:` plus 16 hex digits of the SHA-256
of `config.json` followed by the byte length of `model.safetensors`, so
moving or renaming the directory is not a model change.

*Check:* `rust/cli/tests/cli.rs`: `open_flags_reach_the_library`,
`port_flags_that_do_not_fit_together_are_invalid_argument`,
`encryption_flags_round_trip_and_the_secret_stays_off_the_output`,
`no_flag_takes_the_secret_itself`,
`the_http_ports_write_vectors_and_search_like_the_library_with_the_same_ports`;
`rust/cli/src/http.rs`: `the_embedder_fails_with_the_code_of_its_port`,
`statuses_map_to_transient_credential_and_refused_problems`;
`rust/cli/src/model2vec.rs` (the model id).

---

## How a request is given

A structured request is its Protobuf JSON, in a flag or on stdin when the
flag is absent: `--schema`, `--query`, `--search`. A Document's bytes come
from `--file PATH` or stdin; with `--file`, stdin is not read (an open
pipe nobody closes cannot block the command). A batch reads JSON Lines from
stdin, lazily.

Every JSON is parsed strictly (`scheda::parse_command`): a key the message
does not know is `INVALID_ARGUMENT` naming its path, and nothing is
written. A file that cannot be read, or a missing input, is
`INVALID_ARGUMENT` before the Vault is opened. Every write takes `--note`,
its commit note; the CLI supplies no default and an empty one is
`INVALID_ARGUMENT`.

---

## The Index and waiting

A CLI command is its own process and `Vault.Close` stops its indexer, so
the commits of earlier commands are indexed by whichever later process has
the ports. A query, a count, or a search therefore first waits, up to 60 s,
on `Index.Watch` for a settled state: the Index has a `version`, `pending`
is 0, no rebuild runs, or `lastError` is set. Past the deadline the call
runs as the Index is, and fails with its own code
(`INDEX_REBUILDING`). `--no-wait` skips the wait. `index reindex` waits for
the rebuild it started to end; a failed rebuild is its failure.

*Check:* `rust/cli/tests/cli.rs`: `no_wait_queries_the_index_as_it_is_and_waiting_sees_the_write`,
`index_watch_until_settled_and_reindex_end`.

**Known gaps** (small, listed in `.claude/notes/p8-follow-ups.md`):

- `index status` in a new process prints `{}`: the process asks before its
  indexer has looked at the Index. Use `index watch --until-settled`; the
  last line is the settled state.
- A process without an embedder flag does not index a collection with a
  `vector` column, so on a Vault no process has indexed yet a query waits
  the 60 s and then fails (`INDEX_REBUILDING`, or `INVALID_ARGUMENT` for a
  column the Index does not know yet).
- `index watch` without `--until-settled` learns that its reader has gone
  only when it writes the next state.

---

## Vault commands

| Command | Args |
|---|---|
| `scheda-info` | `Scheda.Info`: the version and features of this build. Opens no Vault; no `--vault`. |
| `info` | `Vault.Info`: `path`, `indexPath`, `writer`. With `--mode create-new` it makes the Vault. |
| `check` | `Vault.Check`: `CheckReport`; `{}` when the Vault is sound. |
| `list-collections` | `Vault.ListCollections`: the `CollectionDeclaration`s. |
| `create-collection` | `NAME [--schema JSON]` (or stdin). JSON is the `Schema`, for example `{"standard":{}}`. |
| `update-collection` | `NAME [--schema JSON]` (or stdin). |
| `delete-collection` | `NAME` [`--ignore-missing`]. |
| `batch-collections` | JSON Lines of `CollectionOperation` on stdin; all or none. |
| `batch-documents` | JSON Lines of `VaultDocumentOperation` (`{"collection":…,"operation":{"create":{…}}}`) on stdin; all or none. |
| `query-documents` | `[--query JSON]` (`VaultQuery`). |
| `count-documents` | `[--query JSON]` (`VaultQuery`). Prints `{"count":"N"}`. |
| `search-documents` | `[--search JSON]` (`VaultSearch`). First line the summary (`vectorLeg`), then one hit per line. |
| `collection` | `NAME <subcommand>` |
| `mirror` | `<subcommand>` |
| `index` | `<subcommand>` |

`query-documents`, `count-documents`, and `search-documents` take
`collections.names` in the request. Unset or empty: every collection that
is neither isolated nor encrypted, so a broad query surfaces nothing
sealed. An unknown name is `NOT_FOUND`. An isolated name is an ordinary
query: the one Vault Index holds those Documents (ISO-4), and a set that
mixes isolated with plain names is one page. An encrypted name is
`UNIMPLEMENTED`, because no Index holds one (ISO-4). A collection with
both Index legs excluded is `UNIMPLEMENTED` on a search; one with
`exclude_lexical_index` alone is `UNIMPLEMENTED` on a query. Keys and
revisions only unless the request sets `includeRaw`.

`DeleteCollection`, `UpdateCollection`, and the batches follow the rules in
the rpc comments of `api/scheda/v1/vault.proto` and `collection.proto`.
`create-collection NAME` and the batches write the declaration through the
library; a `fields` or `version` key, or any key the message does not know,
is refused.

## Collection commands

`scheda … collection NAME <subcommand>`

| Subcommand | Args |
|---|---|
| `describe` | The declaration of this collection. |
| `get` | `--id ID`. The `Document`: `collection`, `id`, `raw` (Base64), `revision`. |
| `get-many` | `--ids ID,…`, or a listing: [`--limit N`] [`--start-after ID`] (not with `--ids`). [`--include-raw`]. Reads files, not the Index. |
| `history` | `--id ID`. The past versions, newest first, one `DocumentHistoryEntry` per line. A journal is `UNIMPLEMENTED`. |
| `create` | `--id ID` (`--file PATH` or stdin bytes) `--note TEXT` [`--keep-existing`]. In a journal, appends a line. Present → `ALREADY_EXISTS`, or the stored Document with `--keep-existing`. |
| `update` | `--id ID` (`--file` or stdin) `--note TEXT` [`--revision REV`]. Absent → `NOT_FOUND`; another stored revision → `FAILED_PRECONDITION`. A journal is `UNIMPLEMENTED`. |
| `upsert` | `--id ID` (`--file` or stdin) `--note TEXT`. A journal is `UNIMPLEMENTED`. |
| `delete` | `--id ID` `--note TEXT` [`--revision REV`] [`--ignore-missing`]. A journal is `UNIMPLEMENTED`. |
| `batch` | JSON Lines of `DocumentOperation` (`{"create":{"id":…,"raw":…,"note":…}}`) on stdin; all or none. |
| `query` | `[--query JSON]` (`DocumentQuery`: `filter`, `includeRaw`, `offset`, `limit`, `startAfter`). This collection only; a leaf names no collection. |
| `count` | `[--query JSON]`. |
| `search` | `[--search JSON]` (`DocumentSearch`: `text` or `like`, `filter`, `k`, `lexicalWeight`, `vectorWeight`, `rerankLimit`, `includeRaw`, `offset`). |

Bytes of any kind are a Document (STO-2); empty bytes are allowed. A
`--file` that cannot be read is `INVALID_ARGUMENT`; the path is in the
message and `path` stays unset because it is not Vault-relative.

An isolated `NAME` is the handle for those Documents; `query` and `search`
on it are ordinary queries. An encrypted `NAME` is `UNIMPLEMENTED` on
`query` and `search`. Without the secret, `get`, `get-many`, and writes on
it are `CRYPTO`.

`search` with `rerankLimit` needs the reranker flags; without a reranker it
is `INVALID_ARGUMENT`. `rerankLimit` below `k` is `INVALID_ARGUMENT`.

## Mirror commands

`scheda … mirror <subcommand>`

| Subcommand | Args |
|---|---|
| `info` | `{url, autoSync}`, and the last auto-sync failure when there was one. |
| `set` | `URL` [`--replace-existing`]. An absolute URI or an scp-style `git@host:path`. Another URL set already is `ALREADY_EXISTS` without `--replace-existing`. |
| `remove` | Detach the mirror. None is `NOT_FOUND`. |
| `push` | Push the Vault to the mirror. |
| `pull` | Pull the mirror into the Vault. |
| `sync` | Pull, then push. |

Only `push`, `pull`, `sync`, and an open with `--auto-sync` use the
network. The mirror never carries `.isolated/` (ISO-6).

*Check:* `rust/cli/tests/cli.rs`: `the_mirror_is_set_pushed_and_removed`.

## Index commands

`scheda … index <subcommand>`

| Subcommand | Args |
|---|---|
| `status` | `Index.Status`: the `IndexState` now. |
| `watch` | `Index.Watch`: the state, then each change, one line each. [`--until-settled`] stops after the first settled state. |
| `reindex` | `Index.Reindex`: rebuild the Index with the ports of this process, wait until the rebuild is done (unless `--no-wait`), print `{}`. |

`IndexState` is `version` (the commit the Index covers), `pending`, `role`,
`modelMismatch`, `rebuild` (`stage`, `documentsRead`, `documentsSkipped`,
`segmentsRead`), and `lastError`; unset fields are left out.

---

## Examples

Each runs against a scratch Vault. With no embedder flag, a collection
whose Documents the CLI Projector would embed is not indexed, so the first
example excludes the vector leg: with `excludeVectorIndex`, a process
without an embedder indexes the collection for the filter and the lexical
search.

```text
scheda --vault ./vault --mode create-new info
scheda --vault ./vault create-collection notes \
  --schema '{"standard":{},"excludeVectorIndex":true}'
scheda --vault ./vault collection notes create --id giant-panda.md \
  --file ./panda.md --note 'add panda'
scheda --vault ./vault collection notes get --id giant-panda.md
scheda --vault ./vault collection notes query \
  --query '{"filter":{"phrase":{"field":"body","terms":["giant","panda"]}},"limit":10}'
scheda --vault ./vault query-documents \
  --query '{"collections":{"names":["notes"]},"filter":{"all":{}},"includeRaw":true}'
scheda --vault ./vault collection notes search \
  --search '{"text":"bamboo","k":2,"vectorWeight":0}'
scheda --vault ./vault index watch --until-settled
printf '%s\n' \
  '{"collection":"notes","operation":{"create":{"id":"a.md","raw":"aGk=","note":"batch"}}}' \
  | scheda --vault ./vault batch-documents
```

A search with a model and a reranker over HTTP:

```text
scheda --vault ./vault \
  --embedder-openai-url 'http://127.0.0.1:8080/v1/' \
  --embedder-model 'Qwen/Qwen3-Embedding-0.6B' \
  --embedder-dimensions 1024 \
  --reranker-cohere-url 'http://127.0.0.1:8081/v1/' \
  --reranker-model 'Qwen/Qwen3-Reranker-0.6B' \
  search-documents \
  --search '{"search":{"text":"seasonal migration","k":5,"rerankLimit":50}}'
```

Local Qwen sidecars (Linux, host NVIDIA driver; not sandbox-hermetic):

```text
bazel run //python/vllm:embedder
bazel run //python/vllm:reranker
```

Python is locked in `uv.lock` (`vllm==0.29.0`). Sidecars
`snapshot_download` the Qwen repo (tqdm on stderr) into
`<workspace>/.cache/huggingface` (`HF_HOME`; an already-set `HF_HOME`
wins) before `vllm serve`. `tags = ["manual"]` so `bazel build //...`
does not fetch torch.
Defaults fit both processes on one 16 GiB GPU (`--gpu-memory-utilization
0.35`, `--max-model-len 8192`). No extra argv required. Override after
`--` if the card is larger. On WSL2, `python/vllm/serve.py` sets
`VLLM_WSL2_ENABLE_PIN_MEMORY=1` so vLLM's default V2 runner can use UVA.

---

## Test driver

`python/cli` talks to this binary. It is not a language binding (`08`).
Three Bazel targets, three Vaults:

```text
bazel run //python/cli:cli_LLMs -- import
bazel run //python/cli:cli_LLMs -- reindex
bazel run //python/cli:cli_LLMs -- find
bazel run //python/cli:cli_LLMs -- search
bazel run //python/cli:cli_jina -- import
bazel run //python/cli:cli_model2vec -- import
bazel run //python/cli:cli_model2vec -- reindex
python -m python.cli --backend llms import
python -m python.cli --backend jina import
python -m python.cli --backend model2vec import
```

`cli_LLMs` uses `<workspace>/.vault-test-LLMs` and the Qwen HTTP
sidecars. `cli_jina` uses `<workspace>/.vault-test-jina` and the Jina
HTTP sidecars (`jinaai/jina-embeddings-v5-text-small` +
`jinaai/jina-reranker-v3.5`, CC BY-NC 4.0; same ports as Qwen — stop
the Qwen processes first). `cli_model2vec` uses
`<workspace>/.vault-test-model2vec`. `--backend` is a top-level flag
(before the subcommand). `--vault` still overrides the Vault path.

Every call is a `scheda` process with `--vault` and `--mode open-existing`
(`--mode create-new` for the one that makes the Vault). Stdout of a
call is parsed as JSON or JSON Lines; a failure is read from the
`ErrorReport` on stderr.

`import` makes the Vault with `--mode create-new info` if the path is not
a Vault yet. Then it walks `python/cli/testdata/animals/`:
`create-collection` from each `.collection.yaml` when `list-collections`
does not list the name; `collection NAME get` then `collection NAME create
--file --note 'import fixture'` for each Document file. Existing ids are
skipped. Those calls pass no embedder flags. A write does not wait for the
Index and each `scheda` process stops its indexer when it exits, so for
`cli_LLMs` and `cli_jina` the driver then runs `index watch
--until-settled` with the HTTP embedder: that process indexes the writes
and fills the vectors. `cli_model2vec` `import` loads no model; the first
later command that passes `--model2vec-embedder` (`reindex`, `find`, or
`search`) indexes.

`find`, `search`, and `reindex` require that Vault already exists
(`import` first). `reindex` runs `index reindex`, which waits for the
rebuild, then `index watch --until-settled`, and prints the settled
`IndexState` as YAML (a new process's `index status` would be `{}`).
`cli_LLMs` `reindex` uses the Qwen HTTP embedder; `cli_jina` uses Jina
HTTP; `cli_model2vec` `reindex` uses `--model2vec-embedder`, downloading
the missing files first from `minishlab/potion-multilingual-128M` with
`huggingface_hub` (`hf-xet`; `config.json`, `tokenizer.json`,
`model.safetensors`; size and hash must match the Hub tree: LFS SHA-256,
or git blob SHA-1 for small files; `HF_TOKEN` is used when set). `find`
and `search` use the cache as-is (no Hub). The default cache is
`<workspace>/.cache/model2vec/potion-multilingual-128M`.

The embedder flags go on every call that may index or embed (`reindex`,
`find`, `search`, the settle step of `import`), so the Vault's index is
built with one model. `--lexical-only` keeps them: the Vault indexes with
the embedder, and only the ranking changes. It sets `vectorWeight: 0` on
the `DocumentSearch` and turns the reranker off.

`find` is `query-documents` (Vault) or `collection NAME query`, with a
`count-documents` / `count` for the total (`--filter` default
`{"all":{}}`, `--offset` 0, `--count` 50; the request's `limit`). Stdout is
YAML: `total`, then each `Document` (`---` between documents; `raw` as text
and only with `--include-raw`). `search --text` / `--query` / `--topic ID`
runs `search-documents` or `collection NAME search`, and prints `vectorLeg`
and each hit, flattened with its Document, as YAML (`---` between hits).
`--query` is the `DocumentSearch` JSON; the driver adds `includeRaw`,
`vectorWeight`, and `rerankLimit` as the options ask. Bare `search` runs
every topic in fixture `topics.yaml`, checks `expect_ids` /
`expect_collections`, prints a YAML report per topic (`---`), and exits 1
if a topic misses. `--collection NAME` calls `collection NAME
query|search`; `--collections a,b` is Vault-wide.

HTTP embedder and reranker (`cli_LLMs`) default to the local Qwen sidecars
(`bazel run //python/vllm:embedder` on `:8080`, `:reranker` on `:8081`;
`Qwen/Qwen3-Embedding-0.6B` dim 1024 and `Qwen/Qwen3-Reranker-0.6B`).
`cli_jina` uses `//python/vllm:embedder_jina` / `:reranker_jina` on the
same ports (CC BY-NC). Reranker flags go on `search` only, for `cli_LLMs`
and `cli_jina` (`--no-reranker` leaves RRF). `cli_model2vec` has no
reranker: RRF. `--workspace`, `--vault`, `--scheda`, `--fixture`,
`--model2vec-embedder`, and the HTTP URL/model flags override the
defaults. The test Vaults and `.cache/` are gitignored. Index files live at
`{vault}/.scheda/` inside that tree.

*Check:* `bazel test //python:lint` (the driver) and the Python driver run
by hand against a Vault (`import`, `reindex`, `find`, `search`); no
automated test runs it against the sidecars.
