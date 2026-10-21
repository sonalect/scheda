# Scheda

Scheda is a human-, AI-agent-, and process-friendly **library** for a
document store: records are ordinary text files in a git working tree,
not a database file. It is not a wiki, a second brain, a daemon, or a
hosted product.

**License: proprietary.** All rights reserved. See [LICENSE](LICENSE).

**What it is.** Documents live as ordinary files in a **Vault** — a git
working tree a person can open in an editor and in `git` without this
library. A Document is bytes: Scheda does not know their format. Collections
are `standard` (one file per Document; the path is the `id`), `journal`
(append-only envelope lines), or `singleton`. A **collection declaration**
in the Vault names the kind and perimeter, not Document fields. Columns for
queries and searches come from a caller **Projector** passed when the Vault
is opened. The library does not know a consumer's types or file format.
The **Index** is derived (it is built on Lance), lives under
`{vault}/.scheda/` (gitignored), and can be deleted and rebuilt from files. A folder is a Vault when it is a git
working tree with the committed marker file `.scheda.yaml` (`version: 3`,
the generation); `Scheda.Open` with the default mode creates a missing
Vault, and adds the marker in one commit to a git working tree that lacks
it, unless `OpenOptions.mode` asks to open an existing Vault only
(`NOT_A_VAULT`) or to create a new one only.

The public API is protobuf services in one package, `scheda.v1`
(`api/scheda/v1`), reached through objects: `Scheda` opens a `Vault`, which
gives `Collection`, `Mirror`, and `Index`. Each write call is one commit
(`Collection.Create`, `Update`, `Upsert`, `Delete`; `Collection.Batch` and
`Vault.BatchDocuments` for several as one). Reads are `Collection.Get`,
`GetMany`, and `History`, and the queries over the Index are
`Collection.Query` and `Collection.Search` (`Vault.QueryDocuments` and
`Vault.SearchDocuments` across collections). The CLI is one subcommand per
rpc. It is not a compatibility layer for MongoDB, CouchDB, or SQL. The first
implementation is Rust; others (Python, Java, Go, TypeScript, and more) may
follow.

**Why it exists.** Work that matters still lands as text: notes, configs,
journals, Markdown, YAML, TOML. Git already versions that text. The
moment those files need to be queried — by field, by words, by meaning — the
usual move is a document database plus a vector database. The files become
an export. The human and the AI agent no longer share one tree: the agent
searches a store the human cannot `git log`, and the human edits files the
index does not see until someone remembers to sync. Scheda is the case in
between: **files stay the source of truth**, git stays the history, and a
library gives structured access and search without inventing a second store.

**How it is useful.** A program can query and search (BM25, in-process
nearest neighbours, RRF) and write Documents without treating the Index as the database:
wipe the Index, rebuild, get the same files back. An AI agent gets the same
calls as the library (plus `Vault.ListCollections` from collection
declarations and `Vault.CreateCollection`), not a looser “write any Markdown
anywhere” path. A person still diffs a Document as text and recovers last
week's bytes with git. Embedding weights, if any, stay in the caller's
install; without an embedder, vector search is off and the rest still works.
The library does not score Documents for a UI, does not call a language
model, and does not know a profession.

**Status: release `0.24.0` holds the proto contract (`docs/20`), the Index
on Lance (`docs/21`), Scheda on tokio (`docs/22`), and the fixes of the
full review (`docs/23`), with a `vector` column that may be a `text list`,
and a port change whose number the Index state reaches with its verdict
(`CHANGELOG.md`).** What it holds: the API as protobuf services and objects, async only on the
caller's tokio runtime (a blocking caller wraps an object with `to_sync()`
inside a multi-thread runtime, `docs/22`); one commit per write call, which
returns the state token of the Vault it made, and batches that are all or
nothing; a Vault that never needs the Index to read or write; an Index on
Lance (`docs/21`) that follows git `HEAD` in a background task, so queries
are eventually consistent unless a call names the token of its write
(`not_before`; `Index.Status`, `Index.Watch`), and that is rebuilt beside
the live one only when the caller asks (`Index.Reindex`); queries by a typed
filter and hybrid search (BM25 + in-process nearest neighbours + RRF, with a
caller `Embedder` and `Reranker`) in the feature `index`; one mirror
(`Mirror.Push`, `Pull`, `Sync`, `auto_sync`); isolated collections that never
leave the machine; collection encryption (`encrypt` feature); and a CLI that
encodes every rpc (`docs/14`). The step-by-step work orders of the earlier
stages (`11`, `12`, `15`–`19`) are history.

## Documents

The design is a numbered package in `docs/`, in English.

