# Test before commit

Never create a git commit until `bazel test //...` has exited 0 on **this
working tree** in this session. `cargo test` alone is not enough: CI runs
Bazel (`//bazel:lint`, `//bazel:markdown`, generate checks, Python, the
rest).

Follow `run-commands.md`: repo root (`MODULE.bazel`), unsandboxed. After
`api/**/*.proto` edits, generate first. If crate_universe inputs changed,
repin `cargo-bazel-lock.json` first (`cargo-bazel-lock.md`).

```bash
bazel test //... --keep_going --test_output=errors
```

If it fails, fix and re-run. Do not commit a red tree.

```text
# BAD — commit after a local cargo test, skip Bazel
# (this failed Linux CI: MODULE.bazel reformat from //bazel:lint)

# GOOD — Bazel green, then git add / commit / push
```

Do not skip for "format-only", "comments", `.bazelrc`, `MODULE.bazel`, or
Markdown: markdownlint is part of the gate. The owner must explicitly
waive this rule to commit without that test.
