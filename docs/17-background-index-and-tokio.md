# 17. Background Index and the `tokio` feature

Document 17 of the Scheda package. Version 0.1, 4 October 2026.

Work order for two independent changes:

- **Part B — background Index.** The Index stops being updated inside
  `add` / `replace` / `remove` / `commit`. A thread owned by the Vault
  handle follows git `HEAD`: when it moves, the thread diffs the last
  indexed commit against `HEAD` and indexes the changed Records. On
  `open` it catches up the same way. The caller never drives indexing;
  the only explicit call left is `reindex`, for an embedder-model change
  or repair.
- **Part T — `tokio` feature.** An opt-in async API next to the
  unchanged sync API, safe to call from a Tokio runtime without the
  caller wrapping anything in `spawn_blocking`.

Do Part B first: it carries the value (embedding leaves the write path,
human commits get indexed, no forgotten `reindex`). Part T is additive
and can land later or in parallel.

Order inside each part: code and tests first, `bazel test //...` and
`cargo clippy` green, then **one** docs pass (§5). Tick a box only when
its *Check:* passes under `bazel test //...`.

---

## 1. Why

Today (IDX-9, `06` §3) every successful write updates three Index
artifacts inline: lexical (Tantivy), vector (USearch, calling the
embedder), and the journal `id` map. Consequences for any consumer, as
first seen by the anchor consumer (Knowqore):

1. **The write path waits on the network.** With a remote embedder,
   every write of an indexed Record is an HTTP round trip under
   `write.lock`. An embedder outage fails the write.
2. **Vectors go missing silently.** A handle without `set_embedder`
   writes the lexical leg only. Knowqore hit this: its writer's Vault had
   no embedder, and `lesson-index` was written without vectors for a
   whole run.
3. **Edits outside Scheda need a manual `reindex`.** A human commit, a
   `git pull` from another tool, or a commit by another process is not
   indexed until someone remembers to call `reindex`.
4. **The writer pays for the Index on every commit.** Suspected (not
   confirmed): `index.usearch` and `keys.json` are saved on every commit
   even when no vector changed. Read the save path while doing B4.

Git already records exactly what changed between two points. Indexing
from `HEAD` diffs turns the Index into what `06` §1 says it is: derived
data that follows the files.

---

## 2. Decisions

Each item has a recommendation. Change it before coding if you disagree;
otherwise it stands.

**D1. What stays in the write path.** Recommendation: the **journal
`id` map** stays synchronous on write (IDX-5, **core**). It is a local
append with no network, and `15` §3 explains why a lagging map is worse
than none: `get` of a line written a moment ago would return
`not_found`. Lexical and vector legs move to the background. Any method
that resolves an `id` for a *write* (`replace`, `remove`) must not
depend on the lagging lexical leg; audit those paths in B4.

**D2. What triggers indexing.** Recommendation: (a) a successful
in-process commit (autocommit, `commit`, `pull`, `sync`) wakes the
indexer at once through a channel; (b) the indexer also polls `HEAD`
(read the ref, compare hashes) every `index_poll` interval, default
1 s, set in `OpenOptions`; (c) `open` compares `HEAD` with the stored
marks and starts catch-up. No filesystem watcher: they are unreliable
on WSL and network mounts, and polling a ref file is cheap.

**D3. Only committed state is indexed.** Uncommitted writes inside a
transaction, and uncommitted editor edits, are not in the Index until a
commit. Inside a transaction, `find` / `search` do not see that
transaction's own writes. `discard` no longer rebuilds the Index (it has
nothing to undo).

**D4. Marks per leg.** The generation directory stores the commit each
leg has reached: `<gen>/marks` with `lexical`, `vector`, and, when
`{vault}/.isolated/` exists, the same pair for the nested repository.
A leg advances its mark only after its writes are durable. Format
version `4` → `5` (IDX-2).

