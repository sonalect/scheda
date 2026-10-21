# Folders of bounded size for a collection

A storage format change asked by the owner on 21 October 2026 ("the
solution must work on millions of Documents"): a write in a collection
whose folder holds every Document diffs and rewrites a git tree the size
of the folder. To be designed in a fresh session at `xhigh`; nothing is
built yet.

## Measured (21 October 2026, after `0.25.0`)

One `Upsert` through Scheda's write path, thin build (no Index), the
collection made in one pack; the bounded layout emulated by ids with two
levels of 256 folders by a hash:

| Documents | Flat | Two levels of 256 |
| - | - | - |
| 100 000 | ~130 ms, process peak +0–50 MB | ~1.5–2 ms, +0–3 MB |
| 1 000 000 | ~1.55 s, +190–400 MB | ~2–3 ms, +2–14 MB |

Those runs left git's own index (`.git/index`) without the Documents. In
a real Vault it holds every path of the repository, and every write reads
it whole and writes it whole (`commit::settle`: `index.read(true)`, then
`index.write()`). libgit2 1.9.7 reads only the `TREE`, `REUC`, and `NAME`
extensions (`index.c`, around line 2710), so neither a split index nor a
sparse index is available. Measured again on 21 October 2026 with a throwaway
probe (removed): one `Upsert`, thin build, on tmpfs:

| Documents | Flat, index empty | Flat, index full | Two levels, index empty | Two levels, index full |
| - | - | - | - | - |
| 100 000 | ~123 ms | ~160 ms | ~1.2–1.7 ms | ~52–62 ms |
| 1 000 000 | ~1.34 s | ~1.75 s | ~2.3 ms | ~570–610 ms |

`git update-index` of the git command line, one entry in an index of
1 000 000: 0.24 s and 277 MB of peak memory. The same probe on the owner's
disk (not tmpfs) added a constant of about 190 ms to every write, at any
size: the cost of the disk, not of the layout.

The time is libgit2 parsing and rewriting the folder's tree. Fixed on the
way (no format change, not committed yet): the declarations loader listed
each collection's folder on every read of the declarations (60 MB of Rust
allocations per write at 1 000 000); it now looks the declaration names
up (`collection::tests::memory::reading_the_declarations_costs_a_large_collection_what_it_costs_a_small_one`).

## Decided

- **Scheme (owner and the anchor consumer, 21 October 2026):** an adaptive
  prefix tree over the bytes of the id. A folder holds at most N entries;
  past N it is split into subfolders by the next byte of the ids, only
  where ids are dense. Subfolder names cannot be ids (e.g. `_64`). The
  file name stays the id. A listing in id order is an in-order walk: lazy,
  memory in the depth. A write touches a few trees of at most N entries; a
  split rewrites one folder. Rejected: a hash of the id (loses the order
  of ids; a lazy listing would merge 65 536 folders) and a fixed prefix of
  the id (folders unbounded when ids share their start).
- **Split from the first byte where the names differ** (common-prefix
  compression, a radix tree): the consumer's leaf ids share a constant
  letter (`o…`, `l…`, `c…`) before hex, so a split by the first byte gives
  one subfolder (`_6f`) at the first level.
- **Hand edits:** the consumer makes none; a file in the wrong folder
  reported by `Check` is enough.
- **Decision 1 (owner, 21 October 2026): a Vault without a work tree**,
  chosen when it is created: no files on disk and no git index, so a write
  does not pay for every path of the repository. The Vault with a work tree
  stays the default for other consumers. The anchor consumer creates every
  Vault of its own this way; its two places that look at files on disk
  are tests (`rust/server/src/init.rs:295` in `mod tests`,
  `rust/app/tests/common/events.rs`), its product reads a Vault only
  through the API, never calls `Check`, and does not use `History`, so the
  loss of `History` and `write_id` in the move is accepted.
- **Proof of decision 1, by runs (owner):** (1) `Pull` / `Sync` with a
  join and with a conflict between two Vaults without a work tree: the
  join touches no index and no checkout; (2) a process killed in the
  middle of a write: the Vault opens and `Check` is clean; (3) a Vault of
  the older generation is not opened by the new version but refused with
  the code that names the generation; (4) the mode is visible through the
  API (`Info`).
