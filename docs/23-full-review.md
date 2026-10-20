# 23. Full review before the release

Document 23 of the Scheda package. Version 0.2, 20 October 2026.

Status: approved by the owner on 20 October 2026, decision by decision
(§3). A full review of the repository (`full-review.md`) on `main` at
`34e61d1`, before the first release of the protobuf contract and the
Index on Lance. Items R1–R22 are the findings, most severe first inside
each group; D1–D9 are the owner's decisions. Every finding R1–R21 is
fixed before the release `0.22.0` (D2); R22 stays a note. The work goes
on the branch `full-review-fixes`, one commit per stage (§4).

Order inside each stage (§4): code and a regression test first,
`cargo test` and `cargo clippy` green, then the documents the stage
changes, once (`code-first.md`). Tick a box only when its *Check:* passes
under `bazel test //...`.

---

## 1. Scope and method

**Read whole:** every file of `rust/scheda/src`, `rust/cli/src`,
`rust/blocking/src`, `rust/api/lib.rs`; every `api/scheda/v1/*.proto`;
the Cargo, Bazel, Buf, and CI files; the normative documents `03`–`07`,
`09`, `13`, `14`; README; the `[Unreleased]` section of `CHANGELOG.md`.

**Read in part:** `01`, `02`, `08`, `10` (searched for stale claims, `10`
§2–3 read); the work orders `11`, `12`, `15`–`22` (status, open boxes,
the rules later code must keep); the integration tests (fixed sleeps,
cited test names, the gaps the documents name); `python/` (the driver's
shape, the sidecar launcher). Not reread line by line: the test bodies
and the history work orders.

**Run:** `bazel test //... --keep_going --nocache_test_results` on this
tree: 22 of 22 targets pass (20 October 2026, 67 s). Four throwaway
probes were added to `rust/scheda/tests/integration`, run once, and
removed; the tree is clean. Their results are quoted in R1, R2, R3, and
R5.

**Anchor consumer** (`consumers.md`): Knowqore pins a `0.21`-era revision
(`knowqore/v0.21-tokio`, features `encrypt`, `find`, `search`, `simd`),
so it moves to the new contract as a whole. What the findings mean for
it: it sets `exclude_lexical_index` / `exclude_vector_index` on eight
collections and re-declares a collection whose schema differs at open
(R2); its embedder refuses a whole batch (R4); it syncs with a mirror
(R1); it uses no journal (R3), no rerank `k` from user input (R18), and
an optional passphrase (ISO-7, accepted below).

---

## 2. Findings

### 2.1 Correctness (High)

