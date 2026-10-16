# Flaky tests under a fast disk

Since `fcb21e9` the Linux gate keeps test Vaults on tmpfs
(`.bazelrc`, `--test_tmpdir=/dev/shm/...`). Without fsync cost, races the
slow disk hid show up. Fixed there: torn `format` / projector dump reads,
journal `id` lines lost across a generation publish, `EINTR` in the CLI
HTTP client, and three test races.

Seen once each in 30 uncached gates (16 October 2026), not reproduced in
144+ isolated stress runs, open:

- `isolated_test::isolated_write_has_no_nested_scheda_and_reindex_counts_it`:
  `reindex` → `locked` ("vault reindex lock is held"). Suspect the
  background `rebuild_generation` (it takes `reindex.lock`, triggered by a
  damage probe or `Pass::Rebuild`) on a fresh Vault; `acquire_lock` waits
  only 8 × 10 ms.
- `scheda_test::vault::tests::background::format_5_generation_is_rebuilt_in_the_background`:
  "no new generation published" within 20 s.

**Why:** a rare red gate wastes a run and hides real failures.

**How to apply:** to reproduce, loop
`bazel test //... --nocache_test_results` and copy
`.bazel/testlogs/<pkg>/<target>/test.log` of a failed target at once. The
CLI `EINTR` source was never identified (no signal handler in this repo).
