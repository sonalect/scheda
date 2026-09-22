---
name: error-handling
description: >-
  Define and process Scheda api.v1.Error (code, message, path, repeat, stack).
  Use when adding Error.Code values, editing error.proto, constructing
  scheda::Error, converting map_err / anyhow-style sites, or bubbling failures
  with context / ensure / bail.
---

# Error handling

Public `Result<T, E>` uses generated `api.v1.Error` (`scheda::Error`). Do not
add anyhow. Do not replace proto `Error` with a Rust-only type.

Reference: `api/v1/error.proto`, helpers in `rust/scheda/src/error.rs`,
pattern in `rust/scheda/src/ann.rs`.

## When to add a code

Add a `Error.Code` variant only if a caller can **branch** on it. Groups use
hundreds (identity `101`, input `201`, capability `301`, IO `401`, git `501`,
index `601`). Leave room inside a group.

Do **not** enum every Schema/envelope rule — that stays `CODE_INVALID` +
Catch-alls (`CODE_IO_OTHER`) only for `ErrorKind::Other` and future/unstable
kinds. Git CLI failures are classified from exit status and stderr onto the
same `git_*` / `not_fast_forward` codes; do not join several kinds into one
proto value.

After editing the proto: `bazel run //api/v1:generate` from the repo root
(unrestricted permissions). Confirm the generated `Validate` impl contains
the rule.

## Fields

| Field | Role |
|---|---|
| `code` | Root/deep class. Same `get` may be `NOT_FOUND` or `IO_NOT_FOUND`. |
| `message` | Detail. Must not replace `code`. |
| `path` | Vault-relative POSIX of the file this failure is about. Unset if there is no such file. |
| `repeat` | Same call, same args, may succeed later? |
| `stack` | Causal frames, outermost first. Not a backtrace. |

JSON (agent / `08`):

```json
{
  "code": "CODE_IO_NOT_FOUND",
  "message": "No such file or directory",
  "path": "notes/a.md",
  "repeat": "REPEAT_ALLOWED",
  "stack": ["[Collection::get] read record", "[store::read_file] read record"]
}
```

### path

Set with `.with_path("notes/a.md")` when the failure is about a Record file.
Leave unset for `sync` / network, lock (`{vault}/.scheda/write.lock`), and
Index dumps (`.scheda/index/…`). Do not put OS absolute paths, URLs, or `.git`
here.
Do not copy `path` into `stack`.

### repeat

`Error::new` / named constructors fill `repeat` from `default_repeat(code)`
in `error.rs`. Transient classes (lock, most IO, git timeout/auth, index rebuild)
are `REPEAT_ALLOWED`. Bad input, identity, `NOT_FAST_FORWARD`, `GIT_HEAD`
are `REPEAT_FORBIDDEN`.

A **method** may override: `.with_repeat(Repeat::Forbidden)`.

### stack frames

English, one frame per step that failed. Format:

`[Type::method] what happened`

- Type is the Rust type in this crate (`AnnIndex`, `Collection`). No module
  or crate prefix (`ann.AnnIndex` is wrong).
- Public LIB-2 can use that type (`[Collection::get] …`).
- Distinguishing two steps in one method is the point:
  `[AnnIndex::new] create new index` vs
  `[AnnIndex::new] reserve capacity 16 vectors`.
- Literal frame: `.context("…")`.
- Format args: `.with_context(|| format!("[AnnIndex::add] reserve capacity {next}"))`.
  The closure runs only on `Err`. Do not pass `format!(...)` to eager
  `.context`.
- A method cannot take `format!` args.

If a lower frame already names the public method (e.g. reserve inside `add`),
do not wrap again with a second `[Type::add]`.

Input `ensure!` at the start of a method should still get a frame when other
failures from that method have one:

```rust
ensure!(
    vector.len() == self.inner.dimensions(),
    Error::invalid(format!(
        "vector length {} does not match dimensions {}",
        vector.len(),
        self.inner.dimensions()
    ))
    .context("[AnnIndex::add] vector length")
);
```

## Construct and bubble

`Error` constructors live on the generated type (`rust/api/lib.rs`).
Use them from `scheda`:

```rust
use crate::error::{ensure, bail, Code, Context, ContextAs, ContextIo};
use crate::Error;
```

| Situation | API |
|---|---|
| Our error, add a frame | `.context(frame)` |
| Frame needs format / extra work | `.with_context` with a closure |
| `std::io::Error` | `.context(frame)` (`ContextIo`) |
| `git` CLI (exit + stderr) | `.context(frame)` after `git_code` |
| Other foreign `Display` (`usearch`/`cxx`, tantivy, serde) | `.context(Code::…, frame)` |
| Guard | `ensure!(cond, Error::invalid("…"))` |
| Early return | `bail!(Error::unsupported("…"))` |
| Record file | `.with_path("notes/a.md")` |
| Method-specific retry | `.with_repeat(Repeat::Forbidden)` |

Do not `.map_err(helper).context(...)` for foreign errors. IO uses
one-arg `.context(frame)` (code comes from `io_code`). Git CLI failures
are classified with `git_code` then `.context(frame)`. Other
foreign errors use two-arg `.context(code, frame)`. Interpolated frames use
`.with_context(|| format!("… {n}"))`, not `.context(format!(...))`.

```rust
// GOOD — foreign, static frame
Index::new(&options)
    .context(Code::IndexVector, "[AnnIndex::new] create new index")?;

// GOOD — interpolated frame (closure runs only on Err)
inner.reserve(next).with_context(
    Code::IndexVector,
    || format!("[AnnIndex::add] reserve capacity {next}"),
)?;

// GOOD — already Error
path_str(path).context("[AnnIndex::restore] path must be UTF-8")?;

// BAD
Index::new(&options)
    .map_err(|e| Error::index_vector(e.to_string()))
    .context("[AnnIndex::new] create new index")?;
```

`code` stays the leaf class while bubbling. Upper frames only grow `stack`.

## Do not

- anyhow, `bail!`/`ensure!` from anyhow, or `From<anyhow::Error>`
- `format!("read {}: {err}", path)` stuffing path + foreign Display into
  `message` when `path` / `code` / `stack` exist
- Language backtraces (`file:line`, `rustc` frames)
- Re-checking proto `buf.validate` rules in `src/` (see proto-validation rule)
- Patching `repeat` only at constructors with no `with_repeat` escape hatch
- Putting Index/git-remote failures in `path`