**R1. `Mirror.Pull` takes in what this build cannot read.**
`mirror.rs` `pull_join` (164) moves `HEAD` to the mirror's commit, or to
the join, without looking at it. A mirror whose marker is of another
generation, or that holds a collection file that does not load, is
pulled in. *Probe:* a mirror with `version: 3` and a `bad/.collection.yaml`
without a kind: `Pull` returned `Ok`, the work tree took `version: 3`,
and every later call failed (`ListCollections`, `GetCollection`, and
`Check` with `INVALID_ARGUMENT`); by reading, the next `Scheda.Open`
fails with `GENERATION`. With only the marker changed, the Vault would go on
writing generation-2 files into a generation-3 tree. The same path runs
for the pull of `auto_sync` at open. GIT-14 says the marker is read
before any other file; the clone path does this, the pull does not.
*Change:* before `advance_to`, read the marker of the target commit (the
mirror's, or the join's tree) with `marker::read` on a snapshot of it
(another generation: `GENERATION`), then `collection::load` of it (a file
that does not load: `INVALID_ARGUMENT` with its path); both refuse with
nothing changed, as `MIRROR_CONFLICT` does. *Check:* `tests/mirror.rs`:
a pull of a mirror of another generation is `GENERATION` and changes
nothing; a pull of a broken collection file is `INVALID_ARGUMENT` naming
the file and changes nothing; the same for the pull at open with
`auto_sync` (kept as the Vault's sync failure).

**R2. Changing `exclude_lexical_index` or `exclude_vector_index` on an
indexed collection makes the Vault `INDEX_STALE`.** `tables.rs` `stale`
(214) compares the plan's shape, which already applies the declaration's
exclusion (`plan`, 108–144), with the shape the table was built with. A
`Vault.UpdateCollection` that flips a flag makes them differ, so the
Vault stops indexing and its queries and searches fail until a
`Reindex`. The round would have rebuilt that one table anyway: a
changed declaration file is `Scope::Whole` (`bring_to`). *Probe:* a
Projector with one `fts` column, one Document, then `excludeLexicalIndex:
true`: `last_error` became `INDEX_STALE`, and `CountDocuments` failed
with "the Index holds other columns of the collection notes than this
Vault's Projector gives". Not a documented outcome: the contract makes
only another projection, other Projector columns, or another embedder
stale. *Change:* record in `generation::Table` the columns the Projector
declared, before the exclusion, and compare those in `stale`; the
effective shape follows the declaration, and a changed declaration
rebuilds its table in the round, as it does now. The Index format of
this release is not released yet, so no format number moves. *Check:*
flipping each flag both ways on a collection the Index holds keeps the
Vault indexing, `Count` answers, and the table gains or loses its
columns and vectors (`tests/collections.rs` or `tables::tests`).

**R3. A journal append costs the whole journal, and the object database
grows with the square of a segment.** Two causes:

- (a) `document_calls.rs` `append` (910, 921) reads and parses every
  segment of the journal (`journal::stored_ids`) on each call that
  appends, to check that the id is new.
- (b) each append rewrites its whole segment as a new git blob, and
  nothing ever packs the objects (the `git` commands Scheda runs carry
  `gc.auto=0`, and libgit2 never repacks). STO-5 describes the append as
  writing one line; it writes the segment.

*Probe:* 1000 appends of 200-byte payloads to one journal: the segment
is 209 KB and `.git/objects` 61.5 MB (295 times); the last 100 appends
took 6 times as long as the first 100. At the default 8 MiB segment the
objects of one segment reach tens of GB. *Change:* (a) check the id with
the id map and the segments changed since it (`IdMaps::find`, as `Get`
does), plus the ids the call has appended; a journal with no map is
scanned, as today. (b) is D3. *Check:* a test that counts blob reads
(`repo::blob_reads`) shows an append to a journal with a map reads the
open segment and no other; the object database after 1000 appends stays
within a bound D3 sets.

**R4. A refusal of the embedder is charged to a text that did not cause
it.** `tables.rs` `embed_wanted` (821–852) marks `digests[done]`, the
first text without a vector, as refused when the stream ends with
`REFUSED`. An embedder that batches refuses a whole portion: the CLI's
HTTP embedder sends portions of 2048 and maps a 400, 413, or 422 to
`REFUSED`; Knowqore's embedder refuses a batch whose reply is unusable.
The first text of the failed portion then loses its vector though it was
fine, the rest is sent again, and each try fails the same way until the
culprit is passed: up to one request per text, and innocent Documents
without vectors, silently. *Change:* on `REFUSED`, send the text it
names alone; refused alone, it is refused; embedded, keep its vector
and go on with the rest (bisect the rest when it fails again, so a
portion of `n` costs `log n` calls per culprit). The contract stays;
the comment of `EMBEDDER_ERROR_CODE_REFUSED` says Scheda confirms a
refusal on the text alone. *Check:* `tables::tests`: a `FixedEmbedder`
of portion 8 that refuses any portion holding the text `bad`: every
other Document keeps its vector, the one with `bad` has none, and the
calls stay within the bound.

### 2.2 Correctness and behaviour (Medium)

**R5. `Vault.Check` fails instead of reporting, and reads no marker.**
`check.rs` `run` (105) loads the declarations with `declarations_in` and
returns its error, so one collection file that does not load fails the
whole `Check` (the probe of R1: `Check` failed with `INVALID_ARGUMENT`).
The rpc says it fails only when it cannot run, and lists the collection
declarations among what it reports; GIT-14 says `Check` reads the marker
first, and it does not. *Change:* read the marker (another generation,
or a broken one, is a problem with path `.scheda.yaml`, and the other
checks are skipped); load the declarations file by file, each failure a
problem with its path, and run the other checks over the ones that load.
*Check:* `check::tests`: a broken collection file and a marker of
another generation are problems of the report, not failures of the call.

**R6. Setting a port does not wake the indexer.** IDX-9 says the indexer
wakes at once when a port is set; `Vault::set_projector`,
`set_embedder`, and `set_reranker` (`vault.rs` 239–261) install the port
and signal nothing, so the first round with the new port waits for
`index_poll_ms`, or for the back-off of a failed round (up to eight
times it). *Change:* call the indexer's signal after a port is set or
removed. *Check:* a Vault with `index_poll_ms` 1 h that lacks its
embedder indexes within seconds of `SetEmbedder`.

**R7. `Mirror.Pull` holds the write lock while `git fetch` runs.**
`mirror.rs` `pull_now` (345) takes the lock, then fetches over the
network, then joins. Every writer of every process waits for the
network, and fails `ABORTED` past `write_timeout_ms` behind a slow
mirror. The fetch also goes through `FETCH_HEAD`, shared by every pull,
and an empty mirror is told by the text of `stderr` (`git.rs`
`missing_ref`, 51). *Change:* fetch without the lock, with a glob
refspec into refs of Scheda's own
(`+refs/heads/*:refs/scheda/mirror/*`, `--no-write-fetch-head`); then
take the lock, read the branch's ref with git2 (absent: nothing to
pull), and join as today. This changes STO-4 ("a pull holds the lock")
and its test; D4. *Check:* `tests/mirror.rs`: a write of another handle
is not held up by a fetch that waits on the network; a pull of an empty
mirror is nothing to pull without reading `stderr`.

