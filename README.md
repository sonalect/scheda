# Scheda

Scheda is a human-, AI-agent-, and process-friendly **library** for a
document store: records are ordinary text files in a git working tree,
not a database file. It is not a wiki, a second brain, a daemon, or a
hosted product.

**What it is.** Records live as ordinary UTF-8 files in a **Vault** — a git
working tree a person can open in an editor and in `git` without this
library. Collections are `document` (one file per record; path is the `id`),
`journal` (append-only envelope lines), or `singleton`. A **collection
declaration** in the Vault names write mode and perimeter, not Record
fields. Columns for `find` / `search` come from a caller **Projector** at
`open`. The library does not know a consumer's types or file format. The
**Index** is derived, lives under `{vault}/.scheda/` (gitignored), and can
be deleted and rebuilt from files. The public API is CRUD-shaped
(`open`, `add`, `get`, `replace`, `remove`, `find`, `search`, `begin`, `commit`,
`discard`). The CLI encodes those operations except `begin` / `commit` /
`discard` (each mutating CLI command autocommits). It is not a compatibility layer for MongoDB, CouchDB, or SQL.
The first implementation is Rust; others (Python, Java, Go, TypeScript, and
more) may follow.

**Why it exists.** Work that matters still lands as text: notes, configs,
journals, Markdown, YAML, TOML. Git already versions that text. The
moment those files need to be queried — by field, by words, by meaning — the
usual move is a document database plus a vector database. The files become
an export. The human and the AI agent no longer share one tree: the agent
searches a store the human cannot `git log`, and the human edits files the
index does not see until someone remembers to sync. Scheda is the case in
between: **files stay the source of truth**, git stays the history, and a
library gives structured access and search without inventing a second store.

**How it is useful.** A program can `find` and `search` (BM25, in-process
ANN, RRF) and write records without treating the index as the database:
wipe the Index, rebuild, get the same files back. An AI agent gets the same
operations as the library (plus `info` from collection declarations and `create_collection`),
not a looser “write any Markdown anywhere” path. A person still diffs a
field as text and recovers last week's bytes with git. Embedding weights,
if any, stay in the caller's install; without an embedder, vector search is
off and the rest still works. The library does not score records for a UI,
does not call a language model, and does not know a profession.

**Status: core C0–C10, search C9, and agent A0–A7 in the Rust crate.** Files, git, collection declaration, `find` /
`get`, journal `id` map, hybrid `search` (BM25 + in-process ANN +
RRF from Projector slots), core acceptance, listing via `Vault.info`, `create_collection`, a CLI encoding
of `07` (default whole-file Projector column `body`), HTTP embedder/reranker, and agent tools that omit isolated sealed values. Opaque Records (`16` F0–F19) are in. Consumer readiness P0–P16 is in (`12` §4): fixture, journal `{id}: {payload}` envelopes, `Collection.lines` without the Index, many journal writes as one library `begin`/`commit`, unencrypted `{vault}/.isolated/` nested git, dead `id` knobs out so the slug is the whole `id` rule, `find` / `search` as crate features, `remove` on `document` and `singleton`, acceptance on that fixture, a readable commit subject plus an identity the caller chooses, `documents` / `ids`, collection encryption (`encrypt` feature), one owner-only Index so a named isolated collection is an ordinary `find` / `search`, a CLI that can carry an encryption secret, and a Remote for the nested isolated tree. The Index follows git `HEAD` in a background thread (`17` Part B): writes do not wait for it, `find` / `search` are eventually consistent (`wait_indexed` / `index_status`), several processes share one indexer, and an old or damaged Index rebuilds itself. CLI mutations autocommit; batch writes are in-process.

## Documents

The design is a numbered package in `docs/`, in English.

