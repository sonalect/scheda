# 15. Online reindex

Document 15 of the Scheda package. Version 0.4, 6 September 2026.

Work order so `reindex` no longer holds `{vault}/.scheda/write.lock` for
the whole rebuild. The walk still fills every Index artifact of the new
generation: lexical (Tantivy) in full, vector (USearch) by inherit then
digest-skip / embed, plus edges and the journal `id` maps, same as
IDX-9 / `13` §5 today. Writers (`add` / `replace` / `remove`) stay
allowed during that pass; they take `write.lock` only for their own
mutating call. One `reindex` at a time. Catch-up of those writes is a
folder of files, so a second process on the same working tree can
enqueue without sharing the rebuilder's memory.

This file records the design until R0 copies it into `04` / `06` / `07`
/ `13`. After R0 those documents are normative; tick boxes here only when
the cited *Check:* passes under `bazel test //...`. It does not reopen
C0–C10 or A0–A7. P16 is unrelated.

The first implementation is the crate `scheda`. There is no Index worker
thread (LIB-6). Catch-up runs on the `reindex` call, on that thread.

---

## 1. How to track progress

Tick a box only when the cited *Check:* passes under `bazel test //...`
(or the named `#[test]`). Do not tick from a verbal claim that the code is
ready.

| Mark  | Meaning                                  |
| ----- | ---------------------------------------- |
| `[ ]` | Not done                                 |
| `[x]` | Done; the check lives in the crate tests |

Each stage has **Status:** `not started` | `in progress` | `done`. Set
`done` only when every box in that stage is `[x]` and Clippy is clean.

If `03`–`10`, `13`, or `14` do not decide a path or a `code`, stop. Amend
the spec in R0; do not invent a silent default in `src/`.

Order: R0 → R1 → R2 → R3 → R4 → R5 → R6 → R7. R8 is acceptance on that
surface.

---

## 2. Dashboard