**R8. git failures are classified by message text.** `error.rs`
`GitFailure::classify` (263) reads `stderr`; `git.rs` `missing_ref`;
`repo.rs` `open_fault` (105) matches libgit2's message for SHA-256 and
reftable repositories. `no-workarounds.md` names this. For the network
(credentials, a host that does not answer) the git command line gives no
other signal; for the rest there is one: R7 removes `missing_ref`;
`git push --porcelain` reports each ref's status in a documented
machine format (a `!` flag and the reason); `open_fault` can read
`extensions.objectformat` and `extensions.refstorage` from the
repository's config before it opens it. *Change:* those three; D5 for
what stays.

**R9. Document files get fixed modes whatever the umask.** `commit.rs`
`set_mode` (1095) sets `0644`, or `0755` for an executable, on every
file a write puts in the work tree. A person whose umask is `077` gets
world-readable Documents. git itself honours the umask. *Change:* create
with `0666` / `0777` and let the umask apply; for an executable add the
`x` bits to the mode the file got; isolated files stay owner-only
(`restrict_record`). *Check:* `commit::tests` with umask `077`: a
written file is `0600`.

**R10. A journal line is found by parsing its whole segment, once per
line.** `idmap.rs` `find` (383) and `journal.rs` `find_journal_scan`
(324) call `parse_segment`, which copies every line of the segment to
return one. `queries.rs` `read_document` (857) does that for each hit of
a query or search page with `include_raw`, and the rerank of a search
for each of `rerank_limit` hits: a page of 100 lines of one 8 MiB
segment parses 800 MB. *Change:* a search that walks the lines and
copies only the one it finds; a page (`Page`, `Hits`, the rerank read)
keeps the bytes of the last segment it read, by blob id. *Check:* a
`blob_reads` count: a page of 50 lines of one segment reads it once.

**R11. Every round that writes a table compacts it.** `store.rs`
`maintain` (748) runs `compact_files` after every round that wrote, with
a target of 16 384 rows: the last, partly full fragment is rewritten at
each round until it is full. With vectors (4 KB a row at 1024
dimensions) one write of one Document can rewrite tens of MB (by
arithmetic; not measured). IDX-9 says
a large fragment is never rewritten; it does not say a small one is
rewritten every round. *Change:* D6: measure the bytes written per
single-Document round on a 10 000-row vector table, then compact only
when the small fragments pass a count.

