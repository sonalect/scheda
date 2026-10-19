# 21. The Index on Lance

Document 21 of the Scheda package. Version 0.1, 19 October 2026.

Status: approved by the owner on 19 October 2026. Decisions marked
*settled* were taken with him in discussion; the others were proposed
with a recommendation and accepted as written (`owner-review.md`). The
work goes on the branch `lance-index`, stage by stage (§3). The spike
(§6) stays on the branch `lance-spike` and never reaches `main`.

Work order: replace the two engines of the Index, Tantivy (lexical) and
USearch (vectors), with one: Lance. The Index keeps its place in Scheda
(derived from git, rebuilt from git, never the source of truth), its
indexer thread, and its ports (Projector, Embedder, Reranker). What
changes is how the Index stores, updates, and serves what it holds, and
the query surface, which becomes Lance's own (an SQL predicate) with no
bridge to the `Filter` tree.

Order: the contract first (L0: every proto change, reviewed by the owner
before any code). Inside each later stage: code and tests first, then
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

**D3. The query surface is Lance's own: an SQL predicate.** *Settled.*
`Collection.Query`, `Collection.Count`, `Vault.QueryDocuments`,
`Vault.CountDocuments`, and the prefilter of a search take a predicate in
Lance's SQL dialect (DataFusion) over the columns of the collection. The
`Filter` tree and its proto messages go; there is no translation layer.

**D4. A rebuild is built beside the live Index and switched to.**
*Settled.* `Index.Reindex` (and every rebuild the indexer starts by
itself) builds new datasets from the snapshot of `HEAD` it starts at,
then catches them up with the usual tree difference to the `HEAD` of the
moment, and switches to them under `index.lock` with one atomic write of a
small pointer file. Queries and searches keep answering from the live
datasets until the switch; the indexer keeps the live datasets current
meanwhile. `INDEX_REBUILDING` remains only for an Index never built.

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
predicate runs on real columns and kinds never clash between
collections. A call over several collections queries their datasets and
merges (as `Vault.QueryDocuments` and `Vault.SearchDocuments` merge
today). `{vault}/.scheda/index/current` names the live generation (D4).
*Alternative:* one dataset for the Vault with a `collection` column and
the union of all columns: rejected, the same column name may have
different kinds in two collections. *Recommendation:* per collection.

**D7. Each dataset carries the state it reflects, in the same commit.**
*Proposed.* Every Lance commit the indexer makes carries
`transaction_properties` `{"scheda.state": "main {oid} isolated {oid}"}`
(`WriteParams::with_transaction_properties`; read back with
`Dataset::read_transaction`). Data and state move in one atomic commit,
so a dataset can never hold data its state does not name. A round brings
each dataset of the generation to `HEAD` from its own state; when every
dataset has reached it, the round writes `index/{generation}/commit`
last, as `0.22` writes `index/commit`, and that is `IndexState.version`.
A dataset ahead of `commit` (a round died after it) is harmless: its next
round starts from its own state. If `merge_insert` cannot carry
transaction properties (L2 checks it), the state goes into a separate
commit right after the data; upserts by `id` are idempotent, so a state
behind its data only makes the next round apply the same rows again.
*Recommendation:* accept.

**D8. Columns.** *Proposed.* Identity: `id` (Utf8, BTree index), `file`
(Vault-relative path of the Document or journal segment), `order` (Int64:
the place of a journal line, for append order). Projector kinds map to
Arrow: `text` → Utf8, `integer` → Int64, `number` → Float64, `boolean` →
Boolean, `text list` → List<Utf8>. A column marked `fts` gets a Lance
inverted index. A column marked `vector` gets two more columns,
`{name}.vector` (FixedSizeList<Float32, dims>) and `{name}.digest` (the
git blob hash of the text that was embedded), so an unchanged text keeps
its vector. *Recommendation:* accept.

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
(`OptimizeOptions::num_indices_to_merge`); and prunes versions older than
one hour, never with `delete_unverified` (a searching process may still
read an older version). *Recommendation:* accept.

