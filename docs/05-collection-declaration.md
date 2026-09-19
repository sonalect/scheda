# 05. Collection declaration

Document 5 of the Scheda package. Version 0.51, 16 September 2026.

Normative rules for collection declarations as data in the Vault: where
they live, discovery, kind, journal caps, singleton `id`, isolation,
encryption, and collection-level Index exclusion. Terms are in `02`.
Kinds and identity are in `03`. Index columns and `Projector` are `06` /
`13`. Isolation trees stay `09`. Codes: `SCH`. Phase **core** unless
marked **search** or **later**.

An assertion without a check is not a requirement. A collection
declaration contains no executable code and no field list.

---

## 1. Declaration is files, not library types

**SCH-1. The declaration lives in the Vault.** Collection declarations
are UTF-8 YAML 1.2 (or JSON) files under git, next to the records:
`{name}/.collection.yaml` (also `.collection.yml` or
`.collection.json`). `name` is the collection name and the on-disk
directory (MOD-8). The directory name must equal the `name` in the file.
Changing a declaration is an ordinary git commit. Bindings write the
file through `create_collection` (`07` LIB-21), not by offering a raw
path. A person may still edit those files in an editor (STO-2). Library
source (`rust/scheda/src`) must not name a consumer's collections. Test
data may: the check does not search it, so a fixture shaped like a
consumer cannot turn SCH-1 red. A leftover `{name}/.schema.yaml` (also
`.schema.yml` / `.schema.json`) is `invalid`, not ignored. *Check:* a
second consumer adds collections without rebuilding Scheda; grep over
`rust/scheda/src` does not contain those collection names. *Check:* a
directory with `.schema.yaml` fails `open` / `check`.

`info` (`08`) is this data serialized, not a guess from walking Record
files. It lists write-mode and perimeter marks, not Record fields.

**SCH-2. One declaration per collection.** Each collection has: name
(method argument / directory), kind (`document` | `journal` |
`singleton`), `version` (SCH-13), and the keys in §7 that apply to that
kind (journal caps; fixed Record `id` for `singleton`; `isolated`;
`encrypted`; `exclude_lexical_index`; `exclude_vector_index`). There is
no `fields` key. There is no layout template. Record file paths for
generation 1 are fixed (MOD-8). Kind does not change in place (MOD-2).
Journal caps may change inside generation 1. *Check:* a collection
missing `kind` or `version` is rejected on `open` / `check`; a `fields`
key or a `layout` key is `invalid`. *Check:* two of `.collection.yaml`
/ `.yml` / `.json` in one collection directory is `invalid`.

**SCH-3. No code in the declaration.** No scripts, templates that call
out to a language runtime, or “on write” hooks. Unknown keys are
rejected, so an executable blob cannot hide as an extra field. Scheda
does not scan Record bytes for wiki-links or any other consumer syntax.
*Check:* a declaration that is valid YAML but contains an unknown key is
`invalid`.

---

## 2. Retired field Schema

Field names, types, `required` / `nullable` / `default`, `exclude_find`,
body marks, and edges are not Scheda's. They are not keys on the
declaration. Column names for `find` / `search` live in the caller's
`Projector` (`06` / `13`). `add` does not type-check Record bytes.

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
the Record (`06`).

**SCH-11 (retired).** Edges as Schema fields. Scheda does not store or
validate graph links.

**SCH-13. Collection generation.** `version` on the declaration is the
collection generation: the declaration file **and** the on-disk Record
layout of that collection. Required integer. No default: omit, a
non-integer, `0`, or a generation the library does not implement →
`invalid`. This plan implements **1** only. `create_collection` writes
`1`; the proto argument does not carry `version`. Field-schema `read_as`
/ migrate stays retired. Kind still does not change in place (MOD-2).
Flipping `encrypted` on a non-empty collection is already `invalid`
(ISO-11). Journal caps may change inside generation 1 (SCH-14). *Check:*
missing `version` → `invalid`. *Check:* `version: 2` → `invalid` until
the library implements 2. *Check:* `create_collection` writes
`version: 1`.

---

## 3. Writes vs Index

**SCH-12. Write stores bytes; `get` returns them.** `add` / `replace`
accept opaque UTF-8 (MOD-11). They do not call `Projector.project` to
accept or reject the write. An older file is left on disk. `get` by `id`
returns the file bytes if the path exists (journal: the envelope
payload). `check` does not type-check Record contents (`03` MOD-13). A
Record that `project` rejects or omits stays out of the lexical and
vector indexes; that is a `Projector` failure (`06`), not a Schema type
check. *Check:* bytes that a later `Projector` would omit still return
from `get`; `add` of those bytes succeeds.

