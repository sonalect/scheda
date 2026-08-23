# 05. Schema

Document 5 of the Scheda package. Version 0.5, 23 August 2026.

Normative rules for Schema as data in the Vault: where it lives, what a collection declaration contains, validation, evolution, index marks, edges, and the journal segment cap. Terms are in `02`. Kinds and identity are in `03`. Index behaviour is in `06`. Codes: `SCH`. Phase **core** unless marked **search**.

An assertion without a check is not a requirement. Schema contains no executable code.

---

## 1. Schema is files, not library types

**SCH-1. Schema lives in the Vault.** Collection declarations are UTF-8 YAML 1.2 (or JSON) files under git, next to the records (directory `schema/` unless Schema itself relocates that path in a root file `schema.yaml`). Changing Schema is an ordinary git commit. Implementation source must not name a consumer's collections. *Check:* a second consumer adds collections without rebuilding Scheda; grep over implementation source does not contain those collection names.

The catalog (`08`) is this data serialized, not a guess from walking files.

**SCH-2. One declaration per collection.** Each collection has: name, kind (`document` | `journal` | `singleton`), `schema_version` (integer, starting at `1`), layout template (MOD-8), `id` pattern (MOD-6), field list, and kind-specific options (segment cap for `journal`; fixed `id` for `singleton`). *Check:* a collection missing kind or `schema_version` is rejected on `open`/`check`.

**SCH-3. No code in Schema.** No scripts, templates that call out to a language runtime, or “on write” hooks inside Schema files. Extracting edges from Markdown `[[wiki-links]]` is allowed only if Schema names a built-in extractor the implementation ships; silence is not an extractor. *Check:* a Schema file that is valid YAML but contains an executable blob is rejected; unmarked Markdown is not scanned for edges.

---

## 2. Fields

**SCH-4. Each field is declared.** A field has a name, a type, and flags. Types in **core**:

| Type | On disk | Notes |
|---|---|---|
| `string` | JSON/YAML string | |
| `integer` | JSON/YAML number with no fractional part | `1.5` rejected |
| `number` | finite JSON/YAML number | JSON does not encode NaN/Infinity; YAML `.nan` / `.inf` rejected |
| `boolean` | `true` / `false` | |
| `string_list` | array of strings | empty `[]` is a value, not missing |
| `object` | map | keys Schema lists, or open with `string` values |
| `object_list` | array of maps | empty `[]` is a value, not missing; each item's fields are declared with the same type set (nesting allowed) |

Unknown types are rejected. Flags `required` / `optional` / `nullable` apply to nested fields of `object` and `object_list` items the same way. *Check:* insert with a field not in Schema → error; insert with a known field of the wrong type → error; `object_list` of two maps round-trips; `number` `0.5` accepted; the same value on an `integer` field is rejected.

**SCH-5. Required, optional, nullable.** Required fields must be present on write. Optional fields may be absent. Independently, Schema may mark a field `nullable` (default false).

| | not nullable | `nullable` |
|---|---|---|
| required | key present, value not `null` | key present, value may be `null` |
| optional | key may be absent; if present, not `null` | absent, `null`, or a value — three states |

A default is applied only if Schema states one; otherwise MOD-12 (missing is missing) and MOD-18 (`null` stays `null`). *Check:* omit a required field → write error; omit an optional non-nullable field → read-back has no such key; required nullable with `null` round-trips; optional non-nullable with `null` → write error.

**SCH-6. Identity field.** Default name `id`, overridable in the collection declaration. It must match MOD-13. *Check:* header/object `id` ≠ path-derived `id` → write and `check` fail.

**SCH-7. Body marks (`document` only).** Schema says whether the Markdown body exists, and whether it participates in lexical search, vector search, both, or neither. Default: body allowed, not indexed. *Check:* unmarked body text is not found by `search` once **search** exists; **core** still stores the body.

**SCH-8. Filter keys.** Schema marks which fields may appear in `find` (equality, set, existence, and equality to `null` when the field is `nullable`). Unmarked fields are not filter keys. Path-prefix filter on `document` layout slots is always allowed when the slot is in the template. Existence means the key is present (`null` counts). Equality to `null` matches stored `null`, not an absent key. *Check:* `find` on an unmarked field → error; `find` `f = null` does not return a Record that omitted `f`. **Core** for equality/`find`; ranking is `06`.

---

## 3. Index marks and embeddings

**SCH-9. Search is opt-in per field (and body).** For each field and for the body: lexical (`fts`), vector, both, or neither. Unmarked text does not enter the Index. *Check (**search**):* two fields, one marked; word and meaning queries hit only the marked field.

**SCH-10. Logical embedding slot.** If vector is on, Schema names a `field_id` (logical slot). Model identity (`model_id`, dimension, prompt variant, quantization) lives in the Index, not in the Record. Mismatch with the caller's embedder → rebuild vectors or error, not search against the wrong matrix. Lexical search still works. *Check (**search**):* change of `model_id` in the Index header invalidates the vector table; `find` / FTS still succeed.

---

## 4. Edges

**SCH-11. An edge is a field.** Schema names the target collection, multiplicity (`one` | `many`), and whether a dangling `id` is rejected on write (`strict`) or allowed (`lax`). Traversal and inverse lookup are `06`. *Check:* `strict` insert with a missing target → error; `lax` insert succeeds; `find_one` of the source succeeds; a one-step walk does not return the missing target.

