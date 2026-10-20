# 03. Model

Document 3 of the Scheda package. Version 0.28, 20 October 2026.

Normative data model: collection kinds, Document identity, and paths. Terms
are in `02`. File bytes, git, and locks are in `04`. The collection
declaration is in `05`. Checkable requirements use codes `MOD`. Phase
**core** unless marked otherwise.

An assertion without a check is not a requirement.

---

## 1. Collections are declared, not built in

**MOD-1. No universal table.** The consumer declares collections. Scheda
does not ship domain collections and does not put every Document in one bag.
*Check:* two Vaults with different collection names work on the same
build (`tests/collections.rs`:
`a_collection_declared_by_another_handle_or_by_hand_is_seen`); grep over
`rust/scheda/src` does not contain those names. Test data is not that
scope (SCH-1).

**MOD-2. One kind per collection.** Kind is `standard`, `journal`, or
`singleton`. It does not change while the collection holds Documents
(`Vault.UpdateCollection` is `FAILED_PRECONDITION`); a new kind for
Documents is a new collection. *Check:* a declaration with no kind or two
kinds is rejected (`collection::tests::the_three_kinds_are_read_from_the_oneof`);
`Update` on a `journal` collection is `UNIMPLEMENTED`
(`tests/documents.rs`:
`a_request_that_breaks_a_rule_fails_before_the_lock_is_taken`); the kind
changes only while the collection is empty (`tests/collections.rs`:
`the_kind_isolated_and_encrypted_change_only_while_the_collection_is_empty`).

Kind exists so write mode and disk layout stay coupled. Kind is not a
codec for Document contents:

| Kind | Why it exists |
|---|---|
| `standard` | One editable file per Document; `Collection.Get` is a path. People and git see a file. |
| `journal` | Append-only facts in volume: no `Update`, `Upsert`, or `Delete`; many lines packed into **segments** so git stays usable without LFS. |
| `singleton` | One small file for the collection; replace as a whole. Not a journal dump. |

A rare immutable note can still be a `standard` Document the consumer never
replaces. `journal` is for the packed, append-only case.

---

## 2. Document identity

**MOD-3. Address is `(collection, id)`.** `Collection.Get` uses this pair.
Segment file names, line numbers, and git blob hashes are not the Document
`id`. *Check:* after rotation, `Get` still returns the same journal
Document; the path may name a closed segment (`journal::tests`:
`rotation_by_size_closes_a_segment_without_changing_it`;
`tests/index.rs`:
`a_line_is_found_before_the_indexer_ran_after_it_and_without_its_map`).

**MOD-4. `id` is unique in the collection.** A second `Create` with the
same `id` is `ALREADY_EXISTS` (or, with `keep_existing`, returns the stored
Document and writes nothing). Uniqueness is across all segments of a
`journal`; an append checks it through the id map of the journal and the
segments changed since the map (`06` IDX-5), not by reading every segment,
and a journal with no map is scanned. *Check:* create, create the same `id` → `ALREADY_EXISTS`
(`tests/documents.rs`: `the_table_holds_for_a_standard_collection`,
`the_table_holds_for_a_journal`); two segments that both contain the same
`id` are an `ALREADY_EXISTS` problem in the `Vault.Check` report
(`journal::tests`: `the_check_collects_every_problem_of_every_journal`). *Check:* a duplicate `id` is `ALREADY_EXISTS` (and `keep_existing` returns the stored payload) when the line is in a closed segment the map names and when it was appended after the map's state; a journal with no map, a map whose state is gone, and a map that is wrong about the `id` still refuse it (`idmap::tests`: `an_id_the_map_names_or_a_segment_changed_since_it_holds_is_already_there`, `a_journal_with_no_map_refuses_a_duplicate_as_well`).

**MOD-5. `id` is the path (standard / singleton).** The library derives the
`standard` file from `id`: `{name}/{id}`, where `/` is a directory. It
does not append `.md`. A `.` inside a component is allowed (`foo.md`,
`work/meeting.toml`). Renaming the file is changing `id`. `Get` uses the
`id` the caller passes; it does not follow a rename. *Check:*
`Create("foo.md", …)` writes `{name}/foo.md`; `Get("foo.md")` reads that
file (`documents::tests`: `paths_follow_the_id_and_the_kind`;
`tests/documents.rs`: `the_table_holds_for_a_standard_collection`, whose
ids are `work/meeting.md` and `2026/day-1`). A file a person committed by
hand is read under its path (`tests/documents.rs`:
`a_document_committed_by_a_person_is_read_replaced_and_deleted`).

