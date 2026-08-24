# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
While the major version is 0, compatible additions bump the patch; breaking
API or on-disk format changes bump the minor.

## [Unreleased]

### Added

- Core development plan in `docs/11-core-plan.md` (stages C0–C10).
- LIB-5 error codes on the Rust crate; LIB-2 names including `init` /
  `clone` / `scan` / remote / `check` / `reindex`; `search` returns
  `unsupported`.
- `Vault::init` / `Vault::open` with exclusive write lock (`locked`) and
  local Remote detection without fetch. Index lives at `{vault}/.scheda/`
  (gitignored).
- Language-neutral library messages in `api/v1` (Protobuf edition 2024);
  Rust stubs via hermetic `buf generate` (buffa + protovalidate).
- `init` / `open` / `clone` / `insert_one` / `replace_one` reject
  `api/v1` input messages that fail generated protovalidate rules
  (`invalid`).
- `open` / `check` load Schema from `schema/` (`05` §8 keys); illegal
  declarations are `invalid`.
- Rust crate rustdoc on GitHub Pages (`bazel build //rust/scheda:scheda_doc`).
- `document` / `singleton` `insert_one` / `replace_one` / `find_one` (C3):
  YAML+Markdown or YAML-only files, atomic sibling-temp writes, Schema
  validation on write, `valid: false` on old files that fail the current
  Schema. `replace_one` on `journal` is `unsupported`.
- `journal` `insert_one` appends RFC 8785 JSONL (C4); size cap rotates
  eight-digit hex segments (`00000001.jsonl`) without rewriting closed
  files; `find_one` / empty `find` / `scan` read by `(collection, id)` in
  append order.
- SCH-8 `find` filters (`equal` / `set` / `exists` / `path_prefix`),
  `scan` as empty `find`, `check` with code+path, and `reindex` that
  writes Index format version `1` and returns `ReindexCounts` (C5).
- `commit(message)` stages only paths this handle wrote and creates one
  git commit; `history(path)` returns `HistoryEntry` snapshots that match
  `git show` for `document` / `singleton` / journal segment files (C6).
- `clone` / `remote_repo`, `auto_sync` fast-forward only, and
  `pull_remote` / `push_remote` / `sync_remote` (C7). Diverged histories
  are `not_fast_forward`; a failed push does not roll back the local commit.
- Journal `id` map under `{vault}/.scheda/index/journal/` so `find_one` does
  not scan unrelated segments; writer updates it on `insert_one`, `reindex`
  rebuilds it (C8, IDX-5, OPS-1, OPS-3).
- Hybrid `search` quality bar: BM25 lexical, in-process ANN, RRF fusion,
  Schema-marked fields only, injected embedder, no LLM in the crate (`06`,
  `10` §3). Rust stage C9 in `11` implements it before core acceptance (C10).
- CI runs `bazel test //...` on Debian 13 and macOS.
- USearch 2.26.0 as the in-process HNSW engine (`AnnIndex`); default
  SIMD feature `numkong` is off so Bazel can compile the C++ bridge.
  `search` is still `unsupported` until C9 wires it. The C++ bridge
  uses `/usr/bin/c++` on Linux. macOS keeps Bazel's Apple clang wrapper
  so `__BAZEL_XCODE_SDKROOT__` expands; Windows uses MSVC.
- Crate feature `remote-git` (default) enables `git2` `https` and `ssh` so
  `clone` / `pull_remote` / `push_remote` can talk to `https://` and `git@`
  remotes (libgit2 credential helper / ssh-agent). Without the feature those
  URL schemes are `unsupported`; file remotes still work. TLS is platform
  OpenSSL / Secure Transport / WinHTTP; Linux `+crt-static` is dropped so
  the native crates link.

### Changed

- Collection files live under a 16-hex FNV-1a-64 directory of the collection
  name (any characters in `name` are allowed). Layout templates are relative
  to that directory.
- Index is stored inside the Vault at `.scheda/` and listed in `.gitignore`.
  `OpenOptions` has no Index path field.
- Target crates use the platform CRT on Linux, macOS, and Windows (no
  `+crt-static`). Proc-macros and build scripts stay dynamically linked.
- crate_universe annotations keep only USearch/cxx patches and a Linux
  `CXX=/usr/bin/c++` pin. macOS must not override `CXX` (Apple sysroot
  placeholders). libgit2/OpenSSL crates already see host PATH and vendored
  sources from rules_rust defaults.
- Git operations go through `git2` (libgit2), not the `git` CLI.
- Generated Rust stubs live in `rust/generated` (crate `scheda-generated`):
  one buffa file per `.proto`, packaging `mod.rs`, and protovalidate
  `Validate` impls.
- The `scheda` crate uses generated `api/v1` messages (`Error`,
  `OpenOptions`, `Record`) instead of parallel hand-written structs.
- Schema tests load YAML/JSON fixtures from `rust/scheda/testdata/schema/`
  instead of inlined strings.
- `Collection::find_one` returns `Result<Record, Error>`; a missing `id` is
  `not_found` (LIB-8).

### Removed

- Unused workspace pins `thiserror` and `tempfile`.

## [0.0.1] - 2026-08-23

### Added

- Language-neutral design package in `docs/` (`01`–`10`): Vault model, git
  rules, Schema, Index, library API, agent surface, isolation (later), and
  phases.
- Rust crate skeleton `scheda` (`todo!` bodies; workspace lints deny warnings).
- Hermetic Bazel on the host architecture: library build and tests, Clippy,
  cargo-audit, buildifier, markdownlint-cli2, and GitHub Actions CI.

## Links

- [Unreleased]
- [0.0.1]

[Unreleased]: https://github.com/amsokol/scheda/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/amsokol/scheda/releases/tag/v0.0.1
