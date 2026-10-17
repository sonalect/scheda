---
paths:
  - "api/scheda/v1/error.proto"
  - "rust/scheda/src/**/*.rs"
---

# Error contract

`scheda::Error` **is** `protocontract::Error`. Not anyhow. Callers branch
on the code (`error.is(VaultErrorCode::…)`, `error.code_as::<…>()`), never
on the message, the stack, or the fields of a `Fault`.

Follow `.claude/skills/error-handling/SKILL.md` when adding a
`VaultErrorCode`, constructing errors, or converting `map_err` /
`format!(...: {err})` sites.

## Rules

- **Code**: the class of the **root** reason, not of the public method.
  gRPC canonical meaning first, Scheda's own codes after. No catch-all.
  Schema rule text stays in the message under `INVALID_ARGUMENT`.
- **Message**: detail for a person. It never replaces the code and never
  carries a credential or a remote URL with userinfo
  (`redact_remote_text`).
- **One `Fault`** (`code`, `message`, `path`, `retryable`, `source`) is the
  only type Scheda raises. No per-code constructors.
- **Path**: Vault-relative POSIX of the file the failure is about. Unset
  for the network, the lock, and the Index files. Never an absolute path
  or a URL.
- **Retryable**: set from the code. A method that knows better uses
  `with_retryable`.
- **Source**: the foreign error stays reachable through
  `std::error::Error::source` (an `std::io::Error` keeps its `ErrorKind`).
  Do not flatten it into the message.
- **Stack**: `.context(frame)` frames, outermost first, one per failed
  step. No crate or module prefix (`ann.AnnIndex` is wrong). Do not repeat
  the path or the code in a frame.
- A panic of a lock holder is `RuntimeCode::Panicked`.
- Tests assert the code with `error.is(…)`, not the message.
