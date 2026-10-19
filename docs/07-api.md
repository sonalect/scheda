# 07. Library API

Document 7 of the Scheda package. Version 2.00, 19 October 2026.

Language-neutral contract: objects, operations, arguments, outcomes, and
error codes. The messages and the services are `api/scheda/v1/*.proto`
(Protobuf edition 2024, protovalidate rules). **What each call does, its
rules, and the codes it fails with are the comment of its rpc** in those
files (`20` D12): the plugin turns it into the rustdoc of both traits, and
any other binding reads the same text. This document keeps what spans
methods. Bindings (Rust first; later Python, Java, Go, TypeScript, and
others) use the rpc names or a documented 1:1 mapping. A caller-supplied
message that fails the protovalidate rules is `INVALID_ARGUMENT`, before
the collection declaration, git, or the write lock. Agent transport (`08`)
is the Protobuf JSON mapping of those messages. Codes: `LIB`. Phase
**core** unless marked **search** or **agent**.

Idioms of one language appear only in a sentence that names that language (`02`).

An assertion without a check is not a requirement.

---

## 1. Shape of a binding

**LIB-1. In-process library.** The caller loads Scheda in its own process. A Scheda daemon or HTTP server is not required to read or write. *Check (**core**):* a test writes and reads a local Vault (`auto_sync` off) with no second process and no network (`tests/reads.rs`: `an_arc_dyn_vault_sync_from_open_drives_a_vault_end_to_end`).

**LIB-2. The same objects and calls in every language.** The contract is a set of objects, each a service, reached from the one before it (`20` D24): `Scheda` (`Info`, `Open`) opens a `Vault`; a `Vault` gives a `Collection` (`Vault.GetCollection`), its `Mirror` (`Vault.Mirror`), and its `Index` (`Vault.Index`). An object travels as a *reference message* (`VaultRef`, `CollectionRef`, `MirrorRef`, `IndexRef`; for ports `EmbedderRef`, `RerankerRef`, `ProjectorRef`) that holds the implementation itself within one process; a reference has no Protobuf JSON form, and one that holds nothing is `INVALID_ARGUMENT` where it is given. A binding without such a thing documents its mapping. Required: every rpc of `Scheda`, `Vault`, `Collection`, `Mirror`, and, in a build with `find`, `Index`. A method of a feature the build lacks answers `UNIMPLEMENTED`, and `Scheda.Info` lists the features of the build (`FEATURE_FIND`, `FEATURE_SEARCH`, `FEATURE_ENCRYPT`). No call, field, or term of the API says commit, autocommit, transaction, or `HEAD` (`20` D6): there is no `begin`, `commit`, `discard`, `transaction`, `add`, `replace`, `remove`, `init`, `clone`, `catalog`, `walk`, `walk_inverse`, or `scan` as a public name. Creating or cloning a Vault is `Scheda.Open` (`OpenOptions.mode`, `OpenOptions.mirror`). Ordered journal read without the Index is `Collection.GetMany` with a listing, and listing a `standard` collection without the Index is the same call. In Rust `scheda`, `find`, `search`, and `encrypt` are opt-in crate features; there is no default feature set, so a plain `cargo add scheda` omits those methods (`10` §2). `encrypt` adds no method — it decides whether the Vault can read and write an encrypted collection at all, and a build without it answers `UNIMPLEMENTED` for those collections (ISO-5). *Check:* `Scheda.Info` lists the features of the build, and a build without `find` or `search` answers its calls `UNIMPLEMENTED` naming the feature (`tests/objects.rs`: `scheda_info_gives_the_version_and_the_features_of_the_build`; `tests/query.rs`: `without_find_the_four_calls_are_unimplemented_naming_the_feature`; `tests/search.rs`: `without_search_the_two_calls_are_unimplemented_naming_the_feature`). *Check:* no name of the contract carries a git term (`rust/api/tests/git_terms.rs`).

