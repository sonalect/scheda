---
name: versioning
description: >-
  Bump Scheda crate and Bazel module versions in lockstep (Rust-style 0.x or
  SemVer after 1.0.0), update CHANGELOG.md, and create the matching GitHub tag.
  Use when releasing, tagging, bumping version, editing MODULE.bazel or
  workspace package version, or adding a CHANGELOG section.
---

# Versioning

One release version for the whole repo: the Rust crate `scheda` and the Bazel
module `scheda`. Document versions in `docs/` (`Document N … Version 0.x`) are
**not** this number.

## Scheme

Versions are `MAJOR.MINOR.PATCH`. `module(version = "0.0.1")` and Cargo
`version = "0.0.1"` have **no** `v`. The git tag **does**: `v0.0.1`.

If **major is 0** (Rust approach, also stated in `CHANGELOG.md`):

- Breaking change → bump **minor** (`0.0.1` → `0.1.0`)
- Bugfix or compatible feature → bump **patch** (`0.0.1` → `0.0.2`)

If **major > 0** (standard SemVer):

- Breaking → **major**
- Compatible features → **minor**
- Bugfixes → **patch**

Do not skip numbers. Do not jump to `1.0.0` unless the user explicitly wants a
stable public API.

**Breaking** means consumer-visible: renamed/removed library operations or
types, changed LIB-5 code strings, required new arguments, or on-disk Vault /
Schema / Index format that old bindings cannot read. Toolchain and CI pins
(Bazel, rustc, `g++` in the workflow) are a patch (0.x) or minor (≥1.0) unless
they force a breaking API or on-disk change.

Pinned `bazel_utils_*` / `rules_rust` versions are **not** this number.

## Lockstep files

Set the same `MAJOR.MINOR.PATCH` everywhere:

- Root `MODULE.bazel`: `module(version = …)`
- Root `Cargo.toml`: `[workspace.package] version` (members use
  `version.workspace = true`)
- `rust/scheda/BUILD.bazel`: `rust_library` `version = …`
- `CHANGELOG.md`: move `## [Unreleased]` notes into `## [MAJOR.MINOR.PATCH] -
  YYYY-MM-DD`; add the version to `## Links` and the reference definitions
  (`[Unreleased]` compare from the new tag, `[MAJOR.MINOR.PATCH]` release URL)

Refresh `MODULE.bazel.lock` if it changes.

## Release

After the version commit is on the default branch, create an annotated tag and
push it (only when the user asked to release):

```bash
git tag -a "vMAJOR.MINOR.PATCH" -m "vMAJOR.MINOR.PATCH"
git push origin "vMAJOR.MINOR.PATCH"
```

Examples: `v0.0.1`, `v0.0.2`, `v0.1.0`.
