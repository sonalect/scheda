# 03. Model

Document 3 of the Scheda package. Version 0.15, 25 August 2026.

Normative data model: collection kinds, Record identity, paths, fields, and body. Terms are in `02`. File encoding, git, and locks are in `04`. The Schema language is in `05`. Checkable requirements use codes `MOD`. Phase **core** unless marked otherwise.

An assertion without a check is not a requirement.

---

## 1. Collections are declared, not built in

**MOD-1. No universal table.** The consumer declares collections in Schema. Scheda does not ship domain collections and does not put every Record in one bag. *Check:* two Vaults with different collection names work on the same build; grep over implementation source does not contain those names.

**MOD-2. One kind per collection.** Kind is `document`, `journal`, or `singleton`. It does not change for existing records of that collection. A new kind is a new collection (or a Schema version that `05` treats as incompatible). *Check:* Schema with two kinds on one name is rejected; `replace` on a `journal` collection is rejected.

Kind exists so write mode and disk layout stay coupled:

| Kind | Why it exists |
|---|---|
| `document` | One editable file per Record; `get` is a path. People and git see a file. |
| `journal` | Append-only facts in volume: no update/delete API; many lines packed into JSONL **segments** so git stays usable without LFS. |
| `singleton` | One small file for the collection; replace as a whole. Not a journal dump. |

A rare immutable note can still be a `document` the consumer never replaces. `journal` is for the packed, append-only case.

---

## 2. Record identity

**MOD-3. Address is `(collection, id)`.** `get` and edges use this pair. Segment file names, line numbers, and git blob hashes are not the Record `id`. *Check:* after rotation, `get` still returns the same journal Record; the path may name a closed segment.

**MOD-4. `id` is unique in the collection.** A second `add` with the same `id` is rejected. Uniqueness is across all segments of a `journal`. *Check:* insert, insert same `id` → error; two segments must not both contain the same `id`.

**MOD-5. `id` is stable.** Schema derives the `document` path from `id` (and optional extra path slots). Renaming or moving a file so it no longer matches that derivation is rejected on `check` and must not be treated as a new Record with a new `id`. *Check:* file `notes/alpha.md` with Schema `id` = basename; rename to `beta.md` without changing the header `id` → `check` fails; `get("alpha")` does not silently follow the rename.

**MOD-6. Scheda does not invent domain identifiers.** Schema may require a pattern (ULID, `kebab-case`, or another). The library validates the pattern. The caller supplies `id` on insert (or Schema describes a generation rule the implementation applies without embedding consumer semantics). The library (`07` LIB-7) already requires `id` to be a slug (Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`) so Scheda files (`.schema.yaml`, `.scheda/`) stay distinct from Records. Schema `id.pattern` may only tighten that. *Check:* insert with an `id` that fails the pattern is rejected; implementation source does not hard-code a consumer's id scheme.

**MOD-7. Singleton identity.** A `singleton` collection has exactly one Record. Schema fixes its `id`. `add` is allowed only when the file is absent; afterwards only `replace`. *Check:* second `add` is rejected; `get` with that `id` round-trips the file.

---

## 3. Paths

**MOD-8. The library owns Record paths.** The only filesystem path the caller supplies is the Vault root (`open`). Collection name is a slug (same rules as Record `id`), not a path. Each collection occupies `{vault}/{name}/`. Isolated Records: `{vault}/.isolated/{name}/`; Schema stays `{vault}/{name}/.schema.yaml`. Files:

| Kind | Record file |
|---|---|
| `document` | `{name}/{id}.md` |
| `journal` | `{name}/*.jsonl` (library names segments) |
| `singleton` | `{name}/record.yaml` |

The caller never passes a Record path. `get` / `add` / `history` validate `id` then open, create, or git that file. `find` / `replace` / `search` take `id`s from the Index, then open those files (IDX-10). *Check:* `create_collection` with a `layout` key → `invalid`; a collection named `a/b` is `invalid`; `get("alpha")` reads `{name}/alpha.md` for a `document`.

**MOD-9. Journal path is the segment, not the `id`.** The Record still lives at `(collection, id)`. The file path is the open or closed segment that contains the line. Callers must not treat the segment name as `id`. *Check:* `get` does not require the caller to pass a segment name.

**MOD-10. Journal segments.** A `journal` collection is a directory of `*.jsonl` files. At most one segment is open for append. Schema closes it on a size cap (required) and optionally a line cap or calendar boundary. A closed segment is never rewritten. *Check:* after the cap, the next `add` creates or opens a new segment; bytes of the previous file are unchanged; each segment stays below the cap.

The numeric default cap is in `05`. It must sit well below common git-host file limits (warn ~50 MiB, reject ~100 MiB) and well below what `git diff` remains usable for. This package does not use Git LFS.

---

## 4. Shape of a Record

**MOD-11. Fields are structured; body is optional prose.** `document`: YAML 1.2 header (between `---` for Markdown, or the whole file if YAML-only) plus optional Markdown body. `journal`: the JSON object on the line is the fields; no Markdown body. `singleton`: JSON or YAML file as Schema says; body only if Schema allows a Markdown singleton (default: no body). *Check:* a `document` with empty body is accepted; a `journal` line that is not one JSON object is rejected.

**MOD-12. Missing is missing.** If a field is absent and Schema did not set a default, read-back does not invent `false`, `0`, `""`, `[]`, or `null`. *Check:* insert without an optional field; `get` JSON/YAML does not contain that field.

**MOD-18. Null is a value.** A key present with JSON/YAML `null` is not the same as a missing key. Read-back of a stored `null` is `null`, not omission. Schema says when `null` is allowed (SCH-5). *Check:* write `"f": null` on a nullable field; `get` has the key `f` with null; write the same Record omitting `f` (if optional) → no key `f`.

**MOD-13. Header `id` matches identity.** For `document`, the header field `id` (name may be Schema-configured, default `id`) equals the path-derived `id`. For `journal`, the object contains that field and it equals the Record `id`. *Check:* mismatch is rejected on write and on `check`.

**MOD-14. Empty body is not a journal.** A `document` with only a header is still a `document`. *Check:* `replace` remains allowed; the collection kind is unchanged.

---

## 5. Writes implied by kind

**MOD-15. `document`.** `add` creates the file. `replace` replaces the whole file (header and body). Partial field patch is not a Scheda operation. *Check:* replace changes bytes of that path only.

**MOD-16. `journal`.** `add` appends one canonical line to the open segment (newline `\n`, no comments, one object). `replace` and delete-line do not exist. A truncated or binary-corrupted line is detected on read/`check` and is not silently repaired. *Check:* `replace` → error; mid-file corruption → `check` fails; valid lines before the damage remain readable as specified in `04`.

**MOD-17. `singleton`.** Replace is the whole file after Schema validation. The file must remain small for the same git reasons as a segment. *Check:* `add` of a second logical Record is rejected (see MOD-7).

---

## 6. What this document does not decide

Git commit, Remote, `auto_sync`, `sync`, write lock, and working-tree durability — `04`.

Which fields are indexed, required, or edges — `05`.

How `get` locates a journal `id` without scanning every segment — `06` (Index or equivalent lookup). Until then, a correct but slow scan of segments is allowed for **core** `get`. Order of `find` on a `journal` — `06` (QRY-7: append order).

Canonical JSON (for example RFC 8785) of a journal line — `07`, so consumers can hash bytes. This model only requires: one object, one line, stable field identity under MOD-12.
