# 05. Collection declaration

Document 5 of the Scheda package. Version 0.52, 19 October 2026.

Normative rules for collection declarations as data in the Vault: where
they live, discovery, kind, journal caps, singleton `id`, isolation,
encryption, and collection-level Index exclusion; and the calls that
write them. Terms are in `02`. Kinds and identity are in `03`. Index
columns and `Projector` are `06` / `13`. Isolation trees stay `09`. Codes:
`SCH`. Phase **core** unless marked **search** or **later**.

An assertion without a check is not a requirement. A collection
declaration contains no executable code and no field list.

---

## 1. Declaration is files, not library types

**SCH-1. The declaration lives in the Vault.** Collection declarations
are YAML 1.2 (or JSON) files under git, next to the Documents:
`{name}/.collection.yaml` (also `.collection.yml` or
`.collection.json`). The file is the Protobuf JSON (or YAML) of the
message `CollectionDeclaration`. `name` is the collection name and the
on-disk directory (MOD-8). The directory name must equal the `name` in
the file. Changing a declaration is an ordinary git commit. Bindings write
the file through `Vault.CreateCollection` (SCH-17), not by offering a raw
path. A person may still edit those files in an editor (STO-2). Library
source (`rust/scheda/src`) must not name a consumer's collections. Test
data may: the check does not search it, so a fixture shaped like a
consumer cannot turn SCH-1 red. A leftover `{name}/.schema.yaml` (also
`.schema.yml` / `.schema.json`) is `INVALID_ARGUMENT`, not ignored.
*Check:* a second consumer adds collections without rebuilding Scheda;
grep over `rust/scheda/src` does not contain those collection names. *Check:*
a directory with `.schema.yaml` fails `Scheda.Open` and `Check`
(`collection::tests`: `load_refuses_a_leftover_schema_file`); a directory
whose name is not the `name` in the file fails (`the_directory_must_be_the_collection_name`,
`load_refuses_a_directory_named_otherwise`); a declaration written by
another handle or by hand is seen (`tests/collections.rs`:
`a_collection_declared_by_another_handle_or_by_hand_is_seen`).

`Vault.ListCollections` and `Collection.Describe` give this data as the
message, not a guess from walking Document files. They list write-mode and
perimeter marks, not Document fields.

**SCH-2. One declaration per collection.** Each collection has: name
(directory), kind (`standard` | `journal` | `singleton`, the one arm of
`schema`), and the keys in §7 that apply to that kind (journal caps; fixed
Document `id` for `singleton`; `isolated`; `encrypted`;
`exclude_lexical_index`; `exclude_vector_index`). There is no `version`
(SCH-13) and no `fields` key. There is no layout template. Document file
paths are fixed (MOD-8). Kind does not change while the collection holds
Documents (MOD-2). Journal caps may change (SCH-14). *Check:* a file with
no `schema`, or a `schema` with no kind, is `INVALID_ARGUMENT` on
`Scheda.Open` and `Check` (`collection::tests`:
`a_file_without_a_schema_is_invalid_argument`,
`the_three_kinds_are_read_from_the_oneof`); two of `.collection.yaml` /
`.yml` / `.json` in one collection directory is `INVALID_ARGUMENT`
(`load_refuses_two_declaration_files_in_one_directory`).

**SCH-3. No code in the declaration.** No scripts, templates that call
out to a language runtime, or “on write” hooks. Scheda reads the keys it
knows and acts on nothing else. A key it does not know is not an error
in a stored file: it is read past and kept as it was when Scheda rewrites
the file (`20` D21), so a later Scheda may add a field without a new
generation. In a command (a request to a call, CLI stdin) an unknown
field is `INVALID_ARGUMENT`, so a caller of a later version is not
silently ignored. Scheda does not scan Document bytes for wiki-links or
any other consumer syntax. *Check:* a declaration that is valid YAML but
contains an unknown key opens, and a rewrite keeps the key at its depth
(`collection::tests`: `unknown_keys_are_read_past_wherever_they_are`;
`tests/collections.rs`:
`update_keeps_the_unknown_keys_of_the_file_and_writes_no_default`); a
command with an unknown field is `INVALID_ARGUMENT` (`stored::tests`:
`a_command_with_an_unknown_field_is_invalid_argument_naming_its_path`).

**SCH-17. The calls that write declarations.** A collection write takes no
`note`: there is no history of a collection to show it, and Scheda
describes the commit itself (`create collection notes (standard)`). Each
call is atomic (`04` GIT-2).

