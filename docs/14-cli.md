# 14. CLI

Document 14 of the Scheda package. Version 0.60, 31 August 2026.

Target argv for the `scheda` binary. Thin encoding of `07`. JSON is
the Protobuf JSON mapping of `api/v1`.

Unset embedder → no vector leg; `search` is lexical only (`13` §4).
Unset reranker → RRF order.

---

## Global

| Flag | Meaning |
|---|---|
| `--vault PATH` | Vault root. Required. Only Vault filesystem path the user supplies. |
| `--auto-sync` | `OpenOptions.auto_sync` (default off). |
| `--author-name NAME` | `OpenOptions.author.name`. With `--author-email`, or neither. Git author and committer on this process. |
| `--author-email EMAIL` | `OpenOptions.author.email`. With `--author-name`, or neither. HTML email. |
| `--encryption-passphrase-env-var NAME` | **Not implemented yet.** Read `NAME` and use it as `OpenOptions.encryption.passphrase`. Missing or empty var → `invalid`. Not with `--encryption-identity-file`. |
| `--encryption-identity-file PATH` | **Not implemented yet.** Age identity file (`AGE-SECRET-KEY-…`) as `OpenOptions.encryption.identity`. Not with `--encryption-passphrase-env-var`. |
| `--embedder-openai-url URL` | OpenAI-compatible API root (`http`/`https`). CLI `POST`s `{URL}embeddings`. With `--embedder-model` and `--embedder-dimensions`, or none of the three. Partial set → `invalid`. Implements `Embedder`, passed to `open`. Unset (and no `--model2vec-embedder`) → vector leg off. Not with `--model2vec-embedder`. |
| `--embedder-model NAME` | JSON `model` on the embeddings body. With the other two HTTP embedder flags, or none. |
| `--embedder-dimensions N` | Positive integer. `Embedder.dimensions` (no probe). Response length must match. With the other two HTTP embedder flags, or none. |
| `--embedder-api-key-env-var NAME` | Read `NAME` and send `Authorization: Bearer`. Only with the HTTP embedder flags. Unset → no auth. Missing or empty var → `invalid`. Not with `--model2vec-embedder`. |
| `--model2vec-embedder DIR` | Directory with Model2Vec files (`config.json`, `tokenizer.json`, `model.safetensors`). CLI loads them (`model2vec-rs`, no Hub download), implements `Embedder`, passes it to `open`. Missing a required file → `invalid`. Extra files (README) are allowed. Hidden names are ignored. Not with the HTTP embedder flags. |

HTTP embedder body is `{ "model": <embedder-model>, "input": "<text>" }`.

Both flags are specification ahead of code: the CLI has no secret flag of any kind today, so it cannot open a Vault with an encrypted collection at all. What is decided is the shape they must take when they land, and the work is `12` P16. There is no flag that takes the encryption secret as its value, and adding one would be a defect: argv is readable by every process on the machine, so a passphrase on the command line is a passphrase published to the host and left in the shell history. The secret arrives by environment variable or by file path, the same way the embedder API key does. Both flags together → `invalid`. Neither: the handle has no secret, plain collections work, and an encrypted one fails with `crypto` (ISO-5). In a build without the `encrypt` feature either flag is `unsupported_encryption` and nothing is created (SCH-16).

`--help` / `--version` stay. No Record path.

Stdout / stderr: protobuf JSON of the library result or `api.v1.Error`.

---

## Vault commands

| Command | Args |
|---|---|
| `open` | [`--remote-repo URL`] (absolute URI when set) |
| `info` | |
| `create-collection` | `NAME --schema JSON` (or stdin) [`--message TEXT`] |
| `collection` | `NAME <subcommand>` |
| `check` | |
| `reindex` | |
| `find` | [`--collections NAME,…`] `--filter JSON --offset N --count N` [`--include-raw`] |
| `search` | [`--collections NAME,…`] `--query JSON` (or stdin) [`--include-raw`] [`--reranker-cohere-url URL --reranker-model NAME [--reranker-api-key-env-var NAME]`] |
| `push` | |
| `pull` | |
| `sync` | |

`find` / `search` are Vault methods (`07` LIB-10, LIB-11). `--collections`
is a set of names. Unset or empty → every non-isolated, non-encrypted
collection on this Vault, so a broad query surfaces nothing sealed.
Unknown name → `not_found`. Isolated name → an ordinary query: the one
Vault Index holds those Records (ISO-4), and a set mixing isolated with
plain names is one page with one `total`. Encrypted name →
`unsupported`, because no Index holds one (ISO-4). Collection `find` /
`search` below call the same methods with that `NAME` (no
`--collections`), so an isolated `NAME` works there and an encrypted one
is `unsupported`. Stdout JSON includes `collection` on each `Record`
/ `Hit`. `--include-raw` sets `FindQuery.include_raw` /
`SearchQuery.include_raw` (default off: omit `raw`). `--filter` is
`FindQuery.filter` JSON.

The CLI does not encode `begin` / `commit` / `discard`. Each mutating
command (`create-collection`, `collection … add` / `replace` / `remove`) autocommits
with `--message` when set, otherwise `scheda`. An empty `--message` is
`invalid`. Batch writes are a library-handle concern (`07`
LIB-12, LIB-22, LIB-23).

`--reranker-cohere-url URL`: Cohere/Jina API root (`http`/`https`). CLI
`POST`s `{URL}rerank` with `{ query, documents, model }`. With
`--reranker-model`, or neither. Partial set → `invalid`.
`--reranker-api-key-env-var NAME`: read `NAME` and send
`Authorization: Bearer`. Only with those two flags. Unset → no auth.
Missing or empty var → `invalid`. Only on `search`. Unset → RRF.

