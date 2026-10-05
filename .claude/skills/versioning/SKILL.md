---
name: versioning
description: >-
  Bump Scheda crate and Bazel module versions in lockstep (Rust-style 0.x or
  SemVer after 1.0.0), update CHANGELOG.md, create the matching GitHub tag,
  and publish the GitHub Release. Use when releasing, tagging, publishing a
  GitHub release, bumping version, editing MODULE.bazel or workspace package
  version, or adding a CHANGELOG section.
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
- `CHANGELOG.md`: move `## [Unreleased]` notes into `## [MAJOR.MINOR.PATCH] -
  YYYY-MM-DD`; add the version to `## Links` and the reference definitions
  (`[Unreleased]` compare from the new tag, `[MAJOR.MINOR.PATCH]` release URL)

Refresh `MODULE.bazel.lock` if it changes.

## Release

After the version commit is on the default branch, create an annotated tag,
push it, and **publish a GitHub Release for that tag** (only when the user
asked to release). A tag without a published Release is incomplete. Do not
leave the Release as a draft. Do not mark it prerelease unless the user
asked. Do not backfill older tags.

```bash
git tag -a "vMAJOR.MINOR.PATCH" -m "vMAJOR.MINOR.PATCH"
git push origin "vMAJOR.MINOR.PATCH"
```

Release notes are the Keep a Changelog body for `## [MAJOR.MINOR.PATCH]` in
`CHANGELOG.md` (after that heading, until the next `## [`). Fail if that
heading is missing. Do not use `--generate-notes` or `--notes-from-tag`.

```bash
notes="$(mktemp)"
# The skill loader substitutes positional placeholders, so this block
# avoids them; escape each `.` of the version in the pattern.
sed -n '/^## \[MAJOR\.MINOR\.PATCH\]/,/^## \[/{/^## \[/d;p;}' CHANGELOG.md \
  | sed '/./,$!d' > "$notes"
test -s "$notes"

gh release create "vMAJOR.MINOR.PATCH" \
  --title "vMAJOR.MINOR.PATCH" \
  --notes-file "$notes" \
  --verify-tag
rm -f "$notes"
```

Run `git` / `gh` from the clone root with unrestricted permissions. Return
the Release URL when it succeeds.

Examples: `v0.0.1`, `v0.0.2`, `v0.1.0`.
