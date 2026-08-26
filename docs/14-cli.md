# 14. CLI

Document 14 of the Scheda package. Version 0.33, 26 August 2026.

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
| `--embedder-openai-url URL` | OpenAI-compatible API root (`http`/`https`). CLI `POST`s `{URL}embeddings`. With `--embedder-model` and `--embedder-dimensions`, or none of the three. Partial set → `invalid`. Implements `Embedder`, passed to `open`. Unset (and no `--onnx-embedder`) → vector leg off. Not with `--onnx-embedder`. |
| `--embedder-model NAME` | JSON `model` on the embeddings body. With the other two HTTP embedder flags, or none. |
| `--embedder-dimensions N` | Positive integer. `Embedder.dimensions` (no probe). Response length must match. With the other two HTTP embedder flags, or none. |
| `--embedder-api-key-env-var NAME` | Read `NAME` and send `Authorization: Bearer`. Only with the HTTP embedder flags. Unset → no auth. Missing or empty var → `invalid`. Not with `--onnx-embedder`. |
| `--onnx-embedder DIR` | Directory with exactly `model.onnx` and `tokenizer.json` (hidden names such as `.metadata.yaml` are ignored). CLI loads them, implements `Embedder`, passes it to `open`. Missing either file → `invalid`. Not with the HTTP embedder flags. |

HTTP embedder body is `{ "model": <embedder-model>, "input": "<text>" }`.

`--help` / `--version` stay. No Record path.

Stdout / stderr: protobuf JSON of the library result or `api.v1.Error`.

---

## Vault commands

| Command | Args |
|---|---|
| `open` | [`--remote-repo URL`] |
| `info` | |
| `create-collection` | `NAME --schema JSON` (or stdin) |
| `collection` | `NAME <subcommand>` |
| `begin` | |
| `commit` | `-m` / `--message TEXT` |
| `discard` | |
| `check` | |
| `reindex` | |
| `find` | [`--collections NAME,…`] `--filter JSON --offset N --count N` |
| `search` | [`--collections NAME,…`] `--query JSON` (or stdin) [`--reranker-cohere-url URL --reranker-model NAME [--reranker-api-key-env-var NAME]` \| `--reranker-onnx DIR`] |
| `push` | |
| `pull` | |
| `sync` | |

`find` / `search` are Vault methods (`07` LIB-10, LIB-11). `--collections`
is a set of names. Unset or empty → every non-isolated collection on this
Vault. Unknown name → `not_found`. Isolated name → `unsupported` (same as
`collection NAME`). Collection `find` / `search` below call the same
methods with that `NAME` (no `--collections`). Stdout JSON includes
`collection` on each `Record` / `Hit`.

`--reranker-cohere-url URL`: Cohere/Jina API root (`http`/`https`). CLI
`POST`s `{URL}rerank` with `{ query, documents, model }`. With
`--reranker-model`, or neither. Partial set → `invalid`.
`--reranker-api-key-env-var NAME`: read `NAME` and send
`Authorization: Bearer`. Only with those two flags. Unset → no auth.
Missing or empty var → `invalid`. Only on `search`. Not with
`--reranker-onnx`. Unset (and no `--reranker-onnx`) → RRF.

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

Python is locked in `uv.lock` (`vllm==0.27.1`). Weights download at run.
`tags = ["manual"]` so `bazel build //...` does not fetch torch.
Defaults fit both processes on one 16 GiB GPU (`--gpu-memory-utilization
0.35`, `--max-model-len 8192`). No extra argv required. Override after
`--` if the card is larger.

---

## Collection commands

`scheda … collection NAME <subcommand>`

| Subcommand | Args |
|---|---|
| `schema` | |
| `add` | `--id ID --record JSON` (or stdin) |
| `get` | `--id ID` |
| `history` | `--id ID --offset N --count N` |
| `replace` | `--id ID --record JSON` (or stdin) |
| `find` | `--filter JSON --offset N --count N` (filter JSON or stdin). This collection only; no `--collections`. |
| `search` | `--query JSON` (or stdin) [`--reranker-cohere-url URL --reranker-model NAME [--reranker-api-key-env-var NAME]` \| `--reranker-onnx DIR`]. This collection only. |

---

## Test driver

`python/cli` talks to this binary. It is not a language binding (`08`).

```text
bazel run //python/cli:cli -- import
bazel run //python/cli:cli -- reindex
bazel run //python/cli:cli -- find
bazel run //python/cli:cli -- search
python -m python.cli import
```

`import` creates `<workspace>/.vault-test` with `open` if that path is not
a Vault yet. Then it walks `python/cli/testdata/animals/`:
`create-collection` from each `.schema.yaml` when `info` does not list the
name; `get` then `add` for each Markdown document. Existing ids are
skipped; if any were skipped, `reindex` runs so already-imported files
get vectors. `find`, `search`, and `reindex` require that Vault already
exists (`import` first). `reindex` prints `ReindexResult` and uses the
same embedder flags as `import` (Qwen sidecars unless `--lexical-only`).
`find` is Vault `find` (`--filter` default `{"all":{}}`, `--offset` 0,
`--count` 50). `search --text` / `--query` / `--topic ID`
prints `SearchHits`. Bare `search` runs every topic in fixture
`topics.yaml`, checks `expect_ids` / `expect_collections`, prints a
report, and exits 1 if a topic misses. `--collection NAME` calls
`collection NAME find|search`; `--collections a,b` is Vault-wide.
HTTP embedder and reranker default to the local Qwen sidecars
(`bazel run //python/vllm:embedder` on `:8080`, `:reranker` on
`:8081`; `Qwen/Qwen3-Embedding-0.6B` dim 1024 and
`Qwen/Qwen3-Reranker-0.6B`). Embedder flags go on every `scheda` call
so `add` / `reindex` write `index/vector/`. Reranker flags are on
`search` only (`--no-reranker` leaves RRF). `--lexical-only` turns both
off. `--workspace`, `--vault`, `--scheda`, `--fixture`, and the HTTP
URL/model flags override the defaults. The test Vault is gitignored.
Index files still live at `{vault}/.scheda/` inside that tree.
