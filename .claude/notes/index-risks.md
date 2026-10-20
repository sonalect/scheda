# Index risks (audit of 19 October 2026)

An audit of the indexer as the `proto-contract` branch builds it (commit
`b31fb46`, `20` D23), for discussion with the owner. Nothing here is
decided yet. Each item has a severity, the evidence, and a recommendation.
"Probe" means a throwaway test that ran; the probes are in
`index-probes.patch` beside this note (`git apply` it in a scratch tree;
they print `PROBE-…` lines with `--nocapture`, poll 10 ms, debug build).

## Findings, most severe first

1. **High. A kill in a narrow window leaves the vector side out of step
   for good.** The vector side writes `vectors`, then `state`, then the
   round writes `commit`. A process killed between the `vectors` and
   `state` renames, or after `state` but before `commit` when `HEAD` moves
   before the next round, leaves `state` naming neither `commit` nor the
   target. Every later round fails with `INDEX_BROKEN`
   (`vector.rs`, `out_of_step`), `commit` never moves, and each retry
   re-projects the diff (item 3). Only `Index.Reindex` gets out, at the
   full cost of item 2. Probe A and probe B: still `INDEX_BROKEN` after
   5 s. Probe C (no `HEAD` move): recovers. *Recommendation:* when the
   side is out of step, rebuild the vector side alone inside the round:
   the lexical side offers every Document of the vector collections, and
   the stored vectors are reused by digest, so nothing is embedded again;
   and put the stamp-and-state into `vectors` itself (one rename, no
   window between two files).

2. **High. Every rebuild embeds every text again.** `Index::reset`
   deletes `vectors` before the rebuild indexes `HEAD`, so a `Reindex`
   for a projection change, an `INDEX_BROKEN` repair, a `format` bump on
   upgrade, a pruned commit, or item 1 sends the whole Vault to the
   embedder, and queries and searches are `INDEX_REBUILDING` for all of
   it. `0.21` carried the vectors over. Probe I: a `Reindex` with the same
   model re-embedded 10 of 10 unchanged texts. *Recommendation:* before
   `reset`, load the old `vectors` (same model, readable) into the vector
   side's digest cache, which already exists for failed tries.

3. **Medium–high. An embedder outage multiplies the Projector work.**
   While the embedder fails, every retry projects the whole diff again
   and commits Tantivy again (probe D: 220 `Project` calls in 2 s for a
   10-Document diff); and whenever `HEAD` moved since the previous try,
   the lexical payload is neither `commit` nor the target, so the lexical
   side walks every Document of the Vault (probe J: an unchanged Document
   projected 9 times for 10 writes). The pause is at most 8 ×
   `index_poll_ms` (8 s by default). An app that keeps writing during a
   long outage of a remote model projects its whole Vault every few
   seconds. *Recommendation:* keep the round's offers in memory across
   failed tries (as the vectors already are), let the lexical side diff
   from its own payload instead of walking everything, and give embedder
   failures a longer cap (minutes).

4. **Medium–high. No way to wait for one's own write.** `version` is
   opaque and no call names `HEAD` as a token; a Vault that does not index
   reports `pending` 0, `role` false, and no error while the Index is
   behind (probe K). A caller watching "until `version` changes" can stop
   on a round that ended before its write. The anchor consumer calls
   `wait_indexed` today (`../knowqore/rust/server/src/buffer.rs`,
   `../knowqore/rust/learn/src/records.rs`). *Recommendation:* give a
   write (or `Vault`) the state token of `HEAD` so a caller watches until
   `version` equals it, and make a following Vault report that it is
   behind (`pending` from a tree count, or a flag). Decide before the
   P8 consumer test.

5. **Medium. Closing a Vault waits for the whole embedding of a round.**
   `EmbedderPort::embed` drains the reply stream without looking at the
   stop signal, and a round sends all its texts in one stream. Probe G:
   a drop 500 ms into a 4 s embedding took 3.5 s. During a rebuild of a
   large Vault that is the whole rebuild. *Recommendation:* check the stop
   between items of the reply and drop the stream; cap a stream at a
   bounded number of texts.

6. **Medium. One stale collection breaks every vector search.** The
   vector reader calls `Index::readable`, which fails on the mere presence
   of `index/stale`, while the lexical reader fails only for a stale
   collection in scope. Probe L: `stale = {other}`, a lexical read of
   `notes` succeeds, any vector read is `INDEX_STALE`, and `search.rs`
   propagates it, so a search of `notes` fails. *Recommendation:* scope
   the vector check to the collections of the search, as the lexical one
   is.

7. **Medium. A journal append re-indexes its whole segment.** A changed
   segment is projected line by line, its Tantivy documents and vectors
   removed and added again. Probe E: one line appended to a 200-line
   segment → 201 `Project` calls (the embedder got 1 text, thanks to the
   digests). Cost per append grows with the length of the open segment.
   *Recommendation:* segments are append-only (STO-3): index only the
   lines after the old blob's length when the old bytes are a prefix;
   fall back to the whole segment otherwise.

