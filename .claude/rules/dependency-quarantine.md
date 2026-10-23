# Dependency quarantine

Before adding or bumping an **external** dependency in any language
(crates.io, PyPI, npm, Go modules, git tags, or any other registry), look
up the **publish time of the exact version**. Do not use a version
published less than **2 days** ago.

This is a quiet quarantine against freshly published malicious releases.

- Check the registry API or package page for that version. Do not trust
  memory, download mirrors, or lockfile dates. The `scout` agent can do
  the lookup.
- If the wanted version is too new, take the newest version that is at
  least 2 days old, or wait. Do not add the package if no qualifying
  version exists.
- Applies to new packages and to version bumps of existing ones, git-tag
  pins of forks included. Does not apply to this repo's own workspace /
  path dependencies.

## Rust: ranges in Cargo.toml, versions in Cargo.lock

Scheda is a library: `Cargo.toml` states a caret range, never an `=` pin
(the owner's decision, 23 October 2026). An exact pin forces its version
on every consumer and fails their resolution when another crate needs a
different one. The floor is the version Scheda was tested with; raise it
only when Scheda's code needs a newer version (an API, a fix it relies
on).

`Cargo.lock` holds the exact versions Scheda builds and tests (Bazel reads
it through crate_universe). A move in it is a bump under this quarantine:
`cargo update -p <crate> --precise <version>` after the age check, never a
bare `cargo update`. Check every crate the move drags along, and repin
`cargo-bazel-lock.json` after it (`cargo-bazel-lock.md`).

```toml
# BAD — the consumer cannot take tokio 1.54.1 until Scheda releases
tokio = { version = "=1.54.0", default-features = false }

# GOOD — the lock pins 1.54.1; the consumer's own lock decides for it
tokio = { version = "1.54.0", default-features = false }
```

## Exempt: the Sonalect organization

Releases and tags of repositories in the `sonalect` GitHub organization
(`bazel_utils`, `rules_py`, `cxx`, `NumKong`, `USearch`, `knowqore*`, and
any other) skip the 2-day wait. Update them to the latest tag whenever the
owner asks, and without waiting when a pin is behind. A fork's tag is
exempt; the upstream version it carries is still reported with its age, but
does not block the bump. Forks outside the organization (for example
`amsokol/mimalloc`) are not exempt.

A `git_override` pins its repository by `tag`, never by `commit`. Bazel's
DEBUG advice to pin by commit is not followed (the owner's decision,
23 October 2026).
