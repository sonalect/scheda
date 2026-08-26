# Scheda

Git-backed document store: text files as the source of truth, a derived index
outside git, and a library API. The first implementation is Rust; others
(Python, Java, Go, TypeScript, and more) may follow.

A Vault is a git working tree. Collections are `document` (one file per
record), `journal` (append-only JSONL segments), or `singleton`. The public
collection API is CRUD-shaped (`add`, `get`, `replace`, `find`,
`search`, `begin`, `commit`, `discard`). It is not a compatibility layer for MongoDB, CouchDB, or
SQL.

**Status: core C0–C10 and search C9 in the Rust crate; agent A0–A4
(catalog + `create_collection` + CLI `14`).** Files, git, Schema, `find` /
`get`, journal `id` map, hybrid `search` (BM25 + in-process ANN +
RRF), core acceptance, Schema catalog via `Vault.info`, `create_collection`, and a CLI encoding
of `07`. Next: A5 HTTP embedder/reranker in the CLI, then A6 ISO-3 and the
unencrypted isolated git root (`10` §1).

## Documents

The design is a numbered package in `docs/`, in English.

| File                                                  | Contents                                                                                                              |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| [01-concept.md](docs/01-concept.md)                   | Why Scheda exists, what a Vault is, what the library refuses to be                                                    |
| [02-glossary.md](docs/02-glossary.md)                 | Normative vocabulary: Vault, Collection, Record, Schema, Index, kinds, API names                                      |
| [03-model.md](docs/03-model.md)                       | Collection kinds, Record `id` and paths, fields and body, journal segments                                            |
| [04-files-and-git.md](docs/04-files-and-git.md)       | Files as truth, write lock, commit, `auto_sync`, `sync`, clone, no LFS                                                |
| [05-schema.md](docs/05-schema.md)                     | Schema as Vault data: fields, versions, index marks, edges, journal size cap                                          |
| [06-index-and-search.md](docs/06-index-and-search.md) | Disposable Index, `get`                                                                                               |
| [13-find-and-search.md](docs/13-find-and-search.md)   | Target `find` / `search`: Tantivy Query, hybrid BM25 + USearch, per-field vectors, optional reranker                  |
| [14-cli.md](docs/14-cli.md)                           | Target `scheda` CLI argv (`07`; `--embedder-openai-url`; `--reranker-cohere-url` on `search`)                         |
| [07-api.md](docs/07-api.md)                           | Language-neutral library operations; message fields in `api/v1`                                                       |
| [08-agent.md](docs/08-agent.md)                       | Same operations for an AI agent, Schema catalog, thin transport                                                       |
| [09-isolation.md](docs/09-isolation.md)               | No network by default (**core**); isolated git root and age encryption (**later**)                                    |
| [10-roadmap.md](docs/10-roadmap.md)                   | Phases (core / search / agent / later), acceptance, what `scheda` codes next, size targets, open questions, deferrals |
| [11-core-plan.md](docs/11-core-plan.md)               | Rust work order: C0–C8 and C10 core (done), C9 hybrid search (done)                                                   |
| [12-agent-plan.md](docs/12-agent-plan.md)             | Rust work order: A0–A4 done; A5 HTTP embedder, A6 ISO-3, A7 remaining                                                 |

## Layout

| Path                               | Role                                                                                                                                              |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`Cargo.toml`](Cargo.toml)         | Workspace: members, pinned dependency versions, lints                                                                                             |
| [`api/v1`](api/v1)                 | Library API messages (Protobuf edition 2024; not the Vault on-disk format)                                                                        |
| [`rust/generated`](rust/generated) | Generated Rust stubs (`scheda-generated`) from `api/v1`                                                                                           |
| [`rust/scheda`](rust/scheda)       | First implementation: Rust crate `scheda`                                                                                                         |
| [`rust/cli`](rust/cli)             | Thin CLI binary `scheda` (`07` / `14`, AGT-2–5)                                                                                                   |
| [`python/vllm`](python/vllm)       | `bazel run` vLLM sidecars (locked `uv.lock`; Linux)                                                                                               |
| [`python/cli`](python/cli)         | Testdata driver for the `scheda` binary (`import` / `reindex` / `find` / `search` → `<workspace>/.vault-test`; fixtures in `python/cli/testdata`) |
| [`MODULE.bazel`](MODULE.bazel)     | Bazel module: host toolchain, crate_universe, Buf, linters                                                                                        |

