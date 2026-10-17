---
name: error-handling
description: >-
  Raise and process Scheda failures (scheda::Error = protocontract::Error,
  Fault, VaultErrorCode, retryable, stack). Use when adding a
  VaultErrorCode, editing error.proto, constructing a failure, converting
  map_err / anyhow-style sites, or bubbling failures with context / ensure /
  bail.
---

# Error handling

Public `Result<T, E>` uses `scheda::Error`, which is `protocontract::Error`.
Do not add anyhow. `scheda::v1::VaultErrorCode` (from
`api/scheda/v1/error.proto`) is the code enum; a port's or the runtime's
code (`RuntimeCode::Panicked`) can also appear.

Helpers live in `rust/scheda/src/error.rs`.

## Pick a code

A code is what a caller **acts on**. Use the gRPC canonical meaning when
Scheda means it, a Scheda code otherwise. The cause rides in `source`, not
in a new code.

| Code | Retryable | Use for |
| - | - | - |
| `INVALID_ARGUMENT` | no | the request breaks a rule (Schema text in the message) |
| `NOT_FOUND` | no | collection, Document, mirror absent |
| `ALREADY_EXISTS` | no | duplicate id or name |
| `FAILED_PRECONDITION` | no | stored state is not what the request requires (revision) |
| `UNIMPLEMENTED` | no | unsupported call, or a feature the build lacks |
| `ABORTED` | yes | the write lock was held for the whole write timeout, or `HEAD` moved under a write |
| `UNAVAILABLE` | yes | mirror, network, port |
| `UNAUTHENTICATED` | no | mirror refused the credentials |
| `NOT_A_VAULT` | no | path holds no Vault |
| `GENERATION` | no | Vault of another generation |
| `CRYPTO` | no | secret missing or wrong, bytes do not decrypt |
| `MIRROR_AHEAD` | no | mirror holds writes not pulled |
| `MIRROR_CONFLICT` | no | mirror's writes cannot be joined |
| `UNRESOLVED_CONFLICT` | no | file holds an unfinished merge |
| `INDEX_REBUILDING` | yes | Index not built yet or moving to a new format |
| `INDEX_STALE` | no | other Projector or embedding model: `Reindex` |
| `INDEX_BROKEN` | no | Index damaged: `Reindex` |
| `STORAGE` | per kind | every other I/O failure |
| `REPLICATION` | no | every other git failure |

Add a code only if a caller can **branch** on it. Edit the proto, then
`bazel run //api:generate` from the repo root (unsandboxed) and extend
`default_retryable` and the table above.

## Construct

```rust
use crate::error::{Context, Error, Fault, OrFault, git_fault, io};
use scheda_api::v1::VaultErrorCode;

// Leaf: a Fault, converted by `?` or `.into()`.
return Err(Fault::new(VaultErrorCode::NotFound, "no such collection").into());
```

| Situation | API |
| - | - |
| Our failure | `Fault::new(code, message)` |
| About a file | `.with_path("notes/a.md")` |
| Method knows the retry answer | `.with_retryable(bool)` |
| Add a frame | `.context(frame)`; lazy: `.with_context(\|\| format!(…))` |
| `std::io::Error` | `.map_err(error::io)` (then `.with_path`, `.context`) |
| git CLI failure (stderr) | `error::git_fault(stderr)`; branch inside Scheda with `git_failure(&err)` |
| Other foreign error (serde, Tantivy, USearch, UTF-8) | `.or_fault(code, message)` / `.or_fault_with(code, \|\| …)` |
| Poisoned lock | `error::mutex(&m, frame)` (`RuntimeCode::Panicked`) |
| Guard / early return | `ensure!(cond, fault)` / `bail!(fault)` (anything `Into<Error>`) |

```rust
// GOOD
std::fs::read(&path)
    .map_err(|err| io(err).with_path("notes/a.md"))
    .context("read record")?;
index.add(key, &vector).or_fault(VaultErrorCode::IndexBroken, "add vector")?;

// BAD: message replaces the code and drops the cause
std::fs::read(&path).map_err(|e| Error::new(code, format!("read {path}: {e}")))?;
```

`io` maps network kinds and timeouts to `UNAVAILABLE` (retryable) and
everything else to `STORAGE` (retryable for `Interrupted`, `WouldBlock`,
`ResourceBusy`). `git_fault` classes: auth, certificate -> `UNAUTHENTICATED`;
timeout, host unreachable -> `UNAVAILABLE`; not fast-forward ->
`MIRROR_AHEAD`; conflicts -> `MIRROR_CONFLICT`; locked -> `ABORTED`; the
rest -> `REPLICATION`. Do not define a second `context` method on
`Result<_, io::Error>`: it collides with `protocontract::Context`.

## Stack frames

English, one per failed step, `Type::method` and what happened. No crate or
module prefix. Interpolated frames use `with_context` (the closure runs
only on `Err`); do not pass `format!(…)` to `context`. If a lower frame
already names the method, do not wrap again.

## Tests

Assert the code, not the message: `assert!(error.is(VaultErrorCode::…))`
(or `RuntimeCode::Panicked`), and `error.retryable()`. Reach the cause with
`std::error::Error::source(&error)` and `downcast_ref`.

## Do not

- anyhow, or `From<anyhow::Error>`
- `format!("read {}: {err}", path)` in a message when path, code, and
  source exist
- A language backtrace in a frame, an absolute path or URL in `path`, a
  credential in a message
- Re-checking proto `buf.validate` rules in `src/`
- Branching on the message or on the fields of a `Fault`
