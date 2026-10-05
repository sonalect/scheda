# Two journals with byte-identical segments

Found 5 October 2026 while writing the `19` F2 tests; not fixed (outside
`19`).

`indexer.rs` `apply_changes` caches parsed journal segments by blob id
(`segments: BTreeMap<blob, SegmentLines>`) and takes each line out with
`lines.remove(&change.id)`. When two journal collections hold segments
with the same bytes (same writer token, same `id`, same payload), git
stores one blob; the first collection takes the line, the second finds it
gone and `continue`s. That collection's line never reaches the lexical
leg (or the vector leg), while the marks still move to `HEAD`.

Reproduce: declare two journals, `add("alpha", same_bytes)` to each from
one handle, `wait_indexed`, `find` on the second one returns nothing.

**Why it matters:** silent loss of Index entries; `find` / `search` miss
a Record whose file is there.

**How to apply:** key the cache by `(collection, blob)`, or read the
payload with `get` and drop the segment only when `remaining` reaches 0.
Add a regression test with two identical journals. Consumer-visible only
as a fix (`Fixed` in CHANGELOG).
