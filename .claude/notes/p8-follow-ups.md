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
- **vllm 0.32.0 and huggingface-hub 2.2.0: done 19 October 2026.** vllm
  0.32.0 was taken a day inside its quarantine on the owner's word, after
  a check: its PyPI files match the GitHub release `v0.32.0` byte for
  byte (sha256), released by the same release manager as 0.30.0 and 0.31.0
  from `releases/v0.32.0`, whose commits are cherry-picks of merged PRs;
  vLLM publishes no PEP 740 attestations for any release. Every other
  package that moved is past the quarantine (`uv lock --exclude-newer` with
  a per-package exception for vllm, then `uv lock` again so the lock keeps
  no `[options]`). huggingface-hub 2.x came with it (transformers 5.19.0
  allows `<3.0`). The PYSEC-2026-4319 ignore is gone; PYSEC-2026-3447
  (setuptools) stays, since vllm 0.32.0 still caps `setuptools<81`.
  Checked: the driver end to end on the local Qwen3-4B sidecars, and the
  Model2Vec download through `hf_hub_download` of 2.2.0. Not run: a vllm
  0.32.0 sidecar itself (its wheels and torch are not in the Bazel cache,
  and the GPU was busy); run `bazel run //python/vllm:embedder` once the
  GPU is free. `~/.cache/huggingface/xet` is owned by root on this
  machine, so hf-xet logs to the console instead of its log file.

- **Broken doc links in the generated reference modules: done 19 October
  2026.** Fixed in protoc-gen-protocontract-buffa v0.4.1 (the module doc
  links `collection_ref::CollectionRef`), catalogued in `bazel_utils`
  0.4.2; Scheda pins both and `protocontract` 0.4.1, and `cargo doc -p
  scheda-api` passes.

**How to apply:** take these with the docs pass or right after the merge;
the first one changes library code and needs its own test.