**R12. The Projector is asked for its columns at every look.**
`indexer.rs` `plan` (681) runs `tables::plan` at every round that is not
current, so a Vault that is stale or lacks a port asks for the columns
of every collection every `index_poll_ms`, for as long as it runs; and
`queries.rs` `Reading::of` (70) asks again for each query, count, and
search. The `0.22` contract required once per state of `HEAD`
(`20`, P5: "asks its Projector for the columns once per state of `HEAD`,
not at every look"); the move to Lance lost it with its test. *Change:*
keep the plan per projection and per commit of the declarations, in the
Vault; `SetProjector` and a new `HEAD` drop it. *Check:* a counting
Projector: a stale Vault asks once per `HEAD`; ten queries ask once.

**R23. A call that waits with `not_before` on an Index stale for its
Vault waits out its timeout** (found while S4 was built). `queries.rs`
`not_before` waited for the Index to reach the token before `Reading::of`
looked at the ports, so a Vault whose Index was built with another
projection, behind `HEAD`, waited `index_wait_timeout_ms` (60 s) and
ended `DEADLINE_EXCEEDED`, retryable, though a call without `not_before`
is `INDEX_STALE` at once. The test of the fix also showed a `Reindex`
asked while a round that had found the Index stale was ending was
dropped. *Change:* the wait asks `Reading::of` while no rebuild of this
handle is asked for or running, and fails `INDEX_STALE` at once; the
request is cleared only by the round whose look saw it.

### 2.3 Documents (release blockers)

**R13. The crate documentation still says there is no Index.**
`lib.rs` 12–15 (the front page of the rustdoc): "This version of Scheda
has no Index to query or search …"; `vault.rs`: the struct (57–60: the
Index is "the maps from the id of a journal line"), `Vault::open`
(111–115: a port is `UNIMPLEMENTED`), `set_projector` (236–238).
*Change:* say what the build with `index` does and what one without it
does.

**R14. `CHANGELOG.md` `[Unreleased]` is not a release note.** Line 11:
"The breaking `0.22.0` is under way on the branch `proto-contract`".
*Fixed* (34–63) lists defects of the Tantivy Index that the release
removes (`index_missing`, the projector dump, `reindex.lock`,
`ids.lock`). Line 384 says `tantivy` 0.27.0 keeps its format, against
*Removed* (Tantivy is gone). "The `0.22` Index" (322, 443) names a
version that was never released: it is the Index of `0.21` and earlier.
*Change:* rewrite the section for a caller of `0.21.0`: what changed,
what is gone, what to do; drop the fixes of removed code and the Tantivy
bump; add R1–R12 as they land; the version per D1.

**R15. Normative text that contradicts the code.**

- `04` STO-4, the `Index.Reindex` paragraph: queries and searches are
  `INDEX_REBUILDING` during a rebuild (the live Index answers), and "a
  build without `find`" (the feature is `index`).
- `04` STO-5: the journal technique "write one complete line" (the
  segment is rewritten; R3).
- `04` GIT-14: `Vault.Check` reads the marker (R5).
- `05` SCH-9 and its key table: "the Tantivy Index"; a `Query` that
  names an excluded collection is refused only when it filters or
  orders (as `13` says).
- `07` LIB-20: the declarations are read "from the work tree"; they are
  read from `HEAD`.
- `09` ISO-10: "`Index.Reindex` removes a leftover" nested Index; no
  code does.
- `10` §2–3: "The crate has no default features. `find`, `search`,
  `simd` … opt-in"; "Tantivy BM25; … USearch HNSW"; two items numbered 3.
- `13` §5 step 4: one embedder stream "per round" (one per 256 files);
  §6 step 7: the summary needs "no extra I/O" (it scans for a vector).
- `14`: `vllm==0.29.0` (the lock has 0.32.0); a batch reads stdin
  "lazily" (whole, before the Vault opens).
- README: the status paragraph (built "on the branch `proto-contract`");
  Build ("Git operations invoke the system `git` executable": only
  `clone`, `fetch`, `push` do); the row of `12` ends "P0–P16 in |"; no
  row for `21`'s release box or for this document.
- `rust/cli/src/args.rs` 200, the help of `delete-collection`: "Delete a
  collection and its Documents" (a collection with Documents is refused).
- `12` line 1532: an open box for work that is done.
- Code comments that cite a document code (`code-comments.md`):
  `crypto.rs` 4, `journal.rs` 7 and 697, `documents.rs` 4 and 599,
  `check.rs` 4 ("D20", "D15").