- `Vault.CreateCollection(CollectionDeclaration)` declares a collection and
  returns the declaration as stored, defaults filled in. An existing name is
  `ALREADY_EXISTS`; a request that breaks a rule of the message is
  `INVALID_ARGUMENT`. An isolated collection gets its place under
  `{vault}/.isolated/`.
- `Vault.UpdateCollection(CollectionDeclaration)` changes the schema of an
  existing collection and returns the declaration as stored. Unknown is
  `NOT_FOUND`. The kind, `isolated`, and `encrypted` change only while the
  collection holds no Document; otherwise `FAILED_PRECONDITION`. The
  rewrite keeps the keys of the file this Scheda does not know (SCH-3) and
  writes no default the caller did not set.
- `Vault.DeleteCollection(CollectionDeletion)` deletes an empty collection:
  its declaration and its directory. One that holds Documents is
  `FAILED_PRECONDITION`: they are deleted first. Unknown is `NOT_FOUND`, or
  nothing with `ignore_missing`. The name can be declared again.
- `Vault.BatchCollections(stream CollectionOperation)` is several of these
  as one write: all happen, or none, in one commit. An `Err` item, a broken
  stream, or one write that fails stores nothing, and the error names the
  operation by its index. One result per operation, in order.
- A `CollectionRef` (from `Vault.GetCollection`) names its collection and
  finds it again on each call: after `DeleteCollection` its calls are
  `NOT_FOUND`, and after the name is declared again they work again.

*Check:* create returns the declaration with defaults and an existing name
is `ALREADY_EXISTS` (`tests/collections.rs`:
`create_returns_the_collection_with_defaults_and_writes_none`,
`create_refuses_a_declared_name_and_a_request_that_breaks_a_rule`). *Check:*
an update of an undeclared name is `NOT_FOUND` and a rewrite keeps unknown
keys and writes no default (`update_is_not_found_for_an_undeclared_name`,
`update_keeps_the_unknown_keys_of_the_file_and_writes_no_default`). *Check:*
delete of a collection with Documents is `FAILED_PRECONDITION` and changes
nothing; of an empty one it removes the declaration and the directory, and
the name can be created again; an unknown name is `NOT_FOUND` or nothing with
`ignore_missing` (`delete_refuses_a_collection_with_documents_and_changes_nothing`,
`delete_removes_an_empty_collection_and_the_name_can_be_declared_again`,
`delete_of_an_unknown_name_is_not_found_or_nothing_with_ignore_missing`).
*Check:* a batch whose third operation fails stores nothing; an `Err`
item names its index; a batch that succeeds is one commit; a batch may
create and delete the same name (`a_batch_with_a_failing_third_operation_declares_changes_and_deletes_nothing`,
`an_err_item_and_an_item_without_a_write_store_nothing_and_name_their_index`,
`a_batch_that_succeeds_is_one_commit_with_one_result_per_operation`,
`a_batch_may_create_and_delete_a_name_or_delete_and_create_it_again`).
*Check:* a `CollectionRef` finds its collection again on each call
(`tests/objects.rs`: `a_collection_ref_finds_its_collection_again_on_each_call`).

---

---

## 2. Retired field Schema

Field names, types, `required` / `nullable` / `default`, `exclude_find`,
body marks, and edges are not Scheda's. They are not keys on the
declaration. Column names for `Query` / `Search` live in the caller's
`Projector` (`06` / `13`). A write does not type-check Document bytes.

**SCH-4 (retired).** Declared field types (`string`, `integer`, …,
`object_list`) were a Vault Schema. Gone.

**SCH-5 (retired).** Required / optional / nullable were Schema-field
rules.

**SCH-6 (retired).** A Schema field named `id` or `body`, and a header
`id` that had to match the path. Identity is the path or the journal
envelope (`03` MOD-5, MOD-7, MOD-16). The declaration does not add a
pattern on top of the library slug (MOD-6).

**SCH-7 (retired).** `no_body` and per-body `exclude_fts` /
`exclude_vector`. There is no Scheda body. Collection-level Index
exclusion is SCH-9.

**SCH-8 (retired as a Schema rule).** Filter keys were Schema fields
plus `body`. `Filter` field names other than identity come from the
`Projector` (`06` / `13`).

**SCH-10 (retired as a Schema rule).** Vector column names taken from
Schema fields / `body`. Model identity still lives in the Index, not on
the Document (`06`).

**SCH-11 (retired).** Edges as Schema fields. Scheda does not store or
validate graph links.

