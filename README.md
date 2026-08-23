# Scheda

Git-backed document store: text files as the source of truth, a derived index
outside git, and a library API. The first implementation is Rust; others
(Python, Java, Go, TypeScript, and more) may follow.

A Vault is a git working tree. Collections are `document` (one file per
record), `journal` (append-only JSONL segments), or `singleton`. The public
collection API is CRUD-shaped (`insert_one`, `find_one`, `replace_one`, `find`,
`scan`, `search`, `commit`). It is not a compatibility layer for MongoDB, CouchDB, or
SQL.

**Status: skeleton.** The Rust crate compiles and exposes the intended surface.
The methods are not implemented.

## Documents

The design is a numbered package in `docs/`, in English.

| File | Contents |
|---|---|
| [01-concept.md](docs/01-concept.md) | Why Scheda exists, what a Vault is, what the library refuses to be |
| [02-glossary.md](docs/02-glossary.md) | Normative vocabulary: Vault, Collection, Record, Schema, Index, kinds, API names |
| [03-model.md](docs/03-model.md) | Collection kinds, Record `id` and paths, fields and body, journal segments |
| [04-files-and-git.md](docs/04-files-and-git.md) | Files as truth, write lock, commit, `auto_sync`, `sync_remote`, clone, no LFS |
| [05-schema.md](docs/05-schema.md) | Schema as Vault data: fields, versions, index marks, edges, journal size cap |
| [06-index-and-search.md](docs/06-index-and-search.md) | Disposable Index, find vs search, embedder, hybrid ranks, edge walks |
| [07-api.md](docs/07-api.md) | Language-neutral library operations, error codes, git helpers; Rust as first binding |
| [08-agent.md](docs/08-agent.md) | Same operations for an AI agent, Schema catalog, thin transport |
| [09-isolation.md](docs/09-isolation.md) | No network by default (**core**); isolated git root and age encryption (**later**) |
| [10-roadmap.md](docs/10-roadmap.md) | Phases (core / search / agent / later), acceptance, size targets, open questions, deferrals |

## Layout

| Path | Role |
|---|---|
| [`Cargo.toml`](Cargo.toml) | Workspace: members, pinned dependency versions, lints |
| [`rust/scheda`](rust/scheda) | First implementation: Rust crate `scheda` |

Dependency versions live only in the root `[workspace.dependencies]`. Member
crates refer to them with `workspace = true`.

## Build

The Rust implementation requires Rust 1.98.

```bash
cargo test
cargo clippy --all-targets --all-features
```

## License

Apache-2.0. See [LICENSE](LICENSE).
