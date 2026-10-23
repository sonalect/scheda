# Index kinds: what Scheda builds, and the text list gap

Review of 21 October 2026, for the owner; no code changed. Scheda builds
three kinds of Lance index: a B-tree (on `id`, on `path` of the files
table, on each `scalar_index` column), an inverted index (BM25, no
positions, no stemming) on each `fts` column, and IVF_PQ (cosine, refine
10) on each `vector` column from 10 000 rows.

## The gap: `scalar_index` on a `text list` builds nothing

- The contract promises an index. `IndexColumn.scalar_index` in
  `api/scheda/v1/ports.proto`: "The Index keeps a scalar index on it, so a
  `Filter` or an order on it reads that index instead of every row." No
  CEL rule limits it to some kinds (`fts` and `vector` have one), so a
  `text list` may carry it.
- The code skips it. In `store.rs`, where the indexer lists the scalar
  indices: `if column.scalar_index && column.kind != Kind::TextList`. No
  error, no log, and no word in the docs: `06` and `13` say a B-tree per
  `scalar_index` column.
- Likely why (not recorded): Lance's B-tree refuses a nested field ("A
  btree index can only be created on a non-nested field"), and L2
  (`1fdd6d8`) left lists out instead of choosing the kind by the column's
  kind, as `21` C5 planned ("B-tree, bitmap, or label list, by kind").
- The results are right (a scan), only slow: a filter on a list column
  reads that column in every row.
- The anchor consumer hits it: Knowqore marks every column `scalar_index`,
  `records` (a `text list`) included, and its `citing_query` filters by
  `records`. How slow at its scale is not measured.
- The test does not prove the ticked L2 *Check* ("each kind of leaf over
  each column kind … and Lance uses the index"): only `eq` on a `text`
  column is checked against the plan
  (`store::tests::the_indices_are_made_and_the_scans_use_them`). Lists,
  booleans, numbers, and the prefix and range leaves are not.

## Fix: done 23 October 2026

Accepted by the owner on 23 October 2026, all three points. A `text list`
with `scalar_index` gets a LabelList index (`Column::scalar_kind` in
`store.rs`, named `{column}_labels`); the plan test
`store::tests::a_filter_on_each_kind_of_indexed_column_reads_its_index`
checks every column kind and leaf, the rows the label list finds, and the
index after `optimize_indices`. Lance serves on a list `term` and `terms`
(`array_has`); `exists` (`cardinality`) and a negated leaf (`is_null or
not …`) still scan, since a label list does not track nulls. The points as
proposed:

1. A `text list` with `scalar_index` gets Lance's LabelList index, which
   serves `array_has`: Scheda's leaf on a list, `any_of` included. No
   rebuild: the indexer makes a missing index by its name on its next
   round. CHANGELOG under `Fixed`.
2. A test checks that the plan uses the index for every column kind ×
   leaf kind with `scalar_index`.
3. Booleans keep the B-tree; Bitmap only after a measurement.

## The rest of the set

- IVF_PQ was chosen by a measurement (`21` D10: recall 1.00 at 0.9 GB per
  process, against IVF_SQ 0.99 at 2.4 GB and IVF_HNSW_SQ 0.95), on
  synthetic vectors. IVF_RQ (RaBitQ, in Lance 13) was not measured:
  measure it on real embeddings before a fork cuts the vector variants
  (`lance-binary-size.md`).
- The inverted index is the only ranked lexical index in Lance; NGram and
  the FM-index find substrings without a rank.
- LabelList is built on Bitmap. A fork that trims index kinds keeps BTree,
  Inverted, LabelList with Bitmap, and IVF_PQ, plus whatever the RQ
  measurement picks.

## Deferred: IVF_PQ against IVF_RQ

The owner's task of 21 October 2026, set for after 0.26; start only on his
word. Not a format change: the index is rebuilt from the stored vectors.

- Measure the vector index kind, IVF_PQ (today) against IVF_RQ, on real
  Qwen3 embeddings (2560 dimensions), at 100 000 vectors. 1 000 000 only
  if the smaller run gives no clear answer (the owner, 23 October 2026:
  a fast decision, not a week of measuring). In a folder of its own
  outside this repository; the prompt for that session was written on
  23 October 2026.
- Criteria, in order: recall of the top k against exact search as the
  reference (first: a reranker cannot bring back what the vector leg did
  not find); peak memory of building and of searching; time of building
  and of searching; size of the index.
- Refine and the number of partitions go into the same measurement.
- HNSW is out: a graph held in memory breaks Knowqore's rule of bounded
  memory.
- SQ is out too (the owner, 21 October 2026): it holds a byte per dimension
  of every vector in memory per searching process (2.4 GB at 1M × 2560 in
  the spike, against 0.9 GB for IVF_PQ with refine), with no recall gain
  over IVF_PQ with refine. The Lance fork cuts both (its `SCHEDA-PATCH.md`,
  Part 4); the measurement above is IVF_PQ against IVF_RQ only.

**Why:** the owner asked whether the index kinds are the best ones
(21 October 2026) and will come back to it.

**How to apply:** a fork that trims index kinds keeps LabelList (with
Bitmap, which it is built on).
