# Follow-ups found while building P8

Found on 19 October 2026 while the CLI and `tests/consumer.rs` were built
on the object API (`docs/20-proto-contract.md` P8). None blocks the merge;
each is small.

- **The vector-leg reason before a collection is in the Index.** A search
  on a Vault without an embedder, before the indexer has put the
  collection into the Index, reports `VECTOR_LEG_NO_VECTOR_COLLECTION`
  (or fails `INDEX_REBUILDING`), not `VECTOR_LEG_NO_EMBEDDER`:
  `vector_summary` in `rust/scheda/src/search.rs` reads the columns from
  the Index snapshot, which has none for that collection yet, though the
  Projector marks one `vector`. `tests/consumer.rs`
  (`a_vault_without_an_embedder_does_not_index_until_one_is_set`) accepts
  both answers. Fix: take the columns from the Projector when the
  snapshot has none for the collection.
- **`index watch` and a closed stdout.** Without `--until-settled`, the
  CLI notices that its reader has gone only when it writes the next
  state, which comes with the next indexer round. `index watch | head -1`
  can stay alive until the Vault changes.
- **A CLI query on a Vault no process indexes waits 60 s.** When no
  process with the ports has built the Index (no `version`), the settle
  wait never ends early; the query then fails `INDEX_REBUILDING`. A
  process without an embedder flag does not index a collection with a
  `vector` column (the CLI Projector marks `body` `vector`).
- **`ErrorReport.message` of a foreign fault.** `scheda::error_report`
  gives the message of a `scheda::Fault` or a `protocontract::Failure`;
  for a port fault of another type it falls back to the `Display` of the
  error, which also carries the stack and the code (P2 box still open).
- **Docs pass (§5) not done.** `docs/07`, `08`, `13`, `14` still describe
  the 0.21 API and CLI (`collection … get` printing raw bytes, `find`,
  `CODE_*` codes).

- **vllm 0.32.0 after its quarantine.** `//python:vuln` found
  PYSEC-2026-4319 (CVE-2026-105922, a denial of service in vllm's penalty
  handler) in vllm 0.31.0, the local sidecars' pin. The fix, vllm 0.32.0,
  was published 18 October 2026 10:03 UTC, inside the two-day quarantine,
  so `python/BUILD.bazel` ignores the advisory for now. From 20 October
  2026 10:03 UTC: bump `vllm` in `pyproject.toml`, relock `uv.lock`, drop
  the ignore.

**How to apply:** take these with the docs pass or right after the merge;
the first one changes library code and needs its own test; the vllm bump
as soon as its date has passed.
