---
paths:
  - "rust/**/*.rs"
  - "**/Cargo.toml"
---

# No warnings by default

Rustc and Clippy warnings are not allowed. `cargo test` and `cargo clippy`
must be clean. Workspace lints `warnings = "deny"` and `clippy.all = "deny"`
enforce this.

Do not silence warnings with `allow` on hand-written code.

Every workspace crate must include `[lints] workspace = true`.

## Exception: generated files

Generated sources (protobuf, bindgen, crate build scripts, `*_pb.rs`,
`**/generated/**`) may carry `#![allow(warnings)]` at the file root. Do not
copy that allow into handwritten `src/` or `tests/`.

```rust
// BAD — handwritten
#![allow(dead_code)]
#![allow(warnings)]

// GOOD — only at the top of a generated file
#![allow(warnings)]
```
