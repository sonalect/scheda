# Tests fail, they never hang

A test that goes wrong fails within minutes. It never waits forever and
never leaves a process behind. The owner's decision (21 October 2026),
after a failing assertion hung a test run instead of failing it.

- A gate a test closes (a flag that holds a port call, a hook that holds a
  round) is opened by a guard's `Drop`, not by a line after the
  assertions. A held sync port blocks a thread of the blocking pool, and
  dropping a tokio runtime waits for those threads: a panic while the gate
  is closed hangs the test. `Calls::hold` in `tests/search.rs`.
- A child process a test starts is held by a guard that kills and reaps it
  on drop (`common::Child` in `tests/integration`), and is waited for with
  a deadline (`Child::finish`; `finish` in the CLI tests). No unbounded
  `.status()`, `.output()`, or `wait_with_output()` on a test binary or
  on the command line. Plain `git` on a local scratch repository is fine.
- Every polling loop has a deadline: `wait_until`, `soon`, `within`,
  `tokio::time::timeout`.
- A mutation run that proves a test catches a defect runs under
  `timeout`, and the file is restored by the same command.

```rust
// BAD — a failed assertion leaves the Projector held; the runtime hangs
fx.calls.held.store(true, Ordering::SeqCst);
assert_eq!(state.version, before);
fx.calls.held.store(false, Ordering::SeqCst);

// GOOD — the guard lets go on success and on panic
let hold = fx.calls.hold();
assert_eq!(state.version, before);
drop(hold);
```
