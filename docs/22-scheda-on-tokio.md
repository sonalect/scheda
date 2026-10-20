# 22. Scheda on tokio

Document 22 of the Scheda package. Version 0.2, 19 October 2026.

Status: approved by the owner on 19 October 2026, decision by decision
(§6). D3, D4, D9, D11, and D12 were changed or completed with him; the
others were accepted as proposed.

Work order: make Scheda an async library on tokio. Its objects (`Scheda`,
`Vault`, `Collection`, `Mirror`, `Index`) are async only, and a caller
gives them a tokio runtime. The Index on Lance (`21`) then runs on that
same runtime: one runtime in the process besides Lance's own `lance-cpu`
pool, no runtime of Scheda's, and no bridge from blocking code into
async code.

The work goes on the branch `lance-index`, after `21` L1 and the removal
of the old Index (`21` L8, moved forward), and before the stages of `21`
that write the new Index (L2 onwards), so that the Lance store and the
indexer are written async once and nothing that is about to go is
rewritten. One release, `0.23.0`, carries both work orders.

---

## 1. Why

Lance is async, on tokio (`21` D13). `21` first planned a private tokio
runtime inside Scheda for it, with the blocking objects handing each Lance
call to it. The owner chose instead to make Scheda async as a whole
(19 October 2026): a caller built on tokio, as the anchor consumer is,
then runs everything on its own runtime, and Scheda carries no runtime of
its own.

What Scheda does today (inventory of 19 October 2026):

- **Blocking work.** Git in process through `git2` (commits, trees,
  diffs, merges: `commit.rs`, `repo.rs`, `mirror.rs`, `documents.rs`) and
  as a child process for `clone`, `fetch`, `push` (`git.rs`); about 80
  `std::fs` sites with `fsync` and renames; two `flock` files
  (`commit.lock`, `index.lock`); age encryption (`crypto.rs`); the
  caller's blocking ports.
- **Threads.** One indexer thread per open Vault (`indexer.rs`), stopped
  and joined on `Close` and on drop; a helper thread in the CLI for
  `Index.Watch` (`exec.rs`).
- **Waits.** Condvars for the indexer's signal and for `Index.Watch`, the
  service gate that `Close` waits on (`service.rs`), and polling sleeps
  for the write lock (`commit.rs`), for a work tree another process is
  making (`vault.rs`), and for the Tantivy writer lock (`lexical.rs`).
- **The async form today** (`service/asynchronous.rs`, feature `tokio`)
  wraps the blocking objects: writes wait for the lock natively
  (`acquire_async`), everything else runs on tokio's blocking pool
  through protocontract's `Offload`. An async port is driven on the
  runtime current when it is installed (`ports.rs`, `awaited`).
- **The contract** (`rust/api/gen/contract`) has a blocking and an async
  trait for every service, and the bridges `Offload` (blocking to async)
  and `Blocking` (async to blocking), behind `gate_tokio_feature`.

---

## 2. Decisions

**D1. The objects are async only; tokio is required.** `Scheda`,
`Vault`, `Collection`, `Mirror`, and `Index` implement the async traits of
the contract (`SchedaAsync`, `VaultAsync`, …) natively, and no blocking
form of their own. tokio becomes a required dependency of `scheda` and
`scheda-api`; the feature `tokio` goes. Both runtime flavors work:
Scheda never blocks a thread of the runtime (D3) and never calls
`block_on` or `block_in_place`. *Recommendation:* accept.

**D2. A blocking caller wraps the objects itself.** The contract keeps
both trait forms and both bridges (the generator loses
`gate_tokio_feature`, since tokio is always there). A caller without async
code builds a multi-thread runtime and wraps a Vault with protocontract's
`Blocking`, which gives it `VaultSync` (`block_in_place` and
`Handle::block_on`). On a current-thread runtime `Blocking` refuses with
its own error (`CannotBlock`), not a panic and not a deadlock. Scheda
documents this and tests it once. *Alternative:* no blocking form at all.
*Recommendation:* keep the bridges; they cost Scheda nothing.