**SCH-13 (retired).** *Retired (docs/20 D1):* the declaration has no
`version`; the generation of the Vault format is the marker's (`04`
GIT-14). A leftover `version` key in a collection file is an unknown key,
read past and kept (SCH-3). *Check:* a leftover `version` is read past and
kept (`collection::tests`: `a_leftover_version_is_read_past_and_kept`;
`tests/open.rs`:
`an_unknown_key_in_the_marker_and_a_leftover_version_in_a_collection_file_open`).

---

## 3. Writes vs Index

**SCH-12. A write stores bytes; `Get` returns them.** `Create`, `Update`,
and `Upsert` accept any bytes (MOD-11). They do not call
`Projector.Project` to accept or reject the write. An older file is left
on disk. `Collection.Get` by `id` returns the file bytes if the path
exists (journal: the envelope payload). `Vault.Check` does not type-check
Document contents (`03` MOD-13). A Document that `Project` rejects or
omits stays out of the lexical and vector indexes; that is a `Projector`
failure (`06`), not a Schema type check. *Check:* bytes that a later
`Projector` would omit still return from `Get`; the write of those bytes
succeeds (`lexical::tests`:
`a_projector_that_refuses_a_document_leaves_it_out_and_the_round_goes_on`:
the Document is out of the Index and its file stays; `tests/documents.rs`:
`any_bytes_round_trip_in_standard_and_singleton_collections`).

---

## 4. Index exclusion

**SCH-9. A collection may stay out of either Index.**
`exclude_lexical_index: true` keeps every Document of that collection out
of the Tantivy Index (`Query`, filters, BM25). A `Query` or `Count` that
names it is `UNIMPLEMENTED`, not an empty hit list. The journal id map is a
separate file and is still written, so keyed `Get` does not need Tantivy.
`exclude_vector_index: true` keeps Documents out of the vector Index: the
background indexer, the only caller of the embedder, never embeds that
collection; `Search` then uses the lexical
leg only. Either flag, or both. `Search` is `UNIMPLEMENTED` only when the
collection has no remaining search leg (both flags, or encrypted —
SCH-16). Both flags are redundant on `encrypted` and allowed. A
`journal` may set either flag. Default for each is `false`. Per-field
FTS / vector marks are not declaration keys; they belong on the
`Projector` (`06` / `13`). *Check (**search**):* a `Query` that names a
lexically excluded collection is `UNIMPLEMENTED` (`tests/query.rs`:
`find::an_isolated_collection_is_scanned_only_when_named_and_one_without_an_index_is_refused`);
a `Search` on a collection with only `exclude_vector_index` still returns
lexical hits, one with only `exclude_lexical_index` is searched by the
vector leg, and one with both legs left out is `UNIMPLEMENTED` when named
(`tests/search.rs`:
`search::a_collection_that_leaves_the_vector_index_out_is_searched_by_the_lexical_leg`,
`search::a_collection_that_leaves_the_lexical_index_out_is_searched_by_the_vector_leg`,
`search::a_collection_with_both_legs_left_out_is_unimplemented_when_named`);
the marks decide what is indexed (`collection::tests`:
`the_marks_are_read_and_decide_what_is_indexed`).

---

## 5. Journal segments (numbers)

**SCH-14. Size cap is required for `journal`.** The declaration sets
`segment_max_bytes`. If omitted, the default is **8 MiB** (8 × 1024 ×
1024). The implementation rejects a cap greater than **32 MiB** (still
under typical host warn/reject limits of ~50 / ~100 MiB; GIT-6).
Optional: `segment_max_lines`, calendar rotation (`daily` UTC). Rotation
is still size-first: a cap hit closes the segment even mid-day. Caps may
change on a later `Vault.UpdateCollection`. *Check:* a default journal
rotates at 8 MiB; a cap of `48 MiB` is `INVALID_ARGUMENT`; a cap or a line
cap of `0` is `INVALID_ARGUMENT` (`collection::tests`:
`journal_limits_are_checked_and_the_default_cap_is_filled_in`); after the
cap, the next `Create` does not grow the closed file (MOD-10;
`journal::tests`: `rotation_by_size_closes_a_segment_without_changing_it`,
`rotation_by_line_count`, `daily_rotation_names_by_date_and_starts_a_new_segment_on_a_new_day`).