| File                                                  | Contents                                                                                                              |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| [01-concept.md](docs/01-concept.md)                   | Why Scheda exists, what a Vault is, what the library refuses to be                                                    |
| [02-glossary.md](docs/02-glossary.md)                 | Normative vocabulary: Vault, Collection, Document, collection declaration, Projector, Index, kinds, API names           |
| [03-model.md](docs/03-model.md)                       | Collection kinds, Document `id` and paths, bytes, journal envelope                                               |
| [04-files-and-git.md](docs/04-files-and-git.md)       | Files as truth, write lock, one commit per write, the mirror, `auto_sync`, clone, no LFS                                                |
| [05-collection-declaration.md](docs/05-collection-declaration.md) | Collection declaration: kind, perimeter, journal caps; the generation is the marker's                                     |
| [06-index-and-search.md](docs/06-index-and-search.md) | Disposable Index on Lance, its generations, who indexes, rebuild beside the live Index, `not_before`, `Collection.Get`                                                                                               |
| [13-find-and-search.md](docs/13-find-and-search.md)   | Queries select, searches rank: the typed Filter, hybrid BM25 + nearest neighbours, Projector columns, optional reranker                   |
| [14-cli.md](docs/14-cli.md)                           | The `scheda` CLI: one subcommand per rpc, global flags, JSON in and out (`--embedder-openai-url` or `--model2vec-embedder`; `--reranker-cohere-url`) |
| [07-api.md](docs/07-api.md)                           | What spans the calls of the contract (objects, write model, errors, `Close`); each rpc is described in `api/scheda/v1`                                                       |
| [08-agent.md](docs/08-agent.md)                       | Same calls for an AI agent, declaration listing, thin transport                                                  |
| [09-isolation.md](docs/09-isolation.md)               | No network by default (**core**); isolated collections and age encryption (**later**, in this crate)                      |
| [10-roadmap.md](docs/10-roadmap.md)                   | Phases (core / search / agent / later), acceptance, what `scheda` codes next, size targets, open questions, deferrals |
| [11-core-plan.md](docs/11-core-plan.md)               | Rust work order: C0–C8 and C10 core (done), C9 hybrid search (done)                                                   |
| [12-agent-plan.md](docs/12-agent-plan.md)             | Rust work order: A0–A7 done, **agent** accepted; P0–P16 done                                      |
| [15-online-reindex.md](docs/15-online-reindex.md)     | Rust work order (history): `reindex` without holding `write.lock` for the walk (R0–R8); its queue is replaced by `17` |
| [16-opaque-records.md](docs/16-opaque-records.md)     | Rust work order (history): Documents as bytes, collection declaration without field Schema, Projector callback (F0–F19 done). |
| [17-background-index-and-tokio.md](docs/17-background-index-and-tokio.md) | Rust work order: background Index that follows `HEAD` (B1–B10 done), opt-in `tokio` feature with an async API (T1–T4 done) |
| [18-review-fixes.md](docs/18-review-fixes.md) | Rust work order: fixes from the 5 October 2026 full review (S1–S8) |
| [19-follow-ups.md](docs/19-follow-ups.md) | Rust work order (history): declarations follow the work tree, search vector-leg signal, `index_version` repeat, sync and async `transaction` (F1–F5 done) |
| [20-proto-contract.md](docs/20-proto-contract.md) | The 0.22 contract: decisions D1–D24, the layers P1–P8 and R1–R5, what is out of scope |
| [21-lance-index.md](docs/21-lance-index.md) | Rust work order: the Index on Lance in place of Tantivy and USearch, a typed filter, a state token for freshness (D1–D18, L0–L10, the contract C1–C35); released as `0.22.0` |
| [22-scheda-on-tokio.md](docs/22-scheda-on-tokio.md) | Rust work order: Scheda async only on the caller's tokio runtime, the blocking core behind a gate (D1–D12, A1–A5) |
| [23-full-review.md](docs/23-full-review.md) | Rust work order: the full review before the release (R1–R23, D1–D9); stages S1–S5 done, released as `0.22.0` |

## Layout

| Path                               | Role                                                                                                                                              |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`Cargo.toml`](Cargo.toml)         | Workspace: members, pinned dependency versions, lints                                                                                             |
| [`api/scheda/v1`](api/scheda/v1)   | The contract: services and messages (Protobuf edition 2024); the rpc comments are the per-call contract                                           |
| [`rust/api`](rust/api)             | Generated Rust messages and service traits (`scheda-api`) from `api/scheda/v1`                                                                     |
| [`rust/scheda`](rust/scheda)       | First implementation: Rust crate `scheda`                                                                                                         |
| [`rust/cli`](rust/cli)             | Thin CLI binary `scheda`, one subcommand per rpc (`14`, AGT-2–5)                                                                                                   |
| [`python/vllm`](python/vllm)       | `bazel run` vLLM sidecars (locked `uv.lock`; Linux)                                                                                               |
| [`python/cli`](python/cli)         | Testdata driver: `cli_LLMs`, `cli_jina` (CC BY-NC), `cli_model2vec`; fixtures in `python/cli/testdata` |
| [`MODULE.bazel`](MODULE.bazel)     | Bazel module: host toolchain, crate_universe, Buf, linters                                                                                        |

