# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
While the major version is 0, compatible additions bump the patch; breaking
API or on-disk format changes bump the minor.

## [Unreleased]

## [0.1.0] - 2026-08-24

### Added

- Core development plan in `docs/11-core-plan.md` (stages C0–C10).
- Agent development plan in `docs/12-agent-plan.md` (stages A0–A4).
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
- Rust crate rustdoc on GitHub Pages (`bazel build //rust/scheda:scheda_doc`);
  crate docs list Cargo features (`remote-git`, `numkong`, `openmp`) and how
  to call the sync API from Tokio (`spawn_blocking`).
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
- Hybrid `search` (C9): Tantivy BM25 over Schema `fts` fields and body,
  in-process USearch ANN over vector-marked slots, RRF fusion, SCH-8
  `filter` before ranking, one-step `walk` / `walk_inverse`. Inject an
  `Embedder` with `Vault::with_embedder`; without one the vector leg is off
  and writes still work. `api/v1` `SearchQuery` / `SearchHits`.
- CI runs `bazel test //...` on Debian 13, macOS, and Windows amd64.
- USearch 2.26.0 as the in-process HNSW engine (`AnnIndex`). Crate
  feature `numkong` (default) enables SIMD kernels (`numkong` 7.8.0);
  `openmp` is opt-in and needs libomp. Tantivy 0.26.1 is the BM25
  lexical engine for `search`. The C++ bridge uses `/usr/bin/c++` on Linux. macOS
  keeps Bazel's Apple clang wrapper so `__BAZEL_XCODE_SDKROOT__` expands;
  Windows uses MSVC. NumKong headers are copied into `OUT_DIR` so Bazel
  sandboxes can compile the bridge.
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
  sources from rules_rust defaults. On Windows the cxx-build patch writes
  `rust/cxx.h` as a regular file so MSVC can compile USearch. Directory
  symlinks cxx-build leaves in USearch `OUT_DIR` are removed with
  `remove_dir` (Windows `remove_file` cannot unlink them). Windows CI uses
  `--output_user_root=C:/b` and a short crate_universe hub `pvb` so
  `CARGO_MANIFEST_DIR` stays under MAX_PATH.
- Pinned `bazel_utils_*` modules at `v0.2.3` (`buf_generate` installs local
  plugins as `name.exe` so Windows `buf.exe` LookPath can find them; Scheda
  CI still runs Windows amd64 only).
- Git operations go through `git2` (libgit2), not the `git` CLI.
- Generated Rust stubs live in `rust/generated` (crate `scheda-generated`):
  one buffa file per `.proto`, packaging `mod.rs`, and protovalidate
  `Validate` impls.
- The `scheda` crate uses generated `api/v1` messages (`Error`,
  `OpenOptions`, `Record`) instead of parallel hand-written structs.
- Schema tests load YAML/JSON fixtures from `rust/scheda/testdata/schema/`
  instead of inlined strings.
- `atomic_write` replaces the destination on Windows; native `rename`
  cannot overwrite an existing file.
- `Collection::find_one` returns `Result<Record, Error>`; a missing `id` is
  `not_found` (LIB-8).
- `SearchQuery` weights and `SearchHit` scores, when set, must be finite
  and `>= 0` (`api/v1` protovalidate, including `+Inf` / `-Inf` via field
  CEL until codegen emits `double.finite`). `search` rejects a failing
  message as `invalid` before retrieval; it does not re-check those numbers.
- `Record.id` / `SearchHit.id` reject `/` and `\\` in protovalidate
  (layout path component). `HistoryEntry.time` is required. `find` walks
  the validated `Filter` oneof; it does not re-check clause shape.
- Rust **core** is accepted (C10): the five gates in `10` §2 pass in
  `scheda` tests, including deleting `.scheda/` without losing Records.
- `docs/10-roadmap.md` (v0.12): next `scheda` work is the unencrypted
  isolated slice; **agent** (catalog, CLI, ISO-3) follows (`12`); age
  encryption of that root stays deferred. `01`, `11`, and the README
  status match.

### Removed

- Unused workspace pins `thiserror` and `tempfile`.

### Fixed

- Windows CI: GIT-6 inspects only the Vault's local git config, so a
  Git-for-Windows global `filter.lfs` does not fail `init`. Fixture
  checkout stays LF (`.gitattributes`). `scheda_doc_test` is `large` so
  rustdoc on MSVC is not a 60s timeout. bazel_utils `.bash` lint/audit
  tests are skipped on Windows (`CreateProcess` error 193); Linux and
  macOS still run them.
- `search` returns `index_unusable` when a lexical Index is on disk but
  unreadable, instead of an empty hit list. `find_one` still reads files.

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
- [0.1.0]
- [0.0.1]

[Unreleased]: https://github.com/amsokol/scheda/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/amsokol/scheda/releases/tag/v0.1.0
[0.0.1]: https://github.com/amsokol/scheda/releases/tag/v0.0.1