**D5. One indexer per working tree, across processes.** Several
processes may open the same Vault (Knowqore runs several `knq`
processes on one store). Recommendation: `{vault}/.scheda/index.lock`,
`try_lock`, held by the indexer thread while it holds the role. A handle
that does not hold it is a reader: it polls the marks and reloads its
Tantivy reader and USearch view when they move. When the holder dies,
the OS drops the lock and another handle takes the role on its next
poll. The indexer **never** takes `write.lock`.

**D6. The vector leg needs an embedder.** A handle without an embedder
may index the lexical leg, never claims the vector leg, and never
advances the vector mark. Recommendation: two roles under D5 —
`index.lock` for lexical, `vector.lock` for vector, the latter taken only
by a handle with an embedder whose `model_id` and dimensions match the
generation's `meta.json`. So in a mixed set of processes the one with
the embedder writes the vectors, and a process without one cannot block
it. `index_status` (B6) shows vector lag, so a missing embedder is
visible instead of silent.

**D7. Model change is explicit.** An embedder whose `model_id` or
dimensions differ from the live generation does not take `vector.lock`
and does not rebuild on its own; the status says `model_mismatch`, and
the vector leg of `search` stays on the old vectors only if the query
embedder matches them, otherwise it is off (no mixed spaces). The caller
runs `reindex()`. Two processes with different embedders cannot make the
Index flip back and forth.

**D8. Format change is automatic.** An Index with an unknown or old
format (IDX-2 `index_version`) or a corrupt one is derived data: the
indexer rebuilds it in the background into a new generation, as
`reindex` does, without being asked. `find` / `search` fail closed
(QRY-4) until `current` points at a good generation.

**D9. Diff edge cases → full rebuild of the leg.** The stored mark is
not an ancestor of `HEAD` (reset, amend, rebase in `sync`, branch
switch), or its object is gone (gc): diff is still possible while the
object exists (tree-to-tree diff does not need ancestry); when it does
not exist, rebuild that leg into a new generation. A change of the
collection declaration or a Schema file: reindex the affected
collection in full. Encrypted collections stay out of the Index (as
today).

**D10. Online reindex catch-up uses marks, not the queue.** `15` built a
queue folder so writes during an unlocked `reindex` reach `N+1`. With
marks, `N+1` records the commit it was built from; after publish the
indexer diffs from that commit to `HEAD`. Recommendation: delete the
queue (`{vault}/.scheda/reindex/` queue files, enqueue in mutating
calls) together with this change. The marker and `reindex.lock` stay
only if they are still needed for one-`reindex`-at-a-time.

**D11. Thread lifetime.** The indexer thread is owned by the handle,
started by `open` only when the `find` feature is on, stopped and joined
when the last clone of the handle drops. It never detaches, never
outlives the handle, and never holds a strong reference back to the
handle (no cycle). Between batches it checks the stop flag; at most one
embedder call is in flight when drop waits.

**D12. Errors are retried, not raised on writes.** An embedder or Index
failure in the background does not fail any write. The indexer retries
with capped exponential backoff, keeps the mark where it was, and
exposes the last error in `index_status`. Error text must not carry
embedder secrets or Record bodies.

**D13. CLI reads wait.** The CLI process is short-lived: an `add`
followed by exit leaves catch-up to the next `open`. Recommendation:
CLI `find` / `search` call `wait_indexed` with a timeout before
querying (`--no-wait` to skip). CLI `reindex` stays.

**D14. LIB-6 still holds.** No async runtime is required: the indexer is
a plain thread. `15`'s line "There is no Index worker thread" is
replaced by D11.

**D15. Version.** Behaviour of `find` / `search` after a write changes
(eventual instead of immediate): release as `0.19.0`, with a CHANGELOG
entry under *Changed* that says so.

Accepted with the B1–B4 plan (4 October 2026), after reading the code:

**D16. Journal `id` map follows `HEAD` too.** Writes still update it
inline (D1). Lines that reach `HEAD` another way (`pull`, `sync`, a
`git` commit, another process) are applied from the same diff by the
lexical-role indexer, with its own marks `ids` / `isolated-ids`. Every
change to an `ids` file, by a writer or the indexer, holds a short flock
`ids.lock` next to it, never while taking another lock. Before answering
"unknown id" (and in the `duplicate_id` check) a handle re-reads the
file when its length or mtime changed.

**D17. `wait_indexed` lands in B3** with a minimal `IndexStatus`; B6
completes the fields. Tests need a wait as soon as B4 removes inline
indexing.

**D18. `replace` / `remove` resolve `id` from the Record file**, not the
lexical leg (D1 audit: `record_exists` used `has_id`). IDX-10's rule
"unknown to the Index → `not_found`" for those two calls is retired.

**D19. The indexer reads Records and declarations from the commit**
(`git cat-file`), not the working tree, so an open transaction's
uncommitted writes never reach the Index (D3).

**D20. Format `5` layout.** `<gen>/marks/` is a directory with one file
per leg (`lexical`, `vector`, `ids`, and the `isolated-` trio), each
written by temp + rename, so two processes holding different roles never
rewrite one file. The vector dump moves to `vector/<seq>/`, and the
`vector` mark names `<commit> <seq>`, so a reader never loads a dump
that is half written.

**D21. D8 lands in B3.** A generation whose format is not `5` is rebuilt
by the indexer from the empty tree into a new generation under
`reindex.lock`, then published. The indexer and readers follow `current`
when it moves; a handle no longer stays bound to the generation it saw
at `open`. B8 adds corruption detection and the model path.

**D22. `index_poll`** is `uint32 index_poll_ms` on `OpenOptions` (no
well-known types in `api/` yet): unset → 1000, valid 10 to 3 600 000.

**D23. Graceful shutdown** (accepted 4 October 2026). `Vault::close(self)`
stops and joins the indexer, flushes, releases `write.lock`, and returns
an error where `Drop` cannot; collections that outlive it keep reading
and writing but no longer wake an indexer. The indexer checks the stop
flag before every embedder call, so at most one call is in flight when
`close` or the last drop waits. `close` has no timeout: a sync embedder
call cannot be interrupted and the thread must not be abandoned (D11);
the caller bounds it with its embedder's own HTTP timeout. Scheda
installs no signal handler: the program catches `SIGINT` / `SIGTERM`,
calls `close`, and exits; a second Ctrl-C (or `kill -9`) is safe because
marks move only after durable writes (B7 has the test). The vector dump
becomes atomic (D20) in B4. Part T adds `AsyncVault::close().await`.

Settled while building B4 (4 October 2026); each one closes a gap the
tests found:

**D24. The lexical role needs the generation's projector.** A handle
takes `index.lock` only when its projector matches the generation's
projector dump, or the generation has none yet, or it is in another
format and about to be rebuilt. Otherwise the first process to open a
Vault (a CLI without a projector, say) would index without the
consumer's columns. Mismatch: no role, `wait_indexed` does not wait for
this handle, and `reindex` from a handle with the right projector is the
explicit way over, as D7 is for embedders.

**D25. `vector/current` names the live dump.** The dump is saved after
every batch while the vector mark moves once per diff, so the pointer is
a file of its own, not part of the mark: `vector/<seq>/` holds one whole
save, `vector/current` (temp + rename) names it, the save before it is
kept for a reader that is loading it. The flat `vector/` of an older
release still loads.

**D26. `reindex` marks `N+1` with the commits its walk started from**,
read under `write.lock` before the walk. A commit that reaches `HEAD`
during the walk without a Scheda call (a `git` command) is then applied
by the indexer after publish instead of being lost.

**D27. A generation switch keeps the handle's journal lines.** Lines the
handle wrote that the new generation's maps lack (an open transaction's)
are carried into them, so `get` keeps finding them.

**D28. A publish pauses the handle's indexer.** `reindex` swaps the
engine the handle shares with its indexer; the round in flight ends at
its next batch boundary and the next waits for the swap, so no round
writes the old generation's diff into the new one. `check` pauses it the
same way, so a file Tantivy just created is not reported as a wide mode;
`index_status` reads the marks of the generation the handle queries.

