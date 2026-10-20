# 19. Follow-ups to 0.19: declarations, search signals, transactions

Document 19 of the Scheda package. Version 0.3, 20 October 2026.

Status: F1–F5 done, docs pass done (5 October 2026).

Work order for four gaps found while planning the anchor consumer's move to
`0.19.0` and to the async API (`tokio` feature). None is a defect of a
documented rule; each leaves a consumer to work around Scheda instead of
using it:

- **F1.** A handle follows `HEAD` for the Index, but not for collection
  declarations.
- **F2.** `search` gives no sign that its vector leg did not run.
- **F3.** `index_version` says "repeat" also where repeating never helps.
- **F4.** `transaction` cannot express an idempotent batch, and its closure
  cannot keep the caller's error type.

Order inside each stage: code and a regression test first, `cargo test`
and `cargo clippy` green, then `bazel test //...` (after `bazel run
//api:generate` for F2). One docs pass at the end (§4). Tick a box only
when its *Check:* passes under `bazel test //...`.

Decisions below carry a recommendation each. The owner comments only on
the ones he disagrees with; the rest stand (`owner-review.md`).

---

## 1. Why

The anchor consumer runs several processes on one Vault. Each process has
long-lived handles: one that writes (one transaction per batch of
operations it takes from a queue, replayed after a crash), and readers
that run vector-only `search` (`lexical_weight` 0) on every request. Its
server is async and moves to `AsyncVault`. Against `0.19.0`:

1. A reader opened before another process created a collection never sees
   it, so the consumer reopens handles on a timer. In `0.19.0` a reopen is
   no longer free: it starts an indexer thread, may move the index roles,
   and the drop of the old handle waits for one embedder call.
2. After an embedder model change, `search` silently runs without the
   vector leg. A vector-only query returns no hits, which reads as "nothing
   matched". `index_status()` per request is too dear for a request path:
   it runs a diff (`17` D32).
3. On the first `open` after an upgrade, `search` fails with
   `index_version` until the background rebuild publishes. A projector
   mismatch fails with the same code and the same `repeat: allowed`,
   although no rebuild will come (`17` D24). A caller cannot tell "retry in
   a moment" from "needs `reindex`".
4. The writer replays a batch after a crash; the replay writes nothing.
   `AsyncVault::transaction` always calls `commit`, which is `invalid` when
   there is nothing to commit, so the replay fails. The closure must return
   `scheda::Error`, so the writer loses its own error kinds (refused
   operation, lock held, other) or smuggles them out through a side
   channel. The sync API has no `transaction` at all, so sync and async
   code use two different shapes.

---

## 2. Decisions

**D1. Declarations reload from the work tree, not from `HEAD`.** A
handle's own uncommitted `create_collection` lives only in the work tree;
reloading from `HEAD` would drop it mid-transaction. Recommendation: every
reload reads the work tree, as `open` does.

**D2. When declarations reload.** Recommendation: (a) a name lookup that
misses (`collection(name)`, name resolution in `find` / `search`) reloads
once and retries before it answers `not_found`; (b) with `find`, an
indexer round whose diff touches a declaration (`diff.rs` already reports
that, `17` D9) reloads the handle's declarations after the round. (a)
covers a build without `find`; (b) covers a changed declaration of a
collection the handle already knows. No polling of declaration files
beyond that.

**D3. The vector-leg signal is a field, not an error.** A vector leg that
cannot run is a normal state (no embedder, another model, no vectors yet),
and the lexical leg may still answer. Recommendation: a new enum field on
`SearchHits` (§3 F2), set on every `search`. No new error code.

**D4. Repeat, not a new code, for F3.** Recommendation: keep
`index_version` for both cases and set `repeat: forbidden` on the projector
mismatch. The default for `index_version` stays `allowed`.

**D5. `transaction` leaves no empty commit.** Recommendation: when the
closure succeeds and the transaction has nothing to commit (no recorded
write, or writes that leave both repositories as at `HEAD`), `transaction`
discards and returns the closure's value. Sync `commit()` stays strict:
`invalid` on nothing to commit, as GIT-2 says today.

**D6. The closure's error type is the caller's.** Recommendation:
`F: FnOnce(&Vault) -> Result<T, E>`, `E: From<scheda::Error>`; the result is
`Result<T, E>`. Scheda's own failures (`begin`, `commit`, `discard`, a
`locked` refusal, a join error in the async form) reach the caller through
`E::from`.