**R16. Checks that name tests that do not exist.** Normative text cites
these, and no function of that name exists: `03` MOD-16
`a_vault_opened_only_without_ports_finds_its_lines_without_an_index`;
`04` STO-4 and `10`
`reindex_returns_before_the_rebuild_a_second_is_aborted_and_the_maps_match`;
`05` `a_collection_that_leaves_the_lexical_index_out_is_searched_by_the_vector_leg`,
`a_collection_that_leaves_the_vector_index_out_is_searched_by_the_lexical_leg`,
`a_collection_with_both_legs_left_out_is_unimplemented_when_named`,
`an_encrypted_collection_named_is_unimplemented_and_skipped_when_not`,
`an_isolated_collection_is_scanned_only_when_named_and_one_without_an_index_is_refused`
(also `09`), `a_projector_that_refuses_a_document_leaves_it_out_and_the_round_goes_on`
(under a `lexical::tests` that is gone), `the_marks_are_read_and_decide_what_is_indexed`;
`07` LIB-5 `or_fault_codes_a_foreign_error_and_keeps_it_as_source`;
`09` ISO-4 `the_plaintext_of_an_encrypted_collection_is_never_in_the_index`.
Some behaviours have tests under other names; three have none, and the
documents say so: the plaintext of an encrypted collection is absent
from the Index files (IDX-4, ISO-4); a column without `fts` is not
searched (IDX-6); a Document of one Vault never appears in a query of
another (GIT-5). *Change:* point each check at the test that holds it,
and write the three missing tests.

### 2.4 Robustness and cleanup (Low)

**R17. Tests that order events by sleeping.** `objects.rs` 188 and 196
(the write must be waiting before `Close`), `mirror.rs` 964 (the pull
must hold the lock before the write), `runtime.rs` 470, 487, 535,
`index.rs` 408 (asserts that nothing moved 150 ms after a failure, which
holds trivially when the round has not run yet). On a slow runner they
can fail or pass for the wrong reason. *Change:* wait for an observable
state (the watch item of the failed round; the lock file held, by
`try_lock`), not for time; where only a margin works, say so in the test.

**R18. `k`, `offset`, `rerank_limit`, and a query's `limit` have no
bound.** `search.rs` `Ask::depth` (170) passes `offset + k` (up to about
8.6 billion) to Lance's full-text limit and `nearest`. A test covers
`k` 4 000 000; nothing covers `u32::MAX`. The review of 5 October (`18`
R3) found aborts there with Tantivy. *Change:* D8: test first; clamp the
depth to the rows of the table if Lance allocates by the limit.

**R19. Dead and duplicated pieces.**

- `.claude/notes/index-probes.patch`: probes for the tree of `b31fb46`,
  the Index before Lance; `index-risks.md` keeps only what is deferred
  (deleted in S4).
- `buf.yaml` `breaking: use: FILE`: no breaking check runs, and
  `proto-changes.md` keeps no wire compatibility.
- `pyproject.toml` version `0.18.6`; `Cargo.toml` `repository`,
  `rust/scheda/Cargo.toml` `documentation`, and the links at the end of
  `CHANGELOG.md` name `amsokol/scheda`, while `origin` is
  `sonalect/scheda`.
- `rust/scheda/Cargo.toml` lists `serde_json` in `[dev-dependencies]`
  and in `[dependencies]`.
- `rust/BUILD.bazel` `//rust:lint` runs clippy over `scheda_thin_test`
  but not over `scheda_test`, the unit tests with every feature.
- The message of a panic payload is read in three places
  (`rust/blocking/src/lib.rs` 148, `ports.rs` `task` 257,
  `indexer.rs` `panicked` 1162): one helper in `scheda-blocking`.
- `collections.rs` checks the writes twice (`Writes::check` before the
  lock, `write` again under it); `repo.rs` `Snap::size` keeps a
  `let _ = kind;`; `idmap.rs` 373–378: the comment of `find` sits on
  `segment_of`.
- `indexer.rs` `Indexer::stop` reports a cancelled task as `PANICKED`;
  `commit.rs` `commit_in`: when emptying the note fails after a failed
  swap, that error replaces the swap's `ABORTED`.

**R20. The CLI Projector puts a B-tree on whole Documents.**
`rust/cli/src/projector.rs` 37 marks `body` `scalar_index`: a B-tree
over the full text of every Document, about the size of the texts again,
for a `term` or `prefix` on the whole body. A filter on `body` works
without it (a scan). D7.

**R21. A query or search of the Vault reads its collections one after
another.** `queries.rs` `find_in_vault` and `search.rs` `run` await each
table in turn. *Change:* run the tables of a call together, a few at a
time. Measure first; not for the release.

**R22. Each CLI process starts an indexer round.** A short command that
only reads opens the Vault, and its indexer begins a round (with an
embedder flag, an embedding) that `Close` cuts at its next step, losing
what the round had done. Noted for a later contract question (a Vault
that does not index); not proposed now.