- **Next, asked by the owner:** a write on disk costs about 190 ms at any
  size. Count the syncs to disk of one write and where they are; the
  target is one or two a write, independent of the number of files,
  with no loss of durability. The anchor consumer measured its `learn`
  test binary (158 tests): 15 s with temporary files on tmpfs, 499 s on
  NVMe ext4, the slowest tests making hundreds of writes in a row; the
  cost of a write on disk is the main cost of `learn` for its user.
- **Counted (21 October 2026, strace, throwaway probe removed):** one
  `Upsert` into a Vault with a work tree, thin build, NVMe ext4: 18
  `fsync` calls for the id `d3`, 24 for `a/b/c/d3`; about 5.5 ms each
  (90–110 ms and 120–155 ms a write). Where: every git object, because
  Scheda turns on `GIT_OPT_ENABLE_FSYNC_GITDIR` (`repo::init_library`):
  the blob, one tree for each folder on the path, the root tree, and the
  commit, each its temporary file and its `objects/XX` folder, so
  2 × (3 + folders on the path, the collection's own included); the
  branch, 2 (`refs/heads/main.lock`,
  `refs/heads`); the reflog, 4 (`logs/HEAD`, `logs/refs/heads/main`,
  `logs/refs/heads`, `logs`); the note in `commit.lock`, 2; the file in
  the work tree, 1. git's index: none. After the write, the indexer round
  adds 1 of its own (`.scheda/index/commit`); Lance's in a build with the
  Index were not counted. Each level of the bounded layout adds 2.
- **Decision 2 (owner, 21 October 2026): the syncs first, the layout
  after.** Conditions: (1) a pack per write ships as a release of its
  own with no new generation: a 0.25 Vault opens as it is; dropping the
  reflog and the `commit.lock` note goes with the mode without a work
  tree, in the layout round; (2) one pack per commit, not per Document: a
  batch of 500 Documents costs the same syncs; (3) the number of packs is
  bounded, by a merge whose memory and time grow with what it merges, not
  with the whole repository; proof: after 100 000 writes the time to read
  an object and the memory have not grown; (4) count Lance's syncs in a
  build with the Index, since the consumer pays for a write with its
  indexer round; (5) no write-ahead log now: decided after the version
  with few syncs is measured. The consumer reruns its `pipeline_test` on
  NVMe after the release (499 s now, 15 s on tmpfs).
- **libgit2 writes a pack with 4 syncs**, not 3: the `.idx` and its
  folder (`filebuf.c`, around line 435), then the `.pack` and its folder
  (`indexer.c`, around line 1411). A write in this release: the pack 4,
  the branch 2, the reflog 4, the note 2, and 1 per work-tree file: 13 for
  one Document, at any depth; in a Vault without a work tree, 6.
- **The merge rule must change.** Merging runs only when loose objects
  pile up (`pack::due`); with a pack per write there are none, so the packs
  would never merge. The present rule (16 packs: merge the 4 smallest) with
  a pack per write converges to packs of equal size, about the history over
  13, and the merge work per write grows with the history: 13, 129, 1283
  objects a write after 1 000, 10 000, 100 000 writes (model). Every rule
  that keeps the packs to the log of the history (geometric, tiered) now
  and then merges 30–90 % of everything written; only a cap on what one
  merge takes bounds it. Model of geometric merging (factor 2, run at 16
  packs) below a cap of 64 MiB, a write's pack taken as 2 KB: 7.3 objects
  of merge work a write, a merge every tenth write, the largest merge
  80 MiB, packs 17 / 39 / 88 at most after 100 000 / 1 000 000 /
  3 000 000 writes (one more for each 64 MiB of history).

- **Decision 3 (owner, 21 October 2026): geometric merging (factor 2, at
  16 packs) among packs below 64 MiB** (not 256: the consumer's peak memory
  matters more than the number of packs); a pack that reaches the cap is
  not merged again; merges run in the background of the Vault, off the
  write lock. Proof by runs: the peak memory of the worst merge (about
  80 MiB of packs); the time to read a Document with 100 and with 500
  packs (synthetic is fine; if it degrades at hundreds, a multi-pack index
  comes back); a process killed in the middle of a merge leaves the Vault
  whole and `Check` clean. A person's `git gc` is not offered: nobody
  touches a Vault by hand; a full repack, if ever needed, is an operation
  of Scheda with bounded memory.
- **The format is closed now (owner, 21 October 2026): one generation, one
  migration.** It takes everything that changes the bytes on disk: the
  Vault without a work tree, the bounded layout, a pack per commit with the
  geometric merge below 64 MiB, no reflog, no `commit.lock` note. Releasing
  the mode without a work tree first is withdrawn (two conversions). The
  write-ahead log is decided in this round; the anchor consumer advises
  against it (6 syncs, about 33 ms, are enough; throughput under load from a
  group commit, which leaves the format alone).
- **Lance's syncs, counted (21 October 2026, strace, build with the
  Index, probe removed):** the indexer round after a write syncs 1 file,
  Scheda's own state file in `.scheda/index/g-…/`; Lance writes its data
  file, transaction, and manifest (`_versions/N.manifest`) and syncs none.
  Not checked, a risk for later (the Index is not the Vault format): after
  a power loss Scheda's synced state may outlive Lance's unsynced files.