Dependency versions live only in the root `[workspace.dependencies]`. Member
crates refer to them with `workspace = true`.

## Build

The only hermetic build is Bazel (`.bazelversion`). Bazelisk (or Bazel **9.2.0**).
Put `bazel` on `PATH`. The Rust toolchain and crates come from the Bazel module.
Host architecture only: no linux `amd64` / `arm64` cross targets.
Target crates use the platform CRT (dynamic glibc on Linux). Vendored
libgit2 needs **cmake**, **pkg-config**, and zlib headers. Feature
`remote-git` (default) adds libgit2 **https** + **ssh**: OpenSSL on Linux
(`libssl-dev`), Secure Transport on macOS, WinHTTP on Windows. SSH uses
vendored libssh2, which still needs OpenSSL headers on Unix (Homebrew
`openssl` on macOS; keg-only, so set
`PKG_CONFIG_PATH="$(brew --prefix openssl)/lib/pkgconfig"`). Feature
`numkong` (default) is USearch SIMD. `openmp` is opt-in (Linux
`libomp-dev`). `default-features = false` drops both git TLS and SIMD
(file remotes still work); add `features = ["numkong"]` to keep SIMD.
USearch needs a C++17 compiler: **g++** (Linux), **clang++** / Xcode CLT
(macOS), **MSVC** Build Tools (Windows). CI runs Bazel on Debian 13, macOS,
and Windows amd64.

```bash
bazel build //rust/scheda:scheda
bazel build //rust/cli:scheda
bazel test //...
```

Local Qwen HTTP sidecars (**Linux only**, host NVIDIA driver, **at least
16 GiB** GPU memory for both processes on one card; `tags = ["manual"]`
so `bazel build //...` does not fetch torch):

```bash
bazel run //python/vllm:embedder   # 127.0.0.1:8080  OpenAI /v1/embeddings
bazel run //python/vllm:reranker   # 127.0.0.1:8081  Cohere /v1/rerank
```

Rust API HTML (rustdoc) is built with rules_rust `rust_doc` and published to
[GitHub Pages](https://amsokol.github.io/scheda/) on push to `main` (no
crates.io). Enable **Settings → Pages → Source: GitHub Actions** once.

```bash
bazel build //rust/scheda:scheda_doc   # rustdoc HTML
bazel build //rust/scheda:pages        # same HTML plus redirect and .nojekyll
bazel test //rust/scheda:scheda_doc_test
```

Generate Rust stubs from `api/v1` (writes `rust/generated/api/v1/`; CI checks they match):

```bash
bazel run //api/v1:generate
bazel test //api/v1:generate_tests
```

Lint (`bazel test` never rewrites files; format is `bazel run` only):

```bash
bazel test //api/v1:lint        # Protobuf (buf)
bazel test //rust:lint          # clippy
bazel test //python:lint        # ruff
bazel test //bazel:lint         # Starlark (buildifier)
bazel test //bazel:markdown     # markdownlint-cli2
bazel test //rust:vuln          # cargo-audit (needs network)
bazel test //python:vuln        # pip-audit (needs network)

bazel run //api/v1:format       # buf format
bazel run //python:format       # ruff format
bazel run //bazel:format        # buildifier -mode=fix
```

`cargo test` / `cargo clippy` still work for local iteration. CI runs Bazel.
`cargo build --release` uses thin LTO, one codegen unit, and symbol strip.

Optional Cargo-only **static CRT** CLI. Bazel and default Cargo stay
dynamically linked. Per-target rustflags live in `.cargo/static-glibc.toml`
(Linux gnu, Windows msvc, macOS darwin). `--target` must be this machine
or `+crt-static` hits host proc-macros. `vendored-openssl` is required on
Linux (distro `libcrypto.a` does not link). Unset `RUSTFLAGS` /
`CARGO_ENCODED_RUSTFLAGS` so they do not override the extra config.
Artifact: `target/<host-tuple>/release/scheda` (`.exe` on Windows).

