# 21. The Index on Lance

Document 21 of the Scheda package. Version 0.4, 19 October 2026.

Status: approved by the owner on 19 October 2026. Decisions marked
*settled* were taken with him in discussion; the others were proposed
with a recommendation and accepted as written (`owner-review.md`). The
contract (§8, L0) was reviewed with him on 19 October 2026, item by item;
the decisions it changed say so. The work goes on the branch
`lance-index`, stage by stage (§3). The spike
(§6) stays on the branch `lance-spike` and never reaches `main`.

Work order: replace the two engines of the Index, Tantivy (lexical) and
USearch (vectors), with one: Lance. The Index keeps its place in Scheda
(derived from git, rebuilt from git, never the source of truth), its
indexer thread, and its ports (Projector, Embedder, Reranker). What
changes is how the Index stores, updates, and serves what it holds, and
the query surface: a typed filter the library turns into Lance's own
expressions, with no SQL from the caller.

Order: the contract first (L0: every proto change, reviewed by the owner
before any code), then L1, then L8 (the old Index removed, moved forward
by the owner), then the stages of `22` (Scheda on tokio, A1–A5), then
L2–L7, L9, L10. Inside each later stage: code and tests first, then
`cargo test` and `cargo clippy` (with and without `--all-features`)
green, then the box is ticked when its *Check:* passes. `bazel test
//...` before the work merges into `main`. One docs pass at the end
(L10).

---

## 1. Why

The Index of `0.22` (`20` D23) does not survive the anchor consumer's
scale. A corporate deployment holds millions of Lessons and artefacts,
several index entries each, one vector of 2 560 float32 values per entry
(Qwen3-Embedding-4B, about 10 KB). One million entries is about 10 GB of
vectors. Against that:

- The vector side keeps all vectors in one file, `index/vector/vectors`,
  read whole and written whole by every round that changes one vector:
  about 10 GB read, written, and `fsync`ed per commit at 1M entries.
- Every process that searches loads the whole file into memory and
  reloads it after each round.
- A process killed between the writes of `vectors`, `state`, and
  `commit` leaves the vector side out of step for good (`INDEX_BROKEN`
  until `Reindex`).
- A rebuild empties the Index first, so queries and searches are
  `INDEX_REBUILDING` for its whole length, and every text is embedded
  again.

The audit of 19 October 2026 (`.claude/notes/index-risks.md`, items 1–16)
lists these and the rest; §4 says what becomes of each.

A spike (§6) measured Lance at 1M entries × 2 560 dimensions: a write of
10 rows appends 0.12 MB and takes about 30 ms; a SIGKILL in the middle of
a write leaves a table that opens and searches; vector search at `k` 300
takes 60–100 ms with recall 1.00; BM25 takes 12–27 ms.

---

## 2. Decisions

**D1. Lance replaces Tantivy and USearch; the `lance` crate, not
`lancedb`.** *Settled.* One engine holds the lexical side (Lance's
inverted index, BM25), the vector side (IVF indices), and the scalar
columns a query filters on. Scheda uses the `lance` crate directly.
`lancedb` adds a multi-table database, an embedding registry, rerankers,
and a cloud client, none of which Scheda needs (its ports already do that
work), and `lancedb` 0.38–0.40 does not compile without its `remote`
feature (`job.rs` uses `Error::Http` unconditionally).

**D2. No compatibility, no migration of data.** *Settled.* `0.23.0`
breaks the Index API and the on-disk Index. A Vault opened by `0.23`
removes the old `.scheda/index/` and builds the new Index from git; no
vector is carried over. The anchor consumer runs its learning again from
nothing (it moves to other models anyway).

**D3. The query surface is a typed filter the library builds.**
*Settled; changed by the owner in L0 (§8, C29).* `Collection.Query`,
`Collection.Count`, `Vault.QueryDocuments`, `Vault.CountDocuments`, and the
prefilter of a search take a `Filter`: a tree the caller fills (`all_of`,
`any_of`, `none_of`, `term`, `terms`, `range`, `exists`, `prefix`) over
`id` and the columns of the collection. The library checks it against the
Projector's columns and turns it into a DataFusion expression itself,
which Lance runs (`Scanner::filter_expr`). No caller writes SQL, and no SQL
text is built. The `Filter` of `0.22`, a Tantivy query tree, goes.

**D4. A rebuild is built beside the live Index and switched to.**
*Settled; changed by the owner in L0 (§8, C16).* `Index.Reindex` (and the
first build of an Index that does not exist) builds new datasets from the
snapshot of `HEAD` it starts at, then catches them up with the usual tree
difference to the `HEAD` of the moment, and switches to them under
`index.lock` with one atomic write of a small pointer file. Queries and
searches keep answering from the live datasets until the switch. The live
datasets stay at their state meanwhile: a rebuild is the caller's own
decision, and the writes made during it appear at the switch.
`INDEX_REBUILDING` remains only for an Index never built.

**D5. Freshness by a state token.** *Settled.* Every write returns the
state token of the Vault it made: the commit of the main repository and,
when there is one, of the nested repository (the token `IndexState.version`
already speaks). A query, a count, or a search may name a token
(`not_before`); it then waits, up to a deadline, until the Index reflects
a state that contains that commit (`graph_descendant_of`, or equal). A
token whose commit is no longer in the history of `HEAD` (a reset, a
rewriting pull) does not wait. Without a token the call answers at once
from what the Index holds. This replaces the caller's `wait_indexed`
pattern; the write itself never waits for the Index.

**D6. One Lance dataset per collection.** *Proposed.* Each indexed
collection is a dataset `{vault}/.scheda/index/{generation}/{collection}.lance`,
whose columns are that collection's Projector columns, typed (D8), so a
filter runs on real columns and kinds never clash between
collections. A call over several collections queries their datasets and
merges (as `Vault.QueryDocuments` and `Vault.SearchDocuments` merge
today). `{vault}/.scheda/index/current` names the live generation (D4).
*Alternative:* one dataset for the Vault with a `collection` column and
the union of all columns: rejected, the same column name may have
different kinds in two collections. *Recommendation:* per collection.

**D7. `commit` names the version of every dataset.** *Proposed;
changed in L0 (§8, C22).* The `commit` of a generation holds the state it
reflects (`main {oid} isolated {oid}`) and, for each collection, the Lance
version of its dataset and of its table of files (D8). A reader opens
exactly those versions, so one call sees one state across all
collections. A round writes new versions, then writes `commit` last, as
`0.22` writes `index/commit`; its state is `IndexState.version`. A dataset
ahead of `commit` (a round died after writing it) is restored to the
version `commit` names (`Dataset::restore`) before the next round applies
the difference, so every round starts from one known state. After
maintenance (D11), `commit` is written again with the new versions of the
same state. *Recommendation:* accept.

