# Generation 4: known issues and deferred work

The owner's rule for the push to the demo (21 October 2026): functionality
of G1–G7 first. A race that shows only under load, or anything else the
demo will not exercise, is written here and deferred; the work goes on.
Slow tests run only after every G stage is done, and only with the owner's
explicit permission (`.claude/rules/measure-once.md`).

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
3. **Slow measurements.** The owner allowed them (21 October 2026) for the
   night, once every G stage is done, and not before: 100 000 writes at the pack
   level and through a Vault without a work tree (G2), the worst merge at
   the cap, reads with 1, 100, and 500 packs.

## Known issues (races under load)

None recorded yet.
