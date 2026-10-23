# Notes

Project state, deferred work, and measurements that outlive one session
(`.claude/rules/project-notes.md`). One file per topic; update in place.

| Note | Topic |
|---|---|
| [flaky-tests.md](flaky-tests.md) | Races the tmpfs gate exposed, their fixes, how to reproduce |
| [api-objects.md](api-objects.md) | The approved object API (Scheda, Vault, Collection, Mirror, Index; service references): thirteen decisions, what is open |
| [p8-follow-ups.md](p8-follow-ups.md) | Small follow-ups found while building the CLI and the consumer test (P8): a vector-leg reason, CLI waits, the docs pass |
| [index-risks.md](index-risks.md) | Audit of the indexer (19 October 2026): the six risks that the move to Lance left, the owner's decision on each (1–4 done, 5 and `like` deferred, 6 accepted), and what was found while building it |
| [ci-platforms.md](ci-platforms.md) | CI on six platforms (branch `ci-six-platforms`): what runs where and why, what was fixed, timings with and without optimized deps |
| [scheda-0.25.md](scheda-0.25.md) | The anchor consumer's asks S1–S4 for 0.25 (stored vectors, vector subject, exact search, bounded rounds) and the owner's decision on pruning old versions |
| [generation-4-known-issues.md](generation-4-known-issues.md) | The owner's rule for the push to the demo (functionality first; load-only races and what the demo does not exercise are deferred), the deferred work and the known issues of generation 4, 21 October 2026 |
| [folder-layout.md](folder-layout.md) | The Vault of generation 4 (`docs/24`): every measurement behind it (writes, git's index, syncs, packs, merges, reads, paths) and the owner's decisions one by one, 21 October 2026 |
| [lance-binary-size.md](lance-binary-size.md) | Why a consumer's binary is 121 MB (about 80% Lance, DataFusion, sqlparser, Arrow), the measured fork prototype (−21%), the proposed `sonalect/lance` fork (owner's decision open), why `lance-index` alone is no way, 21 October 2026 |
| [index-kinds.md](index-kinds.md) | The Lance index kinds Scheda builds; the `text list` gap, fixed with LabelList (23 October 2026); the IVF_PQ against IVF_RQ measurement on real embeddings, deferred to after 0.26, 21 October 2026 |

## Pointers

- Current state: generation 4 (`docs/24-generation-4.md`) is released as
  0.26.0 (22 October 2026). What is left of it, the known issues, and the
  measurements are in `generation-4-known-issues.md`. Next: those deferred
  items, then the change feed (S5) as a minor release of its own with no
  format change; S5 has no work-order document yet.
- Open owner decisions: the Lance fork (`lance-binary-size.md`).
- In progress elsewhere: a blocking port called after `Close`, fixed in
  protocontract; Scheda bumps the lock when 0.4.2 is out
  (`ci-platforms.md`).
- Done work orders, for history: `docs/17`–`24` (the full review of
  20 October 2026 is `23`; the Index on Lance is `21`; Scheda on tokio is
  `22`; the 0.22 contract is `20`). The Lance spike stays on the branch
  `lance-spike` and never reaches `main`.