**D29. CLI `find` / `search` wait from B4 on** (60 s, `wait_indexed`):
each CLI command is its own process. `--no-wait` and the status command
stay in B9.

**D30. `remove` of a file git never tracked** deletes it and commits
nothing (reachable since D18).

**D31. What "reload when the marks move" means per leg.** Tantivy needs
no poll: every query reloads its reader, which reads the segment list
another process committed. USearch follows `vector/current` (D25), which
moves before the vector mark: each indexer round, with a role or without,
and each `search` that uses the vector leg compare it with the save the
handle holds and load the newer one outside the query lock (a save that
drops the dump being read makes the load retry). The same round start
re-reads the journal `ord` counters. So a handle that takes a role over
writes on top of the last holder's vectors and append order, not over
them with what it loaded at `open`. A handle never replaces vectors it
has not saved yet.

**D32. "Rows pending" is a count of Records, per leg, from a diff.**
`IndexStatus.lexical_pending` and `vector_pending` count the Records a
leg has yet to apply from its mark to `HEAD`, both repositories
together: changed Records of collections that leg indexes, plus every
Record of a collection whose declaration changed; a leg to rebuild
counts every Record. `index_status` runs that diff, so it costs about a
`git diff`; `wait_indexed` reads only the marks while it waits and
counts once before it returns. `None` means the count failed (the
declarations at `HEAD` do not load), so the status still answers when
the indexer cannot. `model_mismatch` is true when this handle's
embedder differs from the live generation's vectors (D7).

**D33. Backoff per leg, and only a change cuts it short.** A failed
vector leg (the embedder, mostly) holds back that leg alone, 100 ms
doubling to 30 s; the lexical leg and the journal `id` maps keep
following `HEAD` on every poll, and rounds that skip the held leg keep
reporting its error in `last_error`. Any other failure holds back the
whole round the same way. A commit, `pull` / `sync`, or the end of a
`reindex` publish wakes the indexer at once even during a round hold;
`wait_indexed` asks for a round without cutting either hold short, so a
waiting caller cannot turn backoff into a tight retry loop. A new
embedder (`set_embedder`) clears the vector hold.

Settled while building B8 (4 October 2026):

**D34. `reindex` builds from commits, like the indexer's own rebuild.**
D10 deletes the queue, and the queue was what kept a work-tree walk
honest: a walk without `write.lock` can read another handle's open
transaction, and when that transaction is discarded nothing would remove
those Records from `N+1`. So `reindex` (with `find`) fills `N+1` from the
commits at `HEAD` with the same code the indexer uses for a format or
damage rebuild (D8, D19), marks it with those commits, and publishes;
the indexer applies the rest. No `write.lock` and no `reindex/active`:
nothing another handle does to the files can race the build, so
`create_collection`, `discard`, `pull`, and `sync` are no longer refused
during it, and an open transaction elsewhere no longer makes it `locked`.
`reindex.lock` stays for one build at a time. Vectors: with an embedder,
a loadable dump of the same model is copied (unchanged slots are not
embedded again), another model starts empty, every slot of `HEAD` is
filled; without one, the previous dump and its vector marks move to
`N+1` as they are, so a process without an embedder does not throw away
vectors another process paid for. A build without `find` has no indexer:
its `reindex` rebuilds the journal `id` maps from the files under
`write.lock`. `ReindexResult.skipped_invalid` stays `0` with `find` (it
already was).

**D35. Damage is found by reading, and repaired by the lexical role.**
A query or an indexer round that cannot read a generation (codes
`index_lexical`, `index_corrupt`, `index_vector`) leaves a `damaged`
flag file in it, so the handle with the lexical role learns of it in
any process. That handle checks a generation read-only: quickly once per
generation (Tantivy opens and serves a searcher, `ord` decodes), in full
(the vector dump loads) when flagged. A failed check rebuilds the
generation from the commits (D34); a passing one clears the flag. A dump
that does not load is never copied into the new generation. A failed
write alone (a full disk) does not rebuild: the read-only check passes.
Queries fail closed until the new generation is live (D8). With D24, no
handle without the generation's projector rebuilds it.