```text
scheda --vault ./vault \
  --embedder-openai-url 'http://127.0.0.1:8080/v1/' \
  --embedder-model 'Qwen/Qwen3-Embedding-0.6B' \
  --embedder-dimensions 1024 \
  search --query '{"text":"…"}' \
  --reranker-cohere-url 'http://127.0.0.1:8081/v1/' \
  --reranker-model 'Qwen/Qwen3-Reranker-0.6B'
```

Local Qwen sidecars (Linux, host NVIDIA driver; not sandbox-hermetic):

```text
bazel run //python/vllm:embedder
bazel run //python/vllm:reranker
```

Python is locked in `uv.lock` (`vllm==0.27.1`). Sidecars
`snapshot_download` the Qwen repo (tqdm on stderr) into
`<workspace>/.cache/huggingface` (`HF_HOME`; an already-set `HF_HOME`
wins) before `vllm serve`. `tags = ["manual"]` so `bazel build //...`
does not fetch torch.
Defaults fit both processes on one 16 GiB GPU (`--gpu-memory-utilization
0.35`, `--max-model-len 8192`). No extra argv required. Override after
`--` if the card is larger.

---

## Collection commands

`scheda … collection NAME <subcommand>`

| Subcommand | Args |
|---|---|
| `schema` | |
| `add` | `--id ID --record JSON` (or stdin) [`--message TEXT`] |
| `get` | `--id ID` |
| `history` | `--id ID --offset N --count N` |
| `replace` | `--id ID --record JSON` (or stdin) [`--message TEXT`] |
| `remove` | `--id ID` [`--message TEXT`] |
| `find` | `--filter JSON --offset N --count N` [`--include-raw`] (filter JSON or stdin). This collection only; no `--collections`. |
| `search` | `--query JSON` (or stdin) [`--include-raw`] [`--reranker-cohere-url URL --reranker-model NAME [--reranker-api-key-env-var NAME]`]. This collection only. |

Isolated `NAME` is the handle for those Records: `schema`, `add`, `get`,
`history`, `replace`, `remove` (and `lines` on the library, not this CLI).
`find` / `search` on that `NAME` are `unsupported`, same as Vault
`--collections` with an isolated name.

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
`<workspace>/.vault-test-model2vec`.
`--model2vec-embedder` is passed on `reindex` and `search` (default cache
`<workspace>/.cache/model2vec/potion-multilingual-128M`). Missing Model2Vec
files are downloaded on `reindex` from `minishlab/potion-multilingual-128M`
with `huggingface_hub` (`hf-xet`; `config.json`, `tokenizer.json`,
`model.safetensors`). Size and hash must match the Hub tree (LFS SHA-256,
or git blob SHA-1 for small files). `HF_TOKEN` is used when set.
`search` uses the cache as-is (no Hub). `import` / `find` do not load
Model2Vec.
`--backend` is a
top-level flag (before the subcommand). `--vault` still overrides the
Vault path.

`import` creates that Vault with `open` if the path is not a Vault yet.
Then it walks `python/cli/testdata/animals/`:
`create-collection` from each `.schema.yaml` when `info` does not list the
name; `get` then `add` for each Markdown document. Existing ids are
skipped. `open` / `info` / `create-collection` / `get` / `find` do not
pass embedder flags. `cli_LLMs` `add` passes the HTTP embedder so
`index/vector/` is written per Record; skipped ids then `reindex`.
`cli_model2vec` `add` omits `--model2vec-embedder`; it does not
`reindex` (run `reindex` to fill vectors).
`find`, `search`, and `reindex` require that Vault already
exists (`import` first). `reindex` prints `ReindexResult` as YAML.
`cli_LLMs` `reindex` uses the Qwen HTTP embedder; `cli_jina` uses Jina
HTTP; `cli_model2vec` `reindex` uses `--model2vec-embedder` (unless
`--lexical-only`).
`find` is Vault `find` (`--filter` default `{"all":{}}`, `--offset` 0,
`--count` 50). Stdout is YAML: `FindResult.total` then each `Record`
(`---` between documents; `Record.raw` only with `--include-raw`). `search --text` / `--query` / `--topic ID`
prints each `SearchHits.Hit` as YAML (`---` between hits). Bare `search`
runs every topic in fixture `topics.yaml`, checks `expect_ids` /
`expect_collections`, prints a YAML report per topic (`---`), and exits 1
if a topic misses. `--collection NAME` calls
`collection NAME find|search`; `--collections a,b` is Vault-wide.
HTTP embedder and reranker (`cli_LLMs`) default to the local Qwen
sidecars (`bazel run //python/vllm:embedder` on `:8080`, `:reranker` on
`:8081`; `Qwen/Qwen3-Embedding-0.6B` dim 1024 and
`Qwen/Qwen3-Reranker-0.6B`). `cli_jina` uses
`//python/vllm:embedder_jina` / `:reranker_jina` on the same ports
(CC BY-NC). Embedder flags go on every `scheda` call
so `add` / `reindex` / `search` write or query `index/vector/`. Reranker
flags are on `search` only for `cli_LLMs` (`--no-reranker` leaves RRF).
`cli_model2vec` `search` passes `--model2vec-embedder` so the query is
embedded (RRF; no local reranker). `--lexical-only` turns embedder and
reranker flags off. `--workspace`, `--vault`, `--scheda`, `--fixture`,
`--model2vec-embedder`, and the HTTP URL/model flags override the
defaults. The test Vaults and `.cache/` are gitignored.
Index files still live at `{vault}/.scheda/` inside that tree.
