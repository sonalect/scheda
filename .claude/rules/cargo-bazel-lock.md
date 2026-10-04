# Crate universe lockfile

`cargo-bazel-lock.json` is the crate_universe lock for hub `scheda_cargo`
(`lockfile =` on `from_cargo` in `rust.MODULE.bazel`). Bazel analysis uses
it; `Cargo.lock` alone is not enough for a transitive `bazel_dep`. Do not
edit it by hand.

## When to repin

After any of:

- `Cargo.toml` / `Cargo.lock` (crate pin, feature, workspace member,
  version)
- `rust.MODULE.bazel` crate_universe tags (`from_cargo`, `annotation`,
  `spec`, hub name)
- a `BUILD.bazel` dep on `@scheda_cargo//:…` for a crate that was not in
  the hub

**Before every git commit** that includes those files: the lockfile in the
tree must match this working tree. Never commit `Cargo.lock` without the
matching `cargo-bazel-lock.json`. Skip only for changes that cannot affect
the hub (docs, `src/` with no Cargo/BUILD crate-universe edit).

A stale file shows up as a crate_universe checksum / lockfile mismatch at
analysis, or as CI rewriting the JSON.

## How

From the repo root (`MODULE.bazel`), unsandboxed (`run-commands.md`):

```bash
CARGO_BAZEL_REPIN=1 bazel fetch @scheda_cargo//:all
```

If the JSON path is missing, write `{}` first, then the same command. Leave
`MODULE.bazel.lock` if Bazel rewrote it. Then `bazel test //...` as usual.

```text
# BAD — bump a crate in Cargo.toml, commit Cargo.lock, leave cargo-bazel-lock.json
# GOOD — CARGO_BAZEL_REPIN=1 bazel fetch @scheda_cargo//:all, then test, then commit
```