Dependency versions live only in the root `[workspace.dependencies]`. Member
crates refer to them with `workspace = true`.

## Build

The only hermetic build is Bazel (`.bazelversion`). Bazelisk (or Bazel **9.3.0**).
Put `bazel` on `PATH`. The Rust toolchain and crates come from the Bazel module.
Host architecture only: no linux `amd64` / `arm64` cross targets.
Target crates use the platform CRT (dynamic glibc on Linux). Only `clone`,
`fetch`, and `push` invoke the system **`git`** executable (`git` must be on
`PATH`); every other git operation, packing included, is libgit2 in
process. Git is not behind a feature: `https://`, `http://`, `ssh://`,
`git@`, and file remotes all work, and authentication, `~/.ssh/config`,
and host-key checking match a plain `git` command. Features:
`index` is on by default: the Index on Lance (the crates `lance`,
`lance-index`, `lance-linalg`, pinned exactly, no cloud object stores), its
queries, counts, and searches, and the ports. Building it with Cargo needs
`protoc` (`PROTOC`, or `protoc` on `PATH`); Bazel runs the prebuilt protoc
of the proto toolchain and compiles no protoc. A cold build is long; `cargo
add scheda --no-default-features` is git plus Documents, without Lance.
`encrypt` is opt-in. Scheda is async only and needs a tokio runtime in the
caller (`docs/22`): tokio is a required dependency, there is no `tokio`
feature, and a blocking caller builds a multi-thread runtime and takes
`to_sync()` of a reference inside it.

