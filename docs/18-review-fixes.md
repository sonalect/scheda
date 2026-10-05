# 18. Full-review fixes

Document 18 of the Scheda package. Version 0.2, 5 October 2026. S1–S8 done.

Work order for the defects a full review of the repository found on
5 October 2026 (`main` at `e7adf78`). Items R1–R30 are numbered as in
that review. R1–R6 were reproduced by a throwaway test on that tree;
the rest come from reading the code.

Order inside each stage: code and a regression test first,
`cargo test` and `cargo clippy` green, then the docs the stage changes.
Tick a box only when its *Check:* passes under `bazel test //...`.

Owner decisions (5 October 2026), recorded so they are not asked again:

- **D1.** Changing `isolated` on a collection that holds Record files is
  `invalid`, the same rule as `encrypted` (ISO-11). Files stay untouched.
- **D2.** Numeric `range` (R4) changes the on-disk Index: format `6`,
  rebuilt automatically like any older format (IDX-2). It lands with the
  other fixes, so a Vault rebuilds once.
- **D3.** R13 is verified by a test before the fix. If the test does not
  reproduce it, the item closes with that test as the guard.

Consumer-visible changes (`consumers.md`): R4 (format `6`, a rebuild on
first `open`), R6 and D1 (a declaration change that was accepted is now
`invalid`), R12 (`fuzzy.distance` above 2 is `invalid`), R14 (CLI HTTP
and model2vec `model_id` change, so an existing CLI Index reports
`model_mismatch` until `reindex`). Each goes into `CHANGELOG.md`.

---

## S1. Store and declaration safety

- [x] **R1.** Conflict markers are a line that starts with `<<<<<<<` or
      `>>>>>>>` followed by a space or the end of the line, not a
      substring anywhere in the text. A bare `=======` is not counted: it
      is also a Markdown underline, and a real conflict always carries the
      other two. Applies to `check`,
      journal parsing, and `lines`.
- *Check:* a journal line whose payload contains `>>>>>>> x` does not hide
  the other lines of its segment from `get`, `lines`, `find`; a document
  containing `<<<<<<<` mid-line passes `check`; a real conflicted file
  still fails `check` with `conflict_markers`.
- [x] **R2.** Record I/O refuses a symlink: the Record file, or any
      directory between the collection directory (or `.isolated/`) and it.
      `get`, `add`, `replace`, `remove`, `history`, `lines`, `documents`,
      `ids`, and the journal append path never follow one; `check`
      reports one with its path (`invalid`).
- *Check:* a symlinked Record file is `invalid` on `get` and the target is
  not read; a symlinked subdirectory makes `add("dir/x")` `invalid` and
  writes nothing outside the Vault; `check` names the link.
- [x] **R6 / D1.** `create_collection` over an existing declaration with
      another kind is `invalid` (MOD-2). Changing `isolated` on a
      collection that holds Record files is `invalid` (D1).
- *Check:* document → journal on a collection with a Record is `invalid`
  and the declaration file is unchanged; same for `isolated`; both are
  allowed on an empty collection.

## S2. Query and search correctness

- [x] **R3.** `find` clamps `count` and `offset` to the number of indexed
      documents before building the collector; `offset` past the end is
      an empty page with the true `total`. `search` clamps the pool the
      same way; `SearchQuery.k` and `rerank_limit` get an upper bound in
      the proto.
- *Check:* `find(…, 0, u32::MAX)` and `find(…, u32::MAX, 1)` return
  (no abort); `search` with the largest allowed `k` returns.
- [x] **R5.** With `filter` set, the lexical leg runs the compiled Filter
      as a `Must` clause next to the text query (QRY-6), not a post-filter
      of the top pool.
- *Check:* 80 Records outrank one filtered-in Record on BM25; a filtered
  `search` returns that Record.
- [x] **R10.** `search` with a reranker skips a hit whose Record file is
      gone (the Index lags a `remove`), as `find` does.
- *Check:* remove a file without waiting for the indexer; `search` with a
  reranker succeeds without that id.
- [x] **R12.** `fuzzy.distance` is at most 2 in the proto; `regex_phrase`
      patterns are compiled when the Filter is compiled. A failure while
      building a query from caller input is `invalid` and does not flag
      the generation damaged.
- *Check:* `distance: 3` → `invalid`; a broken `regex_phrase` pattern →
  `invalid`; no `damaged` file appears.
- [x] **R15.** `SearchEngine::open` reports a Tantivy or vector load
      failure instead of treating the leg as absent, and flags the
      generation damaged; an unreadable `ord.json` is an error, not an
      empty map.
- *Check:* junk in `lexical/meta.json` → `find` fails with an Index code
  and the lexical-role indexer rebuilds.
