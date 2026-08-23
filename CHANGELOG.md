# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
While the major version is 0, compatible additions bump the patch; breaking
API or on-disk format changes bump the minor.

## [Unreleased]

### Changed

- Target crates link with `+crt-static` (Bazel `extra_rustc_flags` and
  `.cargo/config.toml`); proc-macros and build scripts stay dynamically
  linked.

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