**D7. A sync twin.** Recommendation: `Vault::transaction(message, f)` with
the same rules; `AsyncVault::transaction` runs it on one blocking thread
instead of repeating the logic. A panic in the closure: the sync form
discards, then resumes the unwind (`std::panic::resume_unwind`; Scheda does
not turn a caller's panic into an error in the sync API); the async form
keeps `17` D42 (`io_other`, after the discard the sync form already did).

**D8. Version.** F1, F3, and F4 are consumer-visible (`consumers.md`): a
handle sees declarations it did not see before, an error's `repeat`
changes, `transaction` returns `Ok` where it returned `invalid`, and
`AsyncVault::transaction` changes its signature. Release per
`.claude/skills/versioning/SKILL.md`; expect a minor bump (`0.20.0`).

---

## 3. Stages

### F1. Declarations follow the work tree

Today: `Vault.decls` (`vault.rs`, `Mutex<Arc<Decls>>`) is loaded at `open`
and replaced only in `restore_recorded`, `reload_after_restore` (after
`discard`, `pull`, `sync`), and `create_collection`. `collection()`,
`info()`, and `find` / `search` name resolution read it.

- [x] A name lookup that misses reloads declarations from the work tree
      once and retries (D1, D2a), with and without `find`.
      (`vault::tests::background::a_collection_another_handle_declares_is_found_without_reopen`,
      `a_filter_qualifier_declared_after_open_is_found`; thin:
      `a_collection_declared_after_open_is_found_without_find`.)
- [x] With `find`, an indexer round whose diff touches a declaration
      reloads the handle's declarations after the round (D2b). The round
      reads declarations from the commit (`17` D19); the handle reloads
      from the work tree (D1). As built (owner, 5 October 2026): every
      round of every handle, with a role or without, compares the main
      lexical and vector marks of the live generation with those seen at
      the last read and reads the declarations again when one moved;
      `wait_indexed` does the same before it returns. A handle without a
      role runs no diff, so the diff alone could not cover it.
      (`a_changed_declaration_is_seen_after_wait_indexed`.)
- [x] A reload during this handle's open transaction keeps its own
      uncommitted declarations (they are in the work tree).
      (`a_reload_in_an_open_transaction_keeps_its_own_declaration`.)
- *Check:* handle A open; handle B (another process in one test, same
  process in another) commits `create_collection("x")`; `A.collection("x")`
  succeeds without reopening A.
- *Check:* B changes `index_scope` on a collection A knows;
  after `A.wait_indexed`, A's `info()` shows the new declaration.
- *Check:* A has an open transaction with its own uncommitted
  `create_collection("y")`; a reload triggered by B's commit keeps `y`.
- *Check:* a build without `find` passes the first *Check:* (`thin` test
  target).

### F2. `search` reports its vector leg

Today (`search.rs`, `SearchEngine::search`): the vector leg is off when
the handle has no projector, `vector_weight` is 0, no requested collection
indexes vectors, or the projector has no vector column; the query is not
embedded when there is no embedder or the generation's vectors are of
another model (`holds_vectors_of`). `SearchHits` (`api/v1/search.proto`)
carries only `hits`.

- [x] A new enum on `SearchHits`, set by every `search`, that tells at
      least: the vector leg ran; it was not asked for (`vector_weight` 0);
      no requested collection indexes vectors; no embedder; the embedder's
      model differs from the Index vectors; the Index holds no vectors yet.
      Names, numbering, and `buf.validate` per `proto-validation.md`;
      `bazel run //api:generate`; the generated `Validate` holds the rule.
      (`SearchHits.vector_leg`, enum `VectorLeg`; `required`,
      `defined_only`, `not_in: [0]` plus CEL `this != 0`, which the
      generator emits. Tests `vector_leg_*` in `tests/search.rs`.)
- [x] The value is decided before the query runs, from what the handle
      already holds: no extra I/O, no diff.
- [x] CLI `search` prints the field in its Protobuf JSON output.
      (`scripted_find_search_get_journal_add_commit`.)
- *Check:* one test per state; a vector-only query (`lexical_weight` 0)
  with an embedder of another model returns no hits and the mismatch
  state; the same query with the matching embedder returns hits and "ran".
- *Check:* `AsyncVault::search` / `AsyncCollection::search` return the
  field unchanged.

### F3. `index_version` that repeating cannot fix

Today: `Code::IndexVersion` defaults to `Repeat::Allowed` (`rust/api`).
It is raised for a generation in another format (`search.rs`, the format
check; the background rebuild publishes a new generation, so a repeat
helps) and for a projector mismatch (`search.rs`, the projector-dump
checks; no rebuild comes, `17` D24).

- [x] Every projector-mismatch `index_version` carries
      `repeat: forbidden`; its message names `reindex` from a handle with
      the generation's projector as the way over (D4). As built (owner,
      5 October 2026): `search` without a Filter checks the projector dump
      too, as `find` does; before, only `find` and a filtered `search`
      failed on a mismatch.
      (`vault::tests::background::another_projector_is_index_version_a_repeat_does_not_fix`.)
