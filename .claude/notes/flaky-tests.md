# Flaky tests under a fast disk

Since `fcb21e9` the Linux gate keeps test Vaults on tmpfs
(`.bazelrc`, `--test_tmpdir=/dev/shm/...`). Without fsync cost, races the
slow disk hid show up. All found so far are fixed (17 October 2026; 30
uncached gates in a row green):

- Torn `format` / projector dump reads; journal `id` lines lost across a
  generation publish; `EINTR` in the CLI HTTP client (`fcb21e9`).
- The first isolated collection rebuilt the whole generation under
  `reindex.lock` (a caller's `reindex` then got `locked`).
- Lock files released by `close` alone stay held while another thread's
  forked `git` child has not run `exec`; a new handle then missed the
  indexer role (`format_5_generation_is_rebuilt_in_the_background`).
  `store::Flock` unlocks before close.
- Tests whose "not refreshed yet" precondition raced a late indexer round
  (a round `open` starts, or one a `wait_indexed` nudge left due) now pause
  the indexer around the check.
- Branch `proto-contract`, P5 (18 October 2026; 8 binaries x 16 runs,
  8 at a time on `/dev/shm`, green after the fixes):
  - The Tantivy writer lock (`.tantivy-writer.lock`, an `flock`) stayed
    held by a `git` child another test thread had forked, until it ran
    `exec`; the next round failed with `INDEX_BROKEN` "open the writer of
    the lexical index". 0.21 had a `LockBusy` backoff that the rewrite
    lost; `lexical::open_writer` waits it out again (about 2.5 s), then
    `ABORTED` retryable.
  - Tests that read `status()` after a failure saw the retry already at
    work (`role` true, or `last_error` cleared): read the watch item of
    the failed round instead. A test that read `role` before the first
    look saw `None`: wait for the look.
  - A wait on several Index files checked `commit` first, then the rest:
    a rebuild that started meanwhile removed `commit` after it was read.
    Check `commit` last (the rebuild removes it first).
- Gone with the search tests in work order 21, L8; kept as the lesson for
  L6 (wait for the write's token, never for a count of one side). Branch
  `lance-index` (19 October 2026, 1 run in 10):
  `//rust/scheda:async_test`, `searching::a_search_runs_on_both_runtime_flavors`
  (only `cat2` found) and
  `searching::an_async_embedder_that_takes_a_second_is_awaited_not_blocked_on`
  (the search ended before the slow embedder could answer). `corpus` in
  `tests/async.rs` waits for `Count` to reach 3, the lexical side, not for
  the vectors, so a search can run before them (`index-risks.md` item 4).
  Not fixed in `0.22`: work order 21 replaces it with the write's token
  (L7) and rewrites the search tests (L6).

- Leftovers, not failures (20 October 2026): a day of runs left about
  2,150 scratch folders in `/tmp` and 98 processes. A Vault dropped without
  `Close` lets its indexer finish its round on the blocking pool; the round
  wrote under `.scheda/index/` while `Scratch` removed the folder, and
  `remove_dir_all` stopped at the first `ENOTEMPTY`. Scratch folders now go
  when the test's thread ends (after its runtime, whose drop waits for
  blocking steps); tests on a `static` runtime (`consumer.rs`, the CLI
  tests) close their Vaults on drop. The processes were the mirror's side
  of the dropped `auto_sync` push (`sh`, `git-receive-pack`, the stalling
  hook), which killing `git` alone left running: the library now kills the
  command's process group. Check after a run:
  `pgrep -af '[g]it-receive-pack'`, `pgrep -af 'hooks/[p]re-receive'`.
  Not yet proven on Windows: the job object of `git.rs` (`tree` for
  Windows) and the Windows branch of
  `runtime::a_push_dropped_midway_leaves_the_commit_local_and_the_next_push_carries_it`
  (the hook writes `/proc/$$/winpid`) were only type-checked for
  `x86_64-pc-windows-msvc`; the first Windows CI run is their proof.

**Why:** a rare red gate wastes a run and hides real failures.

**How to apply:** to reproduce, loop
`bazel test //... --nocache_test_results` and copy
`.bazel/testlogs/<pkg>/<target>/test.log` of a failed target at once, or
run the cargo unit-test binary eight at a time with `TEST_TMPDIR` on
`/dev/shm`. A test that asserts the indexer has *not* acted yet must pause
it (`Indexer::pause`). The CLI `EINTR` source was never identified (no
signal handler in this repo).