| File                                                  | Contents                                                                                                              |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| [01-concept.md](docs/01-concept.md)                   | Why Scheda exists, what a Vault is, what the library refuses to be                                                    |
| [02-glossary.md](docs/02-glossary.md)                 | Normative vocabulary: Vault, Collection, Record, collection declaration, Projector, Index, kinds, API names           |
| [03-model.md](docs/03-model.md)                       | Collection kinds, Record `id` and paths, opaque bytes, journal envelope                                               |
| [04-files-and-git.md](docs/04-files-and-git.md)       | Files as truth, write lock, commit, `auto_sync`, `sync`, clone, no LFS                                                |
| [05-collection-declaration.md](docs/05-collection-declaration.md) | Collection declaration: write mode, perimeter, generation `version`, journal caps                                     |
| [06-index-and-search.md](docs/06-index-and-search.md) | Disposable Index, `get`                                                                                               |
| [13-find-and-search.md](docs/13-find-and-search.md)   | Target `find` / `search`: Tantivy Query, hybrid BM25 + USearch, Projector columns, optional reranker                   |
| [14-cli.md](docs/14-cli.md)                           | Target `scheda` CLI argv (`07`; `--embedder-openai-url` or `--model2vec-embedder`; `--reranker-cohere-url` on `search`) |
| [07-api.md](docs/07-api.md)                           | Language-neutral library operations; message fields in `api/v1`                                                       |
| [08-agent.md](docs/08-agent.md)                       | Same operations for an AI agent, declaration listing, thin transport                                                  |
| [09-isolation.md](docs/09-isolation.md)               | No network by default (**core**); isolated git root and age encryption (**later**, in this crate)                      |
| [10-roadmap.md](docs/10-roadmap.md)                   | Phases (core / search / agent / later), acceptance, what `scheda` codes next, size targets, open questions, deferrals |
| [11-core-plan.md](docs/11-core-plan.md)               | Rust work order: C0–C8 and C10 core (done), C9 hybrid search (done)                                                   |
| [12-agent-plan.md](docs/12-agent-plan.md)             | Rust work order: A0–A7 done, **agent** accepted; P0–P16 in (`14` secret flags, nested Remote) |
| [15-online-reindex.md](docs/15-online-reindex.md)     | Rust work order: `reindex` without holding `write.lock` for the walk (R0–R8); its queue is replaced by `17` |
| [16-opaque-records.md](docs/16-opaque-records.md)     | Rust work order: opaque UTF-8 Records, collection declaration without field Schema, Projector callback (F0–F19 done). |
| [17-background-index-and-tokio.md](docs/17-background-index-and-tokio.md) | Rust work order: background Index that follows `HEAD` (B1–B10 done), opt-in `tokio` feature (Part T, next) |

## Layout

| Path                               | Role                                                                                                                                              |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`Cargo.toml`](Cargo.toml)         | Workspace: members, pinned dependency versions, lints                                                                                             |
| [`api/v1`](api/v1)                 | Library API messages (Protobuf edition 2024; not the Vault on-disk format)                                                                        |
| [`rust/api`](rust/api)             | Generated Rust stubs (`scheda-api`) from `api/v1`                                                                                                 |
| [`rust/scheda`](rust/scheda)       | First implementation: Rust crate `scheda`                                                                                                         |
| [`rust/cli`](rust/cli)             | Thin CLI binary `scheda` (`07` / `14`, AGT-2–5)                                                                                                   |
| [`python/vllm`](python/vllm)       | `bazel run` vLLM sidecars (locked `uv.lock`; Linux)                                                                                               |
| [`python/cli`](python/cli)         | Testdata driver: `cli_LLMs`, `cli_jina` (CC BY-NC), `cli_model2vec`; fixtures in `python/cli/testdata` |
| [`MODULE.bazel`](MODULE.bazel)     | Bazel module: host toolchain, crate_universe, Buf, linters                                                                                        |

Dependency versions live only in the root `[workspace.dependencies]`. Member
crates refer to them with `workspace = true`.

## Build

The only hermetic build is Bazel (`.bazelversion`). Bazelisk (or Bazel **9.2.0**).
Put `bazel` on `PATH`. The Rust toolchain and crates come from the Bazel module.
Host architecture only: no linux `amd64` / `arm64` cross targets.
Target crates use the platform CRT (dynamic glibc on Linux). Git
operations invoke the system **`git`** executable (`git` must be on
`PATH`). Git is not behind a feature: `https://`, `http://`, `ssh://`,
`git@`, and file remotes all work, and authentication, `~/.ssh/config`,
and host-key checking match a plain `git` command. Features
`find` and `search` are opt-in: Tantivy `find` and hybrid `search`
(BM25 + USearch). `search` turns `find` on. `simd` turns `search` on
and enables USearch SIMD kernels. `openmp` is opt-in (Linux
`libomp-dev`). There are no default features: `cargo add scheda` is
git plus Records, without Tantivy or USearch. Add `features =
["search", "simd"]` for ANN + SIMD, or `features = ["find"]` for
Tantivy only. USearch needs a C++17 compiler: **g++** (Linux),
**clang++** / Xcode CLT (macOS), **MSVC** Build Tools (Windows). A
build without `search` does not need C++. Bazel on Debian 13, macOS,
and Windows amd64 links `find`, `search`, and `simd`. GitHub Actions
CI for those jobs is currently disabled.