**D36. Model change stays explicit (D7 confirmed, 4 October 2026).** A
vector space per model, filled in the background by whichever process
has that model, was considered and dropped. Changing the embedder model
is a rare, deliberate migration of the application, and the application
owns it: it calls `reindex()` with the new embedder, which re-embeds
every slot. Until then `index_status().model_mismatch` is true for a
handle on the new model, that handle writes no vectors, and its
`search` runs without the vector leg.

**D37. CLI surface (B9).** Global `--no-wait` makes `find` / `search`
query the Index as it is. `scheda index-status` prints `index_status`
and `scheda wait-indexed [--timeout-secs N]` (default 60) prints it
after waiting, `io_timed_out` past the timeout: the CLI names commands
after library methods. `IndexStatus` stays a plain Rust struct, not a
proto message, so its flags stay `bool` for library callers; the CLI
prints it in the Protobuf JSON style the rest of its output uses
(lowerCamelCase, unset fields left out, 64-bit counts as strings).

---

## 3. Part B — background Index

### B1. Marks

- [x] `<gen>/marks` written atomically (temp + rename) per D4; format
      `5`; reading format `4` → D8 (`marks.rs` tests,
      `vault.rs background::format_4_generation_is_rebuilt_in_the_background`).
- *Check:* a write that commits, then the indexer applies it, moves the
  lexical mark to the new `HEAD`; a crash before the rename leaves the
  old mark and the next run reapplies the same diff idempotently
  (`background::commit_moves_the_lexical_mark_and_a_replay_is_harmless`,
  `marks.rs crash_before_rename_keeps_old_mark`).

### B2. Diff

- [x] `(from, to)` → changed `(collection, id)` with add / modify /
      delete, renames as delete + add, for the main repository and
      `.isolated/` (`diff.rs` `plain_commit_upserts_the_new_record`,
      `delete_and_rename`, `journal_lines_are_diffed_one_by_one`,
      `isolated_records_count_only_in_the_nested_repository`).
- [x] Declaration or Schema change → whole collection (D9)
      (`declaration_change_asks_for_the_whole_collection`).
- [x] Missing `from` object → full rebuild of the leg (D9)
      (`pruned_mark_asks_for_a_rebuild`).
- *Check:* git fixtures for: plain commit, several commits at once,
  delete, rename, `reset --hard` back, amend, branch switch, `gc` with
  the old mark pruned, a commit made by the `git` CLI outside Scheda
  (`diff.rs` tests, one per case; the last is
  `git_cli_commit_after_a_scheda_commit`).

### B3. Indexer thread

- [x] Thread per handle (D11), wake channel + `index_poll` (D2),
      `index.lock` / `vector.lock` (D5, D6), batches bounded by the
      embedder's `many_limit` (`indexer.rs`; `open` starts it only where a
      test asks until B4 removes inline indexing;
      `background::poll_picks_up_a_commit_made_outside_the_handle`,
      `vector_role_goes_to_one_handle_with_a_matching_embedder`).
- [x] Catch-up on `open` (D2c)
      (`background::open_catches_up_with_a_commit_made_by_the_git_cli`).
- *Check:* drop of the last handle joins the thread within one batch;
  no thread left after drop (count threads in the test)
  (`background::drop_waits_for_one_batch_not_the_whole_diff`,
  `dropping_the_last_handle_joins_the_thread`: the thread's
  `/proc/<pid>/task/<tid>` is gone).
- *Check:* two handles in two processes on one tree: exactly one holds
  each lock; kill the holder, the other takes over on its next poll and
  finishes the diff (`background::indexer_role_passes_to_another_process`,
  which re-runs the test binary as the holder).

### B4. Write path