**D8. Columns.** *Proposed; changed in L0 (§8, C27, C28, C35).*
Identity: `id` (Utf8, BTree index), `file` (Vault-relative path of the
Document or journal segment), `order` (Utf8: the place of a journal line
in append order, the byte-comparable key of `0.22`, made from the
segment's name and the line). Projector kinds map to Arrow: `text` → Utf8,
`integer` → Int64, `number` → Float64, `boolean` → Boolean, `text list` →
List<Utf8>. A column marked `scalar_index` gets a Lance scalar index, one
marked `fts` an inverted index. A column marked `vector` gets two more
columns, `_vector_{name}` (FixedSizeList<Float32, dims>) and
`_digest_{name}` (the git blob hash of the text that was embedded), so an
unchanged text keeps its vector; a caller's column never starts with `_`,
and Lance reads a `.` in a name as a path into a struct. Beside each
dataset, a table of the files the Index holds and the git blob id of each,
one row per file. *Recommendation:* accept.

**D9. A `vector` column is a `text` column: one vector per value.**
*Proposed.* `0.22` lets a `text list` column be `vector`, one vector per
element (a slot per element in the key map). With Lance that is either a
multivector column (MaxSim scoring, cosine only: another meaning) or a
second dataset of slots. The anchor consumer uses one `text` column. A
`text list` marked `vector` becomes `INVALID_ARGUMENT` when the Projector
describes its columns. *Recommendation:* accept; a consumer that needs
several vectors per Document writes several Documents (as the anchor
consumer does, one per index entry).

**D10. The vector index: IVF_PQ, cosine, refined.** *Proposed.* A
dataset gets its vector index once it holds 10 000 rows (below that, an
exact scan is fast enough and IVF has too little to train on). Searches
use `refine_factor` 10: the PQ candidates are re-ranked with the full
vectors read from disk. In the spike that gave recall 1.00 at `k` 300 on
1M rows in 58 ms (fresh) to 101 ms (four index deltas), with 0.9 GB of
memory per searching process. IVF_SQ gave 0.99 at about the same speed
with 2.4 GB per process; IVF_HNSW_SQ 0.95. *Recommendation:* IVF_PQ with
refine 10; the values are constants in code, not options.

**D11. Maintenance is the indexer's, with fixed settings.** *Proposed.*
After a round that wrote rows, the indexer, still under `index.lock`:
compacts with `target_rows_per_fragment` 16 384 (Lance's default, about
one million rows, rewrites the largest fragment on every compaction: the
spike saw 10 GB rewritten each time); brings the indices over the new
rows when more than 10 000 are unindexed or every 50 rounds; merges the
vector index deltas when there are more than 4
(`OptimizeOptions::num_indices_to_merge`); and prunes a version an hour
after it was replaced, never with `delete_unverified`. Lance prunes by a
version's own creation time, so the cutoff given is the creation time of
the newest version made more than an hour ago (§8, C23). No read of the
Index outlives one call (C24), so the hour is a margin over seconds. The
old generation of a rebuild is deleted an hour after the switch (C25).
*Recommendation:* accept.

**D12. Upserts by `id`, whole rows.** *Proposed.* A changed Document is a
`merge_insert` on `id` with every column (update when matched, insert
when not), a deleted one a delete by `id`; a changed journal segment
appends its new lines (L3). `merge_insert` with a scalar index on the key
has open upstream bugs (lancedb #3177, #3515: rows silently not updated,
reported for partial-column updates, from Python, on 0.30 and 0.33); the
spike found none on Lance 13 with whole rows, before and after
`optimize`, and L2 keeps that as a regression test.
*Recommendation:* accept.

**D13. Async on the caller's runtime.** *Replaced after L0 by `22`
(Scheda on tokio).* Scheda's objects are async only, on the tokio runtime
of the caller; the Index awaits Lance's futures in its own async code,
and the work of a call on Lance runs as a task of its own, so a panic in
it is `Panicked` (`22` D9). Scheda has no runtime of its own and no bridge
from blocking code into async code. *Recommendation:* accept.

**D14. One feature, `index`.** *Proposed.* `find` and `search` split
Tantivy from USearch; with one engine they merge into one feature,
`index`, on by default. Without it there is no Index object (as without
`find` today) and journals keep their id maps. `simd` (USearch's) goes.
*Recommendation:* accept.

**D15. The query request.** *Proposed; changed in L0 (§8, C29, C33).*
`DocumentQuery` becomes `{ filter, order_by, offset, limit, start_after,
include_raw, not_before }`: `filter` the tree of D3 (unset: every
Document); `order_by` a list of `(column, descending)`, default `id` (a
journal: `order`); `start_after` the last `id` of the previous page;
`offset` to jump to a page. A query streams a `QuerySummary { total }`
first, counted in the same snapshot as its page, then the Documents.
`DocumentSearch` gets `filter` as its prefilter. An unknown column, a value
of another kind than its column, and a filter deeper than
`OpenOptions.filter_max_depth` are `INVALID_ARGUMENT`.
*Recommendation:* accept.

**D16. Full-text search.** *Proposed.* BM25 over the `fts` columns with
Lance's default tokenizer (`simple`, lower case, no stemming), without
positions, so no phrase queries in `0.23` (the anchor consumer searches
vectors only). A later version can add positions and phrase search per
column. *Recommendation:* accept.

**D17. What goes.** *Proposed; changed in L0 (§8, C15, C34, C35).*
Tantivy, USearch (the `sonalect/USearch` fork pin), `lexical.rs`,
`vector.rs`, `filter.rs`, the `Filter` messages of `0.22`, `index/vector/`,
`index/lexical/`, `index/columns`, `index/stale`, the features `find`,
`search`, `simd`, and `IndexState.model_mismatch`. No Vault rebuilds the
Index by itself, but for an Index that does not exist (D2). A Vault whose
Projector is of another projection or gives other columns, or whose
embedder is of another model or vector length, than the Index was built
with is `INDEX_STALE`: it does not index, and its queries and searches
fail, until an explicit `Index.Reindex` with those ports has switched the
Index to them; given the Index's ports again, it is not stale. A commit the
Index stands on that is gone from its repository is caught up by
comparing the tree of `HEAD` with the table of files (D8), not by a
rebuild. The journal id maps stay as they are. *Recommendation:* accept.

**D18. Dependencies.** *Proposed.* `lance` (and the `lance-*` crates the
code names: `lance-index` for `IndexType` and the index parameters,
`lance-linalg` for `DistanceType`, which `lance` does not re-export) pinned
exactly (`=13.0.0` today), all with default features off: Lance's defaults
pull every cloud object store and `geo`, and Scheda uses local paths only.
No `arrow-*` or `datafusion` pin of its own: `lance::deps` re-exports
`arrow_array`, `arrow_schema`, and `datafusion` at the versions Lance uses. Lance publishes a major version about every month:
upgrade one version at a time, deliberately, re-running L2's checks. The
build scripts of `lance-datafusion`, `lance-encoding`, `lance-file`,
`lance-index`, and `lance-table` need `protoc`: Cargo gets it from
`PROTOC` (`run-commands.md` says so), Bazel from crate_universe
annotations that hand those build scripts `//bazel:protoc` and
`//bazel:protoc_include` (the well-known types, which prost-build reads from
`PROTOC_INCLUDE`). Every new
crate version passes the two-day quarantine (`dependency-quarantine.md`;
the spike found three too young: `arc-swap` 1.9.3, `cc` 1.9.0,
`find-msvc-tools` 0.1.15). *Recommendation:* accept.

---

## 3. Stages

### L0. The contract first, reviewed by the owner

D3, D5, D9, D14, D15, D16, D17. Before any code, every change this work
order makes to `api/scheda/v1` is written into §8 as a diff against the
protos of `main`, with the comment of each new or changed field, and shown
to the owner. Nothing in `api/` changes before he approves; his comments
go into §8 and into the decisions they touch. Then the approved protos are
written into `api/scheda/v1`, not yet generated, and the owner looks at
the files themselves before any code. They are then kept as
`.claude/notes/l0-protos.patch` (the same text as §8.5), and L5 applies
the whole patch with the generate, so every commit stays green.

- [x] `query.proto`: `Filter` (D3), `DocumentQuery` and `VaultQuery` per
      D15 (`filter`, `order_by`, `offset`, `limit`, `start_after`,
      `include_raw`, `not_before`), `QueryItem`; `DocumentSearch` and
      `VaultSearch` with `filter` and `not_before`; the `Filter` of `0.22`
      replaced; the `buf.validate` rules proto can carry (limits, the order
      list).
- [x] The state token on writes (D5): its message and name, and which
      replies carry it (`Create`, `Update`, `Upsert`, `Delete`, the
      batches of a collection and of the Vault).
- [x] `index.proto`: `IndexState` after D4, D5, and D17 (`version`, how a
      caller tells the Index is behind `HEAD`, `rebuild` for a rebuild
      beside, `role`, `pending`, the `model_mismatch` of `0.22`); the
      comments of `Status`, `Watch`, and `Reindex`.
- [x] `options.proto`: the deadline a `not_before` waits for.
- [x] `error.proto`: `INDEX_REBUILDING` and `INDEX_STALE` after D4 and
      D17 (kept, narrowed, or removed), and the code of a `not_before`
      that reaches its deadline.
- [x] `ports.proto`: `IndexColumn` after D9 (a `vector` mark on a
      `text list`) and D16.
- [x] The search reply: `SearchItem` and the `VectorLeg` reasons after
      D14 and L6.
- [x] The CLI subcommands and flags (`14`) that follow from these
      messages, listed.
- *Check:* the owner approved §8 (the date is written there): 19 October
  2026, and he looked at the proto files the same day.

### L1. Dependencies and the build

D1, D14, D18.

- [x] `lance` and its crates in the workspace (`=13.0.0`), sorted
      (`dependency-order.md`), every new version checked against the
      quarantine; `Cargo.lock` and `cargo-bazel-lock.json` repinned
      (`cargo-bazel-lock.md`).
- [x] `protoc` for the Lance build scripts in Cargo (`PROTOC`) and in
      Bazel (annotations); `run-commands.md` updated.
- [x] The feature `index` (D14) declared, `find` / `search` / `simd`
      still present until L8 removes them.
- [ ] The debug loop measured and written down: a cold
      `cargo build -p scheda --all-features`, and a rebuild after
      touching one file of `rust/scheda/src`. If the rebuild is slow,
      say so to the owner before L2 (the owner cares about debug builds,
      not release ones). Measured on 19 October 2026: 235 s for the new
      crates (238 of them, the rest of the target directory warm); the
      rebuild after `touch rust/scheda/src/lib.rs` took 0.9 s, which
      proves nothing yet because no code uses Lance. Repeat in L2.
- *Check:* `cargo build` and `cargo clippy --workspace --all-targets
  --all-features` green; `bazel build //rust/scheda/...` green on Linux,
  macOS, and Windows CI.

### L2. The Lance store

D6, D7, D8, D9, D10, D11, D12, D13. A new module (`store.rs`), used by
nothing else yet.

- [ ] Async on the caller's runtime (D13, `22`): Lance's futures awaited
      in Scheda's async code, the blocking steps behind `22`'s gate.
- [ ] A dataset per collection: created from the Projector's columns
      (D8, D9), opened, its schema checked against the columns; the
      generation folder and `current` (D6).
- [ ] Upsert of whole rows and delete by `id` (D12); `commit` naming
      the version of each dataset and table of files, read by readers;
      restore of a dataset ahead of `commit` (D7).
- [ ] Owner-only modes on every file Lance writes (Lance writes with the
      process umask): applied after each commit, as `restrict_tree` did
      for Tantivy in `0.22`; `Vault.Check` skips the datasets while a
      round holds `index.lock`, as it skipped `lexical/` (L8 removed both
      with Tantivy).
- [ ] Maintenance (D11): compaction, index optimisation, delta merge,
      prune an hour after a version was replaced.
- *Check:* a reader opens the versions `commit` names; a dataset written
  past `commit` by a killed round is restored, and the next round ends with
  the same rows as a round that was not killed.
- *Check:* a version made long ago and replaced a moment ago survives a
  prune; it goes once its successor is an hour old.
- *Check:* a child process killed by SIGKILL in the middle of upserts
  leaves a dataset that opens, counts, and searches (the spike's `kill`).
- *Check:* 200 upserts of existing rows each carry a marker, before and
  after compaction and index optimisation; no row appears twice
  (regression for lancedb #3177 / #3515).
- *Check:* compaction after small writes rewrites only the small
  fragments: the bytes written are of the order of the new rows, not of
  the dataset.
- *Check:* every file under `index/` is owner-only after a write.

### L3. The indexer on Lance

`20` D23 keeps its shape, as the task `22` A3 makes of it: the round,
`index.lock`, the wake and the look at `HEAD`, who indexes, the journal
id maps. Lance is a new side of its round.

- [ ] The parts of a round: the journal id maps (unchanged), then each
      dataset brought from its own state to `HEAD` (D7), then
      `index/{generation}/commit` last.
- [ ] The Index object (`Status`, `Watch`, `Reindex`) and the ports (a
      Projector, an embedder, a reranker) accepted again, with who
      indexes (D17); L8 left them `UNIMPLEMENTED`.
- [ ] Projection once per changed Document, embedding only the texts
      whose digest changed; the in-memory cache of vectors across failed
      tries kept.
- [ ] A changed journal segment indexes only its new lines when its old
      bytes are a prefix of the new ones; the whole segment otherwise
      (`index-risks.md` item 7).
- [ ] The stop is honoured between the portions of an `Embed` stream:
      closing a Vault does not wait for a whole round's embedding
      (`index-risks.md` item 5).
- [ ] Maintenance after a round that wrote rows (D11).
- [ ] The table of files of each collection, and the catch-up that
      compares the tree of `HEAD` with it when the old commit, or an object
      the difference needs, is gone (D17).
- *Check:* a write is found by a query and a search once the Index
  reached it; a deleted Document is gone from both.
- *Check:* after any changes, the difference of two commits and the
  comparison with the table of files give the same Index; an isolated
  repository made again is caught up without a rebuild and without
  embedding an unchanged text.
- *Check:* one `Project` per changed Document per round; a round that
  re-applies a diff sends only changed texts to the embedder.
- *Check:* an embedder that fails changes no `commit`, fails no write,
  and the next round sends only the texts without vectors.
- *Check:* appending one line to a 200-line segment projects one line.
- *Check:* closing a Vault in the middle of a slow embedding returns
  within one portion.
- *Check:* a commit made with `git`, a pull, and three processes on one
  Vault end at one version (the tests of `0.22` that say so, moved).

### L4. Rebuild beside

D4, D17.

- [ ] A rebuild builds the next generation from the snapshot it starts
      at, catches it up to `HEAD`, and switches `current` under
      `index.lock`; the live generation stays at its state meanwhile. The
      old generation is deleted an hour after the switch (Windows: a file
      still open is tried again later). Generation names are unique.
- [ ] `Index.Reindex` starts a rebuild with the Vault's ports; the indexer
      starts one by itself only for an Index that does not exist (D2). A
      Vault with other ports than the Index was built with is
      `INDEX_STALE` (D17).
- [ ] `IndexState.rebuild` reports its stage, its cause, and its counts.
- [ ] `open` removes an Index of the `0.22` layout (D2), and the first
      build starts (`CAUSE_NEW`); moved here from L8.
- *Check:* queries and searches answer from the live generation for the
  whole length of a rebuild, and from the new one after the switch.
- *Check:* a write during a rebuild is found after the switch.
- *Check:* a process killed during a rebuild leaves the live generation
  serving; the next indexer removes the unfinished generation.
- *Check:* `version` after the switch contains the one before.
- *Check:* a Vault opened with another projection, columns, or embedder
  model is `INDEX_STALE` and writes nothing under `index/`; opened again
  with the Index's ports, it indexes the writes made meanwhile.

### L5. Query and count by filter

D3, D15.

- [ ] The contract of §8 in `api/scheda/v1`: `git apply
      .claude/notes/l0-protos.patch`, then `bazel run //api:generate`; the
      code of every stage before follows it in the same commit.
- [ ] `Collection.Query`, `Collection.Count`, `Vault.QueryDocuments`,
      `Vault.CountDocuments` on the datasets: the filter as a DataFusion
      expression, the order, `start_after`, `offset`, `limit`, the total;
      the keys of the page read at the start of the call (§8, C24).
- *Check:* each kind of leaf over each column kind selects the exact set,
  with and without a scalar index on the column, and Lance uses the index;
  `none_of` matches a Document without a value; the order is stable;
  paging with `start_after` neither repeats nor skips; `total` agrees with
  the page.
- *Check:* an unknown column, a value of another kind, and a filter deeper
  than `filter_max_depth` are `INVALID_ARGUMENT`; a filter of depth 100
  runs on a 2 MiB thread stack.
- *Check:* a query of a collection the Index has not reached yet is
  empty, not `INVALID_ARGUMENT` (`p8-follow-ups.md`, first two items).

### L6. Search on Lance

- [ ] The vector leg (IVF_PQ with refine, or an exact scan below 10 000
      rows), the lexical leg (BM25), RRF fusion with the request's
      weights, the prefilter (`filter`), the Reranker port, `like`.
- [ ] The vector-leg reason decided from the Projector's columns, not
      from what the Index holds (`p8-follow-ups.md`).
- *Check:* the `0.22` search tests that still apply, moved (token query,
  synonym query, hybrid fixture, prefilter, reranker swap, `like`).
- *Check:* recall at `k` 300 against an exact scan on a fixture of
  clustered vectors is at least 0.95.
- *Check:* a write is found by a search given its token before the indices
  are brought over it (`fast_search` stays off).

### L7. The state token

D5.

- [ ] Writes return the state token (`Document`, the batch results).
- [ ] `not_before` on queries, counts, and searches, with a deadline
      (`OpenOptions`), the ancestry check, and the rewritten-history
      case.
- [ ] A Vault that does not index reports that the Index is behind
      `HEAD` (`index-risks.md` item 4), so a caller can tell.
- *Check:* write, then search with the write's token, finds it without a
  sleep; a token from a commit a reset removed does not wait; the
  deadline is `DEADLINE_EXCEEDED` (or the code `20` D9 gives a timeout).

### L8. The old Index removed

D17. *Runs right after L1* (the owner, after L0): nothing about to go is
made async by `22`.

- [x] Tantivy, USearch, `lexical.rs`, `vector.rs`, `filter.rs`, the
      query and search engines, `index/stale`, `index/columns`, the
      features `find` / `search` / `simd` / `openmp`, their tests; Cargo
      and Bazel repinned (`cxx`, `numkong`, `sha1`, and their forks went
      with USearch). What stays is today's build without `find`: the
      indexer with the journal id maps, and the state of the Index.
- [x] `Query`, `Count`, and `Search` answer `UNIMPLEMENTED` until L5 and
      L6; the CLI's `query`, `count`, and `search` go until L9.
- [x] As without `find` today (the owner, §8.4): `Vault.Index` and every
      port (a Projector, an embedder, a reranker) are `UNIMPLEMENTED`
      until L3, so `tests/consumer.rs`, which opens with ports, goes whole
      until L9; the CLI loses its Projector and its `index` subcommand
      until L9, and keeps the embedder and reranker flags, which reach the
      library.
- *Check:* `git grep -n -i -e tantivy -e usearch rust/scheda rust/cli` is
  empty (`rust/api/gen` follows `api/` in L5, with the patch); `cargo
  build` and `cargo clippy --all-targets` (all features, default ones,
  none), `bazel build //...`, and `bazel test //bazel:lint
  //bazel:markdown //api:all //rust:lint` green on 19 October 2026. The
  Rust test suites were not run: the owner keeps them off while the Index
  is replaced, as in L1.

### L9. CLI, Python driver, consumer test

- [ ] The CLI per §8.3: the new fields in its JSON, the summary of a
      query, the replies of writes, `--index-wait-timeout-ms`,
      `--filter-max-depth`; its Projector and the `index` subcommand back
      (L8 removed them); the Python driver follows.
- [ ] `tests/consumer.rs` rewritten to the anchor consumer's use after
      this change: vector-only search on one collection, a write's token
      passed to the search, `Index.Reindex` while searches go on.
- *Check:* `tests/consumer.rs` green; the CLI and the library give the
  same JSON for a query and a search.

### L10. Docs pass and release

- [ ] `06` (the Index on Lance), `13` (queries by filter, searches), `07`,
      `14`, `02`, README, `.claude/notes/index-risks.md` (closed items
      removed), `CHANGELOG.md`; with `22` A5.
- [ ] Release `0.23.0` (breaking: minor while major is 0,
      `.claude/skills/versioning/SKILL.md`).
- *Check:* `bazel test //...` green; markdownlint green.

---

## 4. The audit's items after the move

Items of `.claude/notes/index-risks.md` (and `p8-follow-ups.md`):

| Item | After the move |
| - | - |
| 1. Vector side out of step after a kill | Gone: one atomic Lance commit carries data and state (D7) |
| 2. A rebuild embeds everything, queries down meanwhile | Queries answer from the live Index during a rebuild (D4); embedding everything again stays for a model change, by nature, and only the caller starts a rebuild (D17) |
| 3. Embedder outage re-projects the whole diff | Stays as a cost; the in-memory cache of the round's projections is worth adding in L3 if it shows |
| 4. No way to wait for one's own write | Gone (D5, L7) |
| 5. Close waits for a whole embedding | Fixed in L3 |
| 6. One stale collection breaks vector search | Gone: no `stale` (D17) |
| 7. A journal append re-indexes its segment | Fixed in L3 |
| 8. `vectors` read and written whole | Gone: Lance appends fragments (D11) |
| 9. `Reindex` from a second process rebuilds twice | Stays; cheaper with D4; drop a request older than the last switch (L4) |
| 10. Temp names collide between processes | Unchanged for the journal maps and `commit`; fix with the pid in the name when L3 touches them |
| 11. Windows and files in use | Moves to L4 (deleting an old generation) |
| 12. No directory `fsync` | Lance's own commit protocol; unchanged for `commit` |
| 13. D23's wording on the lexical side | Superseded |
| 14–15. Columns taken from the Index, not the Projector | Fixed in L5 and L6 |
| 16. Tantivy version in `06` | Superseded |

---

## 5. Known problems of Lance to keep in mind

- **Defaults that rewrite everything.** Compaction's default target
  rewrites the largest fragment each time (D11). Never call compaction
  with defaults.
- **Pruning under readers.** Lance prunes a version by its own creation
  time, so one replaced a moment ago may go at once; `delete_unverified`
  may delete files a reader in another process still reads. Scheda counts
  the hour from the replacement and never uses `delete_unverified` (D11).
- **The SQL filter parser.** It pastes the text into `SELECT 1 FROM t
  WHERE …`, drops a tail such as `LIMIT 0` silently, and matches names
  case-insensitively. Scheda never hands it text (D3).
- **Dots in column names** are paths into a struct (D8).
- **Index deltas.** Each index optimisation adds a delta; search slows
  as they pile up (58 → 101 ms with four in the spike) until they are
  merged (D11).
- **No index on the key, slow upserts.** Without a BTree on `id`,
  `merge_insert` scans the key column (109 ms per 10-row write at 1M
  rows, 31 ms with the index).
- **Upstream bugs around `merge_insert` and scalar indices** (lancedb
  #3177, #3515): not reproduced on Lance 13 with whole rows; kept as a
  regression test (L2).
- **Async only.** `block_on` inside an async task panics (D13).
- **Major versions monthly.** Pin exactly, upgrade deliberately (D18).
- **Memory per searching process.** About 0.9 GB per million 2 560-dim
  vectors with IVF_PQ and refine; 2.4 GB with IVF_SQ.
- **Build time.** A cold release build of the spike took 14 minutes; a
  release rebuild of its one crate 9 minutes (Lance's async generics are
  compiled into the caller). The debug loop is L1's first measurement.
- **Windows.** Lance publishes Windows x64 builds (`pylance`,
  `lancedb` wheels); deleting a dataset another reader holds open is
  untested (L4).

---

## 6. The spike

Branch `lance-spike`, `spikes/lance/` (its own Cargo workspace, outside
Scheda's; `lancedb` 0.40.0 on Lance 13.0.0, release build). Synthetic
data shaped like the anchor consumer's `lesson-index`: `id`, `lesson`,
`role`, `text` (20 words with a topic word), one 2 560-dim vector from
1 000 clustered topics. Commands: `build`, `commit`, `search`,
`optimize`, `kill`, `twoproc`, `syncwrap`, `vindex`, `sindex`, `verify`.

At 1 000 000 rows (10.6 GB):

| What | Result |
| - | - |
| Initial load | data 31 s; IVF_PQ 40 s (IVF_SQ 17 s); FTS 1 s |
| Upsert of 10 rows (with BTree on `id`) | p50 31 ms, p95 61 ms; +0.12 MB on disk |
| Compaction (target 16 384) and index catch-up | 0.1 s + 1.5 s; only new fragments written |
| Vector search `k` 300, IVF_PQ, refine 10 | 58–101 ms, recall 1.00, 0.9 GB per process |
| Vector search `k` 300, IVF_SQ | 53–62 ms, recall 0.99, 2.4 GB per process |
| Vector search with an SQL prefilter | 70–140 ms |
| BM25 `k` 300 | 12–27 ms |
| SIGKILL during upserts (8 rounds, on 50k rows) | 8 of 8: opens, counts, searches |
| Reader and writer in two processes (30 s) | 66 versions seen, 0 errors |
| Upserts of existing rows, before and after optimize | every row updated, none duplicated |

Not measured: real embeddings (the data is synthetic), BM25 quality
against Tantivy, phrase and fuzzy search, Windows, 10M rows.

---

## 7. Out of scope

- The anchor consumer's move to `0.23`, in its own repository: its
  adapter's search call, `wait_indexed` replaced by the write's token,
  `index_status` and `reindex` around a model change.
- Carrying vectors or any Index over from `0.22` (D2).
- Multivector columns, phrase and fuzzy search, object-store (S3)
  datasets, GPU indexing.

---

## 8. The contract

Written in L0, 19 October 2026: every change this work order makes to
`api/scheda/v1`, as a diff against the protos of `main` (§8.5), with the
decisions it needs (§8.1). *Status:* C1–C21 approved by the owner on
19 October 2026, C18 changed by him (§8.4). C22–C35 (§8.2), found while
checking the implementation behind them against the sources of Lance 13,
and his own, were decided with him the same day. The owner looked at the
protos in `api/scheda/v1`; they wait in `.claude/notes/l0-protos.patch`
for L5.

How the diff was checked: the protos of `main` were copied out of the tree,
edited there, and run through `protoc` with the plugins `bazel run
//api:generate` uses (`protoc-gen-buffa` with `json=true`,
`protoc-gen-protovalidate-buffa`). They compile; the generated `Validate`
holds every new rule (`index_column.vector_text`, `index_column.fts_text`,
`index_column.name.not_reserved` with its pattern, `filter.range.bounded`,
`filter.value.number.finite`, the `order_by.unique_columns` of both
queries, the bounds of `filter_max_depth`), the CEL compiled to Rust, the
enum comparison included. The `oneof` of `Filter` becomes
`filter::Kind::{AllOf, AnyOf, NoneOf, Term, …}`. Not checked: `buf lint` (`STANDARD`), which
runs under `bazel test //api:lint` once L5 writes the protos, and a test
that exercises the new rules.

### 8.1 Proposals

Each with a recommendation; an item the owner does not mention is accepted
(`owner-review.md`).

**C1. One filter for every collection in scope.** `VaultQuery.filter`,
and `VaultSearch` through its `DocumentSearch.filter`, apply one filter to
each collection in scope. In a collection that does not declare a column
the filter names, each Document has no value there (made exact by C29c);
a column no collection in scope declares is `INVALID_ARGUMENT`, so a typo
is still caught. *Alternative:* one filter per collection, or strict (every
collection in scope must declare every column named). *Recommendation:*
one filter, as written.

**C2. Paging by `start_after`.** D15 drops `offset` from queries (C33
brings it back, to jump to a page); stable pages go by `start_after`. In the default order of a `standard` or
`singleton` collection the id need not still be in the Index, as in
`0.22`. In any other order (`order_by`, a journal) the query finds that
Document in the same snapshot to learn where it stood; one no longer there
is `FAILED_PRECONDITION` (start again), because a query returns no column
values the caller could pass back instead. Today a journal line not in the
Index ends the paging with nothing (`queries.rs`), which a caller cannot
tell from the end; the code says so instead. The paging condition is a
DataFusion expression joined to the filter (C29). `DocumentSearch.offset`
stays. *Recommendation:* accept.

**C3. `order_by`.** A list of `OrderBy { column, descending }`, then the
default order as the tie-break (`id`; a journal `order`), so the order is
total. A Document without a value comes last in both directions. A `text
list` cannot be a key; `order` can, in a journal (newest first is
`{"column": "order", "descending": true}`). In a query of the Vault: the
keys, then the collection name, then the default order; a key of one kind
in one collection and of another in a second is `INVALID_ARGUMENT`. A key
named twice is refused by CEL. *Recommendation:* accept.

**C4. What a filter may name.** `id` and the columns the Projector
declares. Not `file`, not `order` (C28), and not Lance's own columns
(`_rowid`, `_distance`, `_score`): the library checks the column of each
leaf against the Projector's columns. A column the Projector declares that
the Index does not hold yet for a collection (one the Index has not
reached, a rebuild for new columns that runs) has no value there (C29c),
and is not `INVALID_ARGUMENT` (`p8-follow-ups.md`, L5). *Recommendation:*
accept.

**C5. `find` becomes `scalar_index`.** With a real column per value,
every column can be named in a filter; the mark now asks for a scalar
index on the column (Lance's B-tree, bitmap, or label list, by kind: L2
picks, as constants). It takes `find`'s number; the new name makes a
caller's `find: true` stop compiling instead of meaning something else.
*Alternative:* keep the name `find` with the new meaning; or index every
column (a B-tree over a long text, such as the anchor consumer's `text`,
is waste). *Recommendation:* rename.

**C6. The kinds of `vector` and `fts`, in the proto.** Message CEL on
`IndexColumn`: `vector` only on `text` (D9), `fts` only on `text` or `text
list`. The Projector's reply already passes through `stored::validate`
(`ports.rs`, on `Columns` and `Project`), so the rule needs no Rust check.
Lance's inverted index takes `List<Utf8>` (`inverted/builder.rs` in
`lance-index` 13), so `fts` on a `text list` stands. *Recommendation:*
accept.

**C7. Column names.** `^[a-z][a-z0-9_]*$`, and not `id`, `order`, `file`,
or `collection`. Lower case without punctuation: plain in a filter and as
a JSON key, and it can never collide with Lance's `_`-names. `collection` stays reserved for
a later column of the collection name in a query of the Vault. The
derived columns of D8 take names no caller column can (C27).
*Recommendation:* accept.

**C8. `IndexValue.null` goes; what `IndexDoc` takes.** In Lance a missing
value and a null are the same SQL `NULL` (`IS NULL` matches both), so the
arm is removed. `IndexDoc` states what `0.22` does and adds
one rule: a key no column declares is ignored (`0.22` reads the declared
columns only); a value of another kind than its column's leaves the
Document out, as `PROJECTOR_ERROR_CODE_REFUSED` does (an Arrow column
holds one type). *Recommendation:* accept.

**C9. Writes return the state token.** `Collection.Create`, `Update`,
`Upsert`, and `Delete` return `DocumentWriteResult { document, version }`
(`document` unset after `Delete`); `DocumentBatchResult` gets `version`.
The name is `IndexState.version`'s, so a caller passes `not_before =
reply.version`. A call that stored nothing (`keep_existing`, the same
bytes, `ignore_missing`) returns the token of the state it found, which
holds the Document as returned. Set in every build. *Alternative:*
`Document.version`, set by writes only: three signatures stay, but reads,
queries, and searches carry a field they leave unset, and `Delete` changes
anyway. *Recommendation:* the new message.

**C10. No token on collection writes or the mirror.**
`CreateCollection`, `UpdateCollection`, `DeleteCollection`,
`BatchCollections`, `Mirror.Pull`, and `Mirror.Sync` keep their replies;
after them a caller takes `Vault.Info.version` (C13). *Recommendation:*
accept; a token there is an additive change later.

**C11. `not_before`.** On `DocumentQuery`, `VaultQuery`, and
`DocumentSearch` (so on counts and on searches of the Vault too). The call
waits until the Index reflects a state that contains the token: each commit
the token names is the Index's own or an ancestor of it. A token whose
commit is no longer in the history of `HEAD` does not wait (D5). A
malformed token is `INVALID_ARGUMENT`, checked by the library: the token
stays opaque, so the proto gives it no pattern. `Vault.Close` ends a wait
at once with `FAILED_PRECONDITION`, so a close never waits out a deadline.
*Recommendation:* accept.

**C12. The deadline and its code.** `OpenOptions.index_wait_timeout_ms`
(10), unset 60 000, as `write_timeout_ms` and the CLI's 60 s. Past it the
call is `VAULT_ERROR_CODE_DEADLINE_EXCEEDED` (20), retryable: the gRPC name
of that meaning (`ABORTED` is contention, `UNAVAILABLE` a dependency that
cannot be reached). *Alternative:* a deadline per request; additive later.
*Recommendation:* accept.

**C13. Telling the Index is behind.** `IndexState.head` (7): the token of
`HEAD` now, set in every Vault, whether it indexes or not; the Index is
behind while `version` differs from it. `pending` becomes unset in a Vault
that does not index, where today it is `0` and reads as "caught up"
(`index-risks.md` item 4). `VaultInfo.version` (4): the token of `HEAD` now,
for a caller that must see every write made on the path so far, by any
process or `git` command. *Recommendation:* accept.

**C14. The rebuild beside (D4) in the state.** `IndexState.version` stays
set during a rebuild (the live Index's state) and is unset only before the
first build ends. `Rebuild.cause` (6): `CAUSE_REINDEX`, `CAUSE_NEW` (no
Index this version reads: none yet, or one of `0.22`, removed per D2).
`STAGE_DONE` is the switch;
`STAGE_FAILED` keeps the live Index. `INDEX_REBUILDING` narrows to an
Index never built. *Recommendation:* accept.

**C15. Only `Reindex` changes the projection of the Index (D17
changed).** *Changed by the owner:* a rebuild is the caller's own decision,
and no Vault starts one by itself. A Vault whose Projector is of another
projection, or gives other columns, than the Index was built with is
`INDEX_STALE`, as in `0.22`: it does not index, and its queries and
searches fail, until an `Index.Reindex` of a Vault with that Projector has
switched the Index to it. The rebuild builds for the projection and
columns of the Vault that called it, and records them in the `commit` of
its generation. During the rebuild, the Vaults of the live Index's
projection answer from it; after the switch they are `INDEX_STALE`. So
several Vaults with different Projectors on one path cannot rebuild in
turn, and no claim file is needed. `Rebuild.cause` has no
`CAUSE_PROJECTION`.

**C16. The live Index during a rebuild: frozen (D4 changed).** *Changed
by the owner:* a rebuild is rare and the caller's own decision (a
`Reindex`, after a new Projector or a new model), and the cost of keeping
the live Index current beside it (two generations moving at once, and the old Projector
and embedder it would need) is more than a rebuild's few hours of new
writes waiting. So during every rebuild the live Index stays at its
`version`: queries and searches answer from it, the writes made meanwhile
appear at the switch, and a `not_before` past that version waits for the
switch (`DEADLINE_EXCEEDED` on a long rebuild). A Vault whose new
Projector or embedder the rebuild is for is `INDEX_STALE` until the switch
(C15, C34). The indexer does one job at a time.

**C17. One feature.** `FEATURE_FIND` and `FEATURE_SEARCH` go;
`FEATURE_INDEX` (1) and `FEATURE_ENCRYPT` (2) remain (D14). The comments
that named `find` or `search` name `index`. *Recommendation:* accept.

**C18. Removed fields are deleted, not reserved.** *Changed by the
owner:* the contract keeps no wire compatibility between versions, so a
removed field or enum value simply goes, and the fields left are numbered
again in the order they are read: `DocumentQuery`, `VaultQuery`,
`IndexColumn` (`scalar_index` takes `find`'s number), `Feature`. A stale
JSON key (the `0.22` tree's `all`, `phrase`, …) is still refused by the
strict parse of the CLI.

**C19. Left as they are, on purpose.** `SearchSummary.VectorLeg`: its
reasons hold, less `MODEL_MISMATCH` (C34) (L6 decides them from the Projector's columns, as the comments
already say). `Schema.exclude_lexical_index` and `exclude_vector_index`:
the same meaning, worded without "the lexical Index", so a file on disk
keeps its sense and the `VaultMarker` generation does not change.
`DocumentSearch.k` and `offset`, `CollectionNames`, `Count`, `marker.proto`,
`mirror.proto`. `SearchHit` gains the definitions of its scores (BM25;
`1 / (1 + d)` for the cosine distance `d`, as `0.22` computes it in
`search.rs`). *Recommendation:* accept.

**C20. The CLI takes the new fields in its JSON (§8.3).** No flag per
request field: `--query` and `--search` already take the whole message, so
L9's "`--where`, `--order-by`, `--not-before`" becomes the JSON keys
`filter`, `orderBy`, `notBefore`, `offset`. Two new global flags,
`--index-wait-timeout-ms` and `--filter-max-depth`. The CLI's wait before a read becomes
`notBefore` = the Vault's `version` at open. *Recommendation:* accept; L9
follows this list.

**C21. Release order.** `0.22.0` is not released, and `## [Unreleased]`
still holds its notes. *Proposal:* release `0.22.0` from `main` before
`lance-index` merges, so a consumer can take the proto contract without
the Index move, and `0.23.0` lists this work order alone. *Alternative:*
one release of both. *Recommendation:* release `0.22.0` first.

### 8.2 Found while checking the implementation

The owner asked for no workaround and no race behind the contract, and to
be told of one before work starts. Each item below names a race or a trap
in a decision already taken, what the sources of Lance 13 show
(`~/.cargo/registry/src/…/lance-13.0.0` and its crates, read
19 October 2026), and the change that removes it. They touch D3, D4, D7,
D8, D11, D15, D17, and L4; the proto diff changes in C28, C29, and C33.

**C22. Readers open the versions `commit` names (D7 changed).** As D7
stands, a reader opens each dataset at its latest version. While a round
runs, one collection is then at the new state and another at the old, so a
query of the Vault mixes two states, and `IndexState.version` ("every
collection is at this state") is not true. D7's fallback, the state in a
commit of its own after the data, duplicates journal lines after a kill
between the two, because an append is not idempotent. *Proposal:* `commit`
holds the state and the Lance version of every dataset of the generation.
A reader opens exactly those versions (`Dataset::checkout_version`), so a
call sees one state across all collections. A round first restores a
dataset that is ahead of `commit` to the version `commit` names
(`Dataset::restore`, under `index.lock`), then applies the difference from
that one state. After maintenance (D11), `commit` is written again with the
new versions of the same state. `transaction_properties` are no longer
needed, and L2's first check goes. *Recommendation:* accept.

**C23. A version is pruned an hour after it was replaced, not an hour after
it was made (D11 changed).** `cleanup_old_versions(older_than)` removes
every version whose own creation time is older than the cutoff, except the
latest (`CleanupPolicy::should_clean`: `manifest.timestamp() <
before_timestamp`). A version made three hours ago and replaced a second
ago goes at once, and with it the fragments a compaction has just
replaced, under a reader that still reads it, here or in another process.
*Proposal:* the cutoff is the creation time of the newest version made
more than an hour ago, so a version goes only once the one after it is an
hour old: every version lives at least an hour after it stopped being the
latest. Lance's own auto-cleanup is off
for a dataset made with default `WriteParams` (`auto_cleanup: None`) and
stays off. *Recommendation:* accept.

**C24. No read of the Index outlives one call.** A query takes, at its
start and from one snapshot, the total of its matches and the keys of its
page (C33), and then reads the Documents from git as the stream reaches
them, as today; a search already computes its ranking whole. The hour of C23 is then a margin over
the seconds a read takes, not a guess about how slowly a caller walks a
stream. The cost: a query holds the keys of `offset` + `limit` matches in
memory (all of them without `limit`), tens of bytes each. It costs no extra
read: Lance has no read in key order, so even a first page sees every
match. *Recommendation:* accept.

**C25. The old generation is deleted an hour after the switch (L4
changed).** L4's "Unix: at once" is a race: Lance opens data files as it
reads them, so a reader in another process that has the manifest of the
old generation, and not yet a fragment, would lose it. *Proposal:* the rule
of C23 on every OS; on Windows a file still open is tried again later.
Generation names are unique (not a counter that two processes could both
take). *Recommendation:* accept.

**C26. A new root for the Index.** *Rejected by the owner:* `0.23` is
not deployed anywhere, and no Vault will see `0.22` and `0.23` processes
at once; every Vault is rebuilt on `0.23`. D2 stays as written.

**C27. The derived columns are `_vector_{name}` and `_digest_{name}` (D8
changed).** Lance reads a `.` in a column name as a path into a struct
(`parse_field_path` in `lance-core`), so `{name}.vector` would need
backticks in every call that names it. A caller's column cannot start with
`_` (C7), so these names never collide with one. *Recommendation:* accept.

**C28. A journal's `order` is a text key (D8 changed).** `0.22`'s
`order` is a text that compares as bytes the way lines are appended: the
segment's class, date, sequence, writer, and name, then the line
(`journal_order` in `lexical.rs`). It does not fit an Int64 without losing
the writer or colliding, and a counter of the indexer instead would make a
rebuild reorder a journal and move a `start_after` across the switch.
*Proposal:* `order` is Utf8, that key, derived from the stored state alone.
It is a key of `order_by` and not a column of a filter (its values mean
nothing to a caller); the comment of `Filter` says so. *Recommendation:*
accept.

**C29. A filter is a typed tree, not SQL (D3 and D15 changed).** *From
the owner,* in place of the SQL predicate: SQL a caller writes and the
library hands to the engine invites injection, and reading it safely takes
a parser of our own (Lance's filter parser pastes the text into `SELECT 1
FROM t WHERE {filter}` and silently drops a tail such as `LIMIT 0`).

- *(a) The tree.* `Filter` is filled by the caller: `all_of`, `any_of`,
  `none_of` (lists of at least one), `term` (equals; on a `text list`,
  holds), `terms` (equals one of; holds one of), `range` (two ends,
  inclusive or not, at least one set; not on a `boolean` or a `text
  list`), `exists`, `prefix` (a `text` column or `id`). A value is a
  `Filter.Value` (text, integer, finite number, boolean) of the column's
  kind, which the library checks against the Projector's columns. The
  names keep clear of the keywords of Rust and Python (`in`, `and`, `or`,
  `not`) and of a variant `None` in Rust.
- *(b) No SQL text.* The library turns the tree into a DataFusion `Expr`
  itself (`col("role").eq(lit("writer"))`) and runs it with
  `Scanner::filter_expr`: no parser, nothing to escape, nothing to inject.
  The paging condition of C2 and C3 joins it as one more `Expr`. L5 checks
  that Lance uses a scalar index for each kind of leaf given this way.
- *(c) Two-valued logic, as in `0.22`.* A condition on a Document without
  a value is false, and `none_of` it is true (built with `IS NOT TRUE`),
  where SQL's `NOT` would drop that Document as unknown. A column a
  collection does not declare, or that the Index does not hold yet, is a
  column without a value for each of its Documents. This makes C1 and C4
  exact: in a query of the Vault, a collection without the column answers
  `none_of` with its Documents, and any other condition on it with none.
- *(d) Depth.* `OpenOptions.filter_max_depth`: unset 32, at most 100 (a
  proto rule); deeper is `INVALID_ARGUMENT`. L5 runs a filter of depth 100
  through validation, the `Expr`, and Lance's planning and scan on a 2 MiB
  thread stack; if it fails, the owner is told and the ceiling comes down.
  JSON input (the CLI, the Python driver) stops sooner, at about 40
  levels, with `INVALID_ARGUMENT`: `serde_json` stops at 128 levels of
  nesting, a level of `Filter` is three (`{"all_of": {"filters": [ … ]}}`),
  and that limit stays on.

**C30. A switch never moves `version` back.** *Dropped:* with the live
Index frozen (C16), the new one only moves away from it, so no rule is
needed; L4 still tests that `version` after the switch contains the one
before.

**C31. The projection claim under a short lock.** *Dropped:* with C15
as changed, there is no claim.

**C32. A token comes from one commit.** A write's `version` is made under
the write lock from the commit it made; a call that wrote nothing takes it
from the commit it read. `head` reads the two repositories one after the
other, so it may pair commits that were never `HEAD` together. That is
harmless, because a token is compared repository by repository (C11), but
`version == head` means only "caught up", nothing more. *Recommendation:*
accept; L7 says so.

Checked and fine: Lance's vector search and full-text search read the
fragments their indices do not cover yet, and rows changed since, unless
`fast_search` is set (`scanner.rs`: off by default), so a write is found
by a search with its token before D11 brings the indices over it, and D5
holds for searches. Scheda never sets `fast_search`; L6 tests it.

**C33. A query reports its total, and `offset` returns.** *From the
owner.* Since a query sees every match anyway (C24), counting them is free.
`Collection.Query` and `Vault.QueryDocuments` stream `QueryItem`: first a
`QuerySummary { total }`, then the Documents of the page. `total` is how
many Documents the filter matches in scope, whatever `offset`, `limit`,
and `start_after` say, counted in the same snapshot as the page, so a page
and its total always agree (two calls, `Count` and `Query`, can disagree).
`Count` and `CountDocuments` give the same number without the Documents.
`offset` comes back beside `start_after`, as in `0.22`, to jump to a page:
it skips matches after `start_after` when that is set; a Document added
meanwhile shifts the pages after it, which `start_after` does not. A search
gets no total: its vector leg always finds `k`. *Recommendation:* accept.

**C34. Another embedder model makes the Index stale (D17 changed).**
*From the owner,* as C15 for the projection: the vectors of the Index are
of one model, and a Vault whose embedder is of another model or vector
length is `INDEX_STALE` until its `Index.Reindex` has embedded every text
with it and switched. The `0.22` half-mode goes: a Vault that indexes all
but the vectors and searches without the vector leg, and the Documents it
indexed meanwhile, which the Index then holds without vectors.
`IndexState.model_mismatch` and `VECTOR_LEG_MODEL_MISMATCH` are removed; a
caller learns it from `IndexState.last_error` or from the code of its
query. A Vault with no embedder is not stale: it cannot index a collection
with a `vector` column, and its searches run without the vector leg
(`VECTOR_LEG_NO_EMBEDDER`), as in `0.22`. *Recommendation:* accept.

**C35. Catching up without the old commit (D17 changed).** `0.22`
rebuilds the Index by itself when the commit it stands on is gone from its
repository (`indexer.rs`), since no difference can be taken from it. On
Lance that would embed every text again, by no decision of the caller, and
for an isolated collection it is an ordinary event: its repository "may be
lost or rebuilt". *Proposal:* each collection of a generation keeps a small
table of the files the Index holds and the git blob id of each (one row
per file, so an append to a journal segment changes one row, not the rows
of all its lines), written in the same round and named in `commit` like
the datasets (C22). A round takes the difference of two commits as today:
the fast path. When the old commit, or any object that difference needs, is
gone, the round compares the tree of `HEAD` with that table instead and
applies the difference: the general path, for that repository's
collections only. Unchanged texts keep their vectors by their digest (D8).
Both paths run in the round, under `index.lock`, from the same snapshot of
`HEAD`. The Index then rebuilds by itself only when there is none (D2).
L3 tests that after any changes both paths give the same Index.
*Recommendation:* accept.

### 8.3 The CLI and the Python driver

What follows from the messages above (`14`), applied in L9:

- `query`, `count`, `query-documents`, `count-documents`: the JSON is
  `DocumentQuery` or `VaultQuery`: `filter` (the tree of C29), `orderBy`
  (`[{"column": …, "descending": …}]`), `offset`, `limit`, `startAfter`,
  `includeRaw`, `notBefore`. A key of the `0.22` tree is
  `INVALID_ARGUMENT`. `query` and `query-documents` print the summary
  first (`{"summary": {"total": "N"}}`), then one `{"document": …}` per
  line, as `search` does.
- `search`, `search-documents`: `filter` takes the new tree; `notBefore`
  is new.
- `create`, `update`, `upsert`, `delete`: print `DocumentWriteResult`
  (`{"document": {…}, "version": "…"}`; `delete` prints `{"version": "…"}`)
  instead of the Document or `{}`. `batch` and `batch-documents` print
  `DocumentBatchResult` with `version`.
- `info` prints `version`; `scheda-info` lists `FEATURE_INDEX` in place of
  `FEATURE_FIND` and `FEATURE_SEARCH`.
- `index status` and `index watch` print `head` and `rebuild.cause`;
  `--until-settled` stops when `version` equals `head` and no rebuild runs,
  or when `lastError` is set.
- The wait before a query, a count, or a search: today up to 60 s on
  `Index.Watch` for a settled state, then the call runs on the Index as it
  is. It becomes `notBefore` = `Vault.Info.version` taken at open, unless
  the JSON sets its own; past the deadline the call fails with
  `DEADLINE_EXCEEDED` instead of answering from an Index that has not
  reached the write. `--no-wait` sends no `notBefore`.
- New global flags `--index-wait-timeout-ms N` and `--filter-max-depth N`
  (`OpenOptions.index_wait_timeout_ms`, `filter_max_depth`), as
  `--write-timeout-ms`.
- The Python driver: `find --filter` takes the new tree (default: none);
  `total` comes from the summary of the query instead of a second call.

### 8.4 Review

19 October 2026, the owner: C1–C21 accepted as written, except C18 (no
`reserved`: the contract keeps no wire compatibility, and the anchor
consumer moves a long way in any case). The anchor consumer's move is set
aside for now. No workaround and no race behind the contract: one that is
found is told to him before work starts (§8.2). C22–C25, and C33
(his own), accepted; C26 rejected; C27 and C28 accepted. C29 changed by
him: a typed `Filter` tree in place of SQL (D3, D15), with (a)–(d) as
proposed and the depth an option (`filter_max_depth`, 32, at most 100),
accepted. C16 changed by him: the live Index is frozen during every
rebuild (D4), which drops C30. C15 changed by him: only `Reindex` changes
the projection of the Index, no Vault rebuilds by itself (D17), which
drops C31. C34, his own: an embedder of another model makes the Index
stale until a `Reindex`, accepted. C35 accepted, with the table of files
in place of a blob id on each row. C32 accepted. Every item is decided;
the protos were written into `api/scheda/v1` for him to look at, and he
did; they wait in `.claude/notes/l0-protos.patch` for L5.

After L1, the owner moved Scheda to tokio as a whole (`22`, approved the
same day), which replaces D13. Discussed before that and not taken: a
private runtime of Scheda's for Lance, a bridge into it from the blocking
objects, and the indexer kept a thread. He also moved L8 (the old Index
removed) to right after L1.

Before L8, the same day, the owner chose the literal build without
`find`: `Vault.Index` and every port are `UNIMPLEMENTED` until L3, so
`tests/consumer.rs` goes whole until L9, and the CLI loses its Projector
and its `index` subcommand until L9 but keeps the embedder and reranker
flags. The Check's grep covers `rust/scheda` and `rust/cli` only
(`rust/api/gen` follows `api/`), and L8 has the gate of L1, without the
Rust test suites.

### 8.5 The diff

Against `main` (`api/scheda/v1` on `main` and on `lance-index` are the
same today). `query.proto` is rewritten (the Tantivy tree of `0.22` gives
way to the `Filter` of C29), so it is shown whole after the diff of the
other files.

```diff
diff --git a/api/scheda/v1/collection.proto b/api/scheda/v1/collection.proto
--- a/api/scheda/v1/collection.proto
+++ b/api/scheda/v1/collection.proto
@@ -50,26 +50,27 @@ service Collection {
   // Writes.
 
-  // Creates a Document and returns it as stored, with its revision. In a
-  // journal, appends a line. Present, `ALREADY_EXISTS`, or, with
-  // `keep_existing`, the Document already stored is returned and nothing
-  // is written.
-  rpc Create(DocumentCreation) returns (Document);
-
-  // Replaces an existing Document and returns it as stored. Absent,
-  // `NOT_FOUND`. With `revision`, the stored revision must be that one, or
-  // `FAILED_PRECONDITION`. A journal, `UNIMPLEMENTED`. The same bytes store
-  // nothing and keep the revision.
-  rpc Update(DocumentUpdate) returns (Document);
+  // Creates a Document and returns it as stored, with its revision, and
+  // the state token of the write. In a journal, appends a line. Present,
+  // `ALREADY_EXISTS`, or, with `keep_existing`, the Document already stored
+  // is returned and nothing is written.
+  rpc Create(DocumentCreation) returns (DocumentWriteResult);
+
+  // Replaces an existing Document and returns it as stored, and the state
+  // token of the write. Absent, `NOT_FOUND`. With `revision`, the stored
+  // revision must be that one, or `FAILED_PRECONDITION`. A journal,
+  // `UNIMPLEMENTED`. The same bytes store nothing and keep the revision.
+  rpc Update(DocumentUpdate) returns (DocumentWriteResult);
 
   // Creates the Document, or replaces it when present, and returns it as
-  // stored. A journal, `UNIMPLEMENTED`. The same bytes store nothing and
-  // keep the revision.
-  rpc Upsert(DocumentUpsert) returns (Document);
+  // stored, and the state token of the write. A journal, `UNIMPLEMENTED`.
+  // The same bytes store nothing and keep the revision.
+  rpc Upsert(DocumentUpsert) returns (DocumentWriteResult);
 
-  // Deletes a Document. Absent, `NOT_FOUND`, or nothing with
-  // `ignore_missing`. With `revision`, the stored revision must be that
-  // one, or `FAILED_PRECONDITION`. A journal, `UNIMPLEMENTED`: a journal
-  // line never leaves.
-  rpc Delete(DocumentDeletion) returns (google.protobuf.Empty);
+  // Deletes a Document and returns the state token of the write, with no
+  // `document`. Absent, `NOT_FOUND`, or nothing with `ignore_missing`. With
+  // `revision`, the stored revision must be that one, or
+  // `FAILED_PRECONDITION`. A journal, `UNIMPLEMENTED`: a journal line never
+  // leaves.
+  rpc Delete(DocumentDeletion) returns (DocumentWriteResult);
 
   // Several Document writes as one: all happen, or none. An `Err` item, a
@@ -81,14 +82,20 @@ service Collection {
   // The Index.
 
-  // The Documents that match a condition: the exact set, in a stable
-  // order (`id`; a journal in append order), without scores. A lazy
-  // stream, read from one snapshot of the Index, so nothing written
-  // meanwhile moves it. An Index not built yet, `INDEX_REBUILDING`; one
-  // built by another Projector, `INDEX_STALE`. A build without `find`,
+  // The Documents that match a filter (`DocumentQuery.filter`): the
+  // exact set, in the order of the query (by default `id`; a journal in
+  // append order), without scores. The first item is a summary, with how
+  // many Documents match; then the Documents of the page. With
+  // `not_before`, it first waits for the Index to reach that state. The
+  // total and the keys of the page come from one snapshot of the Index,
+  // taken when the call starts, so nothing written meanwhile moves them;
+  // the Documents are read as the stream reaches them. An Index never
+  // built, `INDEX_REBUILDING`; one built for another Projector or embedder
+  // than this Vault's, `INDEX_STALE`. A build without `index`,
   // `UNIMPLEMENTED`.
-  rpc Query(DocumentQuery) returns (stream Document);
+  rpc Query(DocumentQuery) returns (stream QueryItem);
 
-  // How many Documents `Query` would give for the same query, counted in
-  // the Index without opening a file. The codes of `Query`.
+  // How many Documents the filter matches: the `QuerySummary.total` of
+  // `Query` for the same query, counted in the Index without opening a
+  // file. The codes of `Query`.
   rpc Count(DocumentQuery) returns (scheda.v1.Count);
 
@@ -99,6 +106,8 @@ service Collection {
   // computed whole, the Documents are read as the stream reaches them. A
   // ranking is computed again by each call, so pages from two calls need
-  // not agree. An Index not built yet, `INDEX_REBUILDING`. A build without
-  // `search`, `UNIMPLEMENTED`.
+  // not agree. With `not_before`, it first waits for the Index to reach
+  // that state. An Index never built, `INDEX_REBUILDING`; one built for
+  // another Projector or embedder than this Vault's, `INDEX_STALE`. A build
+  // without `index`, `UNIMPLEMENTED`.
   rpc Search(DocumentSearch) returns (stream SearchItem);
 }
@@ -153,9 +162,12 @@ message Schema {
   // it.
   bool encrypted = 5;
-  // No Document of this collection enters the lexical Index; a query that
-  // names it is refused, not answered empty. The journal id map is still
-  // kept. Redundant, and allowed, on `encrypted`, which no Index holds.
+  // The Index holds none of the columns of this collection, only its
+  // identity and, unless `exclude_vector_index`, its vectors: a query of
+  // it, or a search of it with a `filter`, is refused (`UNIMPLEMENTED`), not
+  // answered empty, and the lexical leg passes it by. The journal id map
+  // is still kept. Redundant, and allowed, on `encrypted`, which no Index
+  // holds.
   bool exclude_lexical_index = 6;
-  // No Document of this collection is embedded or enters the vector
+  // No Document of this collection is embedded or holds a vector in the
   // Index; the embedder is never called for it. Redundant, and allowed,
   // on `encrypted`.
diff --git a/api/scheda/v1/document.proto b/api/scheda/v1/document.proto
--- a/api/scheda/v1/document.proto
+++ b/api/scheda/v1/document.proto
@@ -208,7 +208,23 @@ message DocumentOperation {
 
 // The answer of `Collection.Batch` and `Vault.BatchDocuments`: one result
-// per operation, in order.
+// per operation, in order, and the state token of the commit they made.
 message DocumentBatchResult {
   repeated DocumentResult results = 1;
+  // The state token of the Vault once the batch is stored, as
+  // `DocumentWriteResult.version`.
+  string version = 2;
+}
+
+// The answer of `Collection.Create`, `Update`, `Upsert`, and `Delete`.
+message DocumentWriteResult {
+  // The Document as stored, with its revision. Unset after `Delete`.
+  Document document = 1;
+  // The state token of the Vault once the write is stored: pass it as
+  // `not_before` to a query, a count, or a search that must see this
+  // write. When the call stored nothing (`keep_existing`, the same bytes,
+  // `ignore_missing`), the token of the stored state it found, which holds
+  // the Document as returned. Set in every build, with the Index or
+  // without.
+  string version = 2;
 }
 
diff --git a/api/scheda/v1/error.proto b/api/scheda/v1/error.proto
--- a/api/scheda/v1/error.proto
+++ b/api/scheda/v1/error.proto
@@ -47,9 +47,14 @@ enum VaultErrorCode {
   // A file holds an unfinished merge. Resolve it by hand.
   VAULT_ERROR_CODE_UNRESOLVED_CONFLICT = 14;
-  // The Index has not been built yet, or is moving to a new format.
-  // Retryable soon; `Index.Watch` says when.
+  // The Index has not been built yet: its first build runs, and there is
+  // no Index to answer from (a new Vault, or an Index of an earlier version
+  // of Scheda, removed). Retryable soon; `Index.Watch` says when. A rebuild
+  // of a built Index never gives it.
   VAULT_ERROR_CODE_INDEX_REBUILDING = 15;
-  // The Index was built by a Projector of another projection, or with
-  // other columns, than this Vault's. `Index.Reindex`.
+  // The Index was built by a Projector of another projection, with other
+  // columns, or with an embedder of another model or vector length, than
+  // this Vault's. The Vault does not index, and its queries and searches
+  // fail with this code, until an `Index.Reindex` of a Vault with these
+  // ports has switched the Index to them.
   VAULT_ERROR_CODE_INDEX_STALE = 16;
   // The Index is damaged. `Index.Reindex`.
@@ -60,4 +65,8 @@ enum VaultErrorCode {
   // Exchanging writes with the mirror failed otherwise.
   VAULT_ERROR_CODE_REPLICATION = 19;
+  // A query, a count, or a search with `not_before`: the Index did not
+  // reach that state within `OpenOptions.index_wait_timeout_ms`. Retryable;
+  // `Index.Status` says where the Index stands.
+  VAULT_ERROR_CODE_DEADLINE_EXCEEDED = 20;
 }
 
diff --git a/api/scheda/v1/index.proto b/api/scheda/v1/index.proto
--- a/api/scheda/v1/index.proto
+++ b/api/scheda/v1/index.proto
@@ -4,6 +4,11 @@
 // follows the stored state of the Vault in the background: one open Vault
 // of all those on one path, in any process, indexes at a time; the others
-// follow it and take over when it ends. Versions here are opaque tokens:
-// equal tokens are the same stored state.
+// follow it and take over when it ends. A stored state of the Vault is
+// named by a state token: a write returns the token of the state it made,
+// `Vault.Info` the token of the stored state now, and `IndexState` the
+// token of the state the Index reflects. Equal tokens are the same stored
+// state; a token means nothing else to the caller. A query, a count, or a
+// search given a token (`not_before`) waits until the Index reflects a
+// state that contains it.
 
 edition = "2024";
@@ -15,5 +20,5 @@ import "protocontract/v1/options.proto";
 import "scheda/v1/error.proto";
 
-// The Index of one Vault, from `Vault.Index`. A build without `find` has
+// The Index of one Vault, from `Vault.Index`. A build without `index` has
 // none.
 service Index {
@@ -22,13 +27,19 @@ service Index {
 
   // Where the Index stands, now and at every change: the first item at
-  // once, then one item for each indexer round, for the Index catching up
-  // with the last write, and for a rebuild starting, moving on, and
-  // ending. The stream does not end by itself; the caller drops it when it
-  // has what it waited for.
+  // once, then one item whenever the state changes: an indexer round,
+  // `head` moving, a rebuild starting, moving on, and ending. A Vault that
+  // does not index sees the rounds of the one that does within
+  // `OpenOptions.index_poll_ms`. The stream does not end by itself; the
+  // caller drops it when it has what it waited for.
   rpc Watch(google.protobuf.Empty) returns (stream IndexState);
 
   // Starts a rebuild of the Index from the stored Documents, with the
   // ports of this Vault, and returns at once; `Watch` shows its progress
-  // and its result. A rebuild already running, `ABORTED`. A Vault that
+  // and its result. The new Index is built beside the live one, which
+  // answers queries and searches until the switch (`Rebuild`), and is of
+  // this Vault's projection, columns, and embedder model: after the
+  // switch, a Vault of other ones is `INDEX_STALE`. The only way any of
+  // them changes in the Index. A rebuild already running, `ABORTED`. A
+  // Vault that
   // lacks a port the Index needs (a Projector, or an embedder for a
   // `vector` column), `FAILED_PRECONDITION` naming it.
@@ -43,19 +54,18 @@ message IndexRef {
 // Where the Index stands.
 message IndexState {
-  // The stored state of the Vault the Index has reached: the state `HEAD`
-  // named, in the Vault and in its isolated repository, when the Index last
-  // finished a round. The Index has one such state for all its parts, and
-  // none while it is not built yet (before the first round ends, and while
-  // a rebuild runs). Equal tokens are the same state; the token means
-  // nothing else.
+  // The stored state of the Vault the Index reflects, as a state token: the
+  // state `HEAD` named, in the Vault and in its isolated repository, when
+  // the Index last finished a round. Every collection of the Index is at
+  // this state. Unset only before the first build of the Index ends; a
+  // rebuild leaves it at the state of the live Index until the switch.
   string version = 1;
-  // The files the Index has yet to read to reach the stored state of the
-  // Vault, while a round or a rebuild runs: the files of declared
-  // collections that changed since `version`, and every file of a
-  // collection whose declaration changed. It counts files, not Documents:
-  // a journal segment is one file however many lines it holds, so that
-  // the count needs no read of the segments. `0` when the Index is where
-  // this Vault last found the stored state; unset when the files could not be
-  // counted (`last_error` then says why).
+  // The files the Index has yet to read to reach `head`, while a round or a
+  // rebuild runs: the files of declared collections that changed since
+  // `version`, and every file of a collection being built again. It counts
+  // files, not Documents: a journal segment is one file however many lines
+  // it holds, so that the count needs no read of the segments. `0` when the
+  // Index is at `head`. Counted by the Vault that indexes (`role`); unset
+  // in the others, and when the files could not be counted (`last_error`
+  // then says why).
   uint64 pending = 2;
   // This Vault's indexer is the one indexing now. One indexer of all the
@@ -63,34 +73,48 @@ message IndexState {
   // and take over when it ends.
   bool role = 3;
-  // This Vault's embedder is of another model or length than the vectors
-  // of the Index: it writes no vector, and its searches run without the
-  // vector leg, rather than mix two models. `Reindex` moves the Index to
-  // it.
-  bool model_mismatch = 4;
   // The last rebuild this Vault's indexer ran, while it runs and after it
-  // ends: one `Reindex` asked for, or the building of an Index that did not
-  // exist or was made in another format. Unset when this Vault ran none.
-  Rebuild rebuild = 5;
+  // ends. Unset when this Vault ran none.
+  Rebuild rebuild = 4;
   // The failure of this Vault's last indexing round; unset after a round
   // that succeeds. A failed round changes no version and fails no write; the
-  // indexer tries it again after a pause. `INDEX_STALE` while the Index was
-  // built by a Projector of another projection than this Vault's: the Vault
-  // then does not index until its own `Reindex`.
-  ErrorReport last_error = 6;
+  // indexer tries it again after a pause. `INDEX_STALE` while the Index
+  // was built for another Projector or embedder than this Vault's: the
+  // Vault then does not index until a `Reindex`.
+  ErrorReport last_error = 5;
+  // The stored state of the Vault now, as a state token: what `HEAD` names
+  // in the Vault and in its isolated repository, read when this state was
+  // made. The Index is behind while `version` differs from it. Set in
+  // every Vault, whether it indexes or not.
+  string head = 6;
 }
 
-// A rebuild of the Index: the Index made again from nothing. `Reindex` asks
-// for one; an Index not built yet, or built in a format this version does
-// not read, is rebuilt the same way. While it runs the Index has no
-// `version`, and a query is `INDEX_REBUILDING`.
+// A rebuild of the Index: a new Index made from the stored Documents,
+// beside the live one. While it runs, queries and searches answer from the
+// live Index, which stays at its `version`: the writes made meanwhile
+// appear at the switch, and a `not_before` past that version waits for
+// it. At its end the Vault switches to the new Index in one step, and
+// removes the old one an hour later. An Index never built has no live
+// one: its queries and searches are `INDEX_REBUILDING`.
 message Rebuild {
+  // Why it runs.
+  enum Cause {
+    CAUSE_UNSPECIFIED = 0;
+    // `Index.Reindex` asked for it.
+    CAUSE_REINDEX = 1;
+    // There was no Index this version reads: none yet, or one of an
+    // earlier version of Scheda, which is removed.
+    CAUSE_NEW = 2;
+  }
+
   enum Stage {
     STAGE_UNSPECIFIED = 0;
     // It runs.
     STAGE_RUNNING = 1;
-    // It ended and the Index is built: `version` is set again.
+    // It ended with the switch: the new Index is the live one, and
+    // `version` is its state.
     STAGE_DONE = 2;
-    // It ended without building the Index; `error` says why. The indexer
-    // starts it again after a pause.
+    // It ended without the switch; `error` says why. The live Index, when
+    // there is one, stays. The indexer starts the rebuild again after a
+    // pause.
     STAGE_FAILED = 3;
   }
@@ -105,3 +129,5 @@ message Rebuild {
   // Why a failed rebuild failed.
   ErrorReport error = 5;
+  // Why it runs.
+  Cause cause = 6;
 }
diff --git a/api/scheda/v1/options.proto b/api/scheda/v1/options.proto
--- a/api/scheda/v1/options.proto
+++ b/api/scheda/v1/options.proto
@@ -69,4 +69,21 @@ message OpenOptions {
   // not the person whose git configuration it is.
   Signing signing = 9;
+  // How long a query, a count, or a search with `not_before` waits for the
+  // Index to reach that state, in milliseconds. Past it the call is
+  // `DEADLINE_EXCEEDED`; `Vault.Close` ends the wait at once, with
+  // `FAILED_PRECONDITION`. Unset: 60000.
+  uint32 index_wait_timeout_ms = 10 [(buf.validate.field) = {
+    uint32: {gte: 1}
+  }];
+  // How deep a `Filter` of this Vault's queries and searches may nest: a
+  // leaf is one level, each `all_of`, `any_of`, or `none_of` one more.
+  // Deeper, `INVALID_ARGUMENT`. Unset: 32. At most 100, a depth every step
+  // that walks a Filter is tested to take on a 2 MiB thread stack.
+  uint32 filter_max_depth = 11 [(buf.validate.field) = {
+    uint32: {
+      gte: 1
+      lte: 100
+    }
+  }];
 }
 
diff --git a/api/scheda/v1/ports.proto b/api/scheda/v1/ports.proto
--- a/api/scheda/v1/ports.proto
+++ b/api/scheda/v1/ports.proto
@@ -27,5 +27,6 @@ service Embedder {
   // The model and the length of its vectors. Read once, when the embedder
   // is installed. The model id is stored with the vectors, so an embedder
-  // of another model is seen, not mixed into the same Index.
+  // of another model is seen, not mixed into the same Index: the Vault is
+  // `INDEX_STALE` until its `Index.Reindex` embeds every text with it.
   rpc Describe(google.protobuf.Empty) returns (EmbedderDescriptor);
 
@@ -143,12 +144,13 @@ service Projector {
   // Which projection it makes. Read once, when the Projector is given to a
   // Vault. The projection is stored with the Index, so a Projector of
-  // another projection is seen, not mixed into the same Index: the Index is
-  // `INDEX_STALE` for that Vault, which does not index, until a `Reindex`
-  // by a Vault with the new Projector.
+  // another projection is seen, not mixed into the same Index: the Vault
+  // is `INDEX_STALE` until its `Index.Reindex` builds the Index for that
+  // projection. Only `Reindex` starts such a rebuild.
   rpc Describe(google.protobuf.Empty) returns (ProjectorDescriptor);
 
   // The columns of one collection. None: its Documents are indexed by
-  // identity only. Changing the columns of a collection makes the Index
-  // stale until `Reindex`.
+  // identity only. Columns other than the Index was built with make the
+  // Vault `INDEX_STALE`, as another projection does; change the
+  // projection with them.
   rpc Columns(CollectionKey) returns (IndexSchema);
 
@@ -194,13 +196,29 @@ enum ColumnKind {
 // One column of the Index.
 message IndexColumn {
-  // Its name in a `Filter`. Not empty, and not `id` or `collection`.
+  option (buf.validate.message).cel = {
+    id: "index_column.vector_text"
+    message: "a vector column must be text: one vector per Document"
+    expression: "!this.vector || this.kind == 1"
+  };
+  option (buf.validate.message).cel = {
+    id: "index_column.fts_text"
+    message: "an fts column must be text or text list"
+    expression: "!this.fts || this.kind == 1 || this.kind == 5"
+  };
+
+  // Its name in a `Filter` and in an order: a lowercase letter, then
+  // lowercase letters, digits, and `_`. Not `id`, `order`, `file`, or
+  // `collection`, which the Index keeps for itself.
   string name = 1 [(buf.validate.field) = {
     required: true
-    string: {min_len: 1}
+    string: {
+      min_len: 1
+      pattern: "^[a-z][a-z0-9_]*$"
+    }
     cel: [
       {
-        id: "index_column.name.not_identity"
-        message: "name must not be id or collection"
-        expression: "this != 'id' && this != 'collection'"
+        id: "index_column.name.not_reserved"
+        message: "name must not be id, order, file, or collection"
+        expression: "!(this in ['id', 'order', 'file', 'collection'])"
       }
     ]
@@ -213,9 +231,12 @@ message IndexColumn {
     }
   }];
-  // A `Filter` can name it.
-  bool find = 3;
-  // The lexical leg of a search reads it.
+  // The Index keeps a scalar index on it, so a `Filter` or an order on it
+  // reads that index instead of every row. A Filter may name any column;
+  // this only makes it fast.
+  bool scalar_index = 3;
+  // The lexical leg of a search reads it (BM25). A `text` or a `text
+  // list`.
   bool fts = 4;
-  // The vector leg embeds it, one vector per value.
+  // The vector leg embeds it: one vector per Document. A `text` only.
   bool vector = 5;
 }
@@ -236,6 +257,9 @@ message IndexSchema {
 
 // The values of the columns for one Document. A column left out has no
-// value; `IndexValue.null` is a stored null. Empty: the Document is
-// indexed by identity only.
+// value: a `Filter`'s condition on it is false. A key no column of the
+// collection declares is ignored. A value of another kind than its
+// column's leaves the Document out of the Index, as
+// `PROJECTOR_ERROR_CODE_REFUSED` does. Empty: the Document is indexed by
+// identity only.
 message IndexDoc {
   map<string, IndexValue> values = 1;
@@ -244,8 +268,5 @@ message IndexDoc {
 // One value of a column, of the column's kind.
 message IndexValue {
-  // A stored null.
-  message Null {}
-
-  // Several texts; each is a term of its own.
+  // Several texts, in order.
   message TextList {
     repeated string texts = 1;
@@ -260,5 +281,4 @@ message IndexValue {
     bool boolean = 4;
     TextList text_list = 5;
-    Null null = 6;
   }
 }
diff --git a/api/scheda/v1/scheda.proto b/api/scheda/v1/scheda.proto
--- a/api/scheda/v1/scheda.proto
+++ b/api/scheda/v1/scheda.proto
@@ -57,10 +57,9 @@ message SchedaInfo {
 enum Feature {
   FEATURE_UNSPECIFIED = 0;
-  // The Index, the queries and counts, and the `Index` object.
-  FEATURE_FIND = 1;
-  // The searches, the vector leg, and the embedder and reranker ports.
-  FEATURE_SEARCH = 2;
+  // The Index: the queries, counts, and searches, the `Index` object, and
+  // the Projector, embedder, and reranker ports.
+  FEATURE_INDEX = 1;
   // Encrypted collections.
-  FEATURE_ENCRYPT = 3;
+  FEATURE_ENCRYPT = 2;
 }
 
diff --git a/api/scheda/v1/vault.proto b/api/scheda/v1/vault.proto
--- a/api/scheda/v1/vault.proto
+++ b/api/scheda/v1/vault.proto
@@ -28,5 +28,7 @@ import "scheda/v1/query.proto";
 // Reads see stored state only. The Index follows the stored state in the
 // background: queries and searches answer from what it holds, and
-// `Index.Watch` tells how far it is.
+// `Index.Watch` tells how far it is. A write returns the state token of
+// the state it made; a query, a count, or a search given that token
+// (`not_before`) waits until the Index reflects it.
 //
 // After `Close`, every call of this Vault and of every object reached from
@@ -36,6 +38,7 @@ service Vault {
   // The Vault.
 
-  // What this Vault holds: its path, the Index path, and the writer
-  // identity of this clone. Reads no Document.
+  // What this Vault holds: its path, the Index path, the writer identity
+  // of this clone, and the state token of its stored state now. Reads no
+  // Document.
   rpc Info(google.protobuf.Empty) returns (VaultInfo);
 
@@ -58,5 +61,5 @@ service Vault {
   rpc Mirror(google.protobuf.Empty) returns (MirrorRef);
 
-  // The Index of the Vault. A build without `find`, `UNIMPLEMENTED`.
+  // The Index of the Vault. A build without `index`, `UNIMPLEMENTED`.
   rpc Index(google.protobuf.Empty) returns (IndexRef);
 
@@ -128,11 +131,11 @@ service Vault {
   rpc BatchDocuments(stream VaultDocumentOperation) returns (DocumentBatchResult);
 
-  // The Documents of several collections that match a condition, as
-  // `Collection.Query` within one; a leaf on a Projector column names its
-  // collection.
-  rpc QueryDocuments(VaultQuery) returns (stream Document);
+  // The Documents of several collections that match a filter, as
+  // `Collection.Query` within one; the filter applies to each collection
+  // in scope (`VaultQuery.filter`).
+  rpc QueryDocuments(VaultQuery) returns (stream QueryItem);
 
-  // How many Documents `QueryDocuments` would give for the same query. The
-  // codes of `QueryDocuments`.
+  // How many Documents the filter matches: the `QuerySummary.total` of
+  // `QueryDocuments` for the same query. The codes of `QueryDocuments`.
   rpc CountDocuments(VaultQuery) returns (Count);
 
@@ -156,4 +159,9 @@ message VaultInfo {
   // as generated into `{vault}/.scheda/writer`.
   string writer = 3;
+  // The state token of the stored state of the Vault now: what `HEAD`
+  // names in the Vault and in its isolated repository. Passed as
+  // `not_before`, it makes a query see every write stored on this path
+  // before this call, by any Vault, process, or `git` command.
+  string version = 4;
 }
 
```

`api/scheda/v1/query.proto`:

```protobuf
// Queries and searches over the Index.
//
// A query selects: the exact set of Documents a `Filter` matches, in a
// stable order, without scores. A search ranks: the Documents that best
// fit a text or a Document, by relevance, with scores. Both read the
// Index the Projector fills; the Filter is the same for both, and only a
// condition: it never changes a score. The library checks a Filter
// against the Projector's columns and runs it in the Index itself: a
// caller never writes the query language of the Index.

edition = "2024";

package scheda.v1;

import "buf/validate/validate.proto";
import "scheda/v1/document.proto";

// A condition on indexed Documents. Exactly one `kind`; `all_of`,
// `any_of`, and `none_of` nest them, at most
// `OpenOptions.filter_max_depth` deep (a leaf is one level, each list one
// more); deeper, `INVALID_ARGUMENT`.
//
// A leaf names `id` or a column the Projector declares for the collection
// (`IndexColumn`); not a journal's `order`. Its values are of the column's
// kind: a text, an integer, a number, a boolean; on a `text list`, a text.
// Another kind, `INVALID_ARGUMENT`. A condition is true or false, never
// unknown: on a Document without a value in its column it is false, and
// `none_of` it is true. A column the Index does not hold for a collection
// (the collection does not declare it, the Index has not reached the
// collection, a rebuild for new columns runs) is a column without a value
// for each of its Documents. A column no collection in scope declares is
// `INVALID_ARGUMENT`.
message Filter {
  // Conditions combined.
  message Filters {
    // At least one.
    repeated Filter filters = 1 [(buf.validate.field) = {
      repeated: {min_items: 1}
    }];
  }

  // A value in a condition, of the kind of its column.
  message Value {
    oneof kind {
      option (buf.validate.oneof).required = true;
      string text = 1;
      int64 integer = 2;
      // Finite.
      double number = 3 [(buf.validate.field) = {
        double: {finite: true}
        cel: [
          {
            id: "filter.value.number.finite"
            message: "must be finite"
            expression: "!this.isNan() && !this.isInf()"
          }
        ]
      }];
      bool boolean = 4;
    }
  }

  // Documents whose column equals the value; on a `text list`, that hold
  // it among their texts.
  message Term {
    // `id`, or a column the Projector declares.
    string column = 1 [(buf.validate.field) = {
      required: true
      string: {min_len: 1}
    }];
    Value value = 2 [(buf.validate.field) = {required: true}];
  }

  // Documents whose column equals one of the values; on a `text list`,
  // that hold one of them.
  message Terms {
    // `id`, or a column the Projector declares.
    string column = 1 [(buf.validate.field) = {
      required: true
      string: {min_len: 1}
    }];
    // At least one.
    repeated Value values = 2 [(buf.validate.field) = {
      repeated: {min_items: 1}
    }];
  }

  // One end of a `Range`.
  message Bound {
    Value value = 1 [(buf.validate.field) = {required: true}];
    // The end belongs to the range. Unset: it does not.
    bool inclusive = 2;
  }

  // Documents whose column lies between two ends. An unset end is open;
  // at least one is set. Texts, and `id`, compare as UTF-8 bytes. Not on a
  // `boolean` or a `text list` column.
  message Range {
    option (buf.validate.message).cel = {
      id: "filter.range.bounded"
      message: "a range needs at least one end"
      expression: "has(this.lower) || has(this.upper)"
    };

    // `id`, or a column the Projector declares.
    string column = 1 [(buf.validate.field) = {
      required: true
      string: {min_len: 1}
    }];
    Bound lower = 2;
    Bound upper = 3;
  }

  // Documents with a value in the column; for a `text list`, at least one
  // text.
  message Exists {
    // A column the Projector declares.
    string column = 1 [(buf.validate.field) = {
      required: true
      string: {min_len: 1}
    }];
  }

  // Documents whose `text` column, or `id`, begins with `prefix`. Not on a
  // `text list`.
  message Prefix {
    // `id`, or a `text` column the Projector declares.
    string column = 1 [(buf.validate.field) = {
      required: true
      string: {min_len: 1}
    }];
    string prefix = 2 [(buf.validate.field) = {
      required: true
      string: {min_len: 1}
    }];
  }

  oneof kind {
    option (buf.validate.oneof).required = true;
    // Every one of these.
    Filters all_of = 1;
    // At least one of these.
    Filters any_of = 2;
    // None of these.
    Filters none_of = 3;
    Term term = 4;
    Terms terms = 5;
    Range range = 6;
    Exists exists = 7;
    Prefix prefix = 8;
  }
}

// Several collections, for a query or a search of the Vault. Empty: every
// collection that is neither isolated nor encrypted. An isolated
// collection is searched only when named; an encrypted one, named, is
// `UNIMPLEMENTED`: no Index holds it.
message CollectionNames {
  repeated string names = 1 [(buf.validate.field) = {
    repeated: {
      items: {
        string: {min_len: 1}
      }
    }
  }];
}

// One key of the order of a query.
message OrderBy {
  // `id`, in a journal `order` (append order), or a column the Projector
  // declares that is not a `text list`. Another, `INVALID_ARGUMENT`.
  string column = 1 [(buf.validate.field) = {
    required: true
    string: {min_len: 1}
  }];
  // Largest first. Unset: smallest first. Either way, a Document without a
  // value comes after every Document with one.
  bool descending = 2;
}

// The request of `Collection.Query` and `Collection.Count`.
message DocumentQuery {
  // The condition. Unset: every Document.
  Filter filter = 1;
  // The order: these keys, then `id` (a journal: append order). Unset:
  // `id`; a journal in append order.
  repeated OrderBy order_by = 2 [(buf.validate.field) = {
    cel: [
      {
        id: "document_query.order_by.unique_columns"
        message: "a column must not be named twice"
        expression: "this.all(o, this.filter(other, other.column == o.column).size() == 1)"
      }
    ]
  }];
  // Skip this many matches first, after `start_after` when it is set: to
  // jump to a page. A Document added or removed meanwhile shifts the pages
  // that follow it; a page begun with `start_after` is not shifted.
  uint32 offset = 3;
  // Stop after this many. Unset: every match.
  uint32 limit = 4;
  // Begin after the Document with this id, in the order of the query. A
  // page that begins where the last one ended is not shifted by a Document
  // added meanwhile, and a deep page costs no more than the first. In the
  // default order of a `standard` or `singleton` collection the id need not
  // be in the Index. In any other order the query finds that Document in
  // the same snapshot to know where it stood; one no longer there is
  // `FAILED_PRECONDITION`: start again.
  string start_after = 5 [(buf.validate.field) = {
    string: {min_len: 1}
  }];
  // Fill `Document.raw`. Unset: keys and revisions only.
  bool include_raw = 6;
  // A state token: `version` of a write's reply, of `Vault.Info`, or of
  // `IndexState`. The call answers once the Index reflects a state that
  // contains it, waiting up to `OpenOptions.index_wait_timeout_ms`; past
  // it, `DEADLINE_EXCEEDED`. A token whose commit is no longer in the
  // history of `HEAD` (a reset, a pull that rewrote it) does not wait. A
  // token that is not one, `INVALID_ARGUMENT`. Unset: answer at once from
  // what the Index holds.
  string not_before = 7 [(buf.validate.field) = {
    string: {min_len: 1}
  }];
}

// The request of `Vault.QueryDocuments` and `Vault.CountDocuments`: a
// query of several collections.
message VaultQuery {
  // Where to look.
  CollectionNames collections = 1;
  // The condition, applied to each collection in scope (see `Filter` for a
  // column a collection does not declare). Unset: every Document.
  Filter filter = 2;
  // The order: these keys, then the collection name, then `id` (a journal:
  // append order). Unset: the collection name, then `id`; a journal in
  // append order. A key of one kind in one collection and of another in a
  // second, `INVALID_ARGUMENT`.
  repeated OrderBy order_by = 3 [(buf.validate.field) = {
    cel: [
      {
        id: "vault_query.order_by.unique_columns"
        message: "a column must not be named twice"
        expression: "this.all(o, this.filter(other, other.column == o.column).size() == 1)"
      }
    ]
  }];
  // As `DocumentQuery.offset`.
  uint32 offset = 4;
  // Stop after this many. Unset: every match.
  uint32 limit = 5;
  // Begin after this Document, in the order of the query, as
  // `DocumentQuery.start_after`.
  DocumentKey start_after = 6;
  // Fill `Document.raw`. Unset: keys and revisions only.
  bool include_raw = 7;
  // As `DocumentQuery.not_before`.
  string not_before = 8 [(buf.validate.field) = {
    string: {min_len: 1}
  }];
}

// The answer of `Count` and `CountDocuments`.
message Count {
  uint64 count = 1;
}

// One item of `Query` and `QueryDocuments`. The first is always the
// summary; the rest are the Documents of the page, in the order of the
// query.
message QueryItem {
  oneof item {
    QuerySummary summary = 1;
    Document document = 2;
  }
}

// What a query found, known before its Documents are read.
message QuerySummary {
  // How many Documents the filter matches in scope, whatever `offset`,
  // `limit`, and `start_after` say. Counted in the same snapshot of the
  // Index as the page that follows, so a page and its total always agree.
  uint64 total = 1;
}

// The request of `Collection.Search`; inside `VaultSearch`, of
// `Vault.SearchDocuments`.
message DocumentSearch {
  // What to rank by. Exactly one.
  oneof subject {
    option (buf.validate.oneof).required = true;
    // A text: the lexical leg matches its words, the vector leg embeds it
    // (`Embedder.EmbedQuery`).
    string text = 1 [(buf.validate.field) = {
      string: {min_len: 1}
    }];
    // A Document, of any collection: others like it. The lexical leg takes
    // its indexed text; the vector leg takes its stored vector and calls no
    // model.
    DocumentKey like = 2;
  }
  // Rank only the Documents this matches; both legs apply it before they
  // rank. Unset: all in scope.
  Filter filter = 3;
  // How many hits. Unset or 0: 10. No upper limit beyond the Index.
  uint32 k = 4;
  // Weight of the lexical leg. Unset: 1. Zero turns the leg off.
  double lexical_weight = 5 [(buf.validate.field) = {
    double: {
      gte: 0
      finite: true
    }
    cel: [
      {
        id: "document_search.lexical_weight.finite"
        message: "must be finite"
        expression: "!this.isNan() && !this.isInf()"
      }
    ]
  }];
  // Weight of the vector leg. Unset: 1. Zero turns the leg off.
  double vector_weight = 6 [(buf.validate.field) = {
    double: {
      gte: 0
      finite: true
    }
    cel: [
      {
        id: "document_search.vector_weight.finite"
        message: "must be finite"
        expression: "!this.isNan() && !this.isInf()"
      }
    ]
  }];
  // Rerank the first this many fused hits with the Vault's reranker; set
  // without one, `INVALID_ARGUMENT`. Below the effective `k`,
  // `INVALID_ARGUMENT`. Unset: no rerank.
  uint32 rerank_limit = 7 [(buf.validate.field) = {
    uint32: {gte: 1}
  }];
  // Fill the `raw` of each hit's Document.
  bool include_raw = 8;
  // Skip this many hits first; their files are not opened. A second
  // search ranks again, so its page need not follow the first one's:
  // pages that must agree come from one search with a large enough `k`.
  uint32 offset = 9;
  // As `DocumentQuery.not_before`.
  string not_before = 10 [(buf.validate.field) = {
    string: {min_len: 1}
  }];
}

// The request of `Vault.SearchDocuments`: a search of several
// collections.
message VaultSearch {
  // Where to look.
  CollectionNames collections = 1;
  // The search; its `filter` applies to each collection in scope, as
  // `VaultQuery.filter` does.
  DocumentSearch search = 2 [(buf.validate.field) = {required: true}];
}

// One item of `Search` and `SearchDocuments`. The first is always the
// summary; the rest are hits, best first.
message SearchItem {
  oneof item {
    SearchSummary summary = 1;
    SearchHit hit = 2;
  }
}

// What a search did with its legs, known from the Vault before it runs.
message SearchSummary {
  // Why the vector leg ran or did not: the first that applies, in this
  // order.
  enum VectorLeg {
    VECTOR_LEG_UNSPECIFIED = 0;
    // Not asked for: `vector_weight` is 0.
    VECTOR_LEG_NOT_ASKED = 1;
    // No collection in scope has a vector column: each is excluded or
    // encrypted, the Vault has no Projector, or the Projector marks no
    // column `vector`.
    VECTOR_LEG_NO_VECTOR_COLLECTION = 2;
    // The Vault has no embedder.
    VECTOR_LEG_NO_EMBEDDER = 3;
    // The Index holds no vector yet.
    VECTOR_LEG_NO_VECTORS = 4;
    // It ran.
    VECTOR_LEG_RAN = 5;
  }

  VectorLeg vector_leg = 1;
}

// One hit of a search.
message SearchHit {
  // The Document; `raw` only with `include_raw`.
  Document document = 1;
  // The score of the lexical leg, when that leg found it: BM25 over the
  // `fts` columns.
  double lexical_score = 2;
  // The score of the vector leg, when that leg found it: `1 / (1 + d)`,
  // with `d` the cosine distance of its closest vector, so 1 for the same
  // direction.
  double vector_score = 3;
  // The fused score (Reciprocal Rank Fusion of the legs that ran).
  double score = 4;
  // The reranker's score, when the search reranked; the order of the hits
  // is then this one.
  double rerank_score = 5;
}
```
