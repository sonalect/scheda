# Scheda 0.25: vectors on disk, exact search, bounded index rounds

The anchor consumer's asks S1–S4 (its note `scheda-0.25-asks.md`, from a heap
profile of 21 October 2026), worked on `main` after `0.24.1`.

## Asks

- S1. Read the stored vectors of one `vector` column for a list of ids, as a
  lazy stream `(id, position, digest, vector)`, absent ids reported, in
  bounded batches, at the reader's snapshot.
- S2. A third search subject: a vector and the `vector` column it is
  compared with; dimension checked; no lexical leg; rerank refused.
- S3. `exact` (flat scan, ties by id, repeatable) and `min_vector_score` (in
  `vector_score` units), wired from Lance's `use_index(false)` and
  `distance_range` (both in Lance 13.0.0).
- S4. An index round whose memory grows with the Documents it applies, not
  with the table: profile first, then Lance's own mechanism.

## Owner's decision (21 October 2026)

Found before S1: old Lance versions were pruned an hour after the *next
Lance version* was made, not an hour after the live `commit` stopped naming
them. A round longer than an hour on one table, or the round after a killed
one (restore), pruned the version the live `commit` still named; dropped
tables and a retired generation were stamped too early the same way. A
lazy S1 stream would read Lance for as long as its caller holds it.

Decided: the time a version, a dropped table, or a retired generation left
the live `commit` is stamped by the next round when it starts (it starts
after that `commit` was written, so the stamp is never early); only what
left more than an hour ago is removed. A reader is then safe for one hour
from its start, whatever a round's length. A commit of the older format
keeps everything for one more hour. The S1 stream reads its snapshot within
that hour; an item asked later is `FAILED_PRECONDITION` (start again with
the ids not yet given).

**Why:** no timing assumption about the length of a round.
**How to apply:** any new reader of Lance that outlives one call is bound
by `VERSION_LIFETIME` from the moment before it read `commit`.

## What each ask became (21 October 2026)

The anchor consumer confirmed S1–S4 on 21 October 2026; released as
`0.25.0`.

- S1: `Collection.GetVectors(VectorSelection{column, ids, not_before})` →
  stream of `VectorItem{vector: StoredVector{id, position, digest,
  values} | absent: id}`; portions of 64 ids; one snapshot for
  `VERSION_LIFETIME`; CLI `get-vectors`. `src/vectors.rs`.
- S2: `DocumentSearch.vector{column, values}`; needs no embedder
  (`VECTOR_LEG_NO_EMBEDDER` only for `text`/`like`, whose behaviour is
  unchanged); CEL refuses zeros, `vector_weight` 0, `rerank_limit`.
- S3: `DocumentSearch.exact` (Lance `use_index(false)`; ties completed with
  a second `distance_range` query at the last distance, then ordered by
  id) and `min_vector_score` (Lance `distance_range`). **Deviation:**
  `min_vector_score` needs `exact`: with a cosine IVF_PQ index Lance 13.0.0
  applies the bound in the PQ stage to its own L2 distances of residuals
  (`lance/src/index/vector/pq.rs:294-318`, `lance-index …/pq/storage.rs`
  `Cosine => L2`) and again in cosine units at refine; through the index a
  bound returned nothing.
- S4: files table B-tree on `path` (merge_insert joins through it);
  `embeddings` streamed; round datasets keep 256 KiB per Lance cache
  (`ROUND_CACHE`); exact scan bounded by `target_parallelism(1)`,
  `fragment_readahead(1)`, `batch_size(1024)`, `batch_readahead(2)`,
  `io_buffer_size(16 MiB)`; no filter given Lance when the search has none.

## Measurements (vectors of 256 values, debug build of scheda, deps -O2)

| What | Small | Large |
| - | - | - |
| exact search, peak | 46.8 MB (20 000 rows) | 47.0 MB (200 000) |
| exact search, Lance defaults | 47 MB | 124 MB |
| round of one Document, peak / allocated | 5.4 / 29 MB (1 000 rows) | 5.7 / 31 MB (100 000) |
| `put_files` before the `path` index, allocated | 10 MB | 24 MB |
| removing 100 vs 10 000 of 100 000, peak | 5.5 MB | 6.3 MB (2.3 MB more with Lance's default caches) |
| `GetVectors` 1 000 vs 10 000 ids, peak | 0.47 MB | 0.46 MB |

## Found, not done (for the owner)

- Lance's `ObjectWriter` (lance-io 13.0.0, `object_writer.rs:28,298`)
  allocates a 5 MiB buffer for every file it writes; `LANCE_INITIAL_UPLOAD_SIZE`
  can only raise it. Every Lance write (delete, upsert, manifest) holds at
  least 5 MiB and allocates about 6 MiB: the churn the consumer saw under
  `commit` frames.
- An exact search with a filter: Lance marks the admitted rows first,
  about 20 bytes a row of the table.
- A cache limit in `OpenOptions` (owner's idea, 21 October 2026): only
  meaningful with a Lance session shared by the reads of one Vault across
  calls, which Scheda does not have (each query opens its datasets anew).
  **Deferred by the owner, 21 October 2026.** How to apply: when a shared
  read session per Vault is built, give it this limit as an option; the
  rounds keep `ROUND_CACHE`.
- Two rebuilds within an hour: the second switch removes the generation
  the first one retired at once (`commit.retired` holds one). Existing.
- A write to a flat collection of 100 000 files diffs and rewrites a git
  tree of that size (libgit2): grows with the collection, outside S4.
