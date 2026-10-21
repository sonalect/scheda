# Generation 4: known issues and deferred work

The owner's rule for the push to the demo (21 October 2026): functionality
of G1–G7 first. A race that shows only under load, or anything else the
demo will not exercise, is written here and deferred; the work goes on.
Slow tests run only after every G stage is done, and only with the owner's
explicit permission (`.claude/rules/measure-once.md`).

## Release

When G1–G7 work and the result holds up, publish the release (0.26.0,
breaking: `.claude/skills/versioning/SKILL.md`) without waiting for the
owner, as he asked (22 October 2026). The anchor consumer's agent then
adopts it: the release notes say what a consumer changes (docs/24 §5):
`OpenOptions.work_tree = WORK_TREE_NONE`, `initial_branch = "main"`, a new
Vault and a full learn instead of a migration.

## Deferred, to do after G7

1. **Windows: a pack that cannot be removed** (owner's condition 2 on
   decision 3). A file another process (or this one) has open or mapped
   is not removed on Windows. The merge must not fail on that refusal:
   the old pack stays, readers go on, and the next merge removes it
   (a pack whose objects are all in another one is dropped). Needs a
   test with a hook that refuses the removal.
2. **The code of a missing object.** `lib_fault` in `error.rs` maps every
   libgit2 error that is not OS, filesystem, memory, or lock to
   `REPLICATION`, a missing local object included. A catch-all, against
   the error rules. A small task of its own; a proposal goes to the owner
   first.
3. **The gate's `integration_test` takes about 90 s** (about 30 s before
   G1): find the slow G1/G2 tests and shorten them.
4. **Slow measurements.** The owner allowed them (21 October 2026) for the
   night, once every G stage is done, and not before: 100 000 writes at the pack
   level and through a Vault without a work tree (G2), the worst merge at
   the cap, reads with 1, 100, and 500 packs.

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

## Known issues (races under load)

1. **A round of the indexer cut by `Close` or a kill leaves its Lance
   files with the umask's modes** (0644 / 0755 instead of 0600 / 0700),
   and `Check` reports them as `FAILED_PRECONDITION` until something
   narrows them. A round narrows the modes of what it wrote only when it
   ends. Older than generation 4; found by the G2 cut test, which now lets
   the round end before it closes. Fix later: narrow the generation's
   modes at open, or when the next round starts.