- **Group commit needs no format change:** `write_id` is the id of the
  commit and no two writes share one (`04` GIT-3); a commit of several
  writes gives each the commit and its number in the commit, read from its
  trailers. The number goes into each `Scheda-Note:` trailer from the
  start (always 0 until then).

- **Path budget (the anchor consumer's ask, 21 October 2026):** a test
  that the longest path Scheda makes under the root of a Vault without a
  work tree (git, the Index with Lance's tables, the locks) is at most
  120 characters with the longest names the rules allow (Windows: 260 in
  all, the Vault itself 55–70 deep). Measured by a throwaway probe (strace
  of every path made; five collections of every kind with 255-byte names,
  two columns of 255 characters with fts, a scalar and a vector index over
  10 050 Documents, a mirror sync): the journal id maps are named by the
  collection, `.scheda/index/journal/{name}/ids` and their temporary file,
  up to 303 characters; Lance up to 115
  (`.scheda/index/g-1a15f6d962f-1076d0-0/t4.files.lance/_indices/{uuid}/page_lookup.lance`),
  and its names grow (`part_{n}_invert.lance`, `_transactions/{version}-{uuid}.txn`
  with a 20-digit version would reach about 127); git 68 (packs; the
  nested repository 78); `.scheda/` locks under 30. Column names reach no
  path. The fix, local to the Index and rebuilt, so no migration: the id
  maps named without the collection's name, and shorter generation and
  table folders (`g-…`, `t{n}.files.lance`) to leave Lance room. A branch
  name comes from `init.defaultBranch` or the mirror, not from Scheda; the
  test fixes it. The consumer agreed to both fixes and the test (no
  decision needed).
- **`OpenOptions.initial_branch` (the anchor consumer's ask, accepted
  21 October 2026):** the branch a Vault that `Open` makes starts on;
  unset keeps today's `init.defaultBranch`, or `main`. A new Vault starts
  on it. A clone of an empty mirror starts on it (`git clone` gets it as
  `init.defaultBranch`); a mirror that has the branch is checked out on it
  whatever its `HEAD` names; a mirror that holds commits but not the branch
  is `INVALID_ARGUMENT`, told from the clone's own references, with nothing
  left. An attach does not check it. A name libgit2 does not take as a
  branch is `INVALID_ARGUMENT` at `Open`, before anything is written. The
  consumer always passes `main`; the path-budget test does too.

- **Decision 4 (owner, 21 October 2026): no write-ahead log.** The
  write's number goes into its trailer from the start; to the consumer
  `write_id` is just a string.
- **Lance's files synced before the state (the anchor consumer's ask,
  accepted):** not the Vault format, but at millions of Documents a rebuild
  of the Index embeds every text again (hours, and money with a paid
  embedder). Order in the round (background, a write does not wait): sync
  the files the new state names (data, transaction, manifest, index files,
  and their folders), then write and sync the state file
  (`generation.rs`: `commit`, which names the Lance version of every table
  a reader opens). At open, after a crash, at worst the last round is done
  again, never the whole Index; Lance versions newer than the state are
  left over from that round and must not stop the next one.
- **The anchor consumer's real ids (21 October 2026)**, a constant letter
  before 32 hex digits (128-bit ids): `observations/{container of several
  parts, e.g. github.com/org/repo}/{kind}/o<hex>.md` (tens of thousands and
  more in a section folder of a monorepo); `candidates/{container}/k<hex>.yaml`;
  `clusters/{family}/c<hex>.yaml` (up to millions in a family);
  `lessons/l<hex>.yaml` (millions in one folder; was `lessons/shared/…`);
  `answers/{step}/<hex>/<hex>-<hex>` (millions of subfolders in the folder
  of a step, so N counts every entry of a folder, subfolders included).

- **Decision F1 (owner, 21 October 2026): the layout of a folder follows
  from the set of names in it alone**, no margin between split and merge:
  the same Documents give the same tree on every clone and in a join with
  the mirror, and `Check` knows the one place of each file. Fixed with it:
  a folder of more than N entries is split, every entry counted (files and
  id subfolders; dotfiles neither count nor move); the split is at the
  first byte where the names differ; a subfolder is named by the bytes it
  adds to its parent's prefix, in lowercase hex after `_`. Flapping at the
  threshold is cheap and rare for the consumer (its collections grow; only
  the reset of a rebuild deletes in bulk).
- **Condition for S5, the change feed (owner, 21 October 2026):** a move
  of a file by the layout (the same id, the same blob) is no change of the
  Document, as for `History` and the Index; a split folder yields no
  hundreds of false changes.

- **F2, the consumer's argument (21 October 2026):** N sets the bytes of
  every write, not its time: the leaf tree a write rewrites dominates its
  pack (the consumer's Document is about 2 KB). It proposed 256 over 1000
  and asked for the merge model again with the real pack of a write.
- **Measured (21 October 2026, throwaway probe on git2, removed):** ids
  `l<hex32>.yaml` in one folder, each its own blob, 2 KB text Documents
  (0.67 KB packed), the canonical layout. A write's pack (fresh) and its
  share of a merged pack (with path names given to the packer; without
  them about 0.35 KB more):

  | N | Documents | Leaf | Fresh pack | Merged |
  | - | - | - | - | - |
  | 256 | 100 000 | ~29 | 3.2 KB | 2.4 KB |
  | 256 | 250 000 | ~48 | 4.8 KB | 3.9 KB |
  | 256 | 1 000 000 | ~232 | 10.1 KB | 9.0 KB |
  | 64 | 250 000 | ~48 | 4.0 KB | 3.1 KB |
  | 64 | 1 000 000 | ~18 | 3.3 KB | 2.5 KB |

  Leaf trees barely become deltas in a merge: a version can only be a
  delta of another version of the same leaf in the same pack, and random
  ids seldom write one leaf twice. The worst merge, about 82–87 MiB of
  packs, took 13–17 s and wrote as much as it read. Its anonymous memory:
  +256 MB at 238 000 objects with libgit2's defaults, of which the
  library's object cache (process-wide, up to 256 MiB, every tree and
  commit up to 4 KB, `cache.c`) took the most; with that cache at 16 MiB,
  +114–120 MB, about 500 bytes an object of the merge, the packer's own
  (`pack.deltaCacheSize` changes nothing; and libgit2 reads that same key
  as its `big_file_threshold`, `pack-objects.c` line 118). Input packs are
  mapped files on top (about 80 MB, reclaimable). Note for the write path:
  `Mempack::dump` packs every commit of the mempack with its whole tree
  (`git_packbuilder_insert_commit`), and the thin-pack call is not bound in
  `libgit2-sys` 0.18.8; a write packs exactly its own objects.

- **Decision F2 (owner, 21 October 2026): N = 64.** A leaf holds at most
  64 entries; a split folder at most 257 (one per next byte, and one name
  equal to its prefix), 16 for hex ids. History about 3.5 KB a write at
  worst (9 KB with 256); after 1 000 000 writes about 2.7 GiB and at most
  about 58 packs (estimates from the measured points).
- **Decision F3 (owner, 21 October 2026): the segment folders of journals
  take the same layout**: one rule for every folder Scheda puts Document
  files in; a journal only grows, and a flat folder would be a later
  generation. The anchor consumer has no journal (seven standard
  collections and the `singleton` `config`). Segments are named
  `{seq}-{writer}.jsonl` or `{date}-{writer}.jsonl`; their byte order is
  the journal's order but for a day's overflow segments, which keep their
  own rule.
- **The default segment cap becomes 128 KiB (mine, on the consumer's
  hint, 21 October 2026)**, was 1 MiB: an append rewrites the open
  segment, about half the cap on average, and with the layout more
  segments cost nothing. A line longer than the cap still gets a segment of
  its own; a cap set in the declaration (1 byte to 32 MiB) is unchanged. A
  changed default: `CHANGELOG.md` under `Changed`.
- **Next, in this order (owner):** F3 (done); then the libgit2 object cache
  (process-wide, up to 256 MiB): the anchor consumer's server is a process
  that reads a lot and shows 0.53–0.65 GB of anonymous memory with a live
  heap of about 60 MB; its position, a budget of tens of MiB, chosen by a
  measurement of read speed; decided together with a cap on the objects of
  one merge (the consumer prefers peaks of about 50 MB at the price of more
  packs), on a measurement of reads with 100 and 500 packs.

- **Reads with many packs, measured (21 October 2026, throwaway probe on
  git2, removed):** 1 000 000 Documents `l<hex32>.yaml` (600-byte blobs) in
  the layout with N = 64 (69 906 trees, 426 MB), the objects spread at
  random over 1, 100, or 500 packs; a read is the descent by path from
  `HEAD` and the blob; each run its own process; 20 000 random reads, then
  a hot set of 2 000 Documents read ten times. Time of one read, random /
  hot set, in µs:

  | Packs | No cache | 8 MiB | 16 MiB | 32 MiB | 64 / 256 MiB |
  | - | - | - | - | - | - |
  | 1 | 32 / 31 | 23 / 25 | 19 / 14 | 19 / 9.5 | 19 / 9.4 |
  | 100 | 74 / 73 | 43 / 49 | 34 / 28 | 34 / 17 | 34 / 17 |
  | 500 | 218 / 216 | 108 / 131 | 91 / 73 | 82 / 41 | 80 / 40 |

  Anonymous memory is about twice what the cache holds (+16 MB at 8 MiB,
  +30 MB at 16 MiB, +36–38 MB when 20 MB were cached: the whole working set
  of this run, so 32 MiB and more held the same). With N = 64 every tree is
  under the 4 KB the cache takes, so a reading process fills whatever budget
  it is given; `History` caches the trees of old commits too.

- **Decision 5 (owner, 21 October 2026): libgit2's object cache at
  16 MiB, and a merge takes at most 100 000 objects as well as 64 MiB**
  (whichever comes first; a peak of about 50 MB). Scheda sets the budget
  for the process when it first opens a Vault, whoever initialized libgit2
  before (the anchor consumer reads its source mirrors through `git2` in
  the same process, maybe first); one budget for the Vault and those
  mirrors, and the consumer does not override it. A test fills the cache
  through `git2` before the first `Open` and finds the budget after it.

## The consumer's facts that bind the design

- Its ids are already hierarchical (container and kind as folders by
  `/`); the folders that grow are leaves: `observations/<container>/<kind>/`
  (hundreds of thousands in a monorepo), `lessons/shared/` (every shared
  Lesson of an organization: millions), candidates and clusters per
  container. The bounded layout applies inside each such folder.
- It reads the observations of a container by a lazy listing by id
  prefix and stops at the first id outside it: the order by id is
  required, and so is a listing that costs only what it reads.
- **Migration (owner, 21 October 2026, replaces the earlier ask to move
  `answers` in place):** none in the library. The consumer has one Vault
  and moves it with a one-off script of its own: read every Document of
  the old Vault through 0.25 (raw bytes, the id as it is) and write it into
  a new Vault through the new version, which picks the folder. The library
  only has to keep reading and writing a Document as raw bytes under the
  same id through the API or the CLI, in every collection, the `singleton`
  `config` included. 0.25 has it already: `collection NAME get-many
  --include-raw` (JSON Lines, `raw` in Base64) and `batch-documents` /
  `collection NAME batch` (`raw` in Base64). `History` and `write_id` of the old
  Vault do not carry over. Not yet proven by a run: the proof stage moves
  a fixture Vault of every kind this way (`docs/24` G6). Later the owner
  decided the anchor consumer does not move its Vault at all: it starts
  with a new Vault and a full learn; G6 stays for other consumers.

## Open (for the fresh session)

- First, put to the owner on 21 October 2026: git's index makes a write
  O(every path of the repository) whatever the folders (table above).
  Proposed: a Vault without a work tree, chosen when it is created.
- N, the split and merge rules, how a path is found (descent), what
  `History` does across the move of a file into a subfolder, the marker
  of a split folder. The format version is the next generation of the
  marker (no migration step, per the owner's decision above).
- Proof: the write measured at 100 000 and 1 000 000 Documents before and
  after, with the real layout; a listing by prefix that stops early reads
  only what it gives.

**How to apply:** start from this note and the measurements above; put
each design decision to the owner one at a time (`owner-review.md`).