8. **Medium (scale). `vectors` is read and written whole every round that
   changes a vector.** About 340 bytes per vector at 8 dimensions in
   probe E (key map plus graph), so roughly 3.4 KB at 768: 100 000
   vectors ≈ 340 MB read, written, and `fsync`ed per write commit, and
   every other process reloads the whole file at its next search and
   holds its own copy. Fine for thousands of vectors. *Recommendation:*
   measure at the anchor consumer's real size; until then state the limit
   in `10`.

9. **Low–medium. A `Reindex` from a second Vault rebuilds twice.**
   `ABORTED` covers only a request in the same Vault object; another
   Vault or process on the path queues its own rebuild, which runs again
   after the first, at the full cost of item 2. *Recommendation:* drop a
   pending request when the Index was rebuilt after it with the same
   projection and model.

10. **Low. Temp names can collide between processes.** `.scheda.{n}.tmp`
    counts from 0 in every process; a writer whose `create_new` fails
    removes the other's temp file. Under `index/` only the lock holder
    writes, so the Index is safe; a kill mid-write leaves an orphan temp
    (up to the size of `vectors`) until a rebuild. *Recommendation:* add
    the pid to the name; sweep `index/**/.scheda.*.tmp` at the start of a
    round, under the lock.

11. **Low (unverified, Windows). A rebuild may not be able to delete the
    lexical files a reader has mapped.** `reset` removes `index/lexical/`
    while a cached Tantivy snapshot may hold its files; Windows refuses to
    delete an open or mapped file, so the rebuild would fail and retry
    while that reader lives. CI has a Windows job. *Recommendation:* a
    Windows test: hold a lexical snapshot, `Reindex`, expect `DONE`.

12. **Low. Renames are not followed by a directory `fsync`.** The order
    `vectors` → `state` → `commit` relies on rename durability; after a
    power loss (not a kill) a journaling filesystem keeps that order in
    practice. *Recommendation:* accept; note it.

13. **Doc. D23 says an embedder outage holds back the lexical side.** In
    the code the lexical side commits Tantivy before the vector side, so
    queries see new Documents during an outage; only `commit` and
    `version` stay behind. `06` IDX-9 now says what the code does.
    *Recommendation:* keep the behaviour, amend the D23 sentence.

## Found while building the Index on Lance (20 October 2026)

1. **Low. `like` on a long Document asks BM25 with all its words.** The
    lexical leg of `like` matches every word of the Document's `fts`
    columns (`search.rs`); `0.22` used Tantivy's `MoreLikeThis`, which
    picks the most telling terms. Correct, but slow for a long text.
    *Recommendation:* when a consumer searches `like` on long Documents,
    keep the top terms by tf-idf before the query; measure first.
2. **Fixed. A collection declared during a round lacked its table.** The
    round's plan was of the collections at its look, its work of a later
    `HEAD`; fixed by making the plan carry its declarations and making it
    again when `HEAD` moved (`tables.rs`,
    `a_collection_declared_while_a_round_runs_gets_its_table`).
3. **Fixed. A rebuild never switched under a steady stream of writes.**
    It caught up until `HEAD` stood still; it now catches up once and
    switches, as D4 says (`search.rs`,
    `a_process_killed_while_it_indexes_leaves_an_index_that_opens_counts_and_searches`).
4. **Fixed. The status showed the Index ahead of the stored state.** A
    round published `stored_version` from its look and `version` from the
    later `HEAD` of its work; both are now the state the round reached
    (`tables.rs`, `a_collection_declared_while_a_round_runs_gets_its_table`).
5. **Fixed. A rebuild's catch-up left the journal id maps behind.** It
    brought the tables to the later `HEAD` and not the maps, so the
    published `version` claimed more than the maps held and the next round
    took the lock at once; the maps now catch up first and the tables to
    their state (`indexer.rs`,
    `a_rebuild_waits_for_the_lock_and_a_second_request_is_aborted`).

## Checked and fine

- A reset, rebase, or rewriting pull is applied incrementally (tree
  diff); only a pruned commit object forces a rebuild, and a long-lived
  process may not even notice that (libgit2 caches the object), which is
  harmless (probe H).
- USearch reuses the room of removed vectors: 20 cycles of remove-all /
  add-100 keep the same size (probe F).
- A reader that loses files to a rebuild answers `INDEX_REBUILDING`, not a
  read failure (`Reader::failed` in `vector.rs`, the `meta.json` and
  `open` paths in `lexical.rs`).
- `Check` does not report Tantivy files mid-round as wide modes: it takes
  `index.lock` without waiting and skips `lexical/` while a round runs
  (probe M: 211 checks during 150 indexed writes, no problem).
- A new `vectors` stamp is the time in nanoseconds, at least the previous
  one plus one: a reader never mistakes a rebuilt file for its cached one.

## Why

The owner asked how indexing really works, for a list of traps, and for
the documents to say what the code does (19 October 2026).

## How to apply

Discuss the numbered items with the owner; record the decisions in a
work-order document (or `20` §7) and turn the matching probes into
regression tests when an item is fixed. Delete this note once every item
is decided.