| Stage                                              | Status      | What closes                                         |
| -------------------------------------------------- | ----------- | --------------------------------------------------- |
| [R0](#r0-spec)                                     | done        | `04` / `06` / `07` / `13` name the protocol         |
| [R1](#r1-on-disk-lock-marker-queue)                | done        | `reindex.lock`, `reindex/active`, queue files, `AnnIndex` vector read |
| [R2](#r2-start-protocol)                            | done        | Flag before the file walk; write lock released       |
| [R3](#r3-live-generation-stays-up)                 | done        | Rebuild fills `N+1` and nothing prunes it; `find` / `search` on live `N` stay |
| [R4](#r4-mutating-calls-enqueue)                   | done        | `add` / `replace` / `remove` update `N` and enqueue |
| [R5](#r5-catch-up)                                 | done        | Queue applied to every artifact of `N+1`; vectors copied, not re-embedded when the model matches |
| [R6](#r6-publish)                                  | done        | Drain tail, `current`, switch engine and maps, drop marker |
| [R7](#r7-crash-and-stale-marker)                   | done        | Orphan generation; stale `active` without the lock |
| [R8](#r8-acceptance)                              | done        | Writers during rebuild; second `reindex` is `locked` |

---

## 3. Design

Today `Vault::reindex` takes `write.lock` for the whole call, calls
`close_for_rebuild` on this handle, fills a new generation directory
(lexical **and** vector), then points `{vault}/.scheda/index/current` at
it. A second writer is `locked` for minutes. IDX-9 incremental updates
already keep the live generation current without a full rebuild;
`reindex` is repair, editor edits, and embedder-model change.
Those still take too long to freeze writes.

The unlocked walk in this plan is that same full rebuild into `N+1`:
every eligible Record is written into Tantivy; vector slots are inherited
from `N` when the model matches, then digest-skip or re-embed. Catch-up
applies queued ids to **every** artifact of `N+1`. A generation with only
lexical catch-up would make `search` miss a Record that `find` returns.

IDX-9 names four artifacts per write. "Both legs" below is shorthand for
all four; a plan that stops at two publishes a generation that lies.

| Artifact | Where | What catch-up does |
| --- | --- | --- |
| lexical | `<gen>/lexical/` (Tantivy) | upsert the document, or delete by key term |
| vector | `<gen>/vector/` (`index.usearch`, `keys.json`, `meta.json`) | copy the slot from `N`, or embed; drop a slot that is gone |
| edges | `<gen>/edges.jsonl` | forget this source, then re-add its edges |
| journal `id` map | `<gen>/journal/{name}/ids` (IDX-5) | remember / forget the `id`, in the file **and** in the map the handle keeps |

The journal map is the one that is easy to miss. It is not part of the
search engine, and `reindex` installs the walk's map into the handle at
publish. A queued journal line that reached only the engine leaves that
`id` out of the map — and a map that exists but lacks the `id` answers
"unknown", not "scan the segments", so `get` of a line the caller wrote
minutes ago returns `not_found`. That is worse than having no map.

### 3.1 Two locks

`{vault}/.scheda/write.lock` is unchanged: one mutating call per working
tree (STO-4). `add` / `replace` / `remove` / `create_collection` /
`begin` still take it for that call only.

`{vault}/.scheda/reindex.lock` is new. Only `reindex` takes it, and holds
it from the start protocol until after publish (or failure cleanup). A
second `reindex` is `locked`. Mutating calls must **not** take this lock:
they look at the marker file in §3.2.

Both locks are `try_lock`, never a blocking wait. Today's write lock
turns `WouldBlock` straight into `locked` (STO-4), and `reindex.lock`
does the same. That is the whole reason the two orders in this plan are
safe: start takes `write.lock` and then `reindex.lock`, publish takes
`write.lock` while already holding `reindex.lock`. Blocking acquisition
in either place deadlocks two `reindex` calls against each other — one
between start and publish, one waiting on the lock it needs to release.
With `try_lock` the loser gets `locked` and drops what it holds. Do not
switch either lock to a blocking wait, and do not add a third caller of
`reindex.lock`.

### 3.2 Marker and queue

In-memory flags are not visible to another `open` of the same Vault.

| Path | Role |
| --- | --- |
| `{vault}/.scheda/reindex.lock` | flock; same kernel-release story as `write.lock` |
| `{vault}/.scheda/reindex/active` | marker: decimal generation of `N+1` and a newline. Presence means “enqueue”. |
| `{vault}/.scheda/reindex/queue/` | one file per mutating call that finished while `active` existed |

`.scheda/` is already gitignored (GIT-13). Unix modes stay IDX-4 (`0700` /
`0600`).

Queue file body is JSON: `{"collection":"<name>","id":"<id>"}` (UTF-8, one
object). Write a sibling temp in that directory, `fsync`, rename (STO-5).
`remove` uses the same shape so catch-up can `forget`.

The file **name** is the lowercase hex SHA-256 of `{collection}\0{id}`.
Not the `id` (a 252-byte slug is not a file name) and not a decimal
sequence: a counter needs a home shared by every process on the tree,
and nothing under `.scheda/` holds one. The queue is a **set**, not a
log — catch-up re-reads the Record from files, so the tenth write to an
`id` and the first apply to the same state, and order between two ids
does not exist. A digest name folds a hot loop over one Record into one
file for free.

That naming carries one hard rule: **catch-up unlinks the queue file
before it reads the Record**, never after. Unlink-after loses a write
that lands between the read and the unlink, because the writer's file
for that `id` has the same name and the unlink takes it. Unlink-before
can apply one `id` twice, which is exactly what an upsert from files
tolerates.

A write enqueues when the Index can hold that collection: not encrypted
(IDX-4 keeps those out of the Index), and either at least one Index leg
is on or the collection is a `journal` (a journal keeps an `id` map even
with both legs excluded). Every other write leaves the queue alone.

`push` needs nothing from this: it writes no Record file.
`create_collection`, `pull`, `sync`, and `discard` are `locked` while
`reindex/active` exists, with `reindex` in the stack. Each rewrites the
tree in a way no `(collection, id)` entry can express — `pull` moves
whatever the Remote carried, `discard` restores recorded paths to HEAD
and rebuilds the live Index in place, `create_collection` may replace
the `.schema.yaml` the walk already loaded — so letting them run would
publish an `N+1` that no file on disk backs. They are administrative and
rare; the caller retries after the rebuild.

One of those paths is not a caller call: a mutating call that finds
leftover Scheda writes discards them first and repairs the live Index
before doing its own work. That path knows exactly which paths it
restored, so it enqueues one file per restored `(collection, id)`
instead of failing the write.

For a `document` or a `singleton` that id is the file name, known whether
or not the file survived the restore. A `journal` is not: the ids live
inside the segment file, and restoring to HEAD **deletes** a segment that
was entirely uncommitted work. Read after the restore, that file is gone
and nothing is enqueued — so the lines the walk already copied stay in
`N+1` for good. The ids of each recorded segment are therefore read
**before** the restore, and catch-up gets the union of that snapshot and
what the files hold afterwards. The union, not the difference between the
two: catch-up re-reads every id from disk, so a redundant queue file
costs one upsert, while a missing one publishes a generation that holds
lines no file backs. The price is one queue file per line of a restored
open segment, on a path that only runs on crash leftovers while a rebuild
is active.

A human editor still needs a later `reindex` (no watcher).

### 3.3 Start (short `write.lock`)

1. Take `write.lock`, then `reindex.lock`. Second `reindex` → `locked`.
2. Create `reindex/queue/` if needed. Write `reindex/active` with the
   generation number that `prepare_generation` will use (or write `active`
   after `create_generation` returns `N+1`, still under `write.lock`).
3. Flush the live generation's vector dump. Copy it into `N+1` when
   `model_id` and dimensions match (`13` §5 `inherit`). Mismatch → empty
   vector table in `N+1`.
4. Release **only** `write.lock`. Keep `reindex.lock`.

Do not `close_for_rebuild` on the live engine. This handle keeps serving
`find` / `search` from generation `N`. Incremental writes keep going into
`N`. Rebuild fills `N+1` through a **second** engine opened on that
directory for write; the live engine object is shared with every
`Collection` handle and cannot be swapped, so publish points that same
live object at `N+1` (§3.5) and the rebuilder's engine is dropped. Two
engines mean two in-memory vector tables for the length of the walk;
that is the price of not freezing writers, and R0 says so out loud.

`N+1` has to survive the walk, and today it would not. `open` prunes
every generation that is not `current` or `current - 1` as soon as
`write.lock` is free, and during this walk `write.lock` **is** free: a
second `open` would delete the directory being filled. So pruning is
forbidden while `reindex.lock` is held. `open` takes `write.lock`, then
`try_lock`s `reindex.lock`: held → prune nothing and leave `active`
alone; free → prune as today and treat `active` as stale (§3.7). The
prune inside the start protocol is unaffected: it runs before the
marker, under `write.lock`, when nothing is in flight.

The marker exists before any file walk. A write that completed before
`active` is in the files the walk will see. A write that completes after
`active` is in the queue (and in `N`). There is no third window if
`active` is created under `write.lock`.

### 3.4 Mutating calls while `active` exists

Under `write.lock`, after the Record file (or unlink) succeeds and the live
Index for `N` is updated (IDX-9):

- If `reindex/active` exists, write one queue file.
- Always update this handle's live generation (`N`) on all four
  artifacts (IDX-9), never the incomplete `N+1`.

A second process does the same: its SearchEngine is bound to `N` at
`open` (LIB-16). It does not share the rebuilder's memory. The queue file
is how `N+1` learns about that write.

**A `remove` during the walk.** On the queue side both orders already
work: the walk has not reached that file yet, so `N+1` never holds the
Record and catch-up's forget is a no-op; or the walk already upserted
it, and catch-up forgets it. The walk itself is the part that breaks. It
lists a collection's files into a vector and opens each path afterwards,
and a read that finds nothing is `io_not_found` — so one `remove`
landing in that window aborts a rebuild that has been running for
minutes. Under today's exclusive lock that window does not exist; here
it is ordinary. Both walks — the counting pass and the search-engine
pass — must treat a path that vanished between the listing and the read
as absent: skip it, count nothing, carry on, and let the queue entry do
the forgetting. A file that appears after the listing is not the walk's
problem; it has a queue entry.

That makes `ReindexResult` a count of what the walk saw, not a tally of
the tree at any single instant: a Record removed mid-walk may or may not
be in `records_read`. OPS-3 measures a quiet tree, and R0 says that
instead of promising a number this protocol cannot hold.

`find` and `search` after `add` on the writer handle stay correct on `N`
(IDX-9). A reader handle that opened before the write may be slightly
behind (STO-4).

Two Tantivy `IndexWriter`s on the same live lexical directory remain
forbidden. `N` and `N+1` are different directories, so the rebuilder's
Tantivy writer never meets the live one. The vector dump has no
directory lock, but STO-4 still serializes mutating calls so two
processes do not dump `keys.json` / `index.usearch` at once.
Parallelism is: the rebuild writing `N+1` without `write.lock`, and a
mutating call writing `N` with `write.lock`.

A transaction is the one writer that holds `write.lock` for longer than
a call. Mutating calls inside it enqueue as they run, which is correct —
files are truth and catch-up reads files. What it costs is publish: a
transaction still open when the rebuild finishes holds the lock the
publish step needs (§3.5).

### 3.5 Catch-up (mostly without `write.lock`)

After the file walk has filled `N+1` (lexical rebuilt; vectors inherited
then digest-skip / embed as today):

1. Without `write.lock`, take every queue file, apply it to `N+1`,
   commit. More files may appear (writers still run). Repeat until a
   pass sees none, or go once and leave the tail for step 2.
2. Take `write.lock` (still holding `reindex.lock`). Drain whatever
   remains. No concurrent mutating call can add a file during this drain.
3. Point `current` at `N+1` (one atomic write). Point this handle's live
   engine and journal maps at `N+1`. Delete `reindex/active`. Then
   release `write.lock`, then `reindex.lock`.

`write.lock` in step 2 is `try_lock` and can lose to a writer that is
mid-call. Losing must not throw away a rebuild that took minutes, so
retry with a short backoff for a bounded window, applying any queue file
that appears between attempts. Only when the window closes is the answer
`locked`: `current` unchanged, `N+1` an orphan, the caller runs
`reindex` again. R0 names the window; the proposal is 30 seconds at
25 ms steps, which a per-call writer never exhausts and an open
transaction always does.

Apply per queued id: unlink the queue file, then read the Record from
the Vault (files are truth). Present and valid → upsert every artifact
of `N+1`: lexical document, vector slots (below), edges, and for a
`journal` the `id` map, both the `ids` file under `N+1` and the map
object publish will install. Absent → forget the Record on the engine
and drop the `id` from that map. Invalid (`valid` is not true) → leave
it out, same as the walk (IDX-3). Duplicate ids in the queue are a
second upsert; a set of `(collection, id)` is allowed.

Nothing may skip the in-memory map. Publish replaces this handle's
journal maps with the one the walk produced; a queued line that reached
only the `ids` file is dropped from memory at that moment, and the
present-but-incomplete map then answers "unknown" for it — `get` returns
`not_found` for a line the caller wrote and committed.

Any error in catch-up aborts the reindex: no publish, `current` stays on
`N`, `N+1` becomes an orphan. Unlink-before-apply means a taken queue
file is gone, so a half-applied queue must never become the live
generation.

**Vector slots on catch-up.** If `N` and `N+1` share `model_id` and
dimensions, copy that Record's slots — the vector and its digest — into
`N+1` instead of calling the embedder: `add` already paid for them on
`N`. A slot whose digest already matches in `N+1` stays as it is; a slot
`N+1` holds that `N` no longer has goes away.

Read those slots from **`N`'s vector dump on disk**, loaded once per
catch-up pass, not from this handle's in-memory store. The queued write
may come from another handle or another process: its vector reached the
dump when that writer committed and never touched this handle's memory.
Reading memory here finds nothing, and the digest check then re-embeds
text that is already vectorised one directory away.

A Vault with no embedder copies nothing. The walk wrote no vectors, so a
catch-up that created a vector table would publish `N+1` with a vector
leg no other part of that generation has.

If the models differ, `N` did not mix spaces (`13` §5); embed into `N+1`
**before** the final `write.lock`, with `embed_many`. The locked tail
must not wait on a network embedder: either the tail is empty or those
few slots copy from `N` (same-model) or were already embedded in the
unlocked pass.

Reading a stored vector back was not part of `AnnIndex` — `add`,
`remove`, `contains`, `search`, `save`, `restore` and no way out — so R1
adds one accessor over USearch's `Index::get(key, &mut [f32])`. Without
it the same-model path in R5 and the "zero embedder calls" box in R8 are
not reachable, and catch-up pays the embedder a second time for text it
already embedded on `N`.

Clear `active` only after the engine switch, still under `write.lock`.
Clearing earlier lets `add` update `N` and skip the queue while publish
moves `current` to `N+1` without that Record.

### 3.6 Failure

- Rebuild error: do not publish `current`. Drop `active`, leave `N` live,
  restore this handle if it was pointed at `N+1`. Queue files may remain;
  they describe writes already in `N`. The next `reindex` or `open` that
  finds `active` without `reindex.lock` treats it as stale (§3.7).
- Drop of the reindex handle: kernel releases `reindex.lock`. Same stale
  path.
- `locked` on a second `reindex` names `reindex` in the stack, as today's
  `locked` names the method that took `write.lock`.
- Publish window exhausted (a transaction held `write.lock` throughout):
  `locked`, same cleanup as a rebuild error. The rebuild is lost; that is
  honest, and one writer per tree is STO-4, not a regression here.
- `reindex` **inside** an open transaction keeps today's behaviour: this
  handle already holds `write.lock` and cannot release it mid-call, so
  the call takes `reindex.lock`, rebuilds with the lock held, publishes,
  and writes no marker and no queue. Nothing else can be running, since
  `write.lock` was never free.

### 3.7 Stale `active`

`open` already prunes orphan generations when `write.lock` is free. Under
that same held `write.lock`, `try_lock` `reindex.lock` once:

- **Free.** No rebuild is running. Delete `reindex/active` and the queue
  files, then prune as today; the unfinished `N+1` is an orphan
  generation and goes with the rest. Do not apply the queue to live `N`:
  those Records were indexed there when they were written.
- **Held.** A rebuild is between start and publish. Prune nothing, touch
  neither `active` nor the queue, release `write.lock`, and open on `N`
  as usual (§3.3).

A crash after `current` moved but before `active` was deleted is the
free case with an empty queue (the drain ran under `write.lock` before
publish): the marker goes, `current` stays on `N+1`, nothing rebuilds.

### 3.8 Out of this plan

- An Index worker thread or eventually-consistent `find` after `add`.
- Letting two mutating calls overlap (two `IndexWriter`s on `N`).
- Online `pull` / `discard` / `create_collection`: `locked` while a
  rebuild is active (§3.2).
- Holding a rebuild across a long transaction: publish gives up and the
  caller runs `reindex` again (§3.6).
- File watcher (default remains `reindex` after a human edit).
- Changing generation retention (still live + previous).
- Compacting the USearch dump (`13` §5 leaves tombstones as they are).

---

## R0. Spec

**Status:** done

**Codes:** STO-4, LIB-16, IDX-9, QRY-4, IDX-4, GIT-13.

Copy §3 into `04` (second lock, marker, queue, lock order), `06` / `13`
(live generation during rebuild; vector copy on catch-up), and `07`
LIB-16 (write lock is not held for the walk; second `reindex` is
`locked`). No new `Error.Code` unless `07` says so: reuse `locked`.

- [x] STO-4 says `reindex` takes `write.lock` only for start and publish;
      the walk does not hold it. One writer per mutating call is unchanged.
      Both locks stay `try_lock`; neither becomes a blocking wait.
- [x] LIB-16 describes `reindex.lock`, `reindex/active`, and the queue
      directory. Second `reindex` → `locked`.
- [x] LIB-16 names the publish window: `write.lock` is retried for a
      bounded time, and an exhausted window is `locked` with `current`
      unchanged. The number is in the document, not only in `src/`.
- [x] LIB-16 (and LIB-21 / LIB-14 / LIB-23 as they read today) say that
      `create_collection`, `pull`, `sync`, and `discard` are `locked`
      while a rebuild is active, and that `push` is not.
- [x] LIB-16 says `reindex` inside an open transaction keeps the whole
      lock for the call (no marker, no queue).
- [x] IDX-9: while `active` exists, a successful write still updates all
      four artifacts of live `N` and also writes a queue file. Catch-up
      updates the same four on `N+1`, journal `id` map included.
- [x] IDX-1 / IDX-2: `open` must not prune generations while
      `reindex.lock` is held; `N+1` is not an orphan until that lock is
      free.
- [x] `13` §5: catch-up copies vectors from live `N` when the model
      matches; embedder only on mismatch, not under the publish lock.
- [x] QRY-4 still fail-closed on a corrupt `N+1`; a reader on `N` is not
      “being rebuilt”.
- [x] OPS-3 says `ReindexResult` counts what the walk saw. A Record
      written or removed during the walk may fall either side; the hand
      tally holds on a quiet tree.
- [x] A build without `find` follows the same protocol with the artifacts
      it has: marker, queue, journal `id` maps, stale marker on `N+1`.
      Stated once, so the no-`find` path is not left to the reader.
- [x] `bazel test //...` green (docs/lint).

---

## R1. On-disk lock, marker, queue

**Status:** done

**Needs:** R0.

- [x] `reindex.lock` is flock next to `write.lock`, kernel-released on
      process death. Not a pid-file. Not under `index/`.
- [x] `reindex/` is `0700`; `active` and queue files are `0600` (IDX-4).
- [x] Queue file is JSON `collection` + `id`; name is the hex SHA-256 of
      `{collection}\0{id}`, not the slug and not a sequence. Temp +
      `fsync` + rename.
- [x] Two writes to the same `(collection, id)` while `active` exists
      leave exactly one queue file.
- [x] `AnnIndex` can read a stored vector back (`get` over USearch
      `Index::get` / `export`), returns `None` for an absent key, and
      round-trips a vector added through `add`. Without it R5 cannot copy
      slots.
- [x] A leftover `{vault}/.isolated/.scheda/` is still removed by
      `reindex` (LIB-16); this layout does not revive a nested Index.
- [x] `#[test]` writes one queue file and reads it back; illegal
      `collection` / `id` in a hand-written file is ignored or `invalid`
      as `07` states after R0, never a path traversal.

---

## R2. Start protocol

**Status:** done

**Needs:** R1.

- [x] `reindex` takes `write.lock` then `reindex.lock`. Second `reindex`
      on another handle or process → `locked` with `reindex` in the
      stack.
- [x] `active` exists before `prepare_generation`'s file walk (journal
      maps and lexical/vector fill).
- [x] After start, `write.lock` is free: a second handle `add`s and is
      not `locked` (`holds_write_lock` false on the rebuilder).
- [x] `reindex.lock` is still held (`#[test]` in-process: second thread
      `reindex` → `locked` while the first is between start and publish).
- [x] Vector dump of `N` is flushed and inherited into `N+1` before
      `write.lock` is released (same-model). Model mismatch: `N+1` vector
      table empty.
- [x] `create_collection` / `pull` / `sync` / `discard` between start and
      publish → `locked` with `reindex` in the stack; `push` still runs.
- [x] `reindex` inside an open transaction writes no `active` and no
      queue file, and the lock is held for the whole call.

---

## R3. Live generation stays up

**Status:** done

**Needs:** R2.

- [x] This handle does not `close_for_rebuild` at start. `find` /
      `search` on it during the walk still hit generation `N` (lexical
      and vector).
- [x] Rebuild fills a second engine on `N+1`. Incremental `add` on this
      handle upserts `N`, not the incomplete `N+1`.
- [x] A second `Vault::open` of the same path still binds `N` until
      `current` moves (LIB-16). `find` there returns the Record that
      existed before `reindex` started.
- [x] That second `open` does **not** delete `N+1`: it holds
      `write.lock`, sees `reindex.lock` held, and prunes nothing. After
      publish the rebuild is complete and `current` names `N+1`.
- [x] A Record file unlinked between the listing and the read is skipped
      by both walks, not an `io_not_found` that fails `reindex`.
      `#[test]`: `remove` while the rebuilder is inside the walk of that
      collection; the rebuild still publishes.
- [x] Existing generation tests still pass: crash before publish leaves
      `current` on `N`; `open` drops the orphan (`query.rs`
      `crashed_reindex_leaves_old_generation_and_open_drops_orphan`).
- [x] `reindex_without_publish` still fills `N+1` without swapping
      `current` (`vault.rs`
      `reindex_without_publish_keeps_old_generation_live`).

---

## R4. Mutating calls enqueue

**Status:** done

**Needs:** R3.

- [x] `add` / `replace` / `remove` after a successful file mutation: if
      `active` exists, one queue file. Live `N` updated on all four
      artifacts (IDX-9).
- [x] No `active` → no queue file (hot path unchanged).
- [x] A `journal` collection with both Index legs excluded still
      enqueues: its `id` map is an artifact too.
- [x] A mutating call that first discards crash leftovers enqueues one
      file per restored `(collection, id)`, so `N+1` sees the restore.
- [x] A restore that **deletes** an uncommitted `journal` segment still
      enqueues the lines it held, because their ids are read before the
      restore and not from the file that is now gone
      (`vault.rs restore_of_uncommitted_journal_segment_enqueues_deleted_ids`).
      Catch-up then drops them by the missing-file rule in R5.
- [x] Second process (or a second `open` in the same test) `add`s while
      the first handle is in the unlocked walk: queue file appears;
      rebuilder memory is not required.
- [x] `remove` during the walk: live `N` forgets the id; queue file
      exists so `N+1` will forget it whether the walk had already
      upserted it or had not reached the file at all.
- [x] `remove` of the id the walk is reading right now: `reindex`
      succeeds and `N+1` does not hold that Record.
- [x] Encrypted collections still skip the Index (IDX-4); no queue file
      for them, or the catch-up ignores them — one behaviour, stated in
      R0, with a test.

---

## R5. Catch-up

**Status:** done

**Needs:** R4.

- [x] Unlocked pass unlinks each queue file **before** reading the
      Record, then applies it to all four artifacts of `N+1`. Duplicate
      `(collection, id)` is safe.
- [x] A queued `journal` line reaches `<N+1>/journal/{name}/ids` **and**
      the map installed at publish: after publish, `get` of that `id`
      returns the line (not `not_found`) without a second `reindex`.
- [x] Same embedder model: catch-up does not increment a test embedder's
      call count for an `add` that already embedded on `N` (copy slots).
      `search` on `N+1` after publish still returns that Record
      (`vault.rs same_model_catch_up_copies_queued_vectors`, where the
      queued `add` comes from a second handle, so the slots can only be
      found in `N`'s dump).
- [x] A Vault opened without an embedder writes no vector table in `N+1`
      during catch-up: after publish, `<N+1>/vector/` does not exist
      (`vault.rs catch_up_without_embedder_does_not_create_vector_table`).
- [x] Different `model_id`: catch-up embeds into `N+1` without holding
      `write.lock`; a stub embedder records those calls. `search` after
      publish uses the new space.
- [x] Missing file for a queued id → forget it on every artifact of
      `N+1`, journal map included (covers `remove`).
- [x] Invalid Record stays out of `N+1` (IDX-3), same as the walk.
- [x] An error while applying the queue aborts: `current` still names
      `N`, no partly applied generation is published.

---

## R6. Publish

**Status:** done

**Needs:** R5.

- [x] Final `write.lock`: drain remaining queue files, `publish_generation`,
      switch this handle's engine **and** journal maps to `N+1`, delete
      `active`, then release `write.lock` and `reindex.lock`.
- [x] `write.lock` busy at that moment is retried, not fatal: a writer
      that holds it for one call does not fail the publish. A handle that
      holds it past the window → `locked`, `current` still `N`
      (`vault.rs publish_gives_up_when_transaction_holds_write_lock`; the
      window is 30 s in the crate and the test shortens it on its own
      thread rather than waiting it out).
- [x] `add` after return updates `N+1` only; no queue file.
- [x] A handle that opened before publish still uses `N` until it reopens
      or is older than one retained generation (LIB-16).
- [x] `find` and `search` on the rebuilder after return see both the walk
      and the queued writes (lexical hit and vector hit).
- [x] Owner-only modes on `N+1` after publish (IDX-4).

---

## R7. Crash and stale marker

**Status:** done

**Needs:** R6.

- [x] Kill after `active` and `N+1` exist, before `current` moves:
      `current` still names `N`; `open` prunes the orphan; `active` and
      queue files are gone; `add` does not enqueue.
- [x] Kill after publish, before deleting `active`: `open` or the next
      `reindex` must not rebuild from a false `active`. `current` stays
      on `N+1`.
- [x] Rebuild error (inject a failing embedder or a chmod on `N+1`):
      `current` unchanged; this handle still `find`s on `N`; `active`
      gone or stale-cleared on next `open`.
- [x] `tx.log` is still not deleted by `reindex` (GIT-2).

---

## R8. Acceptance

**Status:** done

**Needs:** R0–R7.

- [x] Process or thread A in `reindex` (slow embedder or a test latch in
      the walk). Handle B `add`s a new `document` and is not `locked`.
      After A returns, both handles that use `N+1` `find` **and**
      `search` that id (LIB-16, IDX-9).
- [x] Same setup, B `remove`s an id the walk already copied: after
      publish, `find` and `search` do not return it.
- [x] Same setup on a `journal`: B `add`s a line during the walk; after
      publish `get` of that `id` returns it and does not scan segments.
- [x] Same setup, B `open`s the Vault (a third handle) mid-walk: the
      rebuild still publishes; `N+1` was not pruned.
- [x] Second `reindex` while the first holds `reindex.lock` → `locked`.
- [x] Same-model `reindex` on a fixture where no field text changed: test
      embedder call count is zero after inherit (existing digest skip)
      plus zero extra from catch-up copies
      (`tests/reindex.rs same_model_reindex_does_not_embed_again` for the
      walk, `vault.rs same_model_catch_up_copies_queued_vectors` for the
      queue, and `vault.rs catch_up_without_embedder_does_not_create_vector_table`
      for the Vault that has no embedder at all).
- [x] `bazel test //...` green. Clippy clean. `07` / `04` / `13` match
      the crate (no leftover “`reindex` holds the write lock for this
      call” as the whole duration).