---

## 4. Index exclusion

**SCH-9. A collection may stay out of either Index.**
`exclude_lexical_index: true` keeps every Record of that collection out
of the Tantivy Index (`find`, filters, BM25). `find` that names it is
`unsupported`, not an empty hit list. The journal id map is a separate
file and is still written, so keyed `get` does not need Tantivy.
`exclude_vector_index: true` skips the embedder on `add` / `replace` and
keeps Records out of the vector Index; `search` then uses the lexical
leg only. Either flag, or both. `search` is `unsupported` only when the
collection has no remaining search leg (both flags, or encrypted —
SCH-16). Both flags are redundant on `encrypted` and allowed. A
`journal` may set either flag. Default for each is `false`. Per-field
FTS / vector marks are not declaration keys; they belong on the
`Projector` (`06` / `13`). *Check (**search**):* `find` that names a
lexically excluded collection is `unsupported`; `search` on a collection
with only `exclude_vector_index` still returns lexical hits and does not
call the embedder on `add`.

---

## 5. Journal segments (numbers)

**SCH-14. Size cap is required for `journal`.** The declaration sets
`segment_max_bytes`. If omitted, the default is **8 MiB** (8 × 1024 ×
1024). The implementation rejects a cap greater than **32 MiB** (still
under typical host warn/reject limits of ~50 / ~100 MiB; GIT-6).
Optional: `segment_max_lines`, calendar rotation (`daily` UTC). Rotation
is still size-first: a cap hit closes the segment even mid-day. Caps may
change on a later declaration write; no `version` key is involved.
*Check:* default Vault without the key rotates at 8 MiB; a declaration
with `48 MiB` is rejected; after the cap, the next `add` does not grow
the closed file (MOD-10).

Closed segments are never rewritten. Sequence files are eight lowercase
hex digits, then the clone's writer identity (`00000001-alice.jsonl`),
so name order matches numeric order and two clones cannot create the
same path. With `daily`, the open name is `YYYY-MM-DD-{writer}.jsonl`,
then `YYYY-MM-DD-00000001-{writer}.jsonl` if that day already closed on
size. A writer never appends to a segment named by another clone, nor to
an older writerless name (`00000001.jsonl`, `YYYY-MM-DD.jsonl`): those
files are still read. Rotation caps are evaluated per writer.

---

## 6. Isolated and encrypted collections (**later**)

**SCH-15. Isolation is declared (**later**).** The declaration may mark
a collection `isolated: true`. Record files then live at
`{vault}/.isolated/{name}/` (ISO-2): a gitignored nested git that the
main Vault's `push` / `pull` / `sync` never touch (ISO-6). The
declaration file stays `{vault}/{name}/.collection.yaml` in the main
tree, which also makes a collection `name` unique across the Vault,
isolated and plain alike. `find` / `search` that name no collection
return nothing of theirs; naming one is an ordinary query, because
identity (and Projector columns, when a Projector is present) sit in the
one Vault Index, which is owner-only for that reason (ISO-4). `info`
lists the collection as isolated; there is no field catalog to omit
(ISO-3). *Check:* fixture with gitignored `.isolated/`; a query with no
collection named returns no isolated Records while one naming that
collection returns them; `open` does not require `.isolated/` to exist.

**SCH-16. Encryption is declared, and it is a different question
(**later**).** The declaration may mark a collection `encrypted: true`.
Every Record file of that collection is then age v1 armor on disk
(ISO-8), whole-file (ISO-9), and I/O on it needs the handle secret
(ISO-5). The key is independent of `isolated`: all four combinations are
valid, and neither implies the other. Three rules come with it, and each
one is a refusal rather than a silent accommodation. A `journal` with
the key is `invalid` when the declaration is read, because an age file
cannot be appended to and a line is what a journal is made of (STO-2,
STO-3); `document` and `singleton` are the kinds offered. No Index holds
anything from the collection (ISO-4), so `find` and `search` that name
it fail with `unsupported` and never with an empty result. Setting or
clearing the key on a collection that already holds Record files is
`invalid`, and those files are left untouched (ISO-11). A build without
the `encrypt` feature refuses the collection by code instead of
returning ciphertext as a Record (LIB-15). *Check (**later**):* an
encrypted non-isolated collection and a plain isolated one both
round-trip; an encrypted `journal` declaration is `invalid`; `find`
naming an encrypted collection is `unsupported`, not empty; the key
added to a non-empty collection is `invalid` and its files are
byte-identical afterwards.

---

## 7. On-disk keys (**core**)

