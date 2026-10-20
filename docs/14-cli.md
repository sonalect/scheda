# 14. CLI

Document 14 of the Scheda package. Version 0.86, 20 October 2026.

The `scheda` binary: a thin encoding of `07`. Each subcommand is one rpc
of the contract (`api/scheda/v1`). The process opens the Vault from the
global flags, makes the one call, prints the reply, closes the Vault, and
exits. The reply is the Protobuf JSON of the contract; a failure is an
`ErrorReport` on stderr.

An unset embedder means the process indexes no collection with a `vector`
column, and its searches run without the vector leg (`13`). An unset
reranker means RRF order.

The process runs on a multi-thread tokio runtime (`22` D10); its flags,
JSON, and exit codes are those of the contract.

The CLI gives every Vault it opens a **whole-file Projector**
(`projection_id` `scheda-cli/whole-file@2`): one text column `body`
(marks `fts` and `vector`; no `scalar_index`, which would be a B-tree over
the full text of every Document, and a Filter on `body` scans) whose value
is the whole Document when it is UTF-8 text. An Index built under `@1` is
`INDEX_STALE` for the CLI until one `index reindex`, and the CLI adds that
command to the message of an `INDEX_STALE` failure (test
`an_index_of_another_projection_is_stale_and_one_reindex_mends_it`). A
Filter leaf on a column other than `body`
is `INVALID_ARGUMENT`; `id` is always allowed. One filter applies to every
collection in scope, in a query of the Vault too (`13`). The Projector is
the CLI's: the library installs none when a program passes none.

Because `body` carries the `vector` mark, a CLI process indexes only when
it has an embedder flag, unless the collection's `indexScope` is not `INDEX_SCOPE_ALL`.
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
| `--index-wait-timeout-ms N` | `OpenOptions.index_wait_timeout_ms`: how long a query, a count, or a search waits for the Index to reach the state it needs (`notBefore`); past it the call is `DEADLINE_EXCEEDED`. Unset: 60 000. |
| `--filter-max-depth N` | `OpenOptions.filter_max_depth`: how deep a filter may nest. Unset: 32; at most 100. |
| `--no-wait` | Send no `notBefore` with a query, a count, or a search (the Index answers as it is), and do not wait for the rebuild after `index reindex`. |

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
`RERANKER_ERROR_CODE_REFUSED`. The HTTP embedder sends portions, so one
text can make the embedder refuse a whole portion: the indexer confirms
a refusal on the text alone (`06` IDX-9), and only that text keeps no
vector. A failure of the embedder in the
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
stdin, the whole of it, and parses every line before the Vault is opened: a line that is not valid fails the command with nothing written.

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
the ports. A query, a count, or a search therefore carries a `notBefore`:
the `version` of `Vault.Info` taken when the Vault is opened, unless the
request JSON sets its own. The call waits until the Index reflects that
state, and past `--index-wait-timeout-ms` (60 s) fails with
`DEADLINE_EXCEEDED` (retryable) instead of answering without the latest
write. `--no-wait` sends no `notBefore`: the call answers from the Index as
it is. `index reindex` waits for the rebuild it started to end; a failed
rebuild is its failure, and so is a `lastError` before the rebuild starts,
but for `INDEX_STALE`, which the rebuild ends. `--no-wait` skips that wait
too.

*Check:* `rust/cli/tests/cli.rs`: `no_wait_queries_the_index_as_it_is_and_waiting_sees_the_write`,
`index_watch_until_settled_and_reindex_end`. A query prints the summary
first, a write prints its `version`, and the CLI and the library give the
same JSON: `the_cli_and_the_library_give_the_same_json_and_code`,
`writes_round_trip_with_a_note`, `queries_and_searches_run_across_collections`.

**Known gaps** (small, listed in `.claude/notes/p8-follow-ups.md`):

- A process without an embedder flag does not index a collection with a
  `vector` column (the whole-file Projector marks `body` `vector`), so on a
  Vault no process has indexed yet a query waits `--index-wait-timeout-ms`
  and then fails (`DEADLINE_EXCEEDED`). Index it with a process that has an
  embedder flag (`index watch --until-settled`), or declare the collection
  with `indexScope` `INDEX_SCOPE_NO_VECTORS`. The indexer of the process without the embedder
  reports `lastError` as `FAILED_PRECONDITION` naming the embedder (`index
  watch`).

---

## Vault commands

