# 05. Schema

Document 5 of the Scheda package. Version 0.33, 27 August 2026.

Normative rules for Schema as data in the Vault: where it lives, what a collection declaration contains, validation, evolution, index marks, edges, and the journal segment cap. Terms are in `02`. Kinds and identity are in `03`. Index marks here; `find` / `search` mechanics are `13`. Index disposal and `get` are `06`. Codes: `SCH`. Phase **core** unless marked **search**.

An assertion without a check is not a requirement. Schema contains no executable code.

---

## 1. Schema is files, not library types

**SCH-1. Schema lives in the Vault.** Collection declarations are UTF-8 YAML 1.2 (or JSON) files under git, next to the records: `{name}/.schema.yaml` (also `.schema.yml` or `.schema.json`). `name` is the collection name and the on-disk directory (MOD-8). The directory name must equal the `name` in the file. Changing Schema is an ordinary git commit. Bindings write a collection file through `create_collection` (`07` LIB-21), not by offering a raw path. A person may still edit those files in an editor (STO-2). Library source (`rust/scheda/src`) must not name a consumer's collections. Test data may: the check does not search it, so a fixture shaped like a consumer cannot turn SCH-1 red. *Check:* a second consumer adds collections without rebuilding Scheda; grep over `rust/scheda/src` does not contain those collection names.

`info` (`08`) is this data serialized, not a guess from walking files.

**SCH-2. One declaration per collection.** Each collection has: name (method argument / directory), kind (`document` | `journal` | `singleton`), `version` (integer ≥ `1`), field list, and kind-specific options (segment cap for `journal`; fixed Record `id` for `singleton`). There is no layout template. Record file paths are fixed (MOD-8). *Check:* a collection missing kind or `version` is rejected on `open`/`check`; a `layout` key is `invalid`.

**SCH-3. No code in Schema.** No scripts, templates that call out to a language runtime, or “on write” hooks inside Schema files. Extracting edges from Markdown `[[wiki-links]]` is allowed only if Schema names a built-in extractor the implementation ships; silence is not an extractor. *Check:* a Schema file that is valid YAML but contains an executable blob is rejected; unmarked Markdown is not scanned for edges.

---

## 2. Fields

**SCH-4. Each field is declared.** A field has a name, common flags, and exactly one spec: a SCH-4 type or an SCH-11 edge. Types in **core**:

| Type | On disk | Notes |
|---|---|---|
| `string` | JSON/YAML string | |
| `integer` | JSON/YAML number with no fractional part and magnitude at most 2^53 | `1.5` rejected; a value `google.protobuf.Struct` `number_value` (IEEE 754 binary64) cannot hold exactly is `invalid` (`07` LIB-18) |
| `number` | finite JSON/YAML number | JSON does not encode NaN/Infinity; YAML `.nan` / `.inf` rejected |
| `boolean` | `true` / `false` | |
| `string_list` | array of strings | empty `[]` is a value, not missing |
| `object` | map | keys Schema lists, or open with `string` values |
| `object_list` | array of maps | empty `[]` is a value, not missing; each item's fields are declared with the same type set (nesting allowed) |

Unknown types are rejected. Flags `required` / `optional` / `nullable` apply to nested fields of `object` and `object_list` items the same way. *Check:* insert with a field not in Schema → error; insert with a known field of the wrong type → error; `object_list` of two maps round-trips; `number` `0.5` accepted; the same value on an `integer` field is rejected; an `integer` whose magnitude exceeds 2^53 is rejected.

**SCH-5. Required, optional, nullable.** Required fields must be present on write. Optional fields may be absent. Independently, Schema may mark a field `nullable` (default false).

| | not nullable | `nullable` |
|---|---|---|
| required | key present, value not `null` | key present, value may be `null` |
| optional | key may be absent; if present, not `null` | absent, `null`, or a value — three states |

A default is applied only if Schema states one; otherwise MOD-12 (missing is missing) and MOD-18 (`null` stays `null`). *Check:* omit a required field → write error; omit an optional non-nullable field → read-back has no such key; required nullable with `null` round-trips; optional non-nullable with `null` → write error.

