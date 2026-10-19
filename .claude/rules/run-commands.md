# Run commands from this repository

Every `bazel`, `cargo`, and `buf` command for Scheda **must** run in this
clone's root: the directory with `MODULE.bazel` and the workspace
`Cargo.toml`. The Bash tool's working directory persists between calls and
may have drifted (another repository may be the session's primary
directory), so `cd` to the root by absolute path in the same command when
unsure. Never run them from a sibling project, a scratch or tmp directory,
or a subdirectory.

## Generate

After any edit to `api/**/*.proto`, regenerate stubs **before** tests:

```bash
bazel run //api:generate
```

That writes `rust/api/gen/buffa/` (messages and `Validate` impls) and
`rust/api/gen/contract/` (service traits). Confirm the generated `Validate`
impl contains the new rule. Optional check: `bazel test //api:generate_test`.

## Fast loop and gate

- `cargo test -p scheda --lib --no-default-features`: fastest, no feature,
  no Lance, no C++ compile. (`index`, Lance, is a default feature.)
- `cargo test -p scheda --all-features` and
  `cargo clippy --workspace --all-targets --all-features`: the features
  (`index`, `find`, `search`, `simd`, `encrypt`) compile Lance, Tantivy, and
  USearch.
- `bazel test //... --keep_going --test_output=errors`: the gate before a
  commit (`commit-after-tests.md`). It also runs `//bazel:lint`
  (buildifier) and `//bazel:markdown` (markdownlint over every `*.md`,
  `.claude/` included). `bazel run //bazel:format` fixes BUILD formatting.

## protoc

The build scripts of the Lance crates (`index`) compile `.proto` files and
need `protoc`. Cargo: `protoc` on `PATH`, or `PROTOC=/path/to/protoc` (and
`PROTOC_INCLUDE` when its `include/` directory is not next to the binary).
Do not enable Lance's `protoc` feature: it builds protoc from source.
Bazel needs nothing: `rust.MODULE.bazel` gives those build scripts
`//bazel:protoc` and `//bazel:protoc_include`.

## No sandbox

Do not run `bazel`, `buf`, or `cargo` inside a command sandbox. Bazelisk
needs the network (`.bazelversion`, BCR, crate_universe), the cache lives
under `~/.cache/bazel`, `buf generate` needs the Buf Schema Registry, and
generate writes into the source tree. If Bash sandboxing is on, run these
with `dangerouslyDisableSandbox: true`.