**D3. Waiting is async; doing is one blocking step, behind a gate the
compiler checks.** *Changed by the owner from "one blocking unit per
call".* The core is touched where it waits, and only there.

- *Waiting is async, never inside a blocking step:* the `git` processes
  (`clone`, `fetch`, `push`, through `tokio::process`, so a push to the
  mirror holds no thread for its seconds on the network), the write lock
  (D5), the wait at `open` for a work tree another process is making, the
  caller's async ports, Lance, and the indexer's signals.
- *Doing is one blocking step:* the smallest piece of work that must not
  be cut in two, run on tokio's blocking pool: one commit (blobs, tree,
  commit, ref, the files of the work tree with their `fsync`), the read of
  one Document, the tree difference of a round, the projection of one
  Document. Not the whole call, and not each system call: a commit cut at
  every `await` could be dropped between moving the ref and writing the
  files, a crash in miniature on every cancelled call.
- *No `tokio::fs`:* it runs each call on the blocking pool itself (no
  operating system offers async I/O on regular files to tokio), so it
  would add a hop per call and make nothing async. git2, `fsync`,
  `flock`, and age have no async form either.
- *The gate:* a step enters the core only through one helper,
  `blocking(|cx| core::f(cx, …)).await`. Each entry point of the core
  takes `cx: &Blocking`, a zero-sized token only that helper makes (a
  `cfg(test)` constructor for the core's own unit tests), so an async
  function that calls the core directly does not compile.

*Recommendation:* accept.

**D4. A dropped call stops at its next `await`.** The blocking step it is
in, if any, finishes (a thread of the pool cannot be stopped), and no step
after it starts. For a write: dropped while it waits for the write lock,
nothing is written and the lock is not taken; dropped during the commit,
the commit finishes and is stored, as if awaited, and the caller does not
learn it; dropped during the push to the mirror (`auto_sync`), the `git`
process is killed (`kill_on_drop`), the commit stays local, and the next
push carries it, as after a push that failed. A remote moves its ref only
at the end of a push, so a killed push leaves it either as it was or with
the commit. A read dropped midway stops after its current step and changes
nothing. A write is never cut in two: a commit is one step (D3). The rule
is written into the contract's comment of `Vault`, so a caller knows a
dropped write may have been stored. *Recommendation:* accept.

**D5. The write lock is waited for without a thread.** The turn inside
the process becomes a fair async lock (FIFO), and the `flock` on
`commit.lock` is taken with `try_lock` and an async sleep between tries
(1 ms doubling to 25 ms, as `acquire_async` does today), up to
`write_timeout_ms`. A blocking `flock` on the pool is not used: it cannot
time out, so a thread would stay stuck past the deadline. The same for
the wait on a work tree another process is making at `open`. The polling
is the honest form for a lock between processes, which tells no one when
it is released. *Recommendation:* accept.

**D6. The indexer is a task.** One task per open Vault, spawned on the
runtime that opens it. Its signal and `Index.Watch` become tokio
primitives: a `Notify` for a wake, a `watch` channel for the state, so a
watcher that drops its stream is gone at once (`14`'s known gap of
`index watch`). A round is an async function whose blocking parts (the
look at `HEAD`, the tree difference, reading files, a blocking port) run
as units (D3), whose Lance calls are awaited, and whose `index.lock` is
taken with `try_lock` inside a unit. A round runs as a task of its own,
so a panic in it reaches the indexer as a `JoinError` and becomes
`last_error` (`Panicked`), as `catch_unwind` does today. The poll for
writes made outside the Vault is a tokio interval of `index_poll_ms`.
*Recommendation:* accept; the indexer kept a thread, discussed after `21`
L0, is not taken.

**D7. `Close` waits; a drop does not.** `Vault.Close` shuts the gate (an
async wait for the calls inside it), stops the indexer, awaits its task,
and returns with every lock let go. A Vault whose references are all
dropped cannot wait in its destructor: it signals the indexer to stop,
and the task ends after its current round, in the background; until then
`index.lock` may still be held, and another Vault takes the role a round
later, as when an indexer ends today. A Vault lives on the runtime that
opened it: when that runtime shuts down, its indexer ends with it, and
the Lance commit protocol (`21` C22) leaves the Index sound.
*Recommendation:* accept.

**D8. Ports are called in their async form.** The Vault holds each port
as protocontract gives it and calls its async form: a blocking port runs
on the blocking pool through `Offload`, an async one is awaited, as a task
of its own so that its panic becomes `Panicked` and a dropped call aborts
it. The rule that an async port given outside a runtime is
`INVALID_ARGUMENT` goes: there is always a runtime. The CLI keeps its
blocking HTTP ports (`ureq`). *Recommendation:* accept.

**D9. Lance on the caller's runtime.** The Index awaits Lance's futures in
its own async code. `21` D13 is replaced by D1–D3 here; the private
runtime and the bridge into it, discussed after `21` L0, are not taken. Lance's own
`lance-cpu` pool stays: Lance makes it whoever calls it. Two more points,
from the sources of `lance-core` 13 (`utils/tokio.rs`):

- Lance sizes that pool from `LANCE_CPU_THREADS` and
  `LANCE_IO_CORE_RESERVATION`, read once per process into a `LazyLock`,
  with no API to set them in code, and panics on a value that does not
  parse. They are the application's to set: the pool is shared by every
  user of Lance in the process, and setting them from Scheda would take
  `std::env::set_var`, which is `unsafe` in Rust 2024 and changes the
  caller's environment behind its back. Scheda documents them and sets
  in code only what Lance takes per call (the threads of a compaction,
  read-ahead), as constants (`21` D11).
- The work of an object's call on Lance runs as a task of its own, as an
  indexer round (D6) and a port (D8) do, so a panic in Lance, from a bad
  variable or anything else, reaches the caller as `Panicked`, as the
  contract says of a panic in Scheda, and does not unwind into the
  caller's task.

*Recommendation:* accept.

**D10. The CLI is async.** `main` runs on a multi-thread runtime
(`#[tokio::main]`); every command awaits the async objects; the helper
thread of `Index.Watch` becomes a task with a timeout. Its flags, JSON,
and exit codes do not change (`14`). The Python driver drives the CLI and
does not change. *Recommendation:* accept.

**D11. Tests follow the line D3 draws.** The core is split into its
steps, which stay blocking, and the waiting that joins them, which moves
to the async layer; the tests split the same way.

- *Tests of a step* (a commit writes its blobs, tree, ref, and files; the
  reads of journals and Documents; collections; age; `repo.rs`) stay
  `#[test]` and call the step with the token's `cfg(test)` constructor.
- *Tests of the waiting* (contention for the write lock, the push after a
  write, `clone` and `fetch` through the `git` process, the wait at
  `open`, the gate of `Close`), in `commit.rs`, `mirror.rs`, `git.rs`,
  `vault.rs`, and `service.rs`, move to `#[tokio::test]` with the code
  they test; a test of a function D3 cuts in two is cut the same way.
- *Tests of the `0.22` indexer* are not moved: `21` L3 writes the new
  indexer task with its own tests, and `21` L8 removes the old one.
- *Tests of the objects* (`rust/scheda/tests`, about 240) move to
  `#[tokio::test]`, a mechanical change, and each runs on both runtime
  flavors, as `tests/async.rs` does today; those of the Index and its
  searches are rewritten in `21` L5 and L6 anyway.
- *Tests of the CLI* run its binary and need no async; they show its
  output unchanged (D10).
- Test setup that runs `git` by hand stays blocking.
- The waits by polling with `sleep` (about 60) move as they are in A3
  (`tokio::time::sleep`); `21` L7 replaces those that wait for a write of
  their own with its token (`not_before`).

*Recommendation:* accept.

**D12. The old Index goes first.** *Changed by the owner:* nothing that is
about to be deleted is made async. `21` L8 (Tantivy, USearch, their sides
of the indexer, the query and search engines, the features `find`,
`search`, `simd`, `openmp`) runs right after `21` L1, before A1. What is
left is today's build without `find`: the indexer with one side, the
journal id maps (`JournalMaps`), and the state of the Index; that is what
A3 makes a task (D6) and `21` L3 gives a Lance side. Until `21` L5 and L6,
`Query`, `Count`, and `Search` answer `UNIMPLEMENTED`, as a build without
`find` does today. *Recommendation:* accept.

---

## 3. Stages

Order of the whole branch (the owner, after `21` L8; `21` §8.4): `21` L0,
L1, L8; a run of every test; A1 with the contract of `21` §8; a probe of
the debug loop (`21` L1); A2, A3, and A4 in one commit; then `21` L2–L7,
L9, L10 (with A5). Inside each stage: code and tests first, then `cargo test`
and `cargo clippy` green; a box is ticked when its *Check:* passes. One
docs pass at the end (A5).

On 20 October 2026 the owner set the rule for the rest of the branch: the
end result counts, a stage in between may be red, and no temporary code
is written to keep one green. A2 therefore went into the commit of A3 and
A4: on its own, the blocking objects that A3 removes would have needed a
second, temporary way to make the token.

### A1. The contract and the dependencies

D1, D2.

- [x] tokio required in `scheda` and `scheda-api`; the feature `tokio`
      removed; `gate_tokio_feature` out of `buf.gen.yaml`; generate.
- [x] In the same generate, the contract of `21` §8 (`git apply
      .claude/notes/l0-protos.patch`, moved from `21` L5), and the code
      follows it: a write returns its `version` (moved from `21` L7),
      read under the write lock; an empty batch reads it without the
      lock, as `VaultInfo.version` does (the owner); `Scheda.Info` lists
      `FEATURE_INDEX` only from `21` L3. `IndexState.head` of the review
      is `stored_version`: the contract's own test forbids git terms in
      names (the owner).
- *Check:* `cargo build --workspace` and `bazel build //...` green;
  `bazel test //...` green (19 October 2026); the token tests in
  `tests/documents.rs` (`a_write_returns_the_token_of_the_commit_it_made_in_the_main_repository`,
  `the_token_of_a_write_names_its_own_commit_among_many_writers`,
  `an_empty_batch_waits_for_no_lock_and_answers_with_the_state_now`).

### A2. The core and its gate

D3, D4.

- [x] The token `Blocking` and the helper `blocking`, in a crate of their
      own (`rust/blocking`, `scheda-blocking`); the entry points of the
      core take the token. A doctest sees only public items, so a
      `compile_fail` doctest on the private core would fail to compile
      for the wrong reason and prove nothing; in its own crate the gate is
      public and its doctests are real. The core's own tests call their
      steps through `blocking` on a test runtime, so no `cfg(test)`
      constructor exists.
- *Check:* `rust/blocking` doctests (20 October 2026): each claim is a
  pair that differs in one line, one that compiles and runs, one that does
  not compile, checked to fail for its own reason (a private field, a
  borrow that outlives the step, a move out of the borrow): the token
  cannot be made, kept, or copied outside the helper.

### A3. The objects async

D1, D3, D4, D5, D6, D7, D8, D11.

- [x] `Scheda`, `Vault`, `Collection`, `Mirror`, and the Index object
      implement the async traits on the core, in place of the blocking
      objects and of `Offload` (`21` L8 left `Vault.Index`
      `UNIMPLEMENTED`: the Index object comes back async in `21` L3); the waits of the core (the `git`
      processes, the write lock, the wait at `open`) move out of it (D3,
      D5).
- [x] The indexer as a task (D6): its signal (`Notify`), `Watch` (a
      `watch` channel over the kept states), the poll; the journal id maps
      its one side. A round is one blocking step while every part of it
      reads or writes files; its panic is caught there and becomes
      `last_error`. `21` L3, whose round awaits Lance, runs the round as a
      task of its own.
- [x] `Close` and drop per D7. Each blocking step carries a pass of the
      gate, so `Close` waits for the step of a call that was dropped, and
      returns with every lock let go.
- [x] Ports in their async form (D8): written in `21` L3, since `21` L8
      makes every port `UNIMPLEMENTED` until then.
- *Check:* `rust/scheda/tests` on `#[tokio::test]`, on both runtime
  flavors, green; a write whose future is dropped is stored; a push
  dropped midway leaves the commit local and the next push carries it; a
  Vault wrapped in `Blocking` works on a multi-thread runtime and refuses
  on a current-thread one; a panic in a round becomes `last_error` and
  the next round runs; closing a Vault in the middle of a round returns
  within one step. Passed 20 October 2026: the integration tests are one
  binary (`tests/integration/`, so the library and Lance are linked once,
  as the probe of `21` L1 advised), each object test run by `both` on a
  current-thread and a multi-thread runtime;
  `service::tests::a_write_dropped_during_its_commit_is_stored_and_close_waits_for_it`,
  `runtime::a_push_dropped_midway_leaves_the_commit_local_and_the_next_push_carries_it`,
  `runtime::a_blocking_caller_wraps_the_vault_on_a_multi_thread_runtime_and_is_refused_on_a_current_thread_one`,
  `indexer::tests::a_round_that_fails_or_panics_is_reported_and_tried_again`,
  `index::close_and_drop_end_the_indexer_task`.

### A4. The CLI

D10.

- [x] `main` on a multi-thread runtime; the commands on the async objects;
      the watch helper as a task (gone with `index` in `21` L8; it comes
      back with it in `21` L9). The flags and the input, a batch's stdin
      included, are read before the runtime starts.
- *Check:* `rust/cli/tests` green, with the same output as before (20
  October 2026).

### A5. Docs

- [x] `07` (the API: async only, `Blocking` for a blocking caller),
      `14` (the CLI, unchanged behaviour), `20` D11 (superseded by this
      work order), README, `CHANGELOG.md`.

---

## 4. What changes elsewhere

- `20` D11 (*settled* 17 October 2026: the background work on its own OS
  threads, async at the edges): superseded by D1, D3, D6.
- `21` D13 (async inside, sync outside): replaced by D1–D3, D9; `21` says
  so.
- What was discussed after `21` L0 and not taken (`21` §8.4): a private
  runtime of Scheda's, a bridge from the blocking objects into it, and the
  indexer kept a thread.
- `21` order: L8 runs right after L1 (D12), and its removal of a `0.22`
  Index on disk moves to L4.
- `21` L2 (the store) and L3 (the indexer): written async from the
  start.

---

## 5. Out of scope

- Moving the core itself to async I/O (`tokio::fs`, an async git): the
  core stays blocking behind its gate (D3).
- The anchor consumer's move to `0.23`.
- A blocking form of the objects written by Scheda (D2).

---

## 6. Review

19 October 2026, the owner, item by item: D1, D2 (protocontract's
bridges for a blocking caller), D5, D6, D7, D8, D10 accepted as proposed.
D3 changed: waiting is async and doing is one blocking step, in place of
one blocking unit per call, and no `tokio::fs`. D4 follows D3 (a dropped
call stops at its next `await`). D9 completed: Lance's environment
variables are the application's, and Lance's work runs as a task of its
own for `Panicked`. D11 completed: the tests split along D3's line. D12
changed: the old Index is removed first, so nothing about to go is made
async.