Linux, macOS, and Git Bash:

```bash
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
cargo build -p scheda-cli --release --features vendored-openssl \
  --target "$(rustc --print host-tuple)" --config .cargo/static-glibc.toml
```

PowerShell:

```powershell
Remove-Item Env:RUSTFLAGS, Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
cargo build -p scheda-cli --release --features vendored-openssl `
  --target (rustc --print host-tuple) --config .cargo/static-glibc.toml
```

cmd.exe (interactive; use `%%i` inside a `.bat`):

```bat
set RUSTFLAGS=
set CARGO_ENCODED_RUSTFLAGS=
for /f %i in ('rustc --print host-tuple') do cargo build -p scheda-cli --release --features vendored-openssl --target %i --config .cargo/static-glibc.toml
```

## Python CLI

Driver for the `scheda` binary against `python/cli/testdata/animals`
(`mammals`, `birds`, `reptiles`). Vault: `<workspace>/.vault-test`
(gitignored). Flags and JSON: [`docs/14-cli.md`](docs/14-cli.md).

Run commands from the clone root.

**Requirements (vLLM sidecars / vector `import` / `reindex` / `search`):**
Linux only; host NVIDIA driver; **at least 16 GiB** of GPU memory so
embedder and reranker fit on one card. First start downloads Qwen
weights. `find` and `--lexical-only` do not need a GPU.

### 1. Embedding and rerank models

Leave these two processes running (separate terminals):

```bash
bazel run //python/vllm:embedder   # 127.0.0.1:8080  OpenAI /v1/embeddings
bazel run //python/vllm:reranker   # 127.0.0.1:8081  Cohere /v1/rerank
```

`python/cli` defaults to those URLs (`Qwen/Qwen3-Embedding-0.6B` dim
1024, `Qwen/Qwen3-Reranker-0.6B`). No GPU: pass `--lexical-only` on
every command below (BM25 / RRF only; no vectors).

### 2. Import and reindex

```bash
bazel run //python/cli:cli -- import
bazel run //python/cli:cli -- reindex
```

`import` `open`s `.vault-test` if needed, `create-collection` from each
`.schema.yaml`, then `add`s missing Markdown documents. Embedder flags
go on every `scheda` call, so `add` writes `index/vector/`. `reindex`
rebuilds lexical and vector indexes from files (use it after an Index
format change, or if you imported without the embedder). Stdout of
`reindex` is `ReindexResult` JSON.

### 3. `find`

Structured `Filter` on Schema fields, Record `id`, and Markdown `body`
(unless `exclude_fts`). Not meaning.

```bash
bazel run //python/cli:cli -- find
bazel run //python/cli:cli -- find --collection mammals
bazel run //python/cli:cli -- find --filter '{"phrase":{"field":"title","terms":["giant","panda"]}}'
bazel run //python/cli:cli -- find --filter '{"regex":{"field":"body","pattern":"bamboo"}}'
bazel run //python/cli:cli -- find --filter '{"term":{"field":"id","value":"giant-panda"}}'
```

`term` is the whole field value. A word in title or body is `phrase` /
`regex`, or `search`.

### 4. `search`

Hybrid BM25 + ANN, RRF, then the reranker. Hits are the top `k` after
ranking (`k` unset → 10). `lexicalWeight` `0` turns BM25 off (meaning
only).

```bash
bazel run //python/cli:cli -- search --collection mammals --k 2 --text "seasonal migration"
```

```bash
bazel run //python/cli:cli -- search --query '{
  "text": "species that hunt when there is no light by measuring how long a self-made sound takes to bounce back from prey",
  "k": 2,
  "lexicalWeight": 0
}'
```

```bash
bazel run //python/cli:cli -- search --topic biosonar
bazel run //python/cli:cli -- search
```

`--topic` / bare `search` use `python/cli/testdata/animals/topics.yaml`
(paraphrases; expected ids). `--no-reranker` keeps RRF order.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