**D12. Upserts by `id`, whole rows.** *Proposed.* A changed Document is a
`merge_insert` on `id` with every column (update when matched, insert
when not), a deleted one a delete by `id`; a changed journal segment
appends its new lines (L3). `merge_insert` with a scalar index on the key
has open upstream bugs (lancedb #3177, #3515: rows silently not updated,
reported for partial-column updates, from Python, on 0.30 and 0.33); the
spike found none on Lance 13 with whole rows, before and after
`optimize`, and L2 keeps that as a regression test.
*Recommendation:* accept.

**D13. Async inside, sync outside.** *Proposed.* Lance is async (tokio).
Scheda keeps its blocking surface: one private multi-thread tokio
runtime per process, built on first use, and `block_on` from Scheda's own
threads (the indexer thread, the caller's thread of a sync call). The
async surface (`SchedaAsync`) already runs the sync calls on
`spawn_blocking`, where `block_on` works. A sync call made from inside an
async task (not on the blocking pool) panics in `block_on`; the docs say
so, as they say it for any blocking library. *Recommendation:* accept.

**D14. One feature, `index`.** *Proposed.* `find` and `search` split
Tantivy from USearch; with one engine they merge into one feature,
`index`, on by default. Without it there is no Index object (as without
`find` today) and journals keep their id maps. `simd` (USearch's) goes.
*Recommendation:* accept.

**D15. The query request.** *Proposed.* `DocumentQuery` becomes
`{ where, order_by, limit, start_after, include_raw, not_before }`:
`where` is the SQL predicate (empty: every row), over `id` and the
collection's columns; `order_by` a list of `(column, descending)`,
default `id` (a journal: `order`); `start_after` the last `id` (or
`order`) of the previous page. `DocumentSearch` gets `where` as its
prefilter. An unknown column, a predicate that does not parse, or a
statement (Lance takes an expression, never DDL or DML) is
`INVALID_ARGUMENT`. *Recommendation:* accept.

**D16. Full-text search.** *Proposed.* BM25 over the `fts` columns with
Lance's default tokenizer (`simple`, lower case, no stemming), without
positions, so no phrase queries in `0.23` (the anchor consumer searches
vectors only). A later version can add positions and phrase search per
column. *Recommendation:* accept.

**D17. What goes.** *Proposed.* Tantivy, USearch (the `sonalect/USearch`
fork pin), `lexical.rs`, `vector.rs`, `filter.rs`, the `Filter` messages,
`index/vector/`, `index/lexical/`, `index/columns`, `index/stale`, the
features `find`, `search`, `simd`. A change of the projection or of a
collection's columns no longer makes the Index `INDEX_STALE`: the indexer
rebuilds the affected datasets beside the live ones (D4). A change of
embedder model stays an explicit `Index.Reindex` (it embeds everything);
until then `model_mismatch` holds as today. The journal id maps stay as
they are. *Recommendation:* accept.

**D18. Dependencies.** *Proposed.* `lance` (and the `lance-*` crates the
code names) pinned exactly (`=13.0.0` today), the `arrow-*` crates at the
version Lance uses. Lance publishes a major version about every month:
upgrade one version at a time, deliberately, re-running L2's checks. The
build scripts of `lance-datafusion`, `lance-encoding`, `lance-file`,
`lance-index`, and `lance-table` need `protoc`: Cargo gets it from
`PROTOC` (`run-commands.md` says so), Bazel from crate_universe
annotations that hand those build scripts the Bazel `protoc`. Every new
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
go into §8 and into the decisions they touch. The later stages apply the
approved diff when they implement its part (L5 the queries, L6 the
searches, L7 the token, L4 and L8 the Index state and the errors), so
every commit stays green.

- [ ] `query.proto`: `DocumentQuery` and `VaultQuery` per D15 (`where`,
      `order_by`, `limit`, `start_after`, `include_raw`, `not_before`);
      `DocumentSearch` and `VaultSearch` with `where` and `not_before`;
      the `Filter` messages removed; the `buf.validate` rules proto can
      carry (limits, the order list).
- [ ] The state token on writes (D5): its message and name, and which
      replies carry it (`Create`, `Update`, `Upsert`, `Delete`, the
      batches of a collection and of the Vault).
- [ ] `index.proto`: `IndexState` after D4, D5, and D17 (`version`, how a
      caller tells the Index is behind `HEAD`, `rebuild` for a rebuild
      beside, `role`, `pending`, `model_mismatch`); the comments of
      `Status`, `Watch`, and `Reindex`.
- [ ] `options.proto`: the deadline a `not_before` waits for.
- [ ] `error.proto`: `INDEX_REBUILDING` and `INDEX_STALE` after D4 and
      D17 (kept, narrowed, or removed), and the code of a `not_before`
      that reaches its deadline.
- [ ] `ports.proto`: `IndexColumn` after D9 (a `vector` mark on a
      `text list`) and D16.
- [ ] The search reply: `SearchItem` and the `VectorLeg` reasons after
      D14 and L6.
- [ ] The CLI subcommands and flags (`14`) that follow from these
      messages, listed.
- *Check:* the owner approved §8 (the date is written there).

### L1. Dependencies and the build

D1, D14, D18.

- [ ] `lance` and its crates in the workspace (`=13.0.0`), sorted
      (`dependency-order.md`), every new version checked against the
      quarantine; `Cargo.lock` and `cargo-bazel-lock.json` repinned
      (`cargo-bazel-lock.md`).
- [ ] `protoc` for the Lance build scripts in Cargo (`PROTOC`) and in
      Bazel (annotations); `run-commands.md` updated.
- [ ] The feature `index` (D14) declared, `find` / `search` / `simd`
      still present until L8 removes them.
- [ ] The debug loop measured and written down: a cold
      `cargo build -p scheda --all-features`, and a rebuild after
      touching one file of `rust/scheda/src`. If the rebuild is slow,
      say so to the owner before L2 (the owner cares about debug builds,
      not release ones).
- *Check:* `cargo build` and `cargo clippy --workspace --all-targets
  --all-features` green; `bazel build //rust/scheda/...` green on Linux,
  macOS, and Windows CI.

### L2. The Lance store

D6, D7, D8, D9, D10, D11, D12, D13. A new module (`store.rs`), used by
nothing else yet.

- [ ] The private runtime and the blocking facade (D13).
- [ ] A dataset per collection: created from the Projector's columns
      (D8, D9), opened, its schema checked against the columns; the
      generation folder and `current` (D6).
- [ ] Upsert of whole rows and delete by `id` (D12), each commit
      carrying `scheda.state` (D7); reading the state back.
- [ ] Owner-only modes on every file Lance writes (Lance writes with the
      process umask): applied after each commit, as `restrict_tree` does
      for Tantivy today.
- [ ] Maintenance (D11): compaction, index optimisation, delta merge,
      prune.
- *Check:* `merge_insert` carries `transaction_properties`, read back
  from the new version (else D7's fallback, and the owner is told).
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

`20` D23 keeps its shape: the round, `index.lock`, the wake and the look
at `HEAD`, who indexes, the journal id maps.

- [ ] The parts of a round: the journal id maps (unchanged), then each
      dataset brought from its own state to `HEAD` (D7), then
      `index/{generation}/commit` last.
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
- *Check:* a write is found by a query and a search once the Index
  reached it; a deleted Document is gone from both.
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
      `index.lock`; the old generation is deleted when no reader can
      still be in it (Unix: at once; Windows: retried later, an open
      file cannot be deleted there).
- [ ] The indexer starts a rebuild by itself when the projection or a
      collection's columns change (no more `INDEX_STALE`), and for an
      Index of another layout; `Index.Reindex` starts one with the
      Vault's ports.
- [ ] `IndexState.rebuild` reports its stage and counts as today.
- *Check:* queries and searches answer from the live generation for the
  whole length of a rebuild, and from the new one after the switch.
- *Check:* a write during a rebuild is found after the switch.
- *Check:* a process killed during a rebuild leaves the live generation
  serving; the next indexer starts the rebuild again.

### L5. Query and count on SQL

D3, D15.

- [ ] `DocumentQuery` and `VaultQuery` per D15 in `api/scheda/v1`
      (`bazel run //api:generate`), the `Filter` messages removed.
- [ ] `Collection.Query`, `Collection.Count`, `Vault.QueryDocuments`,
      `Vault.CountDocuments` on the datasets: the predicate, the order,
      `start_after`, `limit`, one snapshot per stream.
- *Check:* a predicate over each column kind selects the exact set; the
  order is stable; paging with `start_after` neither repeats nor skips.
- *Check:* an unknown column, a predicate that does not parse, and a
  statement are `INVALID_ARGUMENT`.
- *Check:* a query of a collection the Index has not reached yet is
  empty, not `INVALID_ARGUMENT` (`p8-follow-ups.md`, first two items).

### L6. Search on Lance

- [ ] The vector leg (IVF_PQ with refine, or an exact scan below 10 000
      rows), the lexical leg (BM25), RRF fusion with the request's
      weights, the prefilter (`where`), the Reranker port, `like`.
- [ ] The vector-leg reason decided from the Projector's columns, not
      from what the Index holds (`p8-follow-ups.md`).
- *Check:* the `0.22` search tests that still apply, moved (token query,
  synonym query, hybrid fixture, prefilter, reranker swap, `like`).
- *Check:* recall at `k` 300 against an exact scan on a fixture of
  clustered vectors is at least 0.95.

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

D17.

- [ ] Tantivy, USearch, `lexical.rs`, `vector.rs`, `filter.rs`,
      `index/stale`, `index/columns`, the features `find` / `search` /
      `simd`, their tests; Cargo and Bazel repinned.
- [ ] `open` removes an Index of the `0.22` layout (D2).
- *Check:* `git grep -n -i -e tantivy -e usearch rust/ api/` is empty;
  `cargo test` and `bazel test //...` green.

### L9. CLI, Python driver, consumer test

- [ ] The CLI's `query` / `count` / `search` take `--where`, `--order-by`,
      `--not-before`; the Python driver follows.
- [ ] `tests/consumer.rs` rewritten to the anchor consumer's use after
      this change: vector-only search on one collection, a write's token
      passed to the search, `Index.Reindex` while searches go on.
- *Check:* `tests/consumer.rs` green; the CLI and the library give the
  same JSON for a query and a search.

### L10. Docs pass and release

- [ ] `06` (the Index on Lance), `13` (queries in SQL, searches), `07`,
      `14`, `02`, README, `.claude/notes/index-risks.md` (closed items
      removed), `CHANGELOG.md`.
- [ ] Release `0.23.0` (breaking: minor while major is 0,
      `.claude/skills/versioning/SKILL.md`).
- *Check:* `bazel test //...` green; markdownlint green.

---

## 4. The audit's items after the move

Items of `.claude/notes/index-risks.md` (and `p8-follow-ups.md`):

| Item | After the move |
| - | - |
| 1. Vector side out of step after a kill | Gone: one atomic Lance commit carries data and state (D7) |
| 2. A rebuild embeds everything, queries down meanwhile | Downtime gone (D4); embedding everything again stays for a model change, by nature |
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
- **Pruning under readers.** `delete_unverified` may delete files a
  reader in another process still reads; prune by age only (D11).
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

Written in L0: every change to `api/scheda/v1` as a diff against `main`,
each new or changed field with its comment, then the owner's review and
the date of his approval. Empty until L0.