The GitHub Actions workflow CI (started by hand) builds and tests on
Linux, macOS, and Windows, each on amd64 and arm64: Bazel on five, Cargo on
Windows arm64, where Bazel cannot build ring (its build script compiles
with clang there and gets the cl.exe flags of Bazel's C++ toolchain). On
Windows, Bazel needs a short output base: cl.exe knows no long paths, and
the C sources of zstd and libgit2 come to the 259 characters of `MAX_PATH`
under `C:/b`. Put `startup --output_base=C:/b` in your own `.bazelrc`.
The third-party crates build with optimizations in every mode, the
workspace's own crates as the mode says.

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

Generate Rust stubs from `api/scheda/v1` (writes `rust/api/gen/buffa/`,
the messages, and `rust/api/gen/contract/`, the service traits;
`bazel test //api:generate_test` checks they match):

```bash
bazel run //api:generate
bazel test //api:generate_test
```

Lint (`bazel test` never rewrites files; format is `bazel run` only):

```bash
bazel test //api:lint           # Protobuf (buf)
bazel test //rust:lint          # clippy
bazel test //rust:rustfmt       # rustfmt --check (generated code left out)
bazel test //python:lint        # ruff
bazel test //bazel:lint         # Starlark (buildifier)
bazel test //bazel:markdown     # markdownlint-cli2
bazel test //rust:vuln          # cargo-audit (needs network)
bazel test //python:vuln        # uv audit (needs network)

bazel run //api:format         # buf format
bazel run //rust:format         # rustfmt (as cargo fmt --all)
bazel run //python:format       # ruff format
bazel run //bazel:format        # buildifier -mode=fix
```

`cargo test` / `cargo clippy` still work for local iteration. `cargo test
-p scheda --lib --no-default-features` is the fast loop: no feature, no
Lance compile. The default feature `index` (Lance) needs `protoc`
to build (`PROTOC`, or `protoc` on `PATH`); `cargo test -p scheda
--all-features` runs the Index tests too.
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
runner can use UVA. `--lexical-only` ranks without a GPU, but the Vault still indexes with
the embedder, so the embedder sidecar must run for new writes to be indexed.

Leave these two processes running (separate terminals):

```bash
bazel run //python/vllm:embedder   # 127.0.0.1:8080  OpenAI /v1/embeddings
bazel run //python/vllm:reranker   # 127.0.0.1:8081  Cohere /v1/rerank
```

`cli_LLMs` defaults to those URLs (`Qwen/Qwen3-Embedding-0.6B` dim
1024, `Qwen/Qwen3-Reranker-0.6B`). No GPU: pass `--lexical-only` on
every command below to rank by BM25 only (`vectorWeight` 0, no reranker).
The driver still passes the embedder, so vectors are built only while the
embedder sidecar runs.

#### 1. Import and reindex

```bash
bazel run //python/cli:cli_LLMs -- import
bazel run //python/cli:cli_LLMs -- reindex
```

`import` makes `.vault-test-LLMs` if needed, `create-collection`s from
each `.collection.yaml`, then `create`s the missing Document files; the
last step runs `index watch --until-settled` with the embedder flags, so
that process indexes the writes and fills the vectors in the Index. `reindex`
builds a new Index generation from the commits beside the live one (use it
after a change of embedder model; an Index of an earlier version is removed
and built again by itself). Stdout
of `reindex` is the settled `IndexState` as YAML. The CLI default Projector
is one text column `body` (the whole Document, when it is UTF-8 text).

#### 2. `find`

A `Filter` on the `id` of a Document and the CLI column `body`. A query
selects; it does not rank. `term` is the whole value and `prefix` its
beginning; a word in the file is a search. Other column names are
`INVALID_ARGUMENT`. Stdout is YAML: `total` (from the summary the query
streams first) then each Document, `---` between documents (`raw` only with
`--include-raw`). One filter applies to every collection in scope;
`--collection NAME` queries one collection.

```bash
bazel run //python/cli:cli_LLMs -- find
bazel run //python/cli:cli_LLMs -- find --collection mammals
bazel run //python/cli:cli_LLMs -- find --collection mammals --filter '{"prefix":{"column":"id","prefix":"giant"}}'
bazel run //python/cli:cli_LLMs -- find --filter '{"term":{"column":"id","value":{"text":"giant-panda"}}}'
```

#### 3. `search`

Hybrid BM25 + nearest neighbours, RRF, then the reranker. Hits are the top `k` after
ranking (`k` unset → 10). `lexicalWeight` `0` turns BM25 off (meaning
only); `--lexical-only` sets `vectorWeight` `0` and turns the reranker off.
`--no-reranker` keeps RRF order. `--topic` / bare `search` use
`python/cli/testdata/animals/topics.yaml` (paraphrases; expected ids).
Hits print as YAML (`---` between hits), after `vectorLeg`, which says
whether the vector leg ran; `--include-raw` adds each hit's `raw`. Bare
`search` prints one YAML document per topic.

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

`cli_jina` defaults to those Hub ids (dim 1024). `--lexical-only` does not
call the reranker, but the embedder sidecar must run for new writes to be
indexed.

### Model2Vec

> **Disclaimer:** Model2Vec is too weak for the paraphrases in this
> walkthrough. Any Hub checkpoint in this family (static token lookup,
> including `potion-multilingual-128M`) returns the **wrong** Documents on
> vector-only `search`. This section exists so you can run the **same
> commands as LLMs** and see the quality gap versus Qwen. For the
> expected hits, use `//python/cli:cli_LLMs`.

No GPU. `import` does not load Model2Vec (no Hub fetch, no
`--model2vec-embedder`); `reindex`, `find`, and `search` do, and `find` and
`search` use the cache as-is (no Hub). On first `reindex`, `huggingface_hub`
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

`import` makes `.vault-test-model2vec` if needed, `create-collection`s
from each `.collection.yaml`, then `create`s the missing Document files. It
does not load Model2Vec. `reindex` downloads the cache if needed, loads
the embedder once, and fills the vectors in the Index. A later `reindex`
builds a new Index generation after a change of embedder model.
Stdout of `reindex` is the settled `IndexState` as YAML. The CLI default
Projector is one text column `body` (the whole Document, when it is UTF-8
text).

#### 2. `find`

A `Filter` on the `id` of a Document and the CLI column `body`. A query
selects; it does not rank. `term` is the whole value and `prefix` its
beginning; a word in the file is a search. Other column names are
`INVALID_ARGUMENT`. Stdout is YAML: `total` (from the summary the query
streams first) then each Document, `---` between documents (`raw` only with
`--include-raw`). One filter applies to every collection in scope;
`--collection NAME` queries one collection.

```bash
bazel run //python/cli:cli_model2vec -- find
bazel run //python/cli:cli_model2vec -- find --collection mammals
bazel run //python/cli:cli_model2vec -- find --collection mammals --filter '{"prefix":{"column":"id","prefix":"giant"}}'
bazel run //python/cli:cli_model2vec -- find --filter '{"term":{"column":"id","value":{"text":"giant-panda"}}}'
```

#### 3. `search`

Hybrid BM25 + nearest neighbours, then RRF (no local reranker). Loads the Model2Vec
cache to embed the query (run `reindex` first). Hits are the top `k`
after ranking (`k` unset → 10). `lexicalWeight` `0` turns BM25 off
(meaning only). `--topic` / bare `search` use
`python/cli/testdata/animals/topics.yaml` (paraphrases; expected ids).
Hits print as YAML (`---` between hits), after `vectorLeg`; `--include-raw`
adds each hit's `raw`. Bare `search` prints one YAML document per topic.

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

Proprietary. All rights reserved. See [LICENSE](LICENSE).
