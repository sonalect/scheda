# Run commands from this repository

Every `bazel`, `cargo`, and `buf` command for Scheda **must** run in this
clone's root: the directory with `MODULE.bazel` and the workspace
`Cargo.toml`. The Bash tool's working directory persists between calls and
may have drifted (another repository may be the session's primary
directory), so `cd` to the root by absolute path in the same command when
unsure. Never run them from a sibling project, a scratch or tmp directory,
or a subdirectory.

## Generate

After changing `api/version.proto` (or another `.proto` at the `api`
package root), `api/v1/*.proto`, or `api/collection/v1/*.proto`,
regenerate stubs **before** tests:

```bash
bazel run //api:generate
bazel run //api/v1:generate
bazel run //api/collection/v1:generate
```

That writes `rust/api/version/`, `rust/api/api/v1/`, and
`rust/api/api/collection/v1/`. Confirm the generated `Validate` impl
contains the new rule. Optional check:
`bazel test //api:generate_test //api/v1:generate_test //api/collection/v1:generate_test`.

## Fast loop and gate

- `cargo test -p scheda --lib`: fastest, no feature, no C++ compile.
- `cargo test -p scheda --all-features` and
  `cargo clippy --workspace --all-targets --all-features`: the features
  (`find`, `search`, `simd`, `encrypt`) compile Tantivy and USearch.
- `bazel test //... --keep_going --test_output=errors`: the gate before a
  commit (`commit-after-tests.md`). It also runs `//bazel:lint`
  (buildifier) and `//bazel:markdown` (markdownlint over every `*.md`,
  `.claude/` included). `bazel run //bazel:format` fixes BUILD formatting.

## No sandbox

Do not run `bazel`, `buf`, or `cargo` inside a command sandbox. Bazelisk
needs the network (`.bazelversion`, BCR, crate_universe), the cache lives
under `~/.cache/bazel`, `buf generate` needs the Buf Schema Registry, and
generate writes into the source tree. If Bash sandboxing is on, run these
with `dangerouslyDisableSandbox: true`.
