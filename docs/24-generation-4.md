# 24. The Vault of generation 4

Document 24 of the Scheda package. Version 0.3, 22 October 2026.

Status: approved by the owner on 21 October 2026. The decisions of §2
were taken one at a time, with the anchor consumer's facts; the proposals
of §3 were accepted, P8 with `Close` cancelling a merge instead of waiting
for it.

Work order: close the on-disk format of a Vault once, in one generation
and one migration, so that a write costs the same at a million Documents
as at a hundred. Generation 4 takes everything that changes the bytes on
disk: folders of bounded size for every collection, a Vault without a work
tree, one pack per commit with a bounded merge, and no reflog or
`commit.lock` note in a Vault without a work tree. The library has no step
from generation 3 to 4: a consumer that keeps its data moves a Vault with a
script of its own (D11); the anchor consumer starts with a new Vault
instead.

---

## 1. Why

Measured on 21 October 2026, after `0.25.0` (throwaway probes, removed;
the numbers are in `.claude/notes/folder-layout.md`):

- **A write grows with its folder.** One `Upsert` into a flat collection
  takes about 160 ms at 100 000 Documents and 1.75 s at 1 000 000, on
  tmpfs: libgit2 parses and rewrites a tree of every entry of the folder.
  Two levels of 256 folders bring it to 2.3 ms at 1 000 000.
- **A write grows with the repository.** Every write reads git's index
  (`.git/index`) whole and writes it whole (`commit::settle`), and it holds
  every path of the repository. With the folders bounded, the write still
  took 570–610 ms at 1 000 000 Documents. libgit2 1.9.7 reads only the
  `TREE`, `REUC`, and `NAME` extensions, so neither a split nor a sparse
  index is available.
- **A write syncs every object.** Scheda turns on
  `GIT_OPT_ENABLE_FSYNC_GITDIR`, so libgit2 syncs each blob, each tree on
  the path, and the commit, each with its folder: 18 `fsync` calls for one
  Document at the top of a collection, 24 three folders down, about 5.5 ms
  each on the owner's NVMe ext4. Each level of folders would add 2. The
  anchor consumer's test binary takes 15 s with its temporary files on
  tmpfs and 499 s on NVMe; the cost of a write on disk is the main cost of
  its user.
- **Lance syncs nothing.** The indexer round syncs Scheda's own state file
  and none of the files of the Lance version it names.

The anchor consumer's ids, a constant letter and 32 hex digits:
`observations/{container}/{kind}/o<hex>.md` (tens of thousands in a
folder), `candidates/{container}/k<hex>.yaml`, `clusters/{family}/c<hex>.yaml`
(up to millions in a family), `lessons/l<hex>.yaml` (millions in one
folder), `answers/{step}/<hex>/<hex>-<hex>` (millions of subfolders in the
folder of a step). It reads the observations of a container by a lazy
listing from an id prefix and stops at the first id outside it.

---

## 2. Decisions

All taken by the owner on 21 October 2026.

**D1. An adaptive prefix tree over the bytes of the ids.** A folder of
more than N entries is split into subfolders, only where ids are dense. The
file name stays the id; a subfolder's name cannot be an id. A listing in id
order is an in-order walk: lazy, memory in the depth. Rejected: a hash of
the id (loses the order of ids) and a fixed prefix (unbounded folders when
ids share their start).