### 2.5 Accepted decisions, not reopened

- A passphrase costs one scrypt per Document read or written (ISO-7: the
  owner chose not to cache a derived key).
- A table version, and an old generation, is removed an hour after it
  was replaced (`21` C23–C25: no read of the Index outlives its call).
- BM25 statistics are per collection (`13` §6), so the lexical scores of
  two collections are compared by rank only in the fusion.
- The Model2Vec model id hashes `config.json` and the size of the
  weights, not the weights (`14`).
- The git command line for `clone`, `fetch`, and `push`, for the
  person's credentials and SSH configuration (GIT-1).

---

## 3. Decisions of the owner (20 October 2026)

Recorded so they are not asked again.

**D1. The version of the release is `0.22.0`.** `0.22.0` was never
released from `main`, and `versioning` says not to skip numbers. `21`
L10, which names `0.23.0`, follows.

**D2. Every finding is fixed before the release.** R1–R21, not only the
blockers the review proposed. R22 is a design question, not a defect:
it stays a note.

**D3. The default `segment_max_bytes` becomes 1 MiB (R3 (b)).** Every
append rewrites its segment, so the cap bounds that cost. A declaration
that sets the cap keeps it; a journal without one closes its segments
sooner, and closed segments are never rewritten. Packing the loose
objects is the implementer's: in process (libgit2's `PackBuilder`, so a
local write still starts no process, GIT-1), after a measurement on the
probe of R3, in this release (D2).

**D4. A pull fetches without the write lock (R7).** The fetch goes into
refs of Scheda's own; the lock is taken for the join only. STO-4 and its
test change with it. What a caller sees of `Pull` does not change.

**D5. One exception stays classified by text (R8).** `missing_ref`
goes with D4, the push rejection reads `git push --porcelain`, and
`open_fault` reads the repository's config first. Credentials and the
network stay classified from `stderr`, the one exception, written into
`no-workarounds.md` with its reason: the git command line gives no other
signal, and git runs with `LC_ALL=C`. An edit of `.claude/rules/` may
need the owner's permission.

**D6. Compaction (R11) is the implementer's.** Measure the bytes a
single-Document round writes on a 10 000-row table with vectors, then
compact only past a count of small fragments.

**D7. The CLI Projector drops `scalar_index` on `body` (R20).** The
projection becomes `scheda-cli/whole-file@2`; a CLI Index is
`INDEX_STALE` until one `index reindex` (an Index of `0.21` is rebuilt
anyway).

**D8. Bounds (R18) are the implementer's.** A test with `u32::MAX` for
`k`, `offset`, `rerank_limit`, and a query's `limit` first; clamp the
depth to the rows of the table inside the library if Lance allocates by
the limit; no bound in the contract.

**D9. tokio 1.54.0 goes into this release, inside its quarantine, on
the owner's word.** Published 19 October 2026, 16:33 UTC; the quarantine
of `dependency-quarantine.md` would end on 21 October, 16:33 UTC. Before
the bump, check as for vllm 0.32.0 (`p8-follow-ups.md`): the version on
crates.io was published by the tokio maintainers, and the tag
`tokio-1.54.0` in `tokio-rs/tokio` matches it. Record the exception in
`CHANGELOG.md`. Then the crate_universe repin (`cargo-bazel-lock.md`).

---

## 4. Stages

Every stage lands before the release (D2). Consumer-visible changes
(`consumers.md`), each into `CHANGELOG.md`: R1 (new refusals of `Pull`),
R2 (no `INDEX_STALE` on a flag change), R4 (which texts lose a vector),
R5 (`Check` reports more), R7 (a pull no longer blocks writers), R9
(file modes follow the umask), D3 (the default segment cap), D7 (the CLI
projection), D9 (tokio 1.54.0).

### S1. Correctness

- [x] R1. `Pull` refuses another generation and a broken collection file
      (`tests/mirror.rs`:
      `a_pull_of_a_mirror_of_another_generation_is_generation_and_changes_nothing`,
      `a_join_with_a_mirror_of_another_generation_is_generation_and_changes_nothing`,
      `a_pull_of_a_collection_file_that_does_not_load_is_invalid_argument_naming_it`,
      `a_pull_of_a_commit_without_the_marker_is_not_a_vault_and_changes_nothing`,
      `auto_sync_at_open_keeps_a_refused_pull_as_the_sync_failure`).
