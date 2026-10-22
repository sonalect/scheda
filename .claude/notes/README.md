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
| [index-kinds.md](index-kinds.md) | The Lance index kinds Scheda builds; the gap: `scalar_index` on a `text list` builds no index (fix with LabelList proposed, owner's decision open); the IVF_PQ against IVF_RQ measurement on real embeddings, deferred to after 0.26, 21 October 2026 |

## Pointers

- Generation 4 (`docs/24-generation-4.md`) is released as 0.26.0 (22 October
  2026); what is deferred, the known issues, and the measurements are in
  `generation-4-known-issues.md`. Next: the deferred items there, then the
  change feed (S5) as a minor release of its own with no format change.
- Before the release: `docs/23-full-review.md` — the full review of
  20 October 2026 on `main` at `34e61d1` (R1–R22, decisions D1–D9,
  stages S1–S5), approved by the owner the same day: every finding is
  fixed before the release `0.22.0`, tokio 1.54.0 taken inside its
  quarantine on his word. R1–R3 and R5 were reproduced by throwaway
  probes (removed); the gate was green. Done on the branch
  `full-review-fixes` (20 October 2026): S1–S4 and S5's documents, a
  commit per stage, plus R23 found on the way; merged into `main` by
  fast-forward and released as `0.22.0` on the owner's word the same day
  (with `20`, `21`, and `22`).
- Current work order: `docs/21-lance-index.md` — the Index on Lance in
  place of Tantivy and USearch, queries by a typed filter, a state
  token for freshness (approved by the owner 19 October 2026, D1–D18).
  Work goes on the branch `lance-index`, stage by stage in the order of
  §3, a commit after each stage under the usual gates. L0: the contract
  in §8 was reviewed with the owner item by item (C1–C35, 19 October
  2026), and he looked at the proto files; they went in with 22 A1
  (the patch is in git history, commit of A1). L1 (Lance in the build) is done. Then the
  owner made Scheda async on tokio: `docs/22-scheda-on-tokio.md`
  (approved 19 October 2026, D1–D12). Order of the branch: 21 L0, L1,
  L8 (done: the old Index removed; `Vault.Index` and every port are
  `UNIMPLEMENTED` until L3, the Rust test suites not run). Then, set by
  the owner after L8: a run of every test (done, green), 22 A1 with the
  L0 patch (done; `IndexState.head` became `stored_version`), the probe of
  the debug loop (done, numbers in 21 L1), 22 A2 + A3 + A4 in one commit
  (done 20 October 2026), then 21 L2–L7, L9,
  L10 (with A5), all done 20 October 2026 (`1fdd6d8`, `0a7e3f9`,
  `f51165e`, `a1aa7a6`), then index risks 1–4 (`index-risks.md`).
  Merged into `main` by fast-forward on 20 October 2026 at the owner's
  word; not pushed. What remains is the release, L10's
  second box, for the owner's word: `0.22.0` was never released from
  `main`, and `21` names `0.23.0`. The spike stays on the branch
  `lance-spike` (worktree `../scheda-lance`, `spikes/lance/`) and never
  reaches `lance-index` or `main`; its `kill` and `verify` commands are
  the models for L2's checks.
- Previous: `docs/20-proto-contract.md` (the 0.22 contract: P1–P8 and
  R1–R5 done, merged into `main` 19 October 2026; the release `0.22.0`
  not yet made; `p8-follow-ups.md`), `docs/19-follow-ups.md` (F1–F5),
  `docs/18-review-fixes.md` (S1–S8), and
  `docs/17-background-index-and-tokio.md`, done.
