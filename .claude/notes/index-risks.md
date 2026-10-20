# Index risks (audit of 19 October 2026, after the move to Lance)

An audit of the indexer as the `proto-contract` branch built it (commit
`b31fb46`, `20` D23), for discussion with the owner. The Index has since
moved to Lance (`21`, `22`), and the items the move closed are removed:
the vector side out of step after a kill, no way to wait for one's own
write (`not_before`), a close that waits for a whole embedding, one stale
collection breaking every vector search, a journal append re-indexing its
segment, `vectors` read and written whole, and the wording of D23 on the
lexical side (`21` §4 has the table). What stays is below, reworded for the
Index on Lance. Nothing here is decided yet. Each item has a severity,
the evidence, and a recommendation. The probes of the old Index are in
`index-probes.patch` beside this note (they apply to the tree of
`b31fb46`, not to the present one).

## Findings that stay, most severe first

1. **High. A rebuild embeds every text again.** A `Reindex` builds a new
   generation from nothing, so it sends the whole Vault to the embedder:
   for a new embedder model that is by nature, but for a new projection or
   columns, an `INDEX_BROKEN` repair, or a `Reindex` with the same model it
   is waste, since the old generation holds the vectors by the digest of
   their text. Queries and searches now answer from the live Index meanwhile
   (`21` D4, C16), so the cost is time and embedder load, not an outage.
   Only the caller starts a rebuild. *Recommendation:* when the model and
   vector length are the same as the live generation's, seed the digest
   cache of the round with the live generation's vectors before the
   rebuild; measure first at the anchor consumer's size.

2. **Medium. An embedder outage multiplies the Projector work.** While the
   embedder fails, each try of the round projects the changed Documents
   again; the vectors already embedded are kept by digest, the projections
   are not (`21` §4, item 3). Whenever `HEAD` moved since the previous try,
   the diff grows. The pause is at most 8 × `index_poll_ms` (8 s by
   default). An app that keeps writing during a long outage of a remote
   model projects its changed Documents every few seconds. No probe on
   Lance yet. *Recommendation:* keep the round's projections in memory
   across failed tries, as the vectors are, and give embedder failures a
   longer cap (minutes).

3. **Low–medium. A `Reindex` from a second Vault rebuilds twice.**
   `ABORTED` covers only a request in the same Vault object; another Vault
   or process on the path queues its own rebuild, which runs again after
   the first. It is cheaper than before, since the live Index answers
   meanwhile, but it is the cost of item 1 again. *Recommendation:* drop a
   pending request when the Index was switched after it with the same
   projection, columns, and model.

4. **Low. Temp names can collide between processes.** `.scheda.{n}.tmp`
   counts from 0 in every process; a writer whose `create_new` fails
   removes the other's temp file. Under `index/` only the lock holder
   writes, so the Index is safe; a kill mid-write leaves an orphan temp
   (the journal id maps, `commit`, `current`) until the next write of the
   same file. Lance names its own files. *Recommendation:* add the pid to
   the name; sweep `index/**/.scheda.*.tmp` at the start of a round, under
   the lock.

5. **Low (unverified, Windows). Deleting a generation, or a file a reader
   holds open, may fail.** The old generation is removed an hour after the
   switch, and Lance opens data files as it reads them; Windows refuses to
   delete an open or mapped file. The removal is tried again later, but no
   test holds a reader open on Windows. *Recommendation:* a Windows test:
   hold a snapshot, `Reindex`, wait out the margin (with a shortened
   margin), expect the generation gone.

6. **Low. Renames are not followed by a directory `fsync`.** For `commit`,
   `current`, and the id maps, the order relies on rename durability; after
   a power loss (not a kill) a journaling filesystem keeps that order in
   practice. Lance has its own commit protocol for the tables.
   *Recommendation:* accept; note it.

## Found while building the Index on Lance (20 October 2026)

1. **Low. `like` on a long Document asks BM25 with all its words.** The
    lexical leg of `like` matches every word of the Document's `fts`
    columns (`search.rs`); `0.22` used Tantivy's `MoreLikeThis`, which
    picks the most telling terms. Correct, but slow for a long text.
    *Recommendation:* when a consumer searches `like` on long Documents,
    keep the top terms by tf-idf before the query; measure first.

Fixed in the same work, each with a test (kept here so the same trap is
not set again): a collection declared during a round lacked its table
(`tables.rs`: `a_collection_declared_while_a_round_runs_gets_its_table`); a
rebuild never switched under a steady stream of writes, now it catches up
once and switches (`search.rs`: `a_process_killed_while_it_indexes_leaves_an_index_that_opens_counts_and_searches`);
the status showed the Index ahead of the stored state, now `version` and
`stored_version` are both the state the round reached (same test as the
first); a rebuild's catch-up left the journal id maps behind, now the maps
catch up first (`indexer.rs`: `a_rebuild_waits_for_the_lock_and_a_second_request_is_aborted`).

## Checked and fine

- A reset, rebase, or rewriting pull is applied incrementally (tree
  diff). A commit the Index stands on that is gone from its repository is
  caught up by comparing the tree of `HEAD` with the table of files; no
  rebuild (`21` C35; `tests/search.rs`:
  `an_isolated_repository_made_again_is_caught_up_without_a_rebuild`).
- A process killed while it indexes, or during a rebuild, leaves an Index
  that opens, counts, and searches, and a live generation that serves
  (`tests/search.rs`: `a_process_killed_while_it_indexes_leaves_an_index_that_opens_counts_and_searches`,
  `a_process_killed_during_a_rebuild_leaves_the_live_generation_serving`).
- Every file of the Index is owner-only after a round, so `Check` does not
  report a file a round is making as a wide mode (`tests/search.rs`:
  `every_file_of_the_index_is_owner_only_after_a_round`).

## Why

The owner asked how indexing really works, for a list of traps, and for
the documents to say what the code does (19 October 2026). The move to
Lance (20 October 2026) closed most of the first list; what is left is the
cost of a rebuild, the work an embedder outage repeats, and small file
hygiene.

## How to apply

Discuss the numbered items with the owner; record the decisions in a
work-order document and turn the matching cases into regression tests when
an item is fixed. Delete this note once every item is decided.
