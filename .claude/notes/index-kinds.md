# Index kinds: what Scheda builds, and the text list gap

Review of 21 October 2026, for the owner; no code changed. Scheda builds
three kinds of Lance index: a B-tree (on `id`, on `path` of the files
table, on each `scalar_index` column), an inverted index (BM25, no
positions, no stemming) on each `fts` column, and a vector index (cosine,
refine 10) on each `vector` column from 10 000 rows: IVF_RQ with 1 bit per
dimension since 24 October 2026, IVF_PQ before (last section).

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

- The vector index kind was chosen by a measurement on real embeddings
  (last section): IVF_RQ, 1 bit. IVF_PQ had been chosen on the spike's
  synthetic vectors (`21` D10: recall 1.00 at 0.9 GB per process, against
  IVF_SQ 0.99 at 2.4 GB and IVF_HNSW_SQ 0.95).
- The inverted index is the only ranked lexical index in Lance; NGram and
  the FM-index find substrings without a rank.
- LabelList is built on Bitmap. A fork that trims index kinds keeps BTree,
  Inverted, LabelList with Bitmap, and IVF with RQ. IVF_PQ only while an
  Index made before 24 October 2026 may still hold one (it is not
  converted; see CHANGELOG).

## IVF_PQ against IVF_RQ: measured, IVF_RQ taken (24 October 2026)

The owner's task of 21 October 2026, run on 23-24 October 2026 in
`/home/amsokol/Coding/vector-index-bench` (its own git repository, outside
this one; `REPORT.md` there has every table, `results/` the raw CSV). The
owner took IVF_RQ with 1 bit on 24 October 2026; `store.rs` builds it
(`VECTOR_INDEX_BITS`), no migration (the only Vault, Knowqore's, had no
vector index: 7 446 vectors, below 10 000).

- Data: 100 000 real passages embedded by Qwen3-Embedding-4B the way
  Knowqore stores a text (16 223 commit messages, 33 280 Markdown
  paragraphs, 50 497 Rust doc comments); queries: 600 of the 1 302 lesson
  statements of Knowqore's Vault, with its retrieval-excerpts instruction.
  Exact top 50 by brute force, equal to Lance `use_index(false)` on every
  query. Lance 13.0.0, Scheda's dev profile (latencies are relative).
- Scheda's settings (316 partitions, `nprobes` unset, refine 10):

  | | IVF_PQ 160 | IVF_RQ 1 bit | IVF_RQ 2 | IVF_RQ 4 | IVF_PQ 320 |
  | - | - | - | - | - | - |
  | recall@10 | 0.997 | 1.000 | 1.000 | 1.000 | 1.000 |
  | recall@50 | 0.964 | 1.000 | 1.000 | 1.000 | 0.998 |
  | query p50 | 65 ms | 13 ms | 13 ms | 19 ms | 102 ms |
  | build | 58 s | 7.5 s | 9 s | 14 s | 115 s |
  | index on disk | 21 MB | 35 MB | 66 MB | 127 MB | 36 MB |
  | search process peak RSS | 741 MB | 827 MB | 981 MB | 1 266 MB | 867 MB |

- Without refine: IVF_PQ 160 0.668, IVF_RQ 1 bit 0.893 at `k` 10; refine 5
  already gives IVF_RQ 1.000.
- With `nprobes` unset, Lance 13 visits every partition (the analyzed
  plan: 27 of 27, 78 of 78, 109 of 109), so partitioning loses nothing and
  a query reads every code. A set `nprobes` costs recall at once (64 of
  316: 0.98; 8: 0.85), for every quantizer alike. Lance caps the
  partitions at about rows / 256.
- Appends cost no recall: Scheda's own path (the index made at 10 000 rows
  with 100 partitions, then 9 appends of 10 000 rows with `merge(4)`)
  gave IVF_PQ 0.9995 / 0.986 and IVF_RQ 1.000 / 1.000; one optimize of
  10 000 rows takes under a second. The partitions stay as made (Lance
  splits only for `num_indices_to_merge: None`), which did not hurt here.
- Not measured: 1 000 000 rows (100 000 decided it), a release build, the
  `Matrix` rotation, the reranker end to end (IVF_RQ's top 50 equals the
  exact top 50 on every query, so the reranker sees the same candidates).

**Why:** the owner asked whether the index kinds are the best ones
(21 October 2026); the earlier choice rested on synthetic vectors.

**How to apply:** keep `nprobes` unset unless a measurement at the
consumer's scale says the full scan is too slow; a set `nprobes` trades
recall at once. A fork that trims index kinds keeps LabelList (with
Bitmap, which it is built on) and IVF with RQ.