**MOD-6. Scheda does not invent domain identifiers.** The caller supplies
`id` on a write. The library requires each `id` component to
be a slug (Unicode letter or digit, then letters, digits, marks, `.`, `_`,
`-`, `+`, `(`, `)`, space, or `@`; max 255 bytes for the whole `id`; no
leading/trailing whitespace; no trailing `.`; not a Windows device name)
so Scheda files (`.collection.yaml`, `.scheda/`) stay distinct from
Documents: a component must not be empty, `.`, or `..`, and must not start
with `.`. A `standard` `id` may join those slugs with `/`; `journal`,
`singleton`, and collection names stay a single slug. The collection
declaration does not add a pattern on top of that rule. The library does
not mint ULIDs, kebab-case, or any other consumer scheme. *Check:* a write
with an `id` that fails the slug is `INVALID_ARGUMENT`; a `standard` `id`
`work/meeting.toml` is accepted; `foo.md` is accepted; `../x`, `foo/./bar`,
`/abs`, `a\\b`, and `.hidden` are `INVALID_ARGUMENT` (`id::tests`:
`accepts_plain_slugs`, `rejects_empty_slash_device_and_edges`,
`document_id_allows_slash_separated_slugs`,
`document_id_rejects_injection`; `tests/documents.rs`:
`a_request_that_breaks_a_rule_fails_before_the_lock_is_taken`);
implementation source does not hard-code a consumer's id scheme.

**MOD-7. Singleton identity.** A `singleton` collection has exactly one
Document. The collection declaration fixes its `id` (may include an
extension). `Create` is allowed only when the file is absent; afterwards
only `Update` and `Upsert`. The request's `id` must equal the declared
`id`. *Check:* a second `Create` is `ALREADY_EXISTS`; `Get` with that `id`
round-trips the file; `Create` with a different `id` is `INVALID_ARGUMENT`
(`tests/documents.rs`: `the_table_holds_for_a_singleton`,
`a_request_that_breaks_a_rule_fails_before_the_lock_is_taken`).

---

## 3. Paths

**MOD-8. The library owns Document paths.** The only filesystem path the
caller supplies is the Vault root (`Scheda.Open`). Collection name is a slug
(same rules as one Document `id` component), not a path. Each collection
occupies `{vault}/{name}/`. Isolated Documents: `{vault}/.isolated/{name}/`;
the collection declaration stays `{vault}/{name}/.collection.yaml`. Files:

| Kind | Document file |
|---|---|
| `standard` | `{name}/{id}` (`/` in `id` is a directory: `work/meeting.toml` → `{name}/work/meeting.toml`) |
| `journal` | `{name}/` plus segment files the library names |
| `singleton` | `{name}/{declared id}` |

The caller never passes a Document path. `Get` and `History` validate
`id` then read that file (or git for it) from `HEAD`. `Update`, `Upsert`,
and `Delete` check the stored state of that same path before they write.
Queries and searches take `id`s from the Index, then read those files.
Isolated is not a special case. A listing (`GetMany`) gives the regular
files except dotfiles; it does not require a `.md` suffix and it does not
skip `.md` files. `.collection.yaml` is a dotfile and is not a Document.
*Check:* a collection named `a/b` is `INVALID_ARGUMENT`
(`collection::tests`: `slugs_are_checked_by_the_library`); `Get("alpha")`
reads `{name}/alpha` for a `standard`, `Get("foo.md")` reads
`{name}/foo.md`, and `Get("work/meeting.toml")` reads
`{name}/work/meeting.toml` (`documents::tests`:
`paths_follow_the_id_and_the_kind`). *Check:* a listing of a collection
that holds nested files walks them in `id` order and does not return a
dotfile such as `.collection.yaml` (`documents::tests`:
`the_walk_yields_ids_in_id_order_across_nested_directories`;
`tests/reads.rs`:
`a_listing_is_in_id_order_whole_ids_and_reads_no_file_but_the_commit`).

**MOD-9. Journal path is the segment, not the `id`.** The Document still
lives at `(collection, id)`. The file path is the open or closed segment
that contains the line. Callers must not treat the segment name as `id`.
*Check:* `Get` does not require the caller to pass a segment name
(`tests/documents.rs`: `the_table_holds_for_a_journal`).

**MOD-10. Journal segments.** A `journal` collection is a directory of
segment files the library names. Each clone of the Vault has a writer
identity (`OpenOptions.writer`, kept in `{vault}/.scheda/writer`) and
appends only to its own segments. The collection declaration
closes that writer's open segment on a size cap (required) and optionally
a line cap or calendar boundary. A closed segment is never rewritten.
*Check:* after the cap, the next `Create` creates or opens a new segment for
that writer; bytes of the previous file are unchanged; each segment stays
below the cap (`journal::tests`: `rotation_by_size_closes_a_segment_without_changing_it`,
`rotation_by_line_count`; `tests/documents.rs`:
`a_journal_rolls_over_to_a_new_segment_inside_one_batch`). *Check:* two
clones with different writers produce disjoint segment names
(`journal::tests`: `the_next_segment_counts_only_the_writers_own`,
`segment_order_is_seq_then_writer_then_daily_overflow`).

The numeric default cap is in `05`. It must sit well below common git-host
file limits (warn ~50 MiB, reject ~100 MiB) and well below what `git diff`
remains usable for. This package does not use Git LFS.

---

## 4. Shape of a Document

