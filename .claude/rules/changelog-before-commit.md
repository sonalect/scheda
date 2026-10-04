# Changelog before commit

Before creating a git commit, make `CHANGELOG.md` **actual** for that
commit. Do not commit product, API, CLI, on-disk, or user-facing docs
changes while `## [Unreleased]` still omits them.

## What to write

Keep a Changelog groups: `Added`, `Changed`, `Fixed`, `Removed`. English.
The user-facing *why* / effect, not a file list. New notes go under
`## [Unreleased]`. Do **not** bump the crate / `MODULE.bazel` version here:
that is a release (`.claude/skills/versioning/SKILL.md`).

If Unreleased already states the change, leave it. If it is stale or
wrong, edit it.

```text
# BAD — commit A5b, Unreleased empty
## [Unreleased]

# GOOD
## [Unreleased]
### Removed
- `Collection::layout()`. A leftover `layout` key in a Schema file is `invalid`.
```

## Skip (nothing notable)

- The commit is **only** `CHANGELOG.md`.
- rustfmt / comment-only / agent rules (`.claude/**`, `.cursor/**`) with
  no library, CLI, or `docs/` product change.
- Clippy / rustc lint-only: same contract, same on-disk bytes, same CLI.
- The owner explicitly waived the changelog for this commit.

Do not skip for "small" API, CLI flags, error codes, or Vault / Schema /
Index behaviour. A lint fix that changes a failure, path, flag, `code`, or
what the caller sees is not lint-only.
