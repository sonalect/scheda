# Dependency list order

When adding, removing, or bumping a dependency, put it in its sorted place.
Do not append at the bottom.

## Cargo.toml

- `[workspace.members]`: alphabetical by path.
- `[workspace.dependencies]`, `[dependencies]`, `[dev-dependencies]`,
  `[build-dependencies]`, per-target tables: alphabetical by package name,
  the path crates (`scheda`, `scheda-api`) sorted in with the rest, as the
  root `Cargo.toml` already does. Optional crates stay in that same sort.

```toml
# BAD — age dumped at the bottom
regex = { workspace = true }
tantivy = { workspace = true, optional = true }
age = { workspace = true, optional = true }

# GOOD
age = { workspace = true, optional = true }
regex = { workspace = true }
tantivy = { workspace = true, optional = true }
```

## BUILD.bazel

`deps` / `proc_macro_deps` lists: alphabetical. Workspace `//rust/…` first,
then `@scheda_cargo//:…`. `bazel run //bazel:format` sorts what
buildifier can.

Does not replace `dependency-quarantine.md` or the crate_universe repin
(`cargo-bazel-lock.md`).