Values are Record `id`s, not URLs and not wiki labels.

---

## 5. Validation vs old files

**SCH-12. Write validates; read does not rewrite.** `insert_one` / `replace_one` reject a Record that fails the collection's current Schema. An older file that fails a tightened Schema is left on disk. `check` reports it. It does not enter the search Index until it validates (or Schema makes the new field optional). `find_one` by `id` still returns the file bytes if the path exists, with a validity flag specified in `07`. *Check:* add a required field in Schema `2`; old file still `find_one`s; `search` omits it; new insert without the field fails.

**SCH-13. Forward-compatible evolution.** Adding an optional field does not invalidate files at `schema_version` N when Schema becomes N+1. Changing a field's type, removing a required field, or changing kind requires a bump of `schema_version` and a rule in Schema (`read_as` / migrate-out-of-band). Scheda does not guess. Writes must use the current `schema_version`. *Check:* files at version `1` read under Schema `2` that only added an optional field; a string where Schema now wants a list is rejected on write.

---

## 6. Journal segments (numbers)

**SCH-14. Size cap is required for `journal`.** Schema sets `segment_max_bytes`. If omitted, the default is **8 MiB** (8 × 1024 × 1024). The implementation rejects a cap greater than **32 MiB** (still under typical host warn/reject limits of ~50 / ~100 MiB; GIT-6). Optional: `segment_max_lines`, `segment_rotate` calendar (`daily` UTC). Rotation is still size-first: a cap hit closes the segment even mid-day. *Check:* default Vault without the key rotates at 8 MiB; Schema with `48 MiB` is rejected; after the cap, the next `insert_one` does not grow the closed file (MOD-10).

Closed segments are never rewritten. The open segment's name is Schema's pattern (sequence `0001.jsonl`, or `YYYY-MM-DD.jsonl` plus a sequence suffix if the day already closed on size).

---

## 7. Isolated collections (**later**)

**SCH-15. Isolation is declared (**later**).** Schema may mark a collection `isolated: true` and name a **git** root outside the main Vault working tree. That root has no Remote (`09`). Encryption of Record files there is `encrypt` on `init` (`09`). Search over the main Vault does not read it. Sealed fields must not be copied into the main Index. *Check (**later**):* fixture with two roots; `search`/`find` on the main Vault returns no isolated Records and does not require that root to exist.

---

## 8. On-disk keys (**core**)

UTF-8 YAML 1.2 or JSON. JSON uses the same keys. Unknown keys on a declaration
are rejected, so an executable blob cannot hide as an extra field (SCH-3).
Keys are snake_case. Values that `03`–`05` already name as literals stay those
literals (`document`, `journal`, `singleton`, field types in SCH-4).

### Root `schema.yaml`

Optional file at the Vault root. If absent, collection files live in `schema/`.
If present, it may contain only:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `schema_dir` | string | `schema` | Directory of collection files, relative to the Vault root. Must stay inside the Vault. |

### Collection files

Each `*.yaml`, `*.yml`, or `*.json` in `schema_dir` (not recursive) is one
collection (SCH-2). Other files (for example Markdown) are ignored and are not
extractors (SCH-3). The file stem is not the collection name.

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | Collection name. Unique in the Vault. Non-empty. |
| `kind` | yes | `document` \| `journal` \| `singleton` |
| `schema_version` | yes | Integer ≥ `1` |
| `layout` | yes | One template string, or a list of them (MOD-8). Non-empty. Each `document` template contains `{id}`. |
| `id` | no | Mapping: `field` (default `id`), `pattern` (regex; omitted means any non-empty `id`). |
| `fields` | yes | List of field declarations (may be empty). |
| `segment_max_bytes` | `journal` only | Integer bytes. Default **8 MiB**. Greater than **32 MiB** is rejected (SCH-14). |
| `segment_max_lines` | `journal` only | Optional integer line cap. |
| `segment_rotate` | `journal` only | Optional `daily` (UTC). |
| `singleton_id` | `singleton` only | The one Record `id` (MOD-7). |

Journal-only keys on `document` / `singleton`, and `singleton_id` on another
kind, are rejected. Two files with the same `name` are rejected. Two `kind`
values on one collection are rejected (one `kind` string).

### Field declarations

Each item of `fields`:

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | Field name. Non-empty. |
| `type` | yes | `string` \| `integer` \| `number` \| `boolean` \| `string_list` \| `object` \| `object_list` |
| `required` | no | Boolean. Default `false` (optional). |
| `nullable` | no | Boolean. Default `false` (SCH-5). |
| `filter` | no | Boolean. Default `false`. `true` marks a `find` key (SCH-8). |
| `default` | no | Applied only when present (MOD-12). |
| `fields` | `object` / `object_list` | Nested field list. Omitted on `object` / `object_list` means an open map of `string` values (SCH-4). |

`fields` on a scalar type is rejected. Nested items use this same table.
Index marks (SCH-9–11) and `isolated` (SCH-15) are not **core** keys; they are
rejected until those phases.

---

## 9. What this document does not decide

Pretty-print (quotes, key order, YAML vs JSON). Bindings must round-trip the
information in SCH-2–SCH-14 using the keys in §8. SCH-15 is **later**.

FTS engine, vector store, and `reindex` — `06`.

Canonical JSON of journal lines — `07`.