- [x] The format case keeps `repeat: allowed`.
      (`another_format_is_index_version_a_repeat_fixes`.)
- *Check:* a format-5 generation: `search` fails with `index_version`,
  `known_repeat() == Allowed`; after `wait_indexed` it succeeds.
- *Check:* a handle without the generation's projector: `search` fails
  with `index_version`, `known_repeat() == Forbidden`.

### F4. `transaction` for sync and async callers

Today: `AsyncVault::transaction` (`nonblocking.rs`) runs `begin`, the
closure, `commit` on one blocking thread; the closure returns
`scheda::Error`; `commit` with nothing to commit is `invalid`
(`git.rs`, `isolated.rs`: "nothing to commit"). There is no sync
`Vault::transaction`.

- [x] `Vault::transaction(message, f)` per D5, D6, D7: `begin`, `f(&self)`,
      then `commit` when there is something to commit, otherwise
      `discard`; on `Err` from `f` or a failed `commit`, `discard` and
      return the error; on a panic, `discard` and resume the unwind.
      "Nothing to commit" is decided from the recorded writes and the
      tree, not by matching an error message. Covers the isolated
      repository the same way. (`git::commit_if_changed`,
      `isolated::commit_recorded_if_changed`; tests
      `vault::tests::transaction_*`.)
- [x] `AsyncVault::transaction` delegates to it, with
      `E: From<scheda::Error> + Send + 'static`; a cancelled task is
      `E::from(io_interrupted)`, a panic `E::from(io_other)` (`17` D42).
- [x] The `locked` rules of `17` D43 hold for both forms.
      (`transaction_while_another_thread_holds_one_is_locked_through_from`,
      `a_transaction_keeps_the_callers_error_type`,
      `a_sync_transaction_on_another_thread_makes_it_locked`.)
- [x] Write first a test of what an identical-bytes `replace` inside a
      transaction does today; D5 makes it commit nothing either way.
      (`identical_replace_in_a_transaction_commits_nothing`: before the
      change, `commit` staged a tree equal to `HEAD`'s and made no commit.)
- *Check:* a closure that writes nothing → `Ok(value)`, `git log`
  unchanged, sync and async.
- *Check:* a closure that replaces a Record with identical bytes → no new
  commit.
- *Check:* a closure returning the caller's own error enum gets that
  variant back, and the writes are discarded; a `locked` refusal arrives
  as `E::from(locked)`.
- *Check:* a panicking closure: the writes are discarded; sync resumes the
  panic (`catch_unwind` in the test), async returns `io_other`.
- *Check:* `concurrent_transactions_commit_whole_or_are_locked`
  (`nonblocking.rs`) still passes.

### F5. Acceptance

- [x] A consumer-shaped test in `rust/scheda/tests/consumer.rs`, from a
      `#[tokio::test]` on an `AsyncVault`: (1) a writer applies a batch in
      `transaction` with its own error enum, then replays it: one commit,
      the replay is `Ok`; (2) a reader opened before another handle
      creates a collection finds that collection and, after
      `wait_indexed`, its Records by `search`; (3) a vector-only `search`
      from a reader whose embedder has another model returns the mismatch
      state of F2. (`an_async_writer_and_readers_share_a_vault`.)
- [x] All *Check:* of F1–F4 green under `bazel test //...`; `cargo test`
      for the thin builds (`--no-default-features`, `--features find`).

---

## 4. Docs pass (once, after the code)

`07` (LIB entries: `collection` / `info` freshness, `search` reply field,
`transaction` sync and async, `commit` unchanged), `06` (IDX entry for the
`index_version` repeat), `13` (the vector-leg field), `14` (CLI `search`
output), `17` §6 (the anchor consumer now runs vector-only `search` in
production, and handles other than its writer carry an embedder of the
same model: rows K5 and K10), `10` (phase table), README (this document's
row), crate rustdoc, CHANGELOG (*Added*: sync `transaction`, the
vector-leg field; *Changed*: declarations follow the work tree,
`transaction` with nothing to commit is `Ok`, `AsyncVault::transaction`
error type, `repeat` of a projector-mismatch `index_version`).

---

## 5. Out of scope

- The whole vector dump is still written once per indexer pass that
  changes a slot (`18` S6). The anchor consumer measures it at its next
  stage before asking for an append log or a save interval.
- Exact version pins of dependencies (`tokio =1.53.2` and others) stay.