Closed segments are never rewritten. Sequence files are eight lowercase
hex digits, then the clone's writer identity (`00000001-alice.jsonl`),
so name order matches numeric order and two clones cannot create the
same path. With `daily`, the open name is `YYYY-MM-DD-{writer}.jsonl`,
then `YYYY-MM-DD-00000001-{writer}.jsonl` if that day already closed on
size. A writer never appends to a segment named by another clone, nor to
an older writerless name (`00000001.jsonl`, `YYYY-MM-DD.jsonl`): those
files are still read. Rotation caps are evaluated per writer.

---

## 6. Isolated and encrypted collections

**SCH-15. Isolation is declared.** The declaration may mark
a collection `isolated: true`. Document files then live at
`{vault}/.isolated/{name}/` (ISO-2): a gitignored nested git that never
leaves the machine and that no `Mirror` call touches (ISO-6). The
declaration file stays `{vault}/{name}/.collection.yaml` in the main
tree, which also makes a collection `name` unique across the Vault,
isolated and plain alike. A `Query` or `Search` that names no collection
returns nothing of theirs; naming one is an ordinary query, because
identity (and Projector columns, when a Projector is present) sit in the
one Vault Index, which is owner-only for that reason (ISO-4).
`Vault.ListCollections` lists the collection as isolated; there is no
field catalog to omit (ISO-3). Changing `isolated` on a collection that
already holds Document files (standard or singleton files, journal
segments) is `FAILED_PRECONDITION` and leaves the declaration and the files
untouched, as for `encrypted` (ISO-11); on an empty collection it is
allowed. *Check:* changing `isolated` on a non-empty collection is
`FAILED_PRECONDITION` and nothing is stored; on an empty collection it
succeeds and the Documents' place moves with it (`tests/collections.rs`:
`the_kind_isolated_and_encrypted_change_only_while_the_collection_is_empty`).
*Check:* a query with no collection named returns no isolated Documents
while one naming that collection returns them (`tests/query.rs`:
`find::an_isolated_collection_is_scanned_only_when_named_and_one_without_an_index_is_refused`);
isolated collections are listed with the others
(`tests/collections.rs`: `collections_are_listed_in_name_order_with_isolated_ones`).

**SCH-16. Encryption is declared, and it is a different question.** The
declaration may mark a collection `encrypted: true`.
Every Document file of that collection is then age v1 armor on disk
(ISO-8), whole-file (ISO-9), and I/O on it needs the Vault secret
(ISO-5). The key is independent of `isolated`: all four combinations are
valid, and neither implies the other. Three rules come with it, and each
one is a refusal rather than a silent accommodation. A `journal` with
the key is `INVALID_ARGUMENT` when the declaration is read, because an age file
cannot be appended to and a line is what a journal is made of (STO-2,
STO-3); `standard` and `singleton` are the kinds offered. No Index holds
anything from the collection (ISO-4), so `Query` and `Search` that name
it fail with `UNIMPLEMENTED` and never with an empty result. Setting or
clearing the key on a collection that already holds Document files is
`FAILED_PRECONDITION`, and those files are left untouched (ISO-11). A build
without the `encrypt` feature refuses the collection with `UNIMPLEMENTED`
instead of returning ciphertext as a Document. *Check:* an encrypted
journal declaration is `INVALID_ARGUMENT` (`collection::tests`:
`an_encrypted_journal_is_invalid_argument`); `Query` naming an encrypted
collection is `UNIMPLEMENTED`, not empty (`tests/query.rs`:
`find::an_encrypted_collection_named_is_unimplemented_and_skipped_when_not`);
the key added to a non-empty collection is `FAILED_PRECONDITION` and
stores nothing (`tests/collections.rs`:
`the_kind_isolated_and_encrypted_change_only_while_the_collection_is_empty`);
an encrypted collection of either kind round-trips (`tests/documents.rs`:
`the_table_holds_for_encrypted_collections`,
`any_bytes_round_trip_in_an_encrypted_collection`); in a build without the
feature it is `UNIMPLEMENTED` (`tests/documents.rs`:
`an_encrypted_collection_in_a_build_without_the_feature_is_unimplemented`).

---

## 7. On-disk keys (**core**)

YAML 1.2 or JSON. JSON uses this file's Protobuf JSON mapping
(camelCase aliases such as `segmentMaxBytes` / `segment_max_bytes` are
accepted on read, and `json_declarations_are_read_and_written_as_json`
keeps a JSON file JSON). A key Scheda does not know is read past and kept
when the file is rewritten; it is not an error (SCH-3). Kind literals
stay `standard`, `journal`, `singleton`.

### Collection declaration file

