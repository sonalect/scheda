# Scheda

Git-backed document store: text files as the source of truth, a derived index
outside git, and a Rust library API.

A Vault is a git working tree. Collections are `document` (one file per
record), `journal` (append-only JSONL), or `singleton`. The public collection
API is CRUD-shaped (`insert_one`, `find_one`, `replace_one`, `find`, `search`,
`commit`) and is not Mongo-compatible.

**Status: skeleton.** The crate compiles and exposes the intended surface. The
methods are not implemented.

## Layout

| Path | Role |
|---|---|
| [`Cargo.toml`](Cargo.toml) | Workspace: members, pinned dependency versions, lints |
| [`rust/scheda`](rust/scheda) | Library crate `scheda` |

Dependency versions live only in the root `[workspace.dependencies]`. Member
crates refer to them with `workspace = true`.

## Build

Requires Rust 1.98.

```bash
cargo test
cargo clippy --all-targets --all-features
```

## License

Apache-2.0. See [LICENSE](LICENSE).