```bash
bazel build //rust/scheda:scheda
bazel build //rust/cli:scheda
bazel test //...
```

Local Qwen HTTP sidecars (**Linux only**, host NVIDIA driver, **at least
16 GiB** GPU memory for both processes on one card; `tags = ["manual"]`
so `bazel build //...` does not fetch torch). Weights go to
`<workspace>/.cache/huggingface`:

```bash
bazel run //python/vllm:embedder   # 127.0.0.1:8080  OpenAI /v1/embeddings
bazel run //python/vllm:reranker   # 127.0.0.1:8081  Cohere /v1/rerank
```

Rust API HTML (rustdoc) is built with rules_rust `rust_doc` (no crates.io).
The Docs workflow that published it to
[GitHub Pages](https://amsokol.github.io/scheda/) is currently disabled.
When it is on again and Pages is enabled (**Settings → Pages → Source:
GitHub Actions**), that HTML is published; otherwise the workflow still
builds rustdoc and skips deploy.

```bash
bazel build //rust/scheda:scheda_doc   # rustdoc HTML
bazel build //rust/scheda:pages        # same HTML plus redirect and .nojekyll
bazel test //rust/scheda:scheda_doc_test
```

Generate Rust stubs from `api/version.proto`, `api/v1`, and
`api/collection/v1` (writes `rust/api/version/`,
`rust/api/api/v1/`, and `rust/api/api/collection/v1/`;
`bazel test //api:generate_test //api/v1:generate_test //api/collection/v1:generate_test`
checks they match):

```bash
bazel run //api:generate
bazel run //api/collection/v1:generate
bazel run //api/v1:generate
bazel test //api:generate_test //api/v1:generate_test //api/collection/v1:generate_test
```

Lint (`bazel test` never rewrites files; format is `bazel run` only):

```bash
bazel test //api:lint //api/v1:lint //api/collection/v1:lint        # Protobuf (buf)
bazel test //rust:lint          # clippy
bazel test //python:lint        # ruff
bazel test //bazel:lint         # Starlark (buildifier)
bazel test //bazel:markdown     # markdownlint-cli2
bazel test //rust:vuln          # cargo-audit (needs network)
bazel test //python:vuln        # pip-audit (needs network)

bazel run //api:format //api/v1:format //api/collection/v1:format       # buf format
bazel run //python:format       # ruff format
bazel run //bazel:format        # buildifier -mode=fix
```

`cargo test` / `cargo clippy` still work for local iteration. `cargo test
-p scheda --lib` is the fast loop: no feature, no C++ compile. For `find`
/ `search` tests: `cargo test -p scheda --features search,simd`. GitHub
Actions CI (Bazel) is currently disabled.
`cargo build --release` uses thin LTO, one codegen unit, and symbol strip.

Optional Cargo-only **static CRT** CLI. Bazel and default Cargo stay
dynamically linked. Per-target rustflags live in `.cargo/static-glibc.toml`
(Linux gnu, Windows msvc, macOS darwin). `--target` must be this machine
or `+crt-static` hits host proc-macros. Nothing links OpenSSL: git is the
system executable, so there is no vendored TLS feature to pass. Unset
`RUSTFLAGS` /
`CARGO_ENCODED_RUSTFLAGS` so they do not override the extra config.
Artifact: `target/<host-tuple>/release/scheda` (`.exe` on Windows).

Linux, macOS, and Git Bash:

```bash
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
cargo build -p scheda-cli --release \
  --target "$(rustc --print host-tuple)" --config .cargo/static-glibc.toml
```

PowerShell:

```powershell
Remove-Item Env:RUSTFLAGS, Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
cargo build -p scheda-cli --release `
  --target (rustc --print host-tuple) --config .cargo/static-glibc.toml
```

cmd.exe (interactive; use `%%i` inside a `.bat`):

```bat
set RUSTFLAGS=
set CARGO_ENCODED_RUSTFLAGS=
for /f %i in ('rustc --print host-tuple') do cargo build -p scheda-cli --release --target %i --config .cargo/static-glibc.toml
```

## Python CLI

Driver for the `scheda` binary against `python/cli/testdata/animals`
(`mammals`, `birds`, `reptiles`). Three Bazel targets, three gitignored
Vaults. Flags and JSON: [`docs/14-cli.md`](docs/14-cli.md).

Run commands from the clone root.

| Target | Vault | Embedder |
| --- | --- | --- |
| `//python/cli:cli_LLMs` | `.vault-test-LLMs` | Qwen HTTP sidecars (`:8080` / `:8081`) |
| `//python/cli:cli_jina` | `.vault-test-jina` | Jina HTTP sidecars (`:8080` / `:8081`, CC BY-NC) |
| `//python/cli:cli_model2vec` | `.vault-test-model2vec` | `minishlab/potion-multilingual-128M` on `reindex` / `search` |

`python -m python.cli --backend llms|jina|model2vec …` is the same driver.

### LLMs

**Requirements:** Linux only; host NVIDIA driver; **at least 16 GiB** of
GPU memory so embedder and reranker fit on one card. First start
`snapshot_download`s Qwen weights into `<workspace>/.cache/huggingface`
(`HF_HOME`; an already-set `HF_HOME` wins; tqdm on stderr). On a cold
cache, start the embedder and wait until it is listening before the
reranker, so the two Hub fetches do not stall each other. On WSL2 the
launcher sets `VLLM_WSL2_ENABLE_PIN_MEMORY=1` so vLLM's default V2
runner can use UVA. `find` and `--lexical-only` do not need a GPU.

Leave these two processes running (separate terminals):

```bash
bazel run //python/vllm:embedder   # 127.0.0.1:8080  OpenAI /v1/embeddings
bazel run //python/vllm:reranker   # 127.0.0.1:8081  Cohere /v1/rerank
```

`cli_LLMs` defaults to those URLs (`Qwen/Qwen3-Embedding-0.6B` dim
1024, `Qwen/Qwen3-Reranker-0.6B`). No GPU: pass `--lexical-only` on
every command below (BM25 / RRF only; no vectors).

#### 1. Import and reindex

```bash
bazel run //python/cli:cli_LLMs -- import
bazel run //python/cli:cli_LLMs -- reindex
```

`import` `open`s `.vault-test-LLMs` if needed, `create-collection` from
each `.collection.yaml`, then `add`s missing document files. Embedder
flags go on `add` / `reindex` / `search`, so the background indexer of
each command fills `index/vector/` after its commit. `reindex` rebuilds
lexical and vector indexes from the commits (use it after a change of
embedder model, or if you imported without the embedder; an old Index
format is rebuilt by itself).
Stdout of `reindex` is `ReindexResult` YAML. The CLI default Projector
is one text column `body` (the whole UTF-8 Record).

#### 2. `find`

Structured `Filter` on Record `id` and the CLI column `body`. Not meaning.
`term` is the whole field value. A word in the file is `phrase` / `regex`
on `body`, or `search`. Other field names are `invalid`. Stdout is YAML:
`total` then each Record, `---` between documents (`raw` only with
`--include-raw`).

```bash
bazel run //python/cli:cli_LLMs -- find
bazel run //python/cli:cli_LLMs -- find --collection mammals
bazel run //python/cli:cli_LLMs -- find --filter '{"phrase":{"field":"body","terms":["giant","panda"]}}'
bazel run //python/cli:cli_LLMs -- find --filter '{"regex":{"field":"body","pattern":"bamboo"}}'
bazel run //python/cli:cli_LLMs -- find --filter '{"term":{"field":"id","value":"giant-panda"}}'
```

#### 3. `search`

Hybrid BM25 + ANN, RRF, then the reranker. Hits are the top `k` after
ranking (`k` unset → 10). `lexicalWeight` `0` turns BM25 off (meaning
only). `--no-reranker` keeps RRF order. `--topic` / bare `search` use
`python/cli/testdata/animals/topics.yaml` (paraphrases; expected ids).
Hits print as YAML (`---` between hits); `--include-raw` adds `Hit.raw`.
Bare `search` prints one YAML document per topic.

```bash
bazel run //python/cli:cli_LLMs -- search --collection mammals --k 2 --text "seasonal migration"
```

```bash
bazel run //python/cli:cli_LLMs -- search --query '{
  "text": "species that hunt when there is no light by measuring how long a self-made sound takes to bounce back from prey",
  "k": 2,
  "lexicalWeight": 0
}'
```

```bash
bazel run //python/cli:cli_LLMs -- search --topic biosonar
bazel run //python/cli:cli_LLMs -- search
```

### Jina (CC BY-NC)

Same ports as Qwen. Stop `//python/vllm:embedder` / `:reranker` first.
Weights are **CC BY-NC 4.0** (not the Apache default). Own Vault
`.vault-test-jina` (SCH-10: do not mix with Qwen vectors).

```bash
bazel run //python/vllm:embedder_jina   # jina-embeddings-v5-text-small
bazel run //python/vllm:reranker_jina   # jina-reranker-v3.5
```

Then the same walkthrough as LLMs, with `cli_jina`:

```bash
bazel run //python/cli:cli_jina -- import
bazel run //python/cli:cli_jina -- reindex
bazel run //python/cli:cli_jina -- search --query '{
  "text": "species that hunt when there is no light by measuring how long a self-made sound takes to bounce back from prey",
  "k": 2,
  "lexicalWeight": 0
}'
```

`cli_jina` defaults to those Hub ids (dim 1024). `find` / `--lexical-only`
do not need the sidecars.

### Model2Vec

> **Disclaimer:** Model2Vec is too weak for the paraphrases in this
> walkthrough. Any Hub checkpoint in this family (static token lookup,
> including `potion-multilingual-128M`) returns the **wrong** Records on
> vector-only `search`. This section exists so you can run the **same
> commands as LLMs** and see the quality gap versus Qwen. For the
> expected hits, use `//python/cli:cli_LLMs`.

No GPU. `import` and `find` do not load Model2Vec (no Hub fetch, no
`--model2vec-embedder`). On first `reindex`, `huggingface_hub`
(`hf-xet`) downloads `minishlab/potion-multilingual-128M` (multilingual,
~512 MiB) into
`<workspace>/.cache/model2vec/potion-multilingual-128M` (`config.json`,
`tokenizer.json`, `model.safetensors`; tqdm on stderr). Later `reindex`
runs reuse the cache after size and Hub hash checks (LFS SHA-256, or git
blob SHA-1 for small files). `search` loads that cache to embed the
query (no Hub). Set `HF_TOKEN` to a Hugging Face token if the Hub
throttles anonymous downloads. `--model2vec-embedder DIR` overrides the
cache path. Search uses RRF (no local reranker).

#### 1. Import and reindex

```bash
bazel run //python/cli:cli_model2vec -- import
bazel run //python/cli:cli_model2vec -- reindex
```

`import` `open`s `.vault-test-model2vec` if needed, `create-collection`
from each `.collection.yaml`, then `add`s missing document files. It
does not load Model2Vec. `reindex` downloads the cache if needed, loads
the embedder once, and fills `index/vector/`. A later `reindex` rebuilds
lexical and vector indexes after a change of embedder model. Stdout of
`reindex` is `ReindexResult` YAML. The CLI default Projector is one text
column `body` (the whole UTF-8 Record).

#### 2. `find`

Structured `Filter` on Record `id` and the CLI column `body`. Not meaning.
`term` is the whole field value. A word in the file is `phrase` / `regex`
on `body`, or `search`. Other field names are `invalid`. Stdout is YAML:
`total` then each Record, `---` between documents (`raw` only with
`--include-raw`).

```bash
bazel run //python/cli:cli_model2vec -- find
bazel run //python/cli:cli_model2vec -- find --collection mammals
bazel run //python/cli:cli_model2vec -- find --filter '{"phrase":{"field":"body","terms":["giant","panda"]}}'
bazel run //python/cli:cli_model2vec -- find --filter '{"regex":{"field":"body","pattern":"bamboo"}}'
bazel run //python/cli:cli_model2vec -- find --filter '{"term":{"field":"id","value":"giant-panda"}}'
```

#### 3. `search`

Hybrid BM25 + ANN, then RRF (no local reranker). Loads the Model2Vec
cache to embed the query (run `reindex` first). Hits are the top `k`
after ranking (`k` unset → 10). `lexicalWeight` `0` turns BM25 off
(meaning only). `--topic` / bare `search` use
`python/cli/testdata/animals/topics.yaml` (paraphrases; expected ids).
Hits print as YAML (`---` between hits); `--include-raw` adds `Hit.raw`.
Bare `search` prints one YAML document per topic.

```bash
bazel run //python/cli:cli_model2vec -- search --collection mammals --k 2 --text "seasonal migration"
```

```bash
bazel run //python/cli:cli_model2vec -- search --query '{
  "text": "species that hunt when there is no light by measuring how long a self-made sound takes to bounce back from prey",
  "k": 2,
  "lexicalWeight": 0
}'
```

```bash
bazel run //python/cli:cli_model2vec -- search --topic biosonar
bazel run //python/cli:cli_model2vec -- search
```

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