Each collection occupies `{vault}/{name}/` (MOD-8). Isolated Document
files are the SCH-15 exception (`{vault}/.isolated/{name}/`). Discover
collections by scanning the Vault root for directories that do not start
with `.` and that contain exactly one of `.collection.yaml`,
`.collection.yml`, or `.collection.json`. Skip `.git`, `.scheda`, and
`.isolated`. Other files in that directory are Documents or ignored
dotfiles (`03` MOD-8); they are not extractors (SCH-3). A directory
whose name is not the `name` in the file is `INVALID_ARGUMENT`. Two declaration
files in one collection directory are `INVALID_ARGUMENT`. Any leftover
`.schema.yaml` / `.schema.yml` / `.schema.json` in a non-dot directory
at the Vault root is `INVALID_ARGUMENT`, whether or not a `.collection.yaml` is
also present. One invalid file fails the load as a whole and names its
path (`collection::tests`: `load_reads_every_collection_directory_and_skips_the_rest`,
`load_fails_whole_on_one_invalid_file_and_names_its_path`).

`Vault.ListCollections` and `Collection.Describe` give each declaration as
the message `CollectionDeclaration`, the same as the file. The fixtures of
generation 2 load (`collection::tests`: `the_fixtures_of_generation_two_load`).

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | Collection name. Unique in the Vault. Same slug as one Document `id` component (library): Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`; max 255 bytes, no trailing `.` or whitespace, not a Windows device name. The on-disk directory is this name (MOD-8). |
| `schema` | yes | Nested `Schema`. Kind is the set oneof arm; exactly one arm, else `INVALID_ARGUMENT`. |
| `schema.standard` | one of three | Empty mapping. `standard` kind. File: `{name}/{id}`. |
| `schema.journal` | one of three | `journal` kind. Caps below. |
| `schema.singleton` | one of three | Mapping with `id` (MOD-7). Same slug as Document `id` (library); an extension, if any, is part of `id`. |
| `schema.journal.segmentMaxBytes` (`segment_max_bytes`) | `journal` only | Integer bytes. Omitted → **8 MiB**. If set, 1…**32 MiB** (SCH-14). `0` is invalid. |
| `schema.journal.segmentMaxLines` (`segment_max_lines`) | `journal` only | Optional integer line cap. If set, ≥ `1`. |
| `schema.journal.daily` | `journal` only | Empty mapping. Calendar rotation UTC, still size-first. Omitted → no calendar rotation. |
| `schema.isolated` | no | Boolean. Default `false`. `true` marks SCH-15: Documents under `{vault}/.isolated/{name}/`. Listed as isolated (ISO-3). |
| `schema.encrypted` | no | Boolean. Default `false`. `true` marks SCH-16: Document files are age v1 armor and I/O needs the Vault secret. Independent of `isolated`. `true` on `journal` is `INVALID_ARGUMENT`. Listed as encrypted (ISO-3). |
| `schema.excludeLexicalIndex` (`exclude_lexical_index`) | no | Boolean. Default `false`. `true` marks SCH-9: no Document of this collection is written to the Tantivy Index. A `Query` that names it is `UNIMPLEMENTED`. |
| `schema.excludeVectorIndex` (`exclude_vector_index`) | no | Boolean. Default `false`. `true` marks SCH-9: no Document of this collection is embedded or stored in the vector Index. |
| `schema.document` | — | Retired kind key (before generation 2; the kind is `standard`). It names no kind this Scheda knows: a file whose only arm is `document` is `INVALID_ARGUMENT` (`collection::tests`: `the_old_kind_key_names_no_kind_and_is_invalid_argument`). |
| `version`, `kind`, `declaration`, `fields`, `layout`, `body`, `id` | — | Retired keys of earlier generations. Unknown now: read past and kept, never acted on (SCH-3, SCH-13). |

`encrypted` on a `journal` is rejected. There is no field-declaration
table. Nested `fields` / `values` / `edge` keys are unknown keys (SCH-3).

---

## 8. What this document does not decide

Pretty-print (quotes, key order, YAML vs JSON). Bindings must round-trip
the information in SCH-2, SCH-9, and SCH-14–SCH-16 using the keys in §7.
SCH-15's `{vault}/.isolated/` tree, owner-only files, and Index rules
stay `09`.

FTS engine, vector store, `Index.Reindex`, and `Projector` column marks —
`06` / `13`.

Journal line envelope — `03` (MOD-16). `Get` / `Create` bytes — the proto
comments of `Collection`.
