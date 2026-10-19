# Notes

Project state, deferred work, and measurements that outlive one session
(`.claude/rules/project-notes.md`). One file per topic; update in place.

| Note | Topic |
|---|---|
| [flaky-tests.md](flaky-tests.md) | Races the tmpfs gate exposed, their fixes, how to reproduce |
| [api-objects.md](api-objects.md) | The approved object API (Scheda, Vault, Collection, Mirror, Index; service references): thirteen decisions, what is open |
| [p8-follow-ups.md](p8-follow-ups.md) | Small follow-ups found while building the CLI and the consumer test (P8): a vector-leg reason, CLI waits, the docs pass |
| [index-risks.md](index-risks.md) | Audit of the indexer (19 October 2026): thirteen risks with probes and recommendations, to discuss; probes in `index-probes.patch` |
| [l0-protos.patch](l0-protos.patch) | The approved contract of work order 21 (`docs/21` §8): the protos of `api/scheda/v1` after the move to Lance, applied whole in L5 with the generate |

## Pointers

- Current work order: `docs/21-lance-index.md` — the Index on Lance in
  place of Tantivy and USearch, queries as an SQL predicate, a state
  token for freshness (approved by the owner 19 October 2026, D1–D18).
  Work goes on the branch `lance-index`, stage by stage in the order of
  §3, a commit after each stage under the usual gates. L0: the contract
  in §8 was reviewed with the owner item by item (C1–C35, 19 October
  2026), and he looked at the proto files; they wait in
  `l0-protos.patch` for L5. L1 (Lance in the build) is done. Then the
  owner made Scheda async on tokio: `docs/22-scheda-on-tokio.md`
  (approved 19 October 2026, D1–D12). Order of the branch: 21 L0, L1
  (done), L8 (the old Index removed: next), 22 A1–A5, then 21 L2–L7, L9,
  L10. The spike stays on the branch
  `lance-spike` (worktree `../scheda-lance`, `spikes/lance/`) and never
  reaches `lance-index` or `main`; its `kill` and `verify` commands are
  the models for L2's checks.
- Previous: `docs/20-proto-contract.md` (the 0.22 contract: P1–P8 and
  R1–R5 done, merged into `main` 19 October 2026; the release `0.22.0`
  not yet made; `p8-follow-ups.md`), `docs/19-follow-ups.md` (F1–F5),
  `docs/18-review-fixes.md` (S1–S8), and
  `docs/17-background-index-and-tokio.md`, done.
