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
- **`ErrorReport.message` of a foreign fault.** `scheda::error_report`
  gives the message of a `scheda::Fault` or a `protocontract::Failure`;
  for a port fault of another type it falls back to the `Display` of the
  error, which also carries the stack and the code (P2 box still open).
- **Docs pass (§5): done 19 October 2026.** Left for the owner: the
  docs pass found these sentences of the owner's Index paragraphs (commit
  `107ae60`) at odds with the code, and renamed them only:
  - `13` §1: "Orphan or uncommitted file: `Get` works" — `Collection.Get`
    reads `HEAD` (D22), so an uncommitted file is seen by no `Get`
    (`tests/documents.rs`:
    `a_persons_uncommitted_edit_is_seen_by_no_get_document_and_a_commit_by_hand_is`).
  - `13` §1: "Inside a transaction, queries do not see its uncommitted
    writes" — there is no transaction (D6); renamed to "Inside a batch",
    which says little.
  - `07` LIB-28: "Then it drops the handle, returning an error where
    `Drop` cannot" and "`Collection` objects … keep reading and writing,
    but nothing indexes their commits" — after `Vault.Close` every call
    of every object is `FAILED_PRECONDITION` (D24 item 9, `vault.proto`,
    `tests/objects.rs`:
    `after_close_every_object_is_failed_precondition_and_a_second_close_is_nothing`).
  - `02` Index: "kept at git `HEAD` by the indexer thread of each open
    Vault" — only a Vault with every port the Index needs indexes (D23,
    `who.rs`); "`get` and `history` still read files … the files win" —
    they read the git objects of `HEAD` (D22).

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