- [x] R2. The exclusion flags change a table, not the Vault's freshness
      (`tables::tests::flipping_an_exclusion_flag_rebuilds_the_table_and_leaves_the_index_fresh`).
- [x] R3 (a). An append checks its id through the id map (`idmap::tests`:
      `an_append_to_a_journal_with_a_map_reads_the_open_segment_and_no_other`,
      `an_id_the_map_names_or_a_segment_changed_since_it_holds_is_already_there`,
      `a_journal_with_no_map_refuses_a_duplicate_as_well`,
      `a_lookup_with_no_map_reads_the_segments_only_as_far_as_its_id_and_each_once`).
- [x] R4. A refusal is confirmed on the text alone
      (`tables::tests::a_refused_portion_costs_only_the_text_that_causes_it_its_vector`).
- [x] R5. `Check` reports a broken declaration and reads the marker
      (`check::tests`: `a_collection_file_that_does_not_load_is_a_problem_and_the_other_checks_go_on`,
      `a_marker_of_another_generation_is_the_one_problem`,
      `a_commit_without_the_marker_is_the_one_problem`).
- [x] D3. The 1 MiB default
      (`idmap::tests::a_journal_with_no_cap_rotates_at_one_mebibyte`).

### S2. Indexer and ports

- [x] R6. Setting a port wakes the indexer (`tests/search.rs`:
      `setting_the_embedder_wakes_an_indexer_that_waits_for_it`,
      `setting_an_embedder_ends_the_pause_after_a_failed_round`).
- [x] R12. The Projector's columns once per installed Projector and
      `HEAD`, kept in the port (`tests/search.rs`:
      `the_projector_is_asked_for_columns_once_per_collection_installed_projector_and_head`).
- [x] R10. A journal line is found without copying its segment, and a
      page reads a segment once
      (`queries::tests::a_page_of_journal_lines_reads_their_segment_once`,
      `search::tests::the_rerank_read_of_journal_lines_reads_their_segment_once`,
      `journal::tests::the_borrowed_walk_and_find_line_follow_the_rules_of_parse_segment`).
- [x] R11 (D6) and R18 (D8). Measured on a table with a 1024-dimension
      vector column: a single-Document round on a tail of 4 000 unindexed
      rows wrote 4.25 MB before and about 20 KB after; a build of 10 000
      rows in ten rounds left 224 MB before and 71 MB after. A table is
      compacted once it holds eight small fragments, or one with more than
      half its rows deleted (`store::tests`:
      `rounds_of_one_document_compact_only_once_the_small_fragments_pile_up`,
      `a_fragment_is_rewritten_once_most_of_its_rows_are_deleted`). Lance
      allocated by the limit (`k` = `u32::MAX` aborted on 34 GB); a leg's
      depth is bounded by the rows of the table (`tests/search.rs`:
      `k_offset_and_rerank_limit_at_their_maximum_answer_with_the_hits_there_are`,
      `a_search_through_the_vector_index_finds_the_exact_nearest_and_bounds_k_at_its_maximum`;
      `tests/query.rs`:
      `limit_and_offset_at_their_maximum_answer_with_the_documents_there_are`).

### S3. Mirror and files

- [x] R7 and R8 (D4, D5), with the exception written into
      `no-workarounds.md`. A pull fetches into `refs/scheda/mirror/*`
      under `.scheda/fetch.lock` and takes the write lock for the join
      only; a push is read from `--porcelain`; the repository format from
      its configuration (`tests/mirror.rs`:
      `a_write_of_another_handle_is_not_held_up_by_a_fetch_waiting_on_the_network`,
      `two_pulls_at_once_from_two_handles_both_succeed`,
      `a_pull_after_the_mirror_deleted_the_branch_is_nothing_to_pull_and_forgets_it`,
      `a_lock_on_a_remote_tracking_reference_fails_neither_a_pull_nor_a_push`,
      `a_pull_from_an_empty_mirror_is_nothing_to_pull`; the porcelain
      parser in `git::tests`;
      `repo::tests::a_repository_the_library_cannot_read_is_unimplemented`,
      a linked work tree included).
- [x] R9. The umask decides the modes
      (`commit::tests::the_umask_decides_the_modes_of_the_files_a_write_puts_in_the_tree`,
      run in a child process under `umask 077`).