- [x] `add` / `replace` / `remove` / `commit` no longer touch lexical or
      vector; the lexical writer is no longer opened inside a
      transaction; `discard` no longer rebuilds (D3)
      (`vault.rs writes_inside_a_transaction_open_no_lexical_writer`).
- [x] Journal `id` map stays inline (D1); audit `replace` / `remove`
      for lexical-leg lookups (D18;
      `background::replace_and_remove_right_after_add_use_the_file`,
      `document.rs replace_of_unindexed_file_succeeds`).
- [x] Vector save writes only when a slot changed (§1 item 4)
      (`background::vector_dump_is_saved_only_when_a_slot_changes`).
- *Check:* a write with a stub embedder that panics or blocks does not
  block or fail the write
  (`background::a_blocking_or_panicking_embedder_neither_blocks_nor_fails_a_write`).
- *Check:* journal `add` then `get` of that `id` succeeds immediately,
  before the indexer ran
  (`background::journal_add_then_get_before_the_indexer_ran`).

### B5. Readers follow the marks

- [x] A handle without the lock reloads Tantivy and USearch when the
      marks move (poll on the same interval, and lazily on a query)
      (D31; `background::a_handle_taking_a_role_over_keeps_the_last_holders_work`
      for the role hand-over).
- *Check:* process A writes and indexes; process B, opened before,
  finds the Record after `wait_indexed` without reopening
  (`background::reader_in_another_process_follows_the_marks`: the child
  process writes; a reader that never polls reloads on its query, one
  that polls reloads before any query).

### B6. Status and wait

- [x] `Vault::index_status() -> IndexStatus`: `head`, per-leg mark, rows
      pending, role held by this handle, `model_mismatch`, last error
      (D32; `background::status_counts_the_records_each_leg_has_yet_to_apply`,
      `background::vector_role_goes_to_one_handle_with_a_matching_embedder`
      for `model_mismatch` and a vector leg nobody writes).
- [x] `Vault::wait_indexed(timeout) -> Result<IndexStatus, Error>`:
      wait until every leg this handle can see as active reaches the
      `HEAD` observed at call time; timeout → `Code::Timeout` (or the
      nearest existing code) with the status attached. A leg with no
      possible owner (vector, no embedder anywhere) does not block it
      forever: report it in the status, do not wait for it
      (`background::indexer_role_passes_to_another_process` for the
      `io_timed_out`; `vector_role_goes_to_one_handle_with_a_matching_embedder`
      for a leg with no owner).
- *Check:* `add` then `wait_indexed` then `search` finds the Record —
  this replaces the IDX-9 check "add then search without reindex"
  (`search.rs add_then_wait_indexed_then_search_finds`, both legs).

### B7. Errors

- [x] Retry with backoff, mark unchanged, last error in status (D12,
      D33; `background::a_failing_embedder_holds_back_only_the_vector_leg`).
- *Check:* stub embedder fails twice then succeeds: vectors land, no
  write failed, status showed the error in between
  (`background::embedder_fails_twice_then_vectors_land_and_the_error_showed_between`:
  the retries keep to the backoff while `wait_indexed` waits, and the
  error text does not carry the Record).
- *Check (D23):* `kill -9` in the middle of a diff breaks nothing; the
  next handle finishes from the vectors saved before the kill
  (`background::killed_mid_indexing_the_next_handle_finishes`).

### B8. Model change and `reindex`

- [x] Mismatch handling per D7 (no role, no rebuild, the query is not
      embedded; `background::model_change_waits_for_reindex_and_does_not_flip_flop`).
- [x] `reindex()` builds `N+1`, records its base commit, publishes; the
      indexer catches up from that commit (D10). Delete the queue (D34;
      `vault.rs reindex_takes_no_write_lock_and_writes_during_the_build_follow`,
      `a_write_during_the_build_is_applied_after_publish_on_both_legs`,
      `reindex_publishes_while_another_handle_holds_a_transaction`,
      `reindex_without_an_embedder_keeps_the_vectors_and_their_mark`,
      `leftover_queue_and_orphan_are_removed_on_open`).
