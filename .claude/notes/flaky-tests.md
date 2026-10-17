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

**Why:** a rare red gate wastes a run and hides real failures.

**How to apply:** to reproduce, loop
`bazel test //... --nocache_test_results` and copy
`.bazel/testlogs/<pkg>/<target>/test.log` of a failed target at once, or
run the cargo unit-test binary eight at a time with `TEST_TMPDIR` on
`/dev/shm`. A test that asserts the indexer has *not* acted yet must pause
it (`Indexer::pause`). The CLI `EINTR` source was never identified (no
signal handler in this repo).