UTF-8 YAML 1.2 or JSON. JSON uses this file's Protobuf JSON mapping
(camelCase aliases such as `segmentMaxBytes` / `segment_max_bytes` are
accepted on read). Unknown keys on the file are rejected, so an
executable blob cannot hide as an extra field (SCH-3). Kind literals
stay `document`, `journal`, `singleton`.

### Collection declaration file

Each collection occupies `{vault}/{name}/` (MOD-8). Isolated Record
files are the SCH-15 exception (`{vault}/.isolated/{name}/`). Discover
collections by scanning the Vault root for directories that do not start
with `.` and that contain exactly one of `.collection.yaml`,
`.collection.yml`, or `.collection.json`. Skip `.git`, `.scheda`, and
`.isolated`. Other files in that directory are Records or ignored
dotfiles (`03` MOD-8); they are not extractors (SCH-3). A directory
whose name is not the `name` in the file is `invalid`. Two declaration
files in one collection directory are `invalid`. Any leftover
`.schema.yaml` / `.schema.yml` / `.schema.json` in a non-dot directory
at the Vault root is `invalid`, whether or not a `.collection.yaml` is
also present.

`info` (`08`) is this data serialized, not inferred from Record files.
The on-disk file and each `VaultInfo.collections` item are
`api.collection.v1.CollectionInfo`.

| Key | Required | Meaning |
|---|---|---|
| `version` | yes | Collection generation (SCH-13). Integer. This plan: **1** only. Field number 1. No default. |
| `name` | yes | Collection name. Unique in the Vault. Same slug as one Record `id` component (library): Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`; max 255 bytes, no trailing `.` or whitespace, not a Windows device name. The on-disk directory is this name (MOD-8). |
| `schema` | yes | Nested `Schema` (same message as `create_collection`). Kind is the set oneof arm. |
| `schema.document` | one of three | Empty mapping. `document` kind. File: `{name}/{id}`. |
| `schema.journal` | one of three | `journal` kind. Caps below. |
| `schema.singleton` | one of three | Mapping with `id` (MOD-7). Same slug as Record `id` (library); an extension, if any, is part of `id`. |
| `schema.journal.segmentMaxBytes` (`segment_max_bytes`) | `journal` only | Integer bytes. Omitted → **8 MiB**. If set, 1…**32 MiB** (SCH-14). `0` is invalid. |
| `schema.journal.segmentMaxLines` (`segment_max_lines`) | `journal` only | Optional integer line cap. If set, ≥ `1`. |
| `schema.journal.daily` | `journal` only | Empty mapping. Calendar rotation UTC, still size-first. Omitted → no calendar rotation. |
| `schema.isolated` | no | Boolean. Default `false`. `true` marks SCH-15: Records under `{vault}/.isolated/{name}/`. `info` lists the collection as isolated (ISO-3). |
| `schema.encrypted` | no | Boolean. Default `false`. `true` marks SCH-16: Record files are age v1 armor and I/O needs the handle secret. Independent of `isolated`. `true` on `journal` is `invalid`. `info` lists the collection as encrypted (ISO-3). |
| `schema.excludeLexicalIndex` (`exclude_lexical_index`) | no | Boolean. Default `false`. `true` marks SCH-9: no Record of this collection is written to the Tantivy Index. `find` that names it is `unsupported`. |
| `schema.excludeVectorIndex` (`exclude_vector_index`) | no | Boolean. Default `false`. `true` marks SCH-9: no Record of this collection is embedded or stored in the vector Index. |
| `kind` | — | Retired as a top-level string. A leftover key is `invalid`. Kind is the `schema` oneof. |
| `declaration` | — | Retired CollectionInfo key. A leftover key is `invalid`. |
| `fields` | — | Retired. A leftover key is `invalid` (SCH-2). |
| `layout` / `body` | — | Retired. A leftover key is `invalid`. |
| `id` | — | Removed as a collection-level identity pattern. Record `id` rules are `03`. |

Journal-only keys on `document` / `singleton`, `singleton` on another
kind, and `encrypted` on `journal`, are rejected. Two files with the
same `name` are rejected. Two kind arms on one `schema` are
rejected.

There is no field-declaration table. Nested `fields` / `values` / `edge`
keys are unknown keys (SCH-3).

---

## 8. What this document does not decide

Pretty-print (quotes, key order, YAML vs JSON). Bindings must round-trip
the information in SCH-2, SCH-9, and SCH-14–SCH-16 using the keys in §7.
SCH-15's `{vault}/.isolated/` tree, owner-only files, and Index rules
stay `09`.

FTS engine, vector store, `reindex`, and `Projector` column marks —
`06` / `13`.

Journal line envelope — `03` (MOD-16). `get` / `add` bytes — `07`.