- [x] Automatic rebuild on format change or corruption (D8, D35;
      `background::format_4_generation_is_rebuilt_in_the_background`,
      `damaged_lexical_index_is_rebuilt_in_the_background`,
      `damaged_vector_dump_is_rebuilt_in_the_background`).
- *Check:* embedder with a new `model_id`: status `model_mismatch`, no
  flip-flop with a second process on the old model; `reindex()` with
  the new one re-embeds every slot and clears the status
  (`background::model_change_waits_for_reindex_and_does_not_flip_flop`:
  the old-model handle keeps polling on both sides of the `reindex` and
  `current` does not move back).

### B9. CLI

- [x] `find` / `search` wait (D13); `scheda status` (or the existing
      `info`) prints `index_status` (D37; `cli.rs
      wait_indexed_and_index_status_print_where_the_index_stands`,
      `no_wait_queries_the_index_as_it_is`). Signals and `close` are in
      the crate rustdoc (D23 item 4).

### B10. Acceptance

- [x] All of B1–B9 *Check:* green under `bazel test //...` (every test
      named in B1–B9 exists; 39 of 39 targets green, `//rust/...` again
      without the cache).
- [x] The Knowqore contract in §6 holds (run its points as tests here
      where they are Scheda behaviour; the mapping is under the §6
      table).

---

## 4. Part T — `tokio` feature

### T1. Feature and types

- [ ] Cargo feature `tokio` (off by default) using the workspace pin
      already in `Cargo.toml` (`tokio = "=1.53.1"`), features `rt` and
      `sync` only. Bazel `crate_features`; crate_universe repin.
- [ ] `AsyncVault` and `AsyncCollection` (module name is free; avoid a
      module literally named `tokio`, which shadows the crate in paths).
      `AsyncVault` wraps `Arc<Vault>`; `From<Vault>` and
      `AsyncVault::sync(&self) -> &Vault`.
- [ ] Every non-transactional method mirrors the sync one with owned
      arguments (`String`, `Vec<u8>`, owned queries) and runs it inside
      `tokio::task::spawn_blocking`. A `JoinError` maps to a Scheda
      error, never a panic.
- [ ] The sync API is unchanged. No sync method calls `block_on` or
      builds a runtime: sync methods are called from inside
      `spawn_blocking`, from plain threads, and from `#[test]`.

### T2. Transactions

The write lock is owned by a thread (`thread::ThreadId` in `vault.rs`).
Separate `begin().await` / `add().await` / `commit().await` calls land on
different blocking threads and the second one gets `locked`. So the
async API has **no** `begin` / `commit` / `discard`. Instead:

```rust
vault
    .transaction("subject", |tx| {
        tx.collection("notes")?.add(raw)?;
        tx.collection("notes")?.replace(id, raw2)?;
        Ok(value)
    })
    .await?;
```

- [ ] The closure is sync, `Send + 'static`, runs in one
      `spawn_blocking` on the sync API: `begin`, closure, `commit`;
      `discard` on error.
- *Check:* 64 concurrent tasks each running a transaction on a shared
  `AsyncVault`: every one ends committed or `locked`, never a
  transaction split across threads.

### T3. Async wait

- [ ] `AsyncVault::wait_indexed(timeout)` fed by a `tokio::sync::watch`
      the indexer thread updates (no polling loop in the task).
- [ ] `Embedder` and `Reranker` stay synchronous traits. `search` runs
      the query embedding and the reranker inside the same
      `spawn_blocking`.

### T4. Tests

- [ ] `#[tokio::test(flavor = "multi_thread")]` and `current_thread`
      both pass for the async surface.
- [ ] A build without `tokio` compiles and its tests run with no
      runtime (LIB-6).

---

## 5. Docs pass (once, after the code)

Done for Part B on 4 October 2026 (`02`, `04`, `06`, `07`, `10`, `13`,
`14`, `15`, README); the Part T items below (async API, `tokio`
feature) follow in one pass after T. The version stays `Unreleased`
until the owner releases.

