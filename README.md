# Scheda

Git-backed document store: text files as the source of truth, a derived index
outside git, and a library API. The first implementation is Rust; others
(Python, Java, Go, TypeScript, and more) may follow.

A Vault is a git working tree. Collections are `document` (one file per
record), `journal` (append-only JSONL segments), or `singleton`. The public
collection API is CRUD-shaped (`insert_one`, `find_one`, `replace_one`, `find`,
`scan`, `search`, `commit`). It is not a compatibility layer for MongoDB, CouchDB, or
SQL.

**Status: core C0–C8 in the Rust crate.** Files, git, Schema, `find` /
`find_one`, journal `id` map. Hybrid `search` (BM25 + in-process ANN + RRF)
is C9 and is still `unsupported`.

## Documents

The design is a numbered package in `docs/`, in English.

| File | Contents |
|---|---|
| [01-concept.md](docs/01-concept.md) | Why Scheda exists, what a Vault is, what the library refuses to be |
| [02-glossary.md](docs/02-glossary.md) | Normative vocabulary: Vault, Collection, Record, Schema, Index, kinds, API names |
| [03-model.md](docs/03-model.md) | Collection kinds, Record `id` and paths, fields and body, journal segments |
| [04-files-and-git.md](docs/04-files-and-git.md) | Files as truth, write lock, commit, `auto_sync`, `sync_remote`, clone, no LFS |
| [05-schema.md](docs/05-schema.md) | Schema as Vault data: fields, versions, index marks, edges, journal size cap |
| [06-index-and-search.md](docs/06-index-and-search.md) | Disposable Index, BM25 + ANN hybrid `search`, RRF, injected embedder, edge walks |
| [07-api.md](docs/07-api.md) | Language-neutral library operations; message fields in `api/v1` |
| [08-agent.md](docs/08-agent.md) | Same operations for an AI agent, Schema catalog, thin transport |
| [09-isolation.md](docs/09-isolation.md) | No network by default (**core**); isolated git root and age encryption (**later**) |
| [10-roadmap.md](docs/10-roadmap.md) | Phases (core / search / agent / later), acceptance, size targets, open questions, deferrals |
| [11-core-plan.md](docs/11-core-plan.md) | Rust work order: C0–C8 core (done), C9 hybrid search, C10 core acceptance |

## Layout

| Path | Role |
|---|---|
| [`Cargo.toml`](Cargo.toml) | Workspace: members, pinned dependency versions, lints |
| [`api/v1`](api/v1) | Library API messages (Protobuf edition 2024; not the Vault on-disk format) |
| [`rust/generated`](rust/generated) | Generated Rust stubs (`scheda-generated`) from `api/v1` |
| [`rust/scheda`](rust/scheda) | First implementation: Rust crate `scheda` |
| [`MODULE.bazel`](MODULE.bazel) | Bazel module: host toolchain, crate_universe, Buf, linters |

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
`PKG_CONFIG_PATH="$(brew --prefix openssl)/lib/pkgconfig"`). Build without
OpenSSL via `scheda` `default-features = false` (file remotes still work).
USearch needs a C++17 compiler: **g++** (Linux), **clang++** / Xcode CLT
(macOS), **MSVC** Build Tools (Windows). CI runs Bazel on Debian 13, macOS,
and Windows amd64.

```bash
bazel build //rust/scheda:scheda
bazel test //...
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
bazel test //bazel:lint         # Starlark (buildifier)
bazel test //bazel:markdown     # markdownlint-cli2
bazel test //rust:vuln          # cargo-audit (needs network)

bazel run //api/v1:format       # buf format
bazel run //bazel:format        # buildifier -mode=fix
```

`cargo test` / `cargo clippy` still work for local iteration. CI runs Bazel.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
