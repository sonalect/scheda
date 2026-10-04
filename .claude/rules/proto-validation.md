---
paths:
  - "api/**/*.proto"
  - "rust/scheda/src/**/*.rs"
---

# Proto validation first

Constraints on `api/version.proto` (package `api`), `api/v1`, and
`api/collection/v1` envelopes belong in `buf.validate` on the `.proto`, not
in `rust/scheda/src`. Call `require_valid()` / `validate()` on
caller-supplied messages before use. After proto edits, generate
(`run-commands.md`) and confirm the generated `Validate` impl actually
contains the rule.

Stay in Rust: Vault files, git, primitive `&str` args (`add(id)`,
`get(id)`, `create_collection(name)`, `clone(url)`), Projector-dependent
checks (unmarked filter field). Record `id`, collection name slugs, and
singleton `id` slugs are library checks, not protovalidate. Collection
envelopes (`CollectionInfo`, `Declaration`, `Journal`, `Singleton`,
`Unit`) live in `api/collection/v1`, not `rust/scheda/src`.

Do not re-check a proto rule in `src/` (range, `is_finite`). If generate
omits a standard rule that is in the proto, add field CEL and keep the
standard rule; do not patch `src/`.

```protobuf
// BAD — only in Rust after require_valid
if w < 0.0 || !w.is_finite() { return Err(...) }

// GOOD
double lexical_weight = 4 [(buf.validate.field) = {
  double: {gte: 0 finite: true}
  cel: [{id: "double.finite" message: "must be finite" expression: "!this.isNan() && !this.isInf()"}]
}];
```