`06` (IDX-2 format 5, IDX-9 rewritten as background + marks, QRY-4 with
D8), `07` (LIB-16, new `index_status` / `wait_indexed`, the async API
and the `tokio` feature), `13` (find / search freshness), `15` (header
note: queue replaced by marks, D10; "no Index worker thread" replaced by
D11), `10` (phase table and what is next), `14` (CLI wait, status),
README (features list, status line), CHANGELOG (`0.19.0`, *Changed*:
`find` / `search` freshness is eventual; *Added*: `tokio` feature,
`index_status`, `wait_indexed`; *Removed*: reindex queue).

---

## 6. Anchor consumer check (Knowqore)

Every decision above is a general library rule; nothing in the API
names a consumer. Knowqore, the anchor consumer, is the real-world sample
to check them against. It pins Scheda by tag with features `encrypt`,
`find`, `search`, `simd`. What it relies on today, checked against its
code on 4 October 2026:

| # | Knowqore relies on | Status after this plan |
|---|---|---|
| K1 | Sync API: `knowqore_store`, its learn engine, and reasoner are sync; its server calls Scheda inside `spawn_blocking`. | Unchanged (T1). |
| K2 | `Embedder` / `Reranker` sync traits, implemented on `ureq`. | Unchanged (T3). |
| K3 | `Vault::open_with` without a runtime, in `#[test]` and in `knq init`. | Holds: indexer is a thread (D14). |
| K4 | Several `knq` processes open one Vault; one of them is the writer (its own `flock`). | One indexer per lock (D5); no conflict with Knowqore's writer lock. |
| K5 | Only the writer's handle has an embedder (`set_embedder`). | That handle takes `vector.lock`; others index lexical only (D6). Status shows vector lag. |
| K6 | The writer drains its queue with a lock wait on `write.lock`. | Indexer never takes `write.lock` (D5). |
| K7 | `get` / `ids` on `document` collections right after a write. | Unaffected: they read files. All Knowqore collections are `document`. |
| K8 | `reindex()` wrapped in `knowqore_store::reindex`. | Stays (D7, B8). |
| K9 | Tests that write `lesson-index` then `search` at once. | Must call `wait_indexed` first (B6). Knowqore changes its tests when it bumps the tag. |
| K10 | No production `find` / `search` yet (its `consider` comes later). | Eventual freshness is acceptable there. |

Knowqore needs nothing from Part T to keep working; it may adopt
`AsyncVault` in its server later.

Checked here (B10): K3, K5, K6, K7, K8, and K9 by `consumer.rs
a_writer_with_an_embedder_and_readers_without_one_share_a_vault` (a
writer with an embedder and a reader without one on one Vault, from a
plain `#[test]`; `ids` / `get` right after the commit; `wait_indexed`
then `search` on both legs; `reindex` and a `git` commit indexed while
the writer holds a transaction open). K4 by
`background::indexer_role_passes_to_another_process` and
`reader_in_another_process_follows_the_marks`. K6 also by
`background::a_blocking_or_panicking_embedder_neither_blocks_nor_fails_a_write`.
K8 also by `vault.rs reindex_after_deleting_the_scheda_directory_rebuilds_it`,
the call Knowqore's own `reindex` test makes. K1, K2, and K10 are API
shape: unchanged sync methods and traits, no new freshness promise.
Knowqore's own test suite was not run against this tree; its K9 tests
need `wait_indexed` when it moves to this release.

---

## 7. Removed together with what is added

- Inline lexical and vector updates in `add` / `replace` / `remove` /
  `commit`; the lexical writer held for the life of a transaction;
  Index rebuild inside `discard`.
- The `reindex` catch-up queue and the enqueue step in mutating calls
  (D10).
- Index format `4` (rebuilt automatically, D8).
- The IDX-9 check "add then search without reindex finds it at once"
  (replaced by the B6 check).