**MOD-11. A Document is bytes.** `standard` and `singleton`: the file
bytes, any bytes (text, an image, protobuf wire). `journal`: the payload
after the first `:` (colon, then space) on the line (see MOD-16).
Scheda does not parse those bytes as YAML, Markdown, JSON, or any other
document format, and does not ask for UTF-8 (`20` D20). Empty bytes are
allowed (empty file / empty payload). A `.` in `id` is not a format
signal. *Check:* `Create` of `foo.md` and `foo.yaml` in one collection
stores two Documents; `Get` of each returns the bytes that were written;
neither file is parsed. *Check:* a PNG and bytes that are not UTF-8
round-trip through `standard` and `singleton` unchanged (`documents::tests`:
`png_and_non_utf8_bytes_round_trip_in_standard_and_singleton`;
`tests/documents.rs`:
`any_bytes_round_trip_in_standard_and_singleton_collections`).

**MOD-12 (retired).** Missing vs default was a Schema-field rule.

**MOD-18 (retired).** JSON `null` vs a missing key was a Schema-field
rule.

**MOD-13 (retired).** Header `id` matching the path. For `standard` /
`singleton`, identity is the path (MOD-5). For `journal`, identity is the
envelope `id` (MOD-16). `Vault.Check` does not parse Document bytes for a
consumer `id` key. It still reports duplicate ids, broken journal
envelopes, an unfinished merge (from git's own state; no encoding check,
`20` D20), declaration errors, isolation gitignore, Index modes, and a
leftover `.schema.yaml`.

**MOD-14 (retired).** Empty body vs journal. Kind is the collection's
write mode, not whether a file has a header.

---

## 5. Writes implied by kind

**MOD-15. `standard`.** `Create` creates the file with the caller's bytes.
`Update` replaces the whole file. `Delete` removes the file. Partial
patch of contents is not a Scheda operation. *Check:* `Update` changes
bytes of that path only. *Check:* `Delete` then `Get` → `NOT_FOUND`; the
file is gone (`tests/documents.rs`:
`the_table_holds_for_a_standard_collection`).

**MOD-16. `journal`.** `Create(id, payload)` appends `{id}: {payload}\n` to
the open segment (colon, then space). `id` is a single slug (MOD-6; `:`
is not in the slug set). `payload` is bytes and must not contain `\n` or
`\r` (`INVALID_ARGUMENT` otherwise). Empty payload is `{id}: \n`.
`Document.raw` is the payload, not the envelope. Scheda splits on the
first `:` (colon, then space); it does not parse the payload. The segment
is not a YAML document. `Update`, `Upsert`, and `Delete` are
`UNIMPLEMENTED`. A journal has no delete: rewriting a segment would
break the append-only guarantee a caller may rest a checksum chain on. A
truncated line, a missing `:` delimiter (colon, then space), or an empty
envelope `id` is detected on read and in `Vault.Check` and is not silently
repaired. *Check:* `Update` / `Upsert` / `Delete` → `UNIMPLEMENTED`
(`tests/documents.rs`: `the_table_holds_for_a_journal`); a payload with
`\n` or `\r` is `INVALID_ARGUMENT` and any other bytes are stored
(`journal::tests`: `a_line_is_id_colon_space_payload_newline`,
`a_payload_with_lf_or_cr_is_invalid_argument`,
`a_payload_that_is_not_utf8_is_encoded_parsed_and_found_by_id`); mid-file
corruption → `Check` reports it, and valid lines before the damage remain
readable as specified in `04` (`journal::tests`:
`a_torn_tail_or_a_broken_line_ends_the_walk_with_the_segment_path`,
`lines_split_into_complete_lines_and_a_torn_tail`). *Check:* a
Vault opened by handles without ports keeps the journal id maps; `Get`
finds a line before the indexer ran, through the map, and by scanning the envelopes when the map is gone (`tests/index.rs`:
`a_line_is_found_before_the_indexer_ran_after_it_and_without_its_map`,
`the_index_object_needs_the_feature_and_the_maps_are_kept_either_way`).

**MOD-17. `singleton`.** Replace is the whole file (the declared `id`
path). No content schema is applied. `Delete` removes that file; a later
`Create` of the declared `id` is allowed again (MOD-7). The file must remain
small for the same git reasons as a segment. *Check:* `Create` of a second
logical Document is `INVALID_ARGUMENT` (see MOD-7). *Check:* `Delete` then
`Get` → `NOT_FOUND` (`tests/documents.rs`:
`the_table_holds_for_a_singleton`).

---

## 6. What this document does not decide

Git commit, the mirror, `auto_sync`, `Mirror.Sync`, write lock, and
working-tree durability — `04`.

Collection declaration keys (kind, isolation, encryption, journal caps)
— `05`. Index columns and `Projector` — `06` / `13`.

How `Get` locates a journal `id` without scanning every segment — `06`
(Index or equivalent lookup). Without it, a correct but slow scan of
envelopes is allowed. Order of `Query` on a `journal` — `06` (QRY-7:
append order). Walking every journal line without the Index — a
`Collection.GetMany` listing, which needs no Index.
