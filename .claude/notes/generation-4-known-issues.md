# Generation 4: known issues and deferred work

The owner's rule for the push to the demo (21 October 2026): functionality
of G1–G7 first. A race that shows only under load, or anything else the
demo will not exercise, is written here and deferred; the work goes on.
Slow tests run only after every G stage is done, and only with the owner's
explicit permission (`.claude/rules/measure-once.md`).

## Release

Released as 0.26.0 on 22 October 2026 (commit 2b033d2, tag v0.26.0,
<https://github.com/sonalect/scheda/releases/tag/v0.26.0>).

When G1–G7 work and the result holds up, publish the release (0.26.0,
breaking: `.claude/skills/versioning/SKILL.md`) without waiting for the
owner, as he asked (22 October 2026). The anchor consumer's agent then
adopts it: the release notes say what a consumer changes (docs/24 §5):
`OpenOptions.work_tree = WORK_TREE_NONE`, `initial_branch = "main"`, a new
Vault and a full learn instead of a migration.

## Deferred, to do after G7

0. **G6, the move of a 0.25 Vault by the command line**, after the
   release (my decision, 22 October 2026, for the owner's review): the
   anchor consumer starts with a new Vault and does not need it, and its
   proof needs the 0.25.0 command line, which the gate cannot build. Do
   it as a script plus one run against a fixture made with the 0.25.0
   binary built from its tag.

1. **Windows: a pack that cannot be removed** (owner's condition 2 on
   decision 3). A file another process (or this one) has open or mapped
   is not removed on Windows. The merge must not fail on that refusal:
   the old pack stays, readers go on, and the next merge removes it
   (a pack whose objects are all in another one is dropped). Needs a
   test with a hook that refuses the removal.
2. **Done 23 October 2026: the code of a missing object.** The owner chose
   a new code, `DATA_LOSS` (variant A of the proposal). `lib_fault` maps
   the classes of the Vault's own data (`Odb`, `Object`, `Zlib`, `Sha1`,
   `Tree`, `Tag`, `Index`, `Indexer`, `Reference`, `Repository`, `Config`,
   `Worktree`) and `HashsumMismatch` to it; the hand-made faults "the
   repository is not there" and a local tree with a non-UTF-8 path too.
   `REPLICATION` stays for the mirror and for what no code names.

3. **Done 23 October 2026: the time of the gate's `integration_test`.**
   The 90 s were gone before the work: 21 s in the gate, after the tests
   moved to tmpfs (22 October). Measured per test (libtest
   `--report-time`, each slow one alone too): most "slow" tests were slow
   only under contention. Fixed: the exact-search fixture 20 000 → 10 400
   rows (just past the vector index's threshold; 17 → 7 s); the path budget
   runs on one runtime (`common::once_on_multi_thread`; 7 → 4 s); the
   empty-batch test waits 0.5 s for the refused write, not 2 s, and its
   copy in `runtime.rs` is gone; the two port-change tests repeat 10 and 5
   times in the gate (under 1.2 s each) and 200 and 50 times in their
   `…_200_times` twins, ignored, run by hand (owner's decision). Measured
   with Cargo on 32 cores: the sum of the test times 311 → 280 s, the run
   24 → 19 s. The run is now bound by contention, not by one test: tests
   under 1 s alone take 10–13 s side by side.
   Left as they are: the pack tests (400 appends: the merges must run many
   times) and the budget's 10 050 Documents (the vector index's files are
   part of what it measures).
4. **Done 23 October 2026: the G3 check of a listing from an id prefix.**
   `documents::tests::a_listing_of_an_id_prefix_reads_only_the_trees_on_its_way`
   counts the folders `Snap::list` reads (`repo::listed`, tests only): 4 of
   34 for a prefix, all 34 for a whole listing. The code needed no change.

5. **Slow measurements: done 22 October 2026** (the section
   "Measurements" below).

## Facts for the documents (G7)

- `0.25` on a Vault without a work tree (run of the 0.25.0 CLI built from
  its tag, 22 October 2026): `info` and a write answer `INVALID_ARGUMENT`
  ("the path is neither an empty folder, a git work tree, nor a Vault"),
  `--mode open-existing` answers `NOT_A_VAULT`; nothing in the Vault
  changed (every file, size, and mtime). P2 predicted `ABORTED`: correct
  the text.
- Syncs counted with `strace` (22 October 2026): with a work tree a write
  13 at any depth; without one a write 6 at any depth and a batch of 500
  Documents 6.
- The bound of 32 packs holds whenever no merge is replacing packs: a
  merge keeps its new pack beside the ones it replaces until it has
  removed them, so for that moment there is one more. The D8 amendment in
  docs/24 says "at every moment": correct it.

## Known issues (races under load)

1. **Done 23 October 2026: Lance's files after a cut round.** A round
   narrows the modes of the Index however it ends (done, failed, stopped
   by `Close`), with the lock still held; a killed round's files keep the
   umask's modes until the next round, which walks the whole Index.
   Tested by `tables::tests::a_round_stopped_after_lance_and_before_the_commit_is_done_again_and_nothing_else`;
   the cut tests of `commit::tests` no longer wait for the round.
2. **`pack::tests::close_cancels_a_running_merge_and_holds_no_lock_afterwards`
   is ignored.** Under the load of `bazel test //...` the pack lock was
   still held right after `Packer::stop` returned, in both test targets;
   under cargo it passed. No holder was found by reading the code (the
   stop awaits the merge's blocking step, which drops the lock file; the
   fallback read takes it shared only while it reads). Find the holder:
   the design promises no lock after `Close`.
3. **Anonymous memory grows with writes**: about 1.2 KB a write through a
   Vault without a work tree (13 MiB after 2 000 writes, 136 MiB after
   100 000; release, tmpfs, 22 October 2026), about 0.5 KB a write at the
   pack level (77 to 126 MiB). Not known whether it levels off (caches) or
   leaks (the overlay object database of each write, the merges' cache of
   pack sizes by name, libgit2's registry of packs). Measure the memory at
   several points of a run and find the holder.

## Measurements (22 October 2026, release, tmpfs, one run each)

- Pack level, 100 000 writes with the merge task: 79 s (about 0.8 ms a
  write); 14 packs after 2 000 and after 100 000; a read 15.4 then 8.9
  us; anonymous memory 77 then 126 MiB.
- The worst merge at the cap: 192 000 objects, 80 MiB, 0.3 s, peak +9
  MiB (libgit2's packer: 13 to 17 s and +114 MB).
- Reads at 100 000 Documents: 7.5 us with 1 pack, 38.9 with 100, 116.8
  with 500 (synthetic; the bound keeps a Vault under 32 below the cap).
- A Vault without a work tree, 100 000 writes of 2 KB, ids l<32 hex>:
  139 s; a write 1.16 ms after 2 000 and 1.45 ms after 100 000 (0.25 with
  a work tree: 52 to 62 ms at 100 000 Documents); a read 34 then 47 us;
  10 then 20 packs; memory as in known issue 3.
- A write into a collection of N Documents, a Vault without a work tree:
  at 100 000, 0.9 ms (max 8.1 ms), a get 25 us; at 1 000 000, 1.1 ms (max
  10.1 ms), a get 46 us (0.25: 123 ms and 1.34 s, git's index).
