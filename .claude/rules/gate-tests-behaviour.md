# The gate tests behaviour, not performance

`bazel test //...` proves that the code works correctly. It does not
measure speed or memory. The owner's decision (21 October 2026).

- A test in the gate asserts behaviour: results, error codes, files,
  locks, what a crash leaves. It takes seconds. A deadline that keeps a
  test from hanging (`within`, `elapsed() < 5 s` on a call that must not
  block) is behaviour.
- A test that measures time, memory, or throughput, or needs a large input
  to show anything (thousands of operations, a release build), is
  `#[ignore]` and runs only by hand: in `memory::` (the target
  `//rust/scheda:scheda_memory_test`, tagged `manual`) or in a module
  such as `measure::` run by Cargo. Its numbers go to a note or the
  CHANGELOG.
- A property that a performance design promises (a bounded number of
  packs, a cap) is proven in the gate on a small input or by simulating a
  pure function, not by a big run.

- Every test writes in memory, never on a disk (the owner's decision,
  22 October 2026): under `TEST_TMPDIR` when it is set (Bazel sets it to
  `/dev/shm/scheda-bazel-test`, a tmpfs, in `.bazelrc`), otherwise under
  `std::env::temp_dir()` (`/tmp`, a tmpfs). Never under `target/`, the
  repository, or the home folder. A new scratch helper takes its base the
  same way: `std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from)`.
- When `/tmp` or `/dev/shm` fill up, by space or by number of files,
  remove the old test leftovers (`scheda-*` folders that no running test
  uses) without asking and go on (the owner's decision, 22 October 2026).

```rust
// BAD — in the gate
#[test]
fn a_hundred_thousand_writes_keep_reads_fast() {}

// GOOD — the gate checks the rule; the measurement runs by hand
#[test]
fn the_pack_count_stays_bounded_over_a_simulated_million_writes() {}

#[test]
#[ignore = "measures time and memory: run by hand"]
fn reads_after_a_hundred_thousand_writes() {}
```
