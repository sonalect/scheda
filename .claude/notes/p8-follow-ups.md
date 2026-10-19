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
- **A CLI query on a Vault no process indexes.** A process without an
  embedder flag does not index a collection with a `vector` column (the
  CLI Projector marks `body` `vector`). On such a Vault a query waits for
  the Index to settle and then fails `INDEX_REBUILDING`, or, seen in the
  docs pass, fails at once with `INVALID_ARGUMENT` ("not a find column")
  when the Index exists but holds no columns for the collection: a filter
  is checked against the columns of the Index, as the vector-leg reason
  above. `14` lists both.
- **`index status` in a new process is empty.** A CLI process opens the
  Vault and asks at once, before its indexer has looked: the state is
  `{}`. Use `index watch --until-settled` (the Python driver does after
  `reindex`); the docs pass says so in `14`.
- **`ErrorReport.message` of a foreign fault: done 19 October 2026.** It
  is the fault's own `Display`, without the frames and the code
  protocontract writes before it; P2 is closed.
- **Docs pass (§5): done 19 October 2026.** The four sentences of the
  owner's Index paragraphs (`107ae60`) that disagreed with the code were
  fixed on the owner's word ("everything needed for the merge"): `13` §1
  (an uncommitted file is seen by no call, `Get` included; no query sees
  a write before its call returns), `07` LIB-28 (after `Vault.Close`
  every object is `FAILED_PRECONDITION`), `02` Index (only a Vault with
  every port indexes; `Get` and `History` read `HEAD`).
- **vllm 0.32.0 after its quarantine.** `//python:vuln` found
  PYSEC-2026-4319 (CVE-2026-105922, a denial of service in vllm's penalty
  handler) in vllm 0.31.0, the local sidecars' pin. The fix, vllm 0.32.0,
  was published 18 October 2026 10:03 UTC, inside the two-day quarantine,
  so `python/BUILD.bazel` ignores the advisory for now. From 20 October
  2026 10:03 UTC: bump `vllm` in `pyproject.toml`, relock `uv.lock`, drop
  the ignore.
- **huggingface-hub 2.x comes with that vllm bump.** With vllm 0.31.0 it
  cannot: vllm 0.31.0 caps `transformers<5.18`, and transformers 5.17.0
  requires `huggingface-hub<2.0`. vllm 0.32.0 allows `transformers<5.20`;
  transformers 5.18.0 and later, and tokenizers 0.23.3, allow
  `huggingface-hub<3.0`; vllm itself asks only `>=1.31.0`. Our calls
  (`hf_hub_download` with `repo_id`, `filename`, `revision`, `local_dir`,
  `token`, `user_agent`; `snapshot_download` with `repo_id`, `token`;
  `huggingface_hub.errors.HfHubHTTPError`) are all in 2.2.0 (checked on
  its wheel, 19 October 2026). huggingface-hub 2.2.0 (8 October) and
  transformers 5.19.0 (6 October) are past the quarantine. Do it in one
  step: `vllm==0.32.0`, `huggingface-hub==2.2.0` in `pyproject.toml`,
  relock, run the driver (`import`, `search`) and a sidecar once.

**How to apply:** take these with the docs pass or right after the merge;
the first one changes library code and needs its own test; the vllm bump
as soon as its date has passed.