- [x] **R16.** USearch expansion is set in `new` / `restore` only, never
      on the query path.

## S3. Numeric range (format 6)

- [x] **R4 / D2.** `integer` and `number` columns are indexed so `range`
      compares by value (a sortable encoding or a numeric field per
      column); `term` keeps working on them. Format `6`; format `5` is
      rebuilt in the background (IDX-2, IDX-9).
- *Check:* `n >= 5` over 9 and 10 returns both; negative numbers and
  floats order correctly; a format `5` generation is rebuilt on `open`.

## S4. Transactions and shared files across processes

- [x] **R7.** `ord.json` is saved only after lexical writes (`ord_dirty`);
      a vector-only commit never writes or removes it.
- *Check:* a vector-only round leaves `ord.json` byte-identical.
- [x] **R8.** After taking `write.lock`, the handle re-reads `tx.log`; the
      set it restores and the set it persists are the file's, not a copy
      from `open`.
- *Check:* handle A opens; process B begins, writes, dies; A's next write
  restores B's leftover and commits only its own path.
- [x] **R9.** A restore clears `tx.log` only after the files are restored.
      `Collection` uses the `Vault` restore / finish code instead of its
      own copy.
- *Check:* a restore that fails keeps its paths in `tx.log`.
- [x] **R31.** Found while testing R7: `find` / `search` on a handle that
      had not written to a journal itself read lines through a stale
      in-memory `id` map and dropped lines another process appended (right
      `total`, fewer `records`). The read path refreshes the map first.
      `Drop for Vault` now unlocks `write.lock` in every build, not only
      with `find`.
- *Check:* the two-process split-roles test returns every line.

## S5. Git

- [x] **R13 / D3.** Test `pull` on a Vault whose branch has no upstream
      and whose Remote has another branch sorted first. If it reproduces,
      fetch the current branch by name (symmetric to `push`).
- [x] **R23.** `info.remote` is `origin` when it exists (GIT-7), else
      empty.
- [x] **R21.** `commit` stages every path in one git call; the work-tree
      check is cached per handle; `history` pages in git (`--skip`, `-n`,
      a diff filter) instead of probing every commit.
- *Check:* `commit` of 100 paths spawns a bounded number of git
  processes; `history` results are unchanged on the existing fixtures.

## S6. Indexer performance

- [x] **R17.** The vector dump is saved once per pass (and on stop), not
      after every embedder batch.
- [x] **R18.** Journal `id` map changes of one pass are written as one
      rewrite per collection; owner-only modes are applied to the files
      written, with a full walk only after Tantivy commits.
- [x] **R19.** A journal segment blob is read and parsed once per pass.
- [x] **R20.** The vector leg projects a Record once; `format` and the
      projector dump are checked once per pass.
- [x] **R22.** A restore rewrites each journal `ids` file once.
- *Check:* existing indexer, journal, and search tests stay green; a
  counter test shows one dump save per pass.

## S7. Secrets and CLI

- [x] **R11.** `Debug` of `Encryption` (and so `OpenOptions`) redacts the
      secret. Prefer a proto option the generator honours; generated files
      are never hand-edited.
- [x] **R14.** CLI HTTP: status checked before parsing; 429 and 5xx are
      retryable codes; `model_id` is the model name without URL, query, or
      userinfo; model2vec `model_id` is the model's `config.json` identity,
      not its path.
- [x] **R28.** CLI maps file and stdin errors through the I/O code table;
      `--file` does not block on an open stdin pipe.

## S8. Cleanup and documents

- [x] **R24.** Remove dead code (contradictory `cfg` items in `index.rs`
      and `journal.rs`, unused `decls` in `put_collection`, unreachable
      branches in `journal_ids_in`, the unused `git_code` argument,
      duplicate `.gitignore` parsing, the repeated `open_existing` in CLI
      `dispatch`).
- [x] **R25.** Rustdoc matches the background indexer (no Tantivy writer
      on the write path, `pull` / `discard` do not rebuild); no `D…` or
      requirement codes in comments.
- [x] **R26.** `07` LIB-21 check and LIB-19 delimiter, `06` IDX-5 format
      line, `05` SCH-9 embedder sentence, README index path.
- [x] **R27.** Rust no longer re-checks rules the proto enforces.
- [x] **R29.** Python driver pins the model revision; CI actions are
      pinned by commit SHA (dependency quarantine applies).
- [x] **R30.** Every item above has its regression test (listed in its
      *Check:*), plus a two-process test with lexical and vector roles
      split and journal order verified.
- *Check:* `bazel test //...` green; CHANGELOG `Unreleased` lists the
  consumer-visible changes above.
