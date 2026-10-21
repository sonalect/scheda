# Binary size of a consumer: where Lance's weight comes from

Research of 21 October 2026, for the owner; no code changed. Measured on
Knowqore's `knq` (HEAD `71a7b08`, Scheda `v0.25.0` with `encrypt` and
`index`; Lance 13.0.0, DataFusion 54.1.0) in Knowqore's release profile
(fat LTO, `codegen-units = 1`, `panic = "abort"`, strip, static-pie).

## The numbers

- `knq` is 121.5 MB: `.text` 98.4 MB, `.rodata` 7.8 MB, `.eh_frame`
  6.2 MB, `.rela.dyn` 4.6 MB. The weight is machine code, not data.
- Share of the code by origin, from a build with symbols (thin LTO,
  `codegen-units = 16`; a symbol counts for the first crate in its name
  that is not std, futures, tokio, or similar infrastructure): Lance
  crates 25%, sqlparser 23%, DataFusion 18%, Arrow 13%, other crates only
  Lance pulls 3%; Knowqore 3%, Scheda 2%, the rest 14%. About 80% of the
  code is the Lance stack.
- Not the cause, already absent: the cloud object stores (off through
  `default-features = false`), the lance-namespace REST client and its
  reqwest 0.12, the tokenizer dictionaries, ICU data, chrono-tz. The linker
  drops them; under 1 MB together.

## Why

1. `datafusion-expr` with its feature `sql` embeds sqlparser AST types in
   `Expr` (`WildcardOptions` → `PlannedReplaceSelectItem` →
   `sqlparser::ast::Expr`). The derived `Hash`, `PartialEq`, `Debug`,
   `Clone`, and `PartialOrd` of `Expr` then pull those impls for the whole
   SQL grammar: about 25 MB of the thin build. `datafusion-sql` always
   turns the feature on; Lance turns on `datafusion/sql` and takes
   `datafusion-expr` with its default features. Still so in DataFusion
   55.2.0.
2. Lance makes its DataFusion sessions with `with_default_features()`
   (`new_session_context` in `lance-datafusion`, `merge_insert`,
   `Dataset::sql`, `mem_wal`: about 12 places). Every built-in scalar,
   aggregate, window, and table function and every file format is
   registered as a trait object, so the linker can drop none of them.
3. `lance-index` registers all ten scalar index plugins, and the vector
   side compiles IVF with every sub-index × quantizer (HNSW, SQ, RQ, PQ,
   Flat): about 11 MB of the thin build, of which HNSW, SQ, and RQ about
   6 MB. Scheda uses BTree, Inverted, and IVF_PQ (`index-kinds.md`).

## The prototype (measured)

Three patches through `[patch.crates-io]` in a copy of Knowqore, outside
both repositories: `with_default_features()` a no-op in `datafusion`;
`datafusion-expr` always uses its own light copies of the five sqlparser
types (and `datafusion-sql` drops `SELECT * EXCLUDE/REPLACE`, for the
measurement only); `lance-index` registers only BTree and Inverted.

Result in the release profile: **121.5 → 95.4 MB (−21%)**, `.text`
98.4 → 75.9 MB. In the thin build: the AST about −27 MB, the registry
about −10 MB (the functions and the Arrow kernels they pulled), the scalar
plugins about −1.5 MB. Not tested functionally: Scheda's tests were not
run against the patches.

Estimated further, not measured: Lance without SQL at all (the parser that
stays, about 7 MB thin) about −5 MB; no HNSW, SQ, or RQ about −4 MB. The
floor with Lance is about 80–85 MB: Lance's core, DataFusion's execution
(the scanner and `merge_insert` plan through it), and Arrow stay.

Switches on the consumer's side, no fork, not measured:
`-C force-unwind-tables=no` (`.eh_frame`; loses backtraces), a non-PIE
build (`.rela.dyn`; loses ASLR), `opt-level = "s"` for the sqlparser and
DataFusion crates in the consumer's profile.

## The fork, as proposed

Point 1 put to the owner on 21 October 2026, not answered yet: a fork
`sonalect/lance`, each change a feature whose default is upstream's
behaviour, and each offered upstream too.

- `sql` off: `datafusion` without `sql`, every `datafusion-*` dependency
  with `default-features = false`, and Lance's SQL-text paths gated (about
  15 files: `parse_filter`, `Dataset::sql`, the SQL conditions of
  `merge_insert` and `update`, schema evolution expressions, `mem_wal`).
  Scheda builds every filter as an `Expr` and needs none of them.
- One minimal session helper in place of the `with_default_features()`
  sessions.
- Index kinds as features, once the index set is settled
  (`index-kinds.md`).
- Apart from the fork, an issue or a PR to DataFusion: `Expr` should not
  carry the SQL AST.

Scheda would depend on the fork by a git tag; a `[patch]` does not reach
consumers. Lance has a major release about monthly, so the patches must
stay small to rebase. Scheda's whole suite runs on the fork before it is
taken.

## Not a way: `lance-index` without `lance`

`lance-index` depends on `lance-table`, `lance-file`, `lance-io`,
`lance-encoding`, `lance-datafusion`, DataFusion, and Arrow. Dropping only
`lance` saves about 15% of the code (16 of 107 MB in the thin build, about
11–12 MB in release) and loses what that crate does: the data on disk
(filters, order, refine, and exact search read it), versions with an
atomic commit (SIGKILL in the spike: 8 of 8 intact), readers on a snapshot
in other processes, upsert, delete, compaction, cleanup, and the glue of
index results to the data (deleted rows out, rows no index covers searched
exactly, addresses remapped after a compaction). Rewriting that is writing
a database on Lance's internal APIs. A much smaller binary (about 30 MB)
needs other engines: back to Tantivy and a vector library, and the
problems of `21` §1.

## How to reproduce

`git archive` Knowqore into a scratch directory outside both repositories,
with `CARGO_TARGET_DIR` there, and `unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS`
(Knowqore's `.cargo/config.toml` needs it for `crt-static`). Symbols:
`CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_LTO=thin
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`; a cold build took 6 minutes on
32 cores, the fat LTO one 11. Attribute with `nm -S --size-sort -C` by
crate name. The copies of 21 October 2026 are in
`~/.cache/scheda-lance-size/` on the owner's machine (local; may be gone).

**Why:** the owner asked whether Lance can be cut down (21 October 2026)
and will come back to it.

**How to apply:** start any work on a fork or on binary size from these
numbers; measure again after a Lance or DataFusion bump, since the causes
are specific to those versions.