**LIB-3. Whole-Document writes.** Mutation is `Create`, `Update`, `Upsert`, or `Delete` of the whole Document's bytes. A Document is bytes: Scheda does not ask for UTF-8 (`20` D20). There is no partial patch of contents. SQL against the Index is not a caller API. The `id` is a method argument; the library checks it before any file is written (`03` MOD-6). *Check:* `Update` on a `journal` is `UNIMPLEMENTED`, and so is `Delete` (`tests/documents.rs`: `the_table_holds_for_a_journal`). *Check:* any bytes round-trip in `standard` and `singleton` collections (`tests/documents.rs`: `any_bytes_round_trip_in_standard_and_singleton_collections`).

**LIB-4. Observable `Scheda.Open`.** `Scheda.Open(VaultOpening)` takes the Vault path, the `OpenOptions`, and the three port references, and returns a `VaultRef`. After it, `Vault.Info` returns `VaultInfo`: the Vault path, the Index path (`{vault}/.scheda/index`), and the writer identity of this clone. `Mirror.Info` returns the mirror `url`, `auto_sync`, and `last_auto_sync_error`; `Vault.ListCollections` returns the declarations (`05`): kind, isolation, encryption, caps, Index-exclusion flags — not Document fields. The Vault root is the only filesystem path the caller supplies. No process-global singleton forbids a second Vault: opening the same path twice makes two Vaults on it, as two processes would. Options at `Scheda.Open`: `auto_sync` (GIT-10), `mirror` (the URL to clone from when the path holds no Vault; on an existing Vault it must be the mirror's URL), `encryption` (`Encryption`, the one secret this Vault uses for every encrypted collection, ISO-5, ISO-7), `author` (`Author`, GIT-2), `writer` (unset → generated into `{vault}/.scheda/writer`), `index_poll_ms` (`06` IDX-9), `mode` (`OPEN_MODE_OPEN_OR_CREATE`, `OPEN_MODE_OPEN_EXISTING`, `OPEN_MODE_CREATE_NEW`), `write_timeout_ms` (LIB-32), and `signing` (not built: `UNIMPLEMENTED`). Setting `encryption` never encrypts anything by itself: what is ciphertext on disk is decided by each collection's declaration (SCH-16); in a build without `encrypt`, setting it is `UNIMPLEMENTED` and nothing is created. The Index directory is not a caller option (GIT-13). `Scheda.Open` does not keep the write lock; every write takes it for that write only (LIB-32). Repairing `.gitignore` and adding the marker to an existing git work tree are one write each, made under the lock and released before `Open` returns. With `auto_sync` on and a mirror set, the pull at `Open` takes the lock for that pull (STO-4, GIT-10); a failure of it neither fails `Open` nor is lost: `Mirror.Info` shows it. *Check (**core**):* `Vault.Info` gives the path, the Index path, and the writer (`tests/objects.rs`: `vault_info_gives_the_path_the_index_path_and_the_writer`). *Check:* creating a Vault is one commit with the marker and the `.gitignore` entries (`tests/write.rs`: `a_missing_path_becomes_a_vault_in_one_commit`); two opens of one new path make one Vault (`tests/write.rs`: `two_opens_of_one_new_path_at_once_make_one_vault_and_both_open_it`). *Check:* `Mirror.Info` gives the URL, `auto_sync`, and the last failure (`tests/objects.rs`: `mirror_info_gives_the_url_auto_sync_and_the_last_failure_of_a_sync_by_itself`). *Check:* each mode refuses what it must (`tests/open.rs`: `a_missing_path_is_not_found_in_open_existing_and_stays_missing`, `create_new_refuses_a_vault_and_the_other_modes_open_it`). *Check:* two Vaults on one path both write and each sees the other (`tests/consumer.rs`: `two_vaults_on_one_path_both_write_and_each_sees_the_other`).

**LIB-5. Failures are coded.** `scheda::Error` is `protocontract::Error`, and `VaultErrorCode` is the enum of the `Vault` service (Protobuf JSON `VAULT_ERROR_CODE_*`). A failure has a stable code, an optional human message, and a cause as its source; the message must not replace the code. A code is what a caller acts on; where Scheda means what a gRPC canonical code means, the code takes that name.

| `VaultErrorCode` | Retryable | The caller |
| - | - | - |
| `INVALID_ARGUMENT` | no | fixes the request |
| `NOT_FOUND` | no | treats it as absent |
| `ALREADY_EXISTS` | no | treats it as present |
| `FAILED_PRECONDITION` | no | reads again |
| `UNIMPLEMENTED` | no | calls something else or uses another build |
| `ABORTED` | yes | tries again |
| `UNAVAILABLE` | yes | tries again later |
| `UNAUTHENTICATED` | no | supplies credentials for the mirror |
| `NOT_A_VAULT` | no | opens another path |
| `GENERATION` | no | moves the Vault to this generation |
| `CRYPTO` | no | supplies the secret |
| `MIRROR_AHEAD` | no | pulls, then tries again |
| `MIRROR_CONFLICT` | no | resolves by hand |
| `UNRESOLVED_CONFLICT` | no | finishes the merge |
| `INDEX_REBUILDING` | yes | tries again soon |
| `INDEX_STALE` | no | runs `Index.Reindex` |
| `INDEX_BROKEN` | no | runs `Index.Reindex` |
| `STORAGE` | per kind | reports it |
| `REPLICATION` | no | reports it |

`STORAGE` is retryable for the transient kinds (`Interrupted`, `WouldBlock`, `ResourceBusy`). The flag is `retryable` on the error; a method that knows better than the table sets it on the instance. `UNIMPLEMENTED` also answers a call that does not apply (`Update` on a `journal`, a query naming an encrypted collection, ISO-4). A port has codes of its own (`EmbedderErrorCode`, `RerankerErrorCode`, `ProjectorErrorCode`: `UNAVAILABLE` retryable, `REFUSED` not); a panic of a port under the async form is protocontract's `RuntimeCode::Panicked`. One Scheda type, `scheda::Fault`, carries a code, a message, an optional Vault-relative `path`, the retry flag, and the source; the library API returns the error, and a caller branches on the code, never on the message or on the `Fault`. For a transport (the CLI, `IndexState.last_error`) the error is written out as an `ErrorReport { code, message, retryable, stack, path }` with `code` the code's name (`08`, `14`). Messages never carry a credential. How a language surfaces a code is that language's affair: Rust `Result` with an error type that carries the code; Go `(value, error)`; Python exception or a result object; Java exception or a result type; TypeScript throw or a result object. The code name is what tests and `08` share. *Check:* negative tests assert codes, not only message text; every code has the retry flag of the table (`error::tests::every_code_has_the_retry_flag_of_the_table`); a report carries the code name, the flag, the stack, and the path (`error::tests::a_report_carries_the_code_name_the_flag_the_stack_and_the_path`); a foreign cause stays reachable through the source (`error::tests::io_failure_keeps_its_error_kind_through_source`, `error::tests::or_fault_codes_a_foreign_error_and_keeps_it_as_source`).

**LIB-6. Sync is enough for core.** Every call has a blocking form and needs no async runtime. The background indexer (IDX-9) is a plain OS thread in every build, not a runtime task (`20` D11). A search with a local embedder or reranker runs in the same process on the caller's thread. An async form is optional and sits beside the sync one (LIB-29). *Check (**core**):* a binding's core tests run without starting an async executor (`tests/reads.rs`: `an_arc_dyn_vault_sync_from_open_drives_a_vault_end_to_end`). (Rust: no Tokio required for those tests; a build without the `tokio` feature has no Tokio dependency.) *Check:* the indexer thread ends with `Vault.Close` and with the last drop (`tests/index.rs`: `close_and_drop_end_the_indexer_thread`).

**LIB-31. Ports.** The `Embedder`, the `Reranker`, and the `Projector` are services the caller implements and Scheda calls (`ports.proto`; `06` IDX-8; `13`). Scheda ships no model and does not know what a Document holds. A port is given as a reference in `VaultOpening` or later (`Vault.SetEmbedder`, `SetReranker`, `SetProjector`) and taken away with the matching `Remove…`; it is the Vault's until then and may change at any time. The indexer takes the new set into its next round (who indexes depends on the ports a Vault holds). The `Describe` of an embedder and a Projector is read once, when the port is given; one that breaks its rules fails the call with `INVALID_ARGUMENT`. A port is given in the form its implementation has, blocking or async, and is called the way Scheda is called: a sync call calls a blocking port directly (an async port through `Blocking`); an async call awaits an async port (a blocking one through `Offload`). A port of a feature the build lacks is `UNIMPLEMENTED`; an async port outside a tokio runtime, and a reference that holds nothing, are `INVALID_ARGUMENT`. Without a Projector, writes and `Collection.Get` still work, a query on identity (`all`, `term` on `id`) still works, any other Filter field is `INVALID_ARGUMENT`, and a search has no FTS/vector slots. A search that sets `rerank_limit` needs a reranker (QRY-5). *Check:* a Projector and a reranker are installed at open, a descriptor is read once (`tests/ports.rs`: `a_projector_is_installed_at_open`, `a_reranker_is_installed_at_open`, `open_reads_the_descriptor_once`). *Check:* a descriptor that breaks its rules fails `Scheda.Open` and `Vault.SetEmbedder` (`tests/ports.rs`: `a_descriptor_that_breaks_its_rules_fails_open_and_set_embedder`); an async port outside a runtime and a reference without an implementation are `INVALID_ARGUMENT` (`tests/ports.rs`: `an_async_port_outside_a_runtime_is_invalid_argument`; `tests/objects.rs`: `a_port_reference_that_cannot_be_used_is_invalid_argument`); a port of a missing feature is `UNIMPLEMENTED` (`tests/ports.rs`: `an_embedder_or_a_reranker_without_search_is_unimplemented`, `a_projector_without_find_is_unimplemented`). *Check:* removing the embedder a `vector` column needs stops indexing until one is given (`tests/objects.rs`: `removing_the_embedder_a_vector_column_needs_stops_indexing_until_one_is_given`).

**LIB-32. The write model.** A single write (`Collection.Create`, `Update`, `Upsert`, `Delete`; `Vault.CreateCollection`, `UpdateCollection`, `DeleteCollection`) is atomic: when the call returns `Ok` the write is stored, and when it fails nothing was written. Several writes are one batch: a stream of operations (`Collection.Batch`, `Vault.BatchDocuments`, `Vault.BatchCollections`) of which all happen or none; an `Err` item, a broken stream, or one write that fails, and none happened, and the error names the operation by its index. A batch holds Document writes or collection writes, never both, and `Vault.BatchDocuments` does not mix isolated collections with the others (`INVALID_ARGUMENT`: they are two repositories). Writes from every object and every process on one Vault are applied one after another, in the order they were made; no write fails because another one is running. A write waits for the write lock (STO-4) up to `OpenOptions.write_timeout_ms` (default 60000); past it the call is `ABORTED` and writes nothing, and once it holds the lock it finishes. A write is `ABORTED` too when `HEAD` moved under it (a `git` command run by hand). One verb per meaning: `Create` of a Document that is present is `ALREADY_EXISTS` (or, with `keep_existing`, the stored Document is returned and nothing is written); `Update` of an absent one is `NOT_FOUND`, with `revision` the stored revision must be that one or `FAILED_PRECONDITION`; `Upsert` creates or replaces; `Delete` of an absent one is `NOT_FOUND` (or nothing with `ignore_missing`). The same bytes store nothing and keep the revision. `Document.revision` is an opaque token that changes when the stored bytes change. Every write carries a `note` (why it is made; `Collection.History` shows it). `auto_sync` pushes after a write and a failure of the push neither fails nor undoes the write (`Mirror.Info`). There is no interactive transaction: a caller builds a write whole before it calls (`10`). The contract of each call is its rpc comment. *Check:* a batch is one commit with one result per operation (`tests/documents.rs`: `a_batch_is_one_commit_with_one_result_per_operation`); a failing write in the middle stores nothing and names its index (`tests/documents.rs`: `a_failing_write_in_the_middle_of_a_batch_stores_nothing_and_names_its_index`; `tests/collections.rs`: `a_batch_with_a_failing_third_operation_declares_changes_and_deletes_nothing`). *Check:* a call that cannot take the lock in time is `ABORTED` and stores nothing (`tests/documents.rs`: `a_call_that_cannot_take_the_lock_in_time_is_aborted_and_stores_nothing`; `tests/consumer.rs`: `a_write_while_another_holds_the_lock_past_the_timeout_is_aborted_and_retryable`). *Check:* writes from one handle and from three processes are each stored whole (`tests/documents.rs`: `sixty_four_document_writes_from_one_handle_are_each_stored_whole`, `sixty_six_document_writes_from_three_processes_are_each_stored_whole`). *Check:* the verbs behave as the table says in every kind of collection (`tests/documents.rs`: `the_table_holds_for_a_standard_collection`, `the_table_holds_for_a_singleton`, `the_table_holds_for_a_journal`).

**LIB-33. Consistency.** Reads see stored state only: a write whose call has not returned is seen by no read (`queries::tests::a_write_that_has_not_returned_is_seen_by_no_query`, `search::tests::a_write_that_has_not_returned_is_seen_by_no_search`), and a file a person changed and did not commit is seen by none (`tests/documents.rs`: `a_persons_uncommitted_edit_is_seen_by_no_get_document_and_a_commit_by_hand_is`). A read of Documents (`Collection.Get`, `GetMany`, `History`) never waits on the Index. A stream is lazy and reads from one snapshot, so a Document written while it is read does not appear in it (`tests/reads.rs`: `a_stream_started_before_a_write_does_not_see_it`). The Index follows the stored state in the background (`06` IDX-9): queries and searches answer from what it holds, so they are eventually consistent, and `Index.Watch` tells how far it is. An Index not built yet is `INDEX_REBUILDING`, never an empty answer (`06` QRY-4). *Check:* a write is found by a query once the Index reached it (`tests/consumer.rs`: `watch_reports_the_version_of_a_write_and_the_write_is_then_found`; `tests/index.rs`: `the_status_gives_the_state_at_once_and_then_the_version_of_each_write`).

**LIB-34. Requests are strict, stored data is read as Protobuf reads it.** A request that breaks a rule of its message is `INVALID_ARGUMENT` before the write lock is taken (`tests/documents.rs`: `a_request_that_breaks_a_rule_fails_before_the_lock_is_taken`). Protobuf JSON a caller sends (CLI stdin, an agent's request) is parsed as Protobuf JSON is by default: an unknown field is `INVALID_ARGUMENT` naming its path (`stored::tests::a_command_with_an_unknown_field_is_invalid_argument_naming_its_path`), since a binary that silently dropped an option of a later caller would do something other than asked. Files Scheda keeps as a message (the marker, a collection file) are read past a field they do not know, and a rewrite keeps it (`20` D21; `tests/open.rs`: `an_unknown_key_in_the_marker_and_a_leftover_version_in_a_collection_file_open`; `tests/collections.rs`: `update_keeps_the_unknown_keys_of_the_file_and_writes_no_default`).

---

## 2. Document and collection calls

Each item below was the per-method contract of `0.21`. It moved into the comment of the rpc (`20` D12), which is the only place it is written.

**LIB-7.** *Retired (docs/20 D12):* `add` is `Collection.Create` (and `Collection.Upsert`); the contract is the rpc comment in `api/scheda/v1/collection.proto`. The `id` rules are `03` MOD-6.

**LIB-8.** *Retired (docs/20 D12):* `get` is `Collection.Get` and `Collection.GetMany` (the `lines`, `documents`, and `ids` walks of LIB-19, LIB-25, and LIB-26 are its listing); the contract is their rpc comments in `collection.proto`.

**LIB-9.** *Retired (docs/20 D12):* `replace` is `Collection.Update`; the contract is its rpc comment in `collection.proto`.

**LIB-24.** *Retired (docs/20 D12):* `remove` is `Collection.Delete`; the contract is its rpc comment in `collection.proto`.

**LIB-10.** *Retired (docs/20 D12):* `find` is `Collection.Query` and `Collection.Count`, and `Vault.QueryDocuments` and `Vault.CountDocuments` over several collections; the contract is their rpc comments in `collection.proto` and `vault.proto`, and `13` §3.

**LIB-11.** *Retired (docs/20 D12):* `search` is `Collection.Search` and `Vault.SearchDocuments`; the contract is their rpc comments in `collection.proto` and `vault.proto`, and `13` §6. An Index built by a Projector of another projection, or a collection whose columns changed, is `INDEX_STALE`, Filter or not, as for a query (`06` IDX-9).

**LIB-20.** *Retired (docs/20 D12):* the catalog is `Vault.ListCollections` and `Vault.GetCollection` (with `Collection.Describe`); the contract is their rpc comments in `vault.proto` and `collection.proto`. A Vault reads the declarations from the work tree, so a collection another Vault or process declared is seen without reopening (`tests/collections.rs`: `a_collection_declared_by_another_handle_or_by_hand_is_seen`).

**LIB-21.** *Retired (docs/20 D12):* `create_collection` is `Vault.CreateCollection`, with `Vault.UpdateCollection`, `Vault.DeleteCollection`, and `Vault.BatchCollections`; the contract is their rpc comments in `vault.proto`.

---

## 3. Mirror and maintenance

**LIB-12.** *Retired (docs/20 D6):* there is no `commit`; a write is stored when its call returns (LIB-32).

**LIB-22.** *Retired (docs/20 D6):* there is no `begin`; several writes are one batch (LIB-32).

**LIB-23.** *Retired (docs/20 D6):* there is no `discard`; a write that fails wrote nothing (LIB-32).

**LIB-30.** *Retired (docs/20 D6):* there is no `transaction`; a batch is the one way to write several things as one (LIB-32).

**LIB-13.** *Retired (docs/20 D12):* `open` is `Scheda.Open`; the contract is its rpc comment in `scheda.proto` and the comments of `OpenOptions` in `options.proto`.

**LIB-14.** *Retired (docs/20 D12, D14):* `pull`, `push`, and `sync` are `Mirror.Pull`, `Mirror.Push`, and `Mirror.Sync` (with `Mirror.Info`, `Mirror.Set`, `Mirror.Remove`); the contract is their rpc comments in `mirror.proto`. The `*_isolated` calls are gone: isolated collections never leave the machine (ISO-6).

**LIB-15.** *Retired (docs/20 D12, D15):* `check` is `Vault.Check`, which returns a `CheckReport`; the contract is its rpc comment in `vault.proto`.

`Vault.Check` does not judge Index freshness: the Index lags `HEAD` by design (IDX-9), and `Index.Status` (LIB-27) reports the state it reached. `Vault.Check` takes `index.lock` without waiting while it reads the Index modes; while a round holds it, the Tantivy files that round is making (and narrows when it ends) are not looked at, so they are not reported as a wide mode. *Check (**find**):* `Collection.Create`, then `Vault.Check` passes.

**LIB-16. `Index.Reindex`.** IDX-1, IDX-9. On the Index object of a Vault (`Vault.Index`), with `find`. It asks this Vault's indexer for a rebuild and returns at once. The rebuild is an indexer round under `{vault}/.scheda/index.lock` (`06` IDX-9): it removes `commit` first, then every other file of the Index, indexes the git objects of `HEAD` of the Vault and of `{vault}/.isolated/` from nothing with this Vault's Projector and embedder — never the work tree, so an uncommitted edit is not indexed — and writes `commit` last. Meanwhile queries and searches are `INDEX_REBUILDING` (retryable), and writes, pulls, and syncs in every handle go on: the rebuild takes no write lock. Its progress and result are in `IndexState.rebuild`: the stage (`RUNNING`, `DONE`, `FAILED`), Documents read, Documents skipped (not read or refused by the Projector), journal segments read. A rebuild that fails is tried again from nothing after a pause. Every text is embedded again, since the stored vectors go with the rest of the Index. Because the rebuild uses this Vault's ports, it is how the Index moves to a new projection or a new embedder model, and how an `INDEX_BROKEN` Index is repaired. Encrypted collections are skipped (ISO-4); isolated collections are indexed. A Vault that lacks a port the Index needs → `FAILED_PRECONDITION` naming it; a rebuild already asked for or running in this Vault → `ABORTED`. *Check:* `Reindex` returns before the rebuild ends, a second one meanwhile is `ABORTED`, and the journal maps match afterwards (`tests/index.rs`: `reindex_returns_before_the_rebuild_a_second_is_aborted_and_the_maps_match`). *Check:* an embedder of another model sets `model_mismatch` and indexes nothing; a `Reindex` from it makes the vectors again with its model (`vector::tests::an_embedder_of_another_model_indexes_nothing_and_a_reindex_from_it_remakes_the_vectors`). *Check:* a Vault without a needed port gets `FAILED_PRECONDITION` (`vector::tests::a_handle_without_a_projector_or_a_needed_embedder_never_takes_a_round`).

**LIB-17.** *Retired (docs/20 D12):* `history` is `Collection.History`; the contract is its rpc comment in `collection.proto`.

**LIB-18 (retired).** RFC 8785 canonical JSON of journal lines. A journal `Collection.Create` writes the envelope `{id}: {payload}\n` (MOD-16). A journal Document's `raw` is the payload. There is no library canonicalization of consumer fields.

**LIB-19.** *Retired (docs/20 D12):* `lines` is `Collection.GetMany` with a listing of a `journal`, in append order; the contract is its rpc comment in `collection.proto`.

**LIB-25.** *Retired (docs/20 D12):* `documents` is `Collection.GetMany` with a listing of a `standard` or `singleton` collection, in `id` order, with `include_raw`; the contract is its rpc comment in `collection.proto`.

**LIB-26.** *Retired (docs/20 D12):* `ids` is `Collection.GetMany` with a listing and `include_raw` unset (keys and revisions only); the contract is its rpc comment in `collection.proto`.

**LIB-27. `Index.Status` / `Index.Watch`.** On the Index object of a Vault, with `find`; a build without `find` has no Index object (`UNIMPLEMENTED`). `Status` returns `IndexState` now. `Watch` streams it: the state at once, then one item per change (an indexer round, a failure, a rebuild starting, moving on, ending); it does not end by itself, and the caller drops it when it has what it waited for. The fields are those of `06` IDX-9: `version` (the opaque token of the state the whole Index reached), `pending`, `role`, `model_mismatch`, `rebuild`, `last_error`. Neither call runs git or reads the Index. There is no call that waits for a given write: `version` is opaque, and no call gives the token of `HEAD` to compare it with. The async form of `Watch` waits without holding a thread of the blocking pool. *Check:* `Watch` gives the state at once and then one item per round; a watcher that falls far behind skips to the oldest item kept (`indexer::tests::the_watch_gives_the_state_at_once_and_then_one_item_per_round`, `indexer::tests::a_watcher_that_falls_far_behind_skips_to_the_oldest_item_kept`; `tests/index.rs`: `the_status_gives_the_state_at_once_and_then_the_version_of_each_write`).

**LIB-28. `Vault.Close`.** On the Vault. Stops the indexer and joins its thread: a round stops at its next check (every 256 Documents, and around the embedding), and an `Embed` call in progress runs to the end of its stream first, so the caller bounds the wait with its embedder's own timeout. New calls through the Vault and every object reached from it fail with `FAILED_PRECONDITION` from the start of `Close`, which returns once the calls that run have ended and no lock is held (`20` D24 item 9). Dropping the last reference does the same. `commit` stays at the last finished round; what a stopped round wrote stays ahead of it, and the next round starts from `commit` again. Scheda installs no signal handler: a program that wants a clean exit on `SIGINT` / `SIGTERM` catches the signal and calls `Vault.Close`; being killed instead loses nothing. *Check:* after `Vault.Close` (or the last drop) the indexer thread is gone (`tests/index.rs`: `close_and_drop_end_the_indexer_thread`); after `Close` every object is `FAILED_PRECONDITION` and a second `Close` is nothing (`tests/objects.rs`: `after_close_every_object_is_failed_precondition_and_a_second_close_is_nothing`).

**LIB-29. Sync and async forms.** Optional (the `tokio` feature in Rust; `20` D11). Each service has a blocking form and an async one (`VaultSync` and `VaultAsync`, …); a reference gives the form its receiver needs, and a matching form comes back as the same implementation, with no bridge. An async object shares the state, the gate, and `Vault.Close` of the sync one. Each async call runs its blocking steps on the blocking pool of the runtime of the call and returns the same outcome and code; where it waits (the write turn, `Index.Watch`) it waits natively and holds no pool thread; it awaits an async port (a blocking one through `Offload`). So a search of an async object runs its blocking steps on the pool and awaits the embedder and the reranker between them, and `Vault.Close` does not wait for a model. A write runs the checks that need no lock first, so a request that can never succeed fails at once, never `ABORTED`. A panic in a port under the async form is `Panicked`. The indexer is an OS thread whatever form its embedder has. *Check:* the async objects pass on a multi-thread and a current-thread runtime (`tests/async.rs`: `the_async_objects_work_on_a_multi_thread_runtime`, `the_async_objects_work_on_a_current_thread_runtime`). *Check:* on a blocking pool of one thread, an open `Index.Watch` does not stop another call (`tests/async.rs`: `watching::an_open_watch_holds_no_pool_thread_and_wakes_on_a_change`). *Check:* a panicking port is `Panicked` in both forms (`tests/async.rs`: `searching::a_panicking_port_is_panicked_whether_it_is_async_or_blocking`). *Check:* a sync caller takes the sync form of an async reference and the reverse (`tests/async.rs`: `a_sync_caller_takes_the_sync_form_of_an_async_reference_and_the_reverse`).

---

## 4. What this API refuses

- Partial patch of Document bytes.
- A Document file path from the caller. `Get` / `Create` / `Update` / `Delete` / `History` take a validated `id` and the library opens, creates, or gits the file. A query and a search take `id`s from the Index (IDX-10). `GetMany` walks the Documents under the collection; the caller does not pass a path, a directory, or a glob.
- Delete of a journal line (`Collection.Delete` on a `journal` is `UNIMPLEMENTED`).
- A request with a field the contract does not know, or that breaks its rules (LIB-34).
- A call, field, or term that says commit, transaction, or `HEAD` (LIB-2), and an interactive transaction that holds the write lock while the caller works (LIB-32).
- Cross-Vault query unless the caller passes two Vaults into a combined helper (**later**).
- Shipping a ranking model in the library (QRY-5). A `Reranker` and a `Projector` are ports the caller gives (LIB-31); they are not Vault data.

---

## 5. First implementation (Rust)

The crate `scheda` is the first binding. It must:

- implement every rpc above through the generated traits (`VaultSync`, `CollectionSync`, `MirrorSync`, `IndexSync`, `SchedaSync`, and the async ones with `tokio`);
- keep the objects private: the public surface is `scheda::Scheda` (the root), `scheda::v1`, `Error`, `Fault`, `Context`, `error_report`, `redact_remote_text`, `parse_command`, the fixed-portion adapters `FixedEmbedder` and `FixedReranker`, and, with `encrypt`, `parse_age_identity`;
- use `Result` with `scheda::Error` carrying the LIB-5 codes (no `panic!` / `unwrap` / `expect` / `assert!` on the library path; tests may assert);
- carry `[lints] workspace = true` as in this repository.

That panic policy is **Rust**, not a requirement on other languages (Python, Java, Go, TypeScript, and others use their usual error conventions, still with LIB-5 codes).

---

## 6. What this document does not decide

How a binding wraps generated types (builders, `Result`, exceptions) as long as proto field names and rpc names hold. How a binding writes a port and gives it to the Vault is the binding's affair.

Isolated `encryption` / age armor — `09` (**later**).
`Projector.Columns` returns an `IndexSchema` (`ports.proto`). Filter column names — `06` / `13`.