- [x] D3: packing of the loose objects, after its measurement. 1000
      appends of 200-byte lines: `.git/objects` 68.5 MB in 4008 loose
      files before, about 7 MB at the worst in 8 packs after; the write
      that packs takes about 0.25 s (`pack::tests`:
      `many_appends_leave_few_loose_objects_and_a_small_database`,
      `a_merge_takes_the_smallest_packs_and_loses_no_object`,
      `a_pack_a_keep_file_protects_is_not_merged`; `tests/pack.rs`:
      `four_hundred_appends_to_one_journal_are_packed_and_stay_readable`,
      `the_nested_repository_of_an_isolated_collection_is_packed_too`).

### S4. Dependencies and cleanup

- [x] D9. tokio 1.54.0 after its check (published by an owner, the
      package matches the tag `tokio-1.54.0` at `66d34e45`); the repin; a
      dropped push still kills its `git` (`tests/runtime.rs`:
      `a_push_dropped_midway_leaves_the_commit_local_and_the_next_push_carries_it`).
- [x] R17, R19, R20 (D7), R21 (after a measurement). R17: a waiting
      write is seen by its open lock file (`write_waits_for_the_lock`); a
      sleep stays only where nothing shows the state and it bounds an "at
      most", with a comment. R19: all twelve items. R20:
      `scheda-cli/whole-file@2` (`tests/cli.rs`:
      `an_index_of_another_projection_is_stale_and_one_reindex_mends_it`).
      R21: four collections at once, 2.4 times faster for a query and 2.5
      for a search on eight collections (`tests/search.rs`:
      `a_call_over_many_collections_answers_as_the_calls_over_each`).
- [x] R23. A wait on an Index stale for the Vault fails `INDEX_STALE` at
      once; a rebuild this handle asked for is waited for
      (`tests/search.rs`:
      `a_call_with_not_before_on_an_index_stale_for_the_vault_fails_at_once`,
      `a_call_with_not_before_waits_for_the_reindex_this_handle_asked_for`).

### S5. Documents and the release

- [ ] R13, R14, R15, R16 in one pass; the three missing tests of R16.
- [ ] The version `0.22.0` (D1, `versioning`), the README row for this
      document, `21` L10's box.

---

## Appendix A. tokio 1.54.0

Published on crates.io on 19 October 2026, 16:33 UTC; the quarantine
(`dependency-quarantine.md`) ends on 21 October 2026, 16:33 UTC. Scheda
pins `=1.53.3`. MSRV 1.85 (Scheda's floor is 1.99.1). The changelog was
read at the tag `tokio-1.54.0`. The owner took it into this release
(D9).

What touches Scheda:

- **sync: preserve FIFO lock order when coop budget is exhausted
  (#8371).** The writes of one Vault take their turn on a
  `tokio::sync::Mutex` used as a fair queue (`commit.rs`, `Committer::turn`;
  LIB-29: "first come, first served"; LIB-32: "applied one after
  another, in the order they were made"). Under coop budget exhaustion
  1.53 could break that order. This is the strongest reason to take the
  release.
- **sync: acquire/release orderings in `Notify` (#8325).** The gate of a
  Vault (`gate.rs`) and the indexer's signal (`indexer.rs` `Signal`) wait
  on `Notify` (`notified`, `enable`, `notify_waiters`, `notify_one`).
- **runtime: fix a `spawn_blocking` hang when only scheduler workers
  exist (#8408).** Every blocking step of Scheda goes through
  `spawn_blocking` (`scheda-blocking`), and a test runs with a blocking
  pool of one thread.
- **process: `mio::unix::pipe` for Unix child stdio (#8603).** The
  `git` children of `clone`, `fetch`, and `push` (`git.rs`, `output()`
  with `kill_on_drop`). To check: the kill of a dropped push.
- **time: the runtime handle removed from timers (#8458).** `sleep`,
  `timeout_at`, `sleep_until` in the write lock wait and the
  `not_before` wait; no API change.
- **sync: `watch::Receiver::is_closed` (#8481).** `Index.Watch` waits on
  a `watch` channel; it could tell the end of the indexer without a
  `changed()`. Small; not needed.
- From 1.53.3, already pinned: `Semaphore` waiters woken outside the
  lock, paused time with I/O on `current_thread`. Nothing else in the
  release (fs, net, wasm, `copy_buf`, `BufReader`, named pipes) is used.