**SCH-6. Identity field.** The Record identity key is always `id` (MOD-13). Schema must not declare a field named `id` or `body` (`body` is the Markdown body in `find`, SCH-7). There is no Schema key that renames `id` or that adds a pattern on top of the library slug (same slug as collection `name`: Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`; max 255 bytes, no trailing `.` or whitespace, not a Windows device name). For `document`, header `id` must match the filename stem (`{id}.md`). For `singleton`, header `id` must match `Schema.singleton.id` (the file is always `record.yaml`). *Check:* header/object `id` ≠ that identity → write and `check` fail. *Check:* a Schema field named `id` or `body` is `invalid`. *Check:* `id` `a/b` is `invalid` on `add`.

**SCH-7. Body marks (`document` only).** Schema says whether a Markdown body is forbidden (`no_body`) and whether it is excluded from the lexical Index and from vector search (`text.exclude_fts` / `text.exclude_vector`). Default: body allowed and in the lexical Index (`find` field `body` and BM25) and in vectors. *Check:* `no_body` with a non-empty body → write error; `exclude_fts` body text is not a `find` key and is not found by lexical `search`; **core** still stores the body.

**SCH-8. Filter keys.** `find` takes `api.v1.Filter` (IDX-10): a Tantivy 0.26 `Query` tree (`find.proto`), then files for those `id`s. Every declared field may appear in a leaf (`term`, `term_set`, `phrase`, `range`, `exists`, …) unless Schema sets `exclude_find`. Record identity `id` is always allowed; it is not a Schema field and cannot be `exclude_find`. Markdown `body` is a find key on `document` when the body is allowed and not `exclude_fts` (SCH-7); it is not a Schema field. `term` on `id` with JSON `null` is `invalid`. There is no path-prefix query: the caller never supplies a Record path. Existence (`exists`) means a non-null indexed value (`ExistsQuery`). Equality to JSON `null` is a `term` whose value is `null` (stored `null`, not absent). Combine with `boolean` (`must` / `should` / `must_not`). *Check:* `find` on an `exclude_find` field → error; `find` on an unmarked field round-trips; `term` `f = null` does not return a Record that omitted `f`. *Check:* `term` `id = "alpha"` returns that Record when it is in the Index. *Check:* `regex` / `phrase` on `body` hits prose that is not in a Schema field; `exclude_fts` body → `invalid`. **Core** for `find`; ranking is `06`.

---

## 3. Index marks and embeddings

**SCH-9. Search is opt-out per field (and body).** `string`, `string_list`, edges, and the body enter lexical (`fts`) and vector search unless Schema sets `exclude_fts` / `exclude_vector` on that arm (`Text`, `Edge`, or `Body`). Other types (`integer`, `number`, `boolean`, `object`, `object_list`) are never FTS or vector. Substring match over every field is not `search`. *Check (**search**):* two string fields, one with `exclude_fts`; word queries hit only the other field.

**SCH-10. Model identity is in the Index.** The vector column is the field `name` (the body uses `body`). `model_id`, dimension, prompt variant, and quantization live in the Index, not on the Record. Mismatch with the caller's embedder → rebuild vectors or error, not search against the wrong matrix. Lexical search still works. *Check (**search**):* change of `model_id` in the Index header invalidates the vector table; `find` / FTS still succeed.

---

## 4. Edges

**SCH-11. An edge is a field.** The field spec is `edge`, not a SCH-4 type. Values are Record `id`s of the named collection: `one` stores a `string`, `many` a `string_list`. Not URLs or wiki labels. `strict` rejects a write whose target `id` is missing; default is lax. There is no `walk` method; the caller reads the field. *Check:* `strict` insert with a missing target → error; lax insert succeeds; `get` of the source succeeds.

---

## 5. Validation vs old files

**SCH-12. Write validates; read does not rewrite.** `add` / `replace` reject a Record that fails the collection's current Schema. An older file that fails a tightened Schema is left on disk. `check` reports it. It does not enter the search Index until it validates (or Schema makes the new field optional). `get` by `id` still returns the file bytes if the path exists, with a validity flag specified in `07`. *Check:* add a required field in Schema `2`; old file still returns from `get`; `search` omits it; new insert without the field fails.

**SCH-13. Forward-compatible evolution.** Adding an optional field does not invalidate files at `version` N when Schema becomes N+1. Changing a field's type, removing a required field, or changing kind requires a bump of `version` and a rule in Schema (`read_as` / migrate-out-of-band). Scheda does not guess. Writes must use the current `version`. *Check:* files at version `1` read under Schema `2` that only added an optional field; a string where Schema now wants a list is rejected on write.

---

## 6. Journal segments (numbers)

**SCH-14. Size cap is required for `journal`.** Schema sets `segment_max_bytes`. If omitted, the default is **8 MiB** (8 × 1024 × 1024). The implementation rejects a cap greater than **32 MiB** (still under typical host warn/reject limits of ~50 / ~100 MiB; GIT-6). Optional: `segment_max_lines`, `segment_rotate` calendar (`daily` UTC). Rotation is still size-first: a cap hit closes the segment even mid-day. *Check:* default Vault without the key rotates at 8 MiB; Schema with `48 MiB` is rejected; after the cap, the next `add` does not grow the closed file (MOD-10).

Closed segments are never rewritten. Sequence files are eight lowercase hex
digits (`00000001.jsonl` … `ffffffff.jsonl`) so name order matches numeric
order. With `segment_rotate: daily`, the open name is `YYYY-MM-DD.jsonl`, then
`YYYY-MM-DD-00000002.jsonl` if that day already closed on size.

---

## 7. Isolated collections (**later**)

**SCH-15. Isolation is declared (**later**).** Schema may mark a collection `isolated: true`. Record files then live at `{vault}/.isolated/{name}/` (ISO-2): a gitignored nested git with no Remote. The Schema file stays `{vault}/{name}/.schema.yaml` in the main tree. Encryption of isolated Record files is `OpenOptions.encrypt` when `.isolated/` is first created (`09`). `find` / `search` on non-isolated collections do not read it. Sealed fields must not be copied into the main Index. *Check (**later**):* fixture with gitignored `.isolated/`; `search`/`find` on a non-isolated collection return no isolated Records; `open` does not require `.isolated/` to exist.

---

## 8. On-disk keys (**core**)

UTF-8 YAML 1.2 or JSON. JSON uses the same keys. Unknown keys on a declaration
are rejected, so an executable blob cannot hide as an extra field (SCH-3).
Keys are snake_case. Values that `03`–`05` already name as literals stay those
literals (`document`, `journal`, `singleton`, field types in SCH-4).

### Collection schema file

Each collection occupies `{vault}/{name}/` (MOD-8). Isolated Record files are the SCH-15 exception (`{vault}/.isolated/{name}/`). Discover collections by scanning the Vault root for directories that do not start with `.` and that contain exactly one of `.schema.yaml`, `.schema.yml`, or `.schema.json`. Skip `.git`, `.scheda`, and `.isolated`. Other files in that directory (for example Markdown) are ignored and are not extractors (SCH-3). A directory whose name is not the `name` in the file is `invalid`. Two schema files in one collection directory are `invalid`.

`info` (`08`) is this data serialized, not inferred from Record files.

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | Collection name. Unique in the Vault. Same slug as Record `id` (library): Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`; max 255 bytes, no trailing `.` or whitespace, not a Windows device name. The on-disk directory is this name (MOD-8). |
| `kind` | yes | `document` \| `journal` \| `singleton` |
| `version` | yes | Integer ≥ `1` |
| `id` | — | Removed. Identity is always Record `id`. No `id_pattern`. The library slug is the only `id` constraint (SCH-6). |
| `fields` | yes | List of field declarations (may be empty). |
| `segment_max_bytes` | `journal` only | Integer bytes. Omitted → **8 MiB**. If set, 1…**32 MiB** (SCH-14). `0` is invalid. |
| `segment_max_lines` | `journal` only | Optional integer line cap. If set, ≥ `1`. |
| `daily` | `journal` only | Empty mapping. Calendar rotation UTC, still size-first. Omitted → no calendar rotation. |
| `singleton` | `singleton` only | Mapping with `id` (MOD-7). Same slug as Record `id` (library). |
| `body` / `document` | `document` only | Mapping (SCH-7). Omitted/empty: body allowed and indexed. |
| `isolated` | no | Boolean. Default `false`. `true` marks SCH-15: Records under `{vault}/.isolated/{name}/`. `info` lists the collection as isolated and omits `schema.fields` (ISO-3). |

Journal-only keys on `document` / `singleton`, `singleton_id` on another
kind, and `body` on `journal` / `singleton`, are rejected. Two files with
the same `name` are rejected. Two `kind` values on one collection are
rejected (one `kind` string).

`body` mapping:

| Key | Required | Meaning |
|---|---|---|
| `allowed` | no | Boolean. Default `true`. `false` rejects a non-empty body on write. |
| `exclude_fts` | no | Boolean. Default `false`. `true` excludes the body from the lexical Index: `find` field `body` and BM25 (SCH-7, SCH-9). |
| `exclude_vector` | no | Boolean. Default `false`. `true` excludes the body from vector search (SCH-9). |

### Field declarations

Each item of `fields`:

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | Field name. Non-empty. Not `id` (SCH-6) or `body` (SCH-7). |
| `required` | no | Boolean. Default `false` (optional). |
| `nullable` | no | Boolean. Default `false` (SCH-5). |
| `exclude_find` | no | Boolean. Default `false`. `true` excludes the field from `find` (SCH-8). |
| `default` | no | Applied only when present (MOD-12). |
| *spec* | yes | Exactly one of the following keys (Protobuf `oneof`). |

Spec (exactly one):

| Key | Meaning |
|---|---|
| `string` | JSON/YAML string. Mapping: `exclude_fts`, `exclude_vector` (SCH-9). Empty mapping is valid. |
| `integer` | JSON/YAML integer. Empty mapping. |
| `number` | Finite JSON/YAML number. Empty mapping. |
| `boolean` | `true` / `false`. Empty mapping. |
| `string_list` | Array of strings. Same search mapping as `string`. |
| `object` | Map. Nested `fields` (this same table). Omitted / empty `fields`: open map of `string` values. |
| `object_list` | Array of maps. Same nested `fields` as `object`. |
| `edge` | SCH-11. Mapping: `collection` (required), `one` **xor** `many` (empty mapping), optional `dangling` (`lax` \| `strict`), optional `exclude_fts` / `exclude_vector`. |

Nested `fields` exist only under `object` / `object_list`. They cannot appear on an edge or a scalar.

---

## 9. What this document does not decide

Pretty-print (quotes, key order, YAML vs JSON). Bindings must round-trip the
information in SCH-2–SCH-14 using the keys in §8. SCH-15's `{vault}/.isolated/`
tree, owner-only files, and Index rules stay `09`.

FTS engine, vector store, and `reindex` — `06`.

Canonical JSON of journal lines — `07`.