**D2. The layout of a folder follows from the set of names in it alone.**
No margin between split and merge: the same Documents give the same tree
on every clone and in a join with the mirror, and `Check` knows the one
place of every file. Every entry counts, files and id subfolders (the
consumer's `answers/{step}/`); dotfiles (`.collection.yaml`) neither count
nor move. The split is at the first byte where the names of the folder
differ; a subfolder is named `_` and the bytes it adds to its parent's
prefix, in lowercase hex. A folder that hovers at N splits and merges
again on each write: a few hundred bytes of trees and up to N moved files,
which no reader counts as a change (D4).

**D3. N = 64.** A leaf holds at most 64 entries. A write's pack is mostly
its leaf tree: measured with the consumer's ids and 2 KB Documents, a
write leaves at most about 3.5 KB of history at N = 64 against 9 KB at 256
(2.5 KB at 1 000 000 Documents against 9.0). A split folder holds at most
257 entries (one per next byte, and one name equal to its prefix); 16 for
hex ids. Every folder shows whole in GitHub's web view, which truncates at
1 000 files. N is a constant of the generation, not a declaration key.

**D4. A move by the layout is no change.** A file that moves to another
folder of the layout with the same id and the same blob is no version for
`History`, no change for the indexer (nothing is projected or embedded
again), and, when the change feed comes (S5), no change there either.

**D5. Journals take the same layout.** The segment folder of a journal is
laid out by the same rule: a journal only grows, and a flat folder would
be a later generation.

**D6. A Vault without a work tree, chosen when it is created.** No files
on disk and no git index: a write does not pay for every path of the
repository. A person reads such a Vault with `git show` or `git log`, or
clones it. A Vault with a work tree stays the default. The anchor consumer
creates every Vault of its own this way.

**D7. One pack per commit.** The objects of one write, or of one batch,
are written as one pack before `HEAD` moves: four syncs (the `.idx` and
its folder, the `.pack` and its folder), whatever the depth and the number
of Documents. With the branch (2), a write in a Vault without a work tree
syncs 6 times, about 33 ms on the owner's disk.

**D8. Packs merge geometrically below a cap.** Factor 2, run when 16 packs
are below the cap; the cap is 64 MiB or 100 000 objects, whichever comes
first; a pack past the cap is not merged again. Merges run in the
background of the Vault, off the write lock. Measured: the worst merge,
about 85 MiB, takes 13–17 s and about 500 bytes of memory an object of it
(about +50 MB at the object cap). Model at N = 64: about 105 packs after
1 000 000 writes. A full repack, if one is ever needed, is an operation of
Scheda with bounded memory; nobody is told to run `git gc`.

*Amended 21 October 2026, after G1 measured it:* libgit2's packer
compresses every object again on every merge (it has no reuse of deltas
or compressed data), about 130 µs an object, and with factor 2 an object
is merged about 15 times on its way to the cap: the merge fell behind
writes from the first 500 (4 000 writes: 107 to 354 packs, writes from
1 ms to 5 ms). So a merge does not use the packer. It copies the
compressed entries of the packs it merges, as `git repack` reuses them,
into one pack stream, and libgit2's indexer takes the stream: it checks
every object, writes the index, and syncs, as for the pack of a write. A
pack whose objects are all in another pack of the merge is dropped; an
entry whose object came already is skipped, and a delta on it by offset
is written as a delta by id. Merged packs keep the size of the packs of
the writes (no new deltas: about 3.3 KB a write against 2.5 KB at
N = 64).

*Amended 21 October 2026:* the packs below the cap number fewer than
32 whenever no merge is replacing packs (a merge keeps its new pack
beside the ones it replaces until it has removed them, so for that moment
there is one more). A write that finds 32 or more merges first, under
the pack lock (it waits for a merge of the task that holds it), until
fewer remain, and then writes; when that merge fails, the write fails
with its code and writes nothing. So the bound holds whatever the load
and however short the process: the command line closes a Vault
milliseconds after it opens it, and `Close` cancels the merge of the
task.

**D9. libgit2's object cache is 16 MiB.** The library caches every tree
and commit up to 4 KB, up to 256 MiB a process; with N = 64 every tree is
under 4 KB, so a process that reads a lot fills any budget. Measured with
1 000 000 Documents: a random read takes 19 µs (1 pack), 34 µs (100
packs), and 91 µs (500 packs) at 16 MiB, against 19, 34, and 80 µs at
256 MiB; about 30 MB of memory. Scheda sets it for the process when it first opens a Vault,
whoever initialized libgit2 before: the anchor consumer reads its source
mirrors through `git2` in the same process, and wants one budget for both.

**D10. No write-ahead log.** Six syncs are enough; a log would make a
Vault after a crash unreadable to plain git until Scheda replays it, and
adding one later is a new generation. Throughput under load comes from a
group commit, which needs no format change (P10).

**D11. No migration in the library.** A consumer that keeps its data
moves a Vault with a script: it reads every Document of the old Vault
through `0.25` (`collection NAME get-many --include-raw`, raw bytes, the id
as it is) and writes it into a new Vault (`batch-documents`), which lays it
out; `History` and `write_id` of the old Vault do not carry over. The
anchor consumer does not move its Vault: the owner decided it starts with
a new Vault and a full learn. G6 proves the way for other consumers.

**D12. Lance's files are synced before the state that names them.** In
the indexer round: the data, transaction, manifest, and index files of the
new versions, with their folders, then the state file. After a crash at
most the last round is done again, never the whole Index: at millions of
Documents a rebuild embeds every text again.

**D13. The longest path under a Vault without a work tree is at most 120
characters**, with the longest names the rules allow (Windows: 260 in all,
the consumer's Vault 55–70 deep). Measured today: the journal id maps,
named by their collection, reach 303; Lance 115 and growing with its
counters; git 68 (the nested repository about 78, by its prefix). The id maps get names of
their own and the Index's folders get shorter (§4 G4). Both are the Index's,
rebuilt, so no migration.

**D14. `OpenOptions.initial_branch`.** The branch a Vault that `Open`
makes starts on. Unset keeps today's `init.defaultBranch`, or `main`. A
new Vault starts on it; a clone of an empty mirror too; a mirror that has
the branch is checked out on it whatever its `HEAD` names; a mirror that
holds commits but not the branch is `INVALID_ARGUMENT`, told from the
clone's own references, with nothing left. An attach does not check it. A
name libgit2 does not take as a branch is `INVALID_ARGUMENT` before
anything is written.

**D15. The default segment cap of a journal is 128 KiB** (was 1 MiB). An
append rewrites the open segment, about half the cap on average; with the
layout more segments cost nothing. A line longer than the cap still gets a
segment of its own; a cap set in the declaration (1 byte to 32 MiB) is
unchanged.

---

## 3. Proposals for review

Mine, filling in what the decisions leave open.

**P1. The mode is `OpenOptions.work_tree` and `VaultInfo.work_tree`.** An
enum `WorkTree`: `WORK_TREE_UNSPECIFIED` (as `WORK_TREE_FILES`),
`WORK_TREE_FILES`, `WORK_TREE_NONE`. It applies when `Open` makes a Vault
(a new one or a clone); on an attach a set value must be the Vault's
(`INVALID_ARGUMENT` otherwise, as `mirror`). `VaultInfo.work_tree` reports
it. The CLI takes `--work-tree none`. *Recommendation:* accept.

**P2. The mark of the mode is git's own `core.bare = true`.** In the local
configuration of `{vault}/.git`, and of `{vault}/.isolated/.git`. Git then
refuses every command that needs a work tree (`git status`, `git commit`),
so no person's command can stage the deletion of a million files, and an
older Scheda refuses the Vault without writing anything (0.25 was predicted to take it for
a Vault another `Open` is making and answer `ABORTED`; a run of the `0.25.0`
binary on 22 October 2026 showed otherwise: `info` and a write answer
`INVALID_ARGUMENT`, `--mode open-existing` answers `NOT_A_VAULT`, and nothing
in the Vault changes, which is what the proposal needed). Local, not committed: a clone chooses its own mode.
*Recommendation:* accept.

**P3. Without a work tree there is no reflog and no note.** Such a Vault
sets `core.logAllRefUpdates = false` and keeps no `logs/`; the paths of a
write are not listed in `commit.lock`, which stays the lock alone, since
there are no files to settle. A Vault with a work tree keeps both: its
files need the note, and the reflog is a person's tool there.
*Recommendation:* accept.

**P4. Folder names.** A subfolder is `_` and the lowercase hex of the
bytes it adds to its parent's prefix (the common prefix of its names and
the byte that splits them). A label longer than 120 bytes is cut into a
chain of folders of at most 120 bytes each, so a folder name stays at most
241 characters. Example, `lessons/l<hex32>.yaml` at 1 000 000:
`lessons/_6c61/_33/_66/_31/la3f1….yaml`. *Recommendation:* accept.

**P5. Reading tolerates a tree that is not canonical; writing makes the
folders it touches canonical; `Check` reports the rest.** The descent finds
a file at any level of its prefix path, so a commit made by hand or a
mirror's tree reads. `Check` reports a file outside its canonical place and
a folder past its bound (`INVALID_ARGUMENT`, with the path); a write lays
out every folder on the paths it touches. *Recommendation:* accept.

**P6. The mirror joins by id.** Each side's changes since the merge base
are read by id, moves netted (D4); the per-Document rule of today is kept
(one side, the same on both, or `MIRROR_CONFLICT`); the result is written
in the canonical layout, so both clones get the same tree. *Recommendation:*
accept.

**P7. A write packs exactly its own objects.** The blob of each `Put`,
the trees on the changed paths, and the commit, written through an
in-memory object database that reads the repository's, then given to the
packer by id. `Mempack::dump` is not used: it packs each commit with its
whole tree (`git_packbuilder_insert_commit`), and the thin-pack call is not
bound in `libgit2-sys` 0.18.8. The pack is complete on disk before `HEAD`
moves. *Recommendation:* accept.

**P8. The merge is a task of the Vault under `{vault}/.scheda/pack.lock`.**
Taken without waiting (another handle's merge runs: nothing to do), woken
after a write and at open. `Vault.Close` does not wait for a merge to
finish (the worst takes 13–17 s, which would hold up the stop of a
server): it cancels it, through the packer's progress callback (at most
every half second) and the stream the pack is written from
(`PackBuilder::foreach` into the object database's pack writer), and waits
only for that, so no lock is held after `Close`. What a cancelled merge
leaves is what a killed one leaves (G1); the merge runs again after the
next open. It names the
objects by their paths, read from the commits of the packs it merges, so
the versions of one leaf or one segment become deltas of each other
(measured: 0.35 KB less a write than without names). The loose objects a
person's commit leaves are merged the same way. *Recommendation:* accept.

**P9. The id maps of journals and the Index's folders.** The id maps are
named by a number the Index assigns and keeps in its state, not by the
collection; the generation and table folders are named in a few characters
(`g{n}`, `t{n}{kind}`). *Recommendation:* accept.

**P10. The number of a write in its commit, from the start.** Each
`Scheda-Note:` trailer carries `write`, the number of its write in the
commit, always 0 until a group commit puts several writes in one commit.
`write_id` stays the commit's for write 0. *Recommendation:* accept.

---

## 4. Stages

Code and tests first in each stage, then `cargo test` and `cargo clippy`
green; a box is ticked when its *Check:* passes under `bazel test //...`.
A stage in between may be red; no temporary code is written to keep one
green. One docs pass at the end (G7).

### G1. One pack per commit, the merge, the cache

D7, D8, D9; P7, P8.

- [x] A write and a batch store their objects as one pack before `HEAD`
      moves; nothing is left loose (`tests/pack.rs`:
      `a_write_and_a_batch_of_500_documents_each_leave_one_new_pack_and_no_loose_object`).
- [x] The merge rule of D8 as a task of the Vault (P8), in place of the
      packing at the end of a write; the loose objects of a person's commit
      join it (`pack::tests`: `the_split_is_geometric_with_factor_two`,
      `the_task_merges_after_a_wake_and_holds_no_lock_when_it_is_done`,
      `the_loose_objects_of_a_person_join_a_merge_and_alone_cause_one_past_a_thousand`).
- [x] The object cache budget set at the first `Open` of the process
      (`pack::tests`: `the_cache_budget_is_16_mib_after_the_first_open_when_git2_filled_the_cache_before`).
- [x] A process killed in the middle of a merge leaves the Vault whole
      (`tests/pack.rs`: `a_process_killed_in_the_middle_of_a_merge_leaves_the_vault_whole`);
      a run of open, write, close never leaves 32 packs below the cap, and
      a write whose merge fails writes nothing (`pack::tests`:
      `a_run_of_open_write_close_never_leaves_hard_packs_below_the_cap`,
      `a_write_whose_merge_fails_fails_with_its_code_and_moves_nothing`).
- [x] The syncs of one write counted by `strace` (13 with a work tree):
      a run on 22 October 2026, noted in the CHANGELOG; no test holds the
      count.
- [x] The slow measurements (`pack::tests::measure`, release, tmpfs, run
      once on 22 October 2026): 100 000 writes, 14 packs after 2 000 and
      after 100 000, a read 15.4 then 8.9 us, anonymous memory 77 then
      126 MiB (a known issue, `.claude/notes/generation-4-known-issues.md`);
      the worst merge at the cap, 192 000 objects in 0.3 s, +9 MiB; a read
      7.5 us with 1 pack, 38.9 with 100, 116.8 with 500. They are
      `#[ignore]` tests, run by hand.
- *Check:* a write and a batch of 500 Documents each leave one new pack
  and no loose object; the syncs of one write counted by `strace` (13 with
  a work tree, any depth), the count noted in the CHANGELOG; after 100 000
  writes of a pack each, with the merge task running, the time to read an
  object and the memory have not grown (a memory target, run alone, below
  the Vault: a Vault with a work tree rewrites git's index whole on every
  write, so its writes grow with the Documents until G2); the peak memory
  of the worst merge measured;
  the time to read a Document with 100 and with 500 packs (synthetic); a
  process killed in the middle of a merge leaves the Vault whole and
  `Check` clean; a run of open, write, close never leaves 32 packs below
  the cap, and a write whose merge fails writes nothing; the budget is
  16 MiB after the first `Open` when `git2` filled the cache before it.

### G2. A Vault without a work tree

D6, D14; P1, P2, P3.

- [x] `OpenOptions.work_tree`, `VaultInfo.work_tree`, `--work-tree` in the
      CLI (generate) (`tests/documents.rs`: `an_attach_asks_for_the_mode_the_vault_has`;
      `rust/cli/tests/cli.rs`: `work_tree_and_initial_branch_flags_reach_the_open_options`).
- [x] `Open` makes, clones, and attaches such a Vault; a write and a pull
      write no file and no git index; no reflog, no note (`tests/documents.rs`:
      `a_vault_without_a_work_tree_writes_no_file_no_index_and_no_reflog`;
      `tests/mirror.rs`: `a_clone_with_no_work_tree_is_bare_holds_no_file_and_reads_the_mirrors_documents`,
      `a_pull_fast_forwards_a_vault_without_a_work_tree_and_touches_no_index_no_file_no_reflog`,
      `both_vaults_without_a_work_tree_writing_different_documents_end_equal_through_a_join`,
      `both_vaults_without_a_work_tree_writing_one_document_differently_conflict_and_change_nothing`;
      `commit::tests`: `a_write_cut_in_a_vault_without_a_work_tree_leaves_it_whole`).
- [x] `OpenOptions.initial_branch` and `--initial-branch`, every case of D14
      (`tests/mirror.rs`: `a_new_vault_starts_on_the_initial_branch_in_both_modes_and_unset_keeps_main`,
      `a_clone_of_an_empty_mirror_starts_on_the_initial_branch_and_its_sync_pushes_it`,
      `a_mirror_that_has_the_branch_is_cloned_on_it_whatever_its_head_names`,
      `a_mirror_with_commits_but_not_the_branch_is_invalid_argument_and_leaves_nothing`,
      `an_attach_does_not_check_the_initial_branch`).
- [x] The syncs of one write (6) and of a batch of 500 (6) counted by
      `strace`: a run on 22 October 2026; no test holds the count.
- [x] `0.25` opening such a Vault writes nothing: shown by a run of the
      released binary on 22 October 2026 (`INVALID_ARGUMENT`, and
      `NOT_A_VAULT` with `--mode open-existing`); no test in the tree.
- [x] 100 000 writes through a Vault without a work tree
      (`pack::tests::measure`, release, run once on 22 October 2026): a
      write 1.16 ms after 2 000 and 1.45 ms after 100 000, a read 34 then
      47 us, 10 then 20 packs; anonymous memory 13 then 136 MiB, a known
      issue.
- *Check:* the syncs counted by `strace`: one write 6, and a batch of
  500 Documents 6 too; `Pull` and `Sync` between two Vaults without a work
  tree, with a join and with a conflict: the join touches no index and no checkout;
  100 000 writes through such a Vault: the time of a write and of a read
  and the memory have not grown (a memory target, run alone); a
  process killed in the middle of a write: the Vault opens and `Check` is
  clean; the mode is in `Vault.Info`; `0.25` opening such a Vault writes
  nothing (a run of the released binary); every case of D14.

### G3. The layout

D1–D5; P4, P5, P6.

- [x] The rule of D2 with N = 64: the path of an id, the descent of a
      read, the in-order walk, the split and the merge of a write; standard
      collections, journal segments, isolated collections (`tests/layout.rs`:
      `the_same_100_ids_written_in_two_orders_in_two_vaults_give_equal_trees`,
      `a_folder_is_flat_at_64_documents_splits_at_65_and_is_flat_again_after_a_removal_with_the_files_where_the_tree_has_them`,
      `get_finds_every_document_of_a_collection_split_by_hand_and_a_missing_id_is_not_found`,
      `an_isolated_collection_of_65_documents_is_split_canonically_in_its_repository_and_a_removal_merges_it`;
      `tests/index.rs`: `a_journal_of_more_than_64_segments_is_laid_out_canonically_and_its_id_map_follows_the_moved_segments`).
- [x] `History` and the indexer by id; a move is no change (D4)
      (`tests/layout.rs`: `the_history_of_a_document_a_split_moved_has_no_new_version`;
      `tests/search.rs`: `a_move_by_the_layout_embeds_and_projects_nothing_again`).
- [x] The join of the mirror by id (P6); `Check` of the layout (P5)
      (`tests/mirror.rs`: `two_clones_that_wrote_different_documents_into_one_folder_join_into_the_canonical_tree`;
      `tests/layout.rs`: `check_reports_a_document_in_the_wrong_layout_folder_with_its_path`,
      `check_reports_a_flat_folder_of_70_documents_with_the_path_of_the_folder`).
- [x] The marker `version: 4`; a Vault of generation 3 is `GENERATION`
      (`tests/open.rs`: `a_new_vault_marker_says_version_4`,
      `a_vault_whose_marker_says_3_is_generation_in_every_open_mode_and_nothing_changes`).
- [ ] A listing from an id prefix that stops at the first id outside it
      reads only the trees on its way: not tested yet.
- [x] A write measured at 100 000 and at 1 000 000 Documents
      (`pack::tests::measure`, release, run once on 22 October 2026): 0.9 ms
      and 1.1 ms (max 8.1 and 10.1 ms), a get 25 and 46 us; before, with
      git's index, 123 ms and 1.34 s.
- *Check:* the same set of ids written in two orders gives the same tree;
  a folder at N + 1 splits and at N merges, and `History` of every moved
  Document is unchanged; a split re-embeds nothing (the embedder's calls
  counted); a listing from an id prefix that stops at the first id outside
  it reads only the trees on its way; two clones that wrote different
  Documents into one folder join into the canonical tree; a write measured
  at 100 000 and 1 000 000 Documents before and after; a Vault of
  generation 3 is `GENERATION` in every open mode and writes nothing.

### G4. The Index: names and syncs

D12, D13; P9.

- [x] The id maps and the Index's folders renamed (P9) (`tests/budget.rs`:
      `no_path_under_a_vault_without_a_work_tree_is_longer_than_120_characters`;
      `store::tests`: `the_paths_of_a_table_with_every_index_and_long_columns_stay_short`).
- [x] The round syncs the files of the new Lance versions, then the state
      (`tables::tests`: `a_round_flushes_the_new_files_of_lance_and_their_folders_before_its_commit`).
- [x] At open, versions newer than the state are left over from a round
      that did not finish and do not stop the next one (`tables::tests`:
      `a_round_stopped_after_lance_and_before_the_commit_is_done_again_and_nothing_else`).
- *Check:* the longest path a Vault without a work tree makes (git, the
  Index with its tables, the locks) is at most 120 characters with
  collection names of 255 bytes, columns of 255 characters with `fts`, a
  scalar and a vector index, a journal, an isolated collection, a mirror
  sync, and `initial_branch` `main`; a round stopped after Lance's commit
  and before the state redoes that round only (the embedder's calls
  counted).

### G5. Journals and trailers

D15; P10.

- [x] The default segment cap 128 KiB (`journal::tests`:
      `a_journal_declared_without_a_cap_rotates_its_open_segment_at_128_kib`;
      `idmap::tests`: `a_journal_with_no_cap_rotates_at_128_kib`).
- [x] `write` in each trailer (`document_calls::tests`:
      `a_trailer_carries_write_zero_and_one_without_write_parses`;
      `tests/documents.rs`: `the_trailers_of_a_batch_name_each_document_with_its_own_note`).
- *Check:* a journal without a cap rotates at 128 KiB; the trailers of a
  batch carry `write` 0 and `History` reads `write_id` as before.

### G6. The move of a Vault

D11. For other consumers; the anchor consumer starts with a new Vault and
does not rely on it.

- [ ] A fixture Vault of `0.25` with every kind of collection (standard,
      journal, the `singleton` `config`, isolated, encrypted) moved by the
      CLI: `get-many --include-raw` of `0.25`, then `batch-documents` of
      this version into a new Vault without a work tree.
- *Check:* every Document of the new Vault has the bytes and the id it had;
  `Check` is clean.
- Not started: deferred until after the release.

### G7. Docs

- [x] `03` (paths), `04` (STO-5, STO-6, GIT-1, GIT-2, GIT-3, GIT-9, GIT-14,
      GIT-15, GIT-16), `05` (the segment cap), `06` (the indexer by id, the
      Index's paths), `07` and `14` (the new options), `CHANGELOG.md`, the
      README: written on 22 October 2026, `bazel test //...` green.
- *Check:* `bazel test //...` green, markdownlint included.

---

## 5. What changes for consumers

Breaking, so the release is a minor (`0.26.0`):

- A Vault of generation 3 is `GENERATION`; a consumer moves it with a
  script (D11) or starts anew, as the anchor consumer does.
- Document files live in folders of the layout: a tool that reads a work
  tree by path must take the layout into account or read through Scheda.
- The default segment cap of a journal is 128 KiB.
- Scheda sets libgit2's object cache of the process to 16 MiB.

Added: `OpenOptions.work_tree`, `OpenOptions.initial_branch`,
`VaultInfo.work_tree`, the trailer's `write`.

The anchor consumer (a sample, not a proof for other consumers): its
product reads a Vault only through the API; the two places that look at
files on disk are tests (`rust/server/src/init.rs:295` in `mod tests`,
`rust/app/tests/common/events.rs:222`); it passes `work_tree` `NONE` and
`initial_branch` `main`.

---

## 6. Out of scope

- A write-ahead log (D10) and a group commit (later, no format change).
- A multi-pack index: if reads slow down at hundreds of packs; local, no
  format change.
- The change feed (S5); D4 binds it. It comes right after, as a minor
  release of its own with no format change: the anchor consumer's
  incremental reading of its choices depends on it.
- Lance's own files outside a round (compaction, index rebuilds): the same
  order applies when they come into a round.