| Command | Args |
|---|---|
| `scheda-info` | `Scheda.Info`: the version and features of this build. Opens no Vault; no `--vault`. |
| `info` | `Vault.Info`: `path`, `indexPath`, `writer`, `version` (the state token of the stored state). With `--mode create-new` it makes the Vault. |
| `check` | `Vault.Check`: `CheckReport`; `{}` when the Vault is sound. |
| `list-collections` | `Vault.ListCollections`: the `CollectionDeclaration`s. |
| `create-collection` | `NAME [--schema JSON]` (or stdin). JSON is the `Schema`, for example `{"standard":{}}`. |
| `update-collection` | `NAME [--schema JSON]` (or stdin). |
| `delete-collection` | `NAME` [`--ignore-missing`]. |
| `batch-collections` | JSON Lines of `CollectionOperation` on stdin; all or none. |
| `batch-documents` | JSON Lines of `VaultDocumentOperation` (`{"collection":…,"operation":{"create":{…}}}`) on stdin; all or none. Prints the `DocumentBatchResult`: `results` and `version`. |
| `query-documents` | `[--query JSON]` (`VaultQuery`: `collections`, `filter`, `orderBy`, `offset`, `limit`, `startAfter`, `includeRaw`, `notBefore`). First line `{"summary":{"total":"N"}}`, then one `{"document":…}` per line. |
| `count-documents` | `[--query JSON]` (`VaultQuery`). Prints `{"count":"N"}`. |
| `search-documents` | `[--search JSON]` (`VaultSearch`). First line the summary (`{"summary":{"vectorLeg":…}}`), then one `{"hit":…}` per line. |
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
`INDEX_SCOPE_IDS` is `UNIMPLEMENTED` on a search when named, and on a query
that filters or orders when named. Left unnamed, such collections are left out of the
Vault's scope. Keys and revisions only unless the request sets
`includeRaw`. The filter is the typed tree of `13` §3 (`allOf`, `anyOf`,
`noneOf`, `term`, `terms`, `range`, `exists`, `prefix`); a key of the
earlier Tantivy tree (`all`, `phrase`, `regex`, …) is `INVALID_ARGUMENT`
naming its path.

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
| `history` | `--id ID`. The past versions, newest first, one `DocumentHistoryEntry` per line; `writeId` is the same on the versions one write made. A journal is `UNIMPLEMENTED`. |
| `create` | `--id ID` (`--file PATH` or stdin bytes) `--note TEXT` [`--keep-existing`]. In a journal, appends a line. Present → `ALREADY_EXISTS`, or the stored Document with `--keep-existing`. Prints a `DocumentWriteResult`: `{"document":…,"version":…}`. |
| `update` | `--id ID` (`--file` or stdin) `--note TEXT` [`--revision REV`]. Absent → `NOT_FOUND`; another stored revision → `FAILED_PRECONDITION`. A journal is `UNIMPLEMENTED`. Prints a `DocumentWriteResult`. |
| `upsert` | `--id ID` (`--file` or stdin) `--note TEXT`. A journal is `UNIMPLEMENTED`. Prints a `DocumentWriteResult`. |
| `delete` | `--id ID` `--note TEXT` [`--revision REV`] [`--ignore-missing`]. A journal is `UNIMPLEMENTED`. Prints `{"version":…}`. |
| `batch` | JSON Lines of `DocumentOperation` (`{"create":{"id":…,"raw":…,"note":…}}`) on stdin; all or none. Prints the `DocumentBatchResult` with `version`. |
| `query` | `[--query JSON]` (`DocumentQuery`: `filter`, `orderBy` as `[{"column":…,"descending":…}]`, `offset`, `limit`, `startAfter`, `includeRaw`, `notBefore`). This collection only. First line `{"summary":{"total":"N"}}`, then one `{"document":…}` per line. |
| `count` | `[--query JSON]`. Prints `{"count":"N"}`. |
| `search` | `[--search JSON]` (`DocumentSearch`: `text` or `like`, `filter`, `k`, `lexicalWeight`, `vectorWeight`, `rerankLimit`, `includeRaw`, `offset`, `notBefore`). |

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

`IndexState` is `version` (the state the Index reflects), `storedVersion`
(the stored state the indexer last saw), `pending`, `role`, `rebuild`
(`stage`, `cause`, `documentsRead`, `documentsSkipped`, `segmentsRead`),
and `lastError`; unset fields are left out. `index watch --until-settled`
stops at the first state in which `version` equals `storedVersion` and no
rebuild runs, or at the first with a `lastError`. `index reindex` builds a
new generation beside the live one, which answers meanwhile.

---

## Examples

Each runs against a scratch Vault. With no embedder flag, a collection
whose Documents the CLI Projector would embed is not indexed, so the first
example holds no vectors: with `INDEX_SCOPE_NO_VECTORS`, a process
without an embedder indexes the collection for the filter and the lexical
search.

```text
scheda --vault ./vault --mode create-new info
scheda --vault ./vault create-collection notes \
  --schema '{"standard":{},"indexScope":"INDEX_SCOPE_NO_VECTORS"}'
scheda --vault ./vault collection notes create --id giant-panda.md \
  --file ./panda.md --note 'add panda'
scheda --vault ./vault collection notes get --id giant-panda.md
scheda --vault ./vault collection notes query \
  --query '{"filter":{"prefix":{"column":"id","prefix":"giant"}},"limit":10}'
scheda --vault ./vault query-documents \
  --query '{"collections":{"names":["notes"]},"includeRaw":true}'
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

Python is locked in `uv.lock` (`vllm==0.32.0`). Sidecars
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

`find` is `query-documents` (Vault) or `collection NAME query`; the total
is the summary the query streams first (`--filter` default none, every
Document; `--offset` 0, `--count` 50; the request's `limit`). Stdout is
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
