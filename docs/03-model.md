# 03. Model

Document 3 of the Scheda package. Version 0.25, 16 September 2026.

Normative data model: collection kinds, Record identity, and paths. Terms
are in `02`. File encoding, git, and locks are in `04`. The collection
declaration is in `05`. Checkable requirements use codes `MOD`. Phase
**core** unless marked otherwise.

An assertion without a check is not a requirement.

---

## 1. Collections are declared, not built in

**MOD-1. No universal table.** The consumer declares collections. Scheda
does not ship domain collections and does not put every Record in one bag.
*Check:* two Vaults with different collection names work on the same
build; grep over `rust/scheda/src` does not contain those names. Test data
is not that scope (SCH-1).

**MOD-2. One kind per collection.** Kind is `document`, `journal`, or
`singleton`. It does not change for existing records of that collection. A
new kind is a new collection. *Check:* a declaration with two kinds on one
name is rejected; `replace` on a `journal` collection is rejected.

Kind exists so write mode and disk layout stay coupled. Kind is not a
codec for Record contents:

| Kind | Why it exists |
|---|---|
| `document` | One editable UTF-8 file per Record; `get` is a path. People and git see a file. |
| `journal` | Append-only facts in volume: no `replace` and no `remove`; many lines packed into **segments** so git stays usable without LFS. |
| `singleton` | One small UTF-8 file for the collection; replace as a whole. Not a journal dump. |

A rare immutable note can still be a `document` the consumer never
replaces. `journal` is for the packed, append-only case.

---

## 2. Record identity

**MOD-3. Address is `(collection, id)`.** `get` uses this pair. Segment
file names, line numbers, and git blob hashes are not the Record `id`.
*Check:* after rotation, `get` still returns the same journal Record; the
path may name a closed segment.

**MOD-4. `id` is unique in the collection.** A second `add` with the same
`id` is rejected. Uniqueness is across all segments of a `journal`.
*Check:* insert, insert same `id` → error; two segments must not both
contain the same `id`.

**MOD-5. `id` is the path (document / singleton).** The library derives the
`document` file from `id`:`{name}/{id}`, where `/` is a directory. It
does not append `.md`. A `.` inside a component is allowed (`foo.md`,
`work/meeting.toml`). Renaming the file is changing `id`. `get` uses the
`id` the caller passes; it does not follow a rename. *Check:*
`add("foo.md", …)` writes `{name}/foo.md`; `get("foo.md")` reads that
file; after a rename of the file to `bar.md`, `get("foo.md")` is
`not_found` and `get("bar.md")` returns the bytes. *Check:*
`add("work/meeting.toml", …)` writes `{name}/work/meeting.toml`.

**MOD-6. Scheda does not invent domain identifiers.** The caller supplies
`id` on insert. The library (`07` LIB-7) requires each `id` component to
be a slug (Unicode letter or digit, then letters, digits, marks, `.`, `_`,
`-`, `+`, `(`, `)`, space, or `@`; max 255 bytes for the whole `id`; no
leading/trailing whitespace; no trailing `.`; not a Windows device name)
so Scheda files (`.collection.yaml`, `.scheda/`) stay distinct from
Records: a component must not be empty, `.`, or `..`, and must not start
with `.`. A `document` `id` may join those slugs with `/`; `journal`,
`singleton`, and collection names stay a single slug. The collection
declaration does not add a pattern on top of that rule. The library does
not mint ULIDs, kebab-case, or any other consumer scheme. *Check:* insert
with an `id` that fails the slug is rejected; a `document` `id`
`work/meeting.toml` is accepted; `foo.md` is accepted; `../x`, `foo/./bar`,
`/abs`, `a\\b`, and `.hidden` are `invalid`; implementation source does
not hard-code a consumer's id scheme.

**MOD-7. Singleton identity.** A `singleton` collection has exactly one
Record. The collection declaration fixes its `id` (may include an
extension). `add` is allowed only when the file is absent; afterwards only
`replace`. The method `id` must equal the declared `id`. *Check:* second
`add` is rejected; `get` with that `id` round-trips the file; `add` with a
different `id` is `invalid`.

---

## 3. Paths

**MOD-8. The library owns Record paths.** The only filesystem path the
caller supplies is the Vault root (`open`). Collection name is a slug
(same rules as one Record `id` component), not a path. Each collection
occupies `{vault}/{name}/`. Isolated Records: `{vault}/.isolated/{name}/`;
the collection declaration stays `{vault}/{name}/.collection.yaml`. Files:

| Kind | Record file |
|---|---|
| `document` | `{name}/{id}` (`/` in `id` is a directory: `work/meeting.toml` → `{name}/work/meeting.toml`) |
| `journal` | `{name}/` plus segment files the library names |
| `singleton` | `{name}/{declared id}` |

The caller never passes a Record path. `get` / `add` / `history` validate
`id` then open, create, or git that file. `find` / `replace` / `remove` /
`search` take `id`s from the Index when `find` is on and the collection is
lexically indexed, then open or unlink those files (IDX-10). When `find`
is off, or the collection is not lexically indexed, `replace` / `remove`
use the Record file (same path as `get`). Isolated is not a special case.
`ids()` / `documents()` list regular files except dotfiles; they do not
require a `.md` suffix and they do not skip `.md` files.
`.collection.yaml` is a dotfile and is not a Record. *Check:*
`create_collection` with a `layout` key → `invalid`; a collection named
`a/b` is `invalid`; `get("alpha")` reads `{name}/alpha` for a `document`;
`get("foo.md")` reads `{name}/foo.md`; `get("work/meeting.toml")` reads
`{name}/work/meeting.toml`. *Check:* `ids()` on a collection that holds
`a.yaml` and `b.md` returns both, and does not return `.collection.yaml`.

**MOD-9. Journal path is the segment, not the `id`.** The Record still
lives at `(collection, id)`. The file path is the open or closed segment
that contains the line. Callers must not treat the segment name as `id`.
*Check:* `get` does not require the caller to pass a segment name.

**MOD-10. Journal segments.** A `journal` collection is a directory of
segment files the library names. Each clone of the Vault has a writer
identity and appends only to its own segments. The collection declaration
closes that writer's open segment on a size cap (required) and optionally
a line cap or calendar boundary. A closed segment is never rewritten.
*Check:* after the cap, the next `add` creates or opens a new segment for
that writer; bytes of the previous file are unchanged; each segment stays
below the cap. *Check:* two clones with different writers produce disjoint
segment names.

The numeric default cap is in `05`. It must sit well below common git-host
file limits (warn ~50 MiB, reject ~100 MiB) and well below what `git diff`
remains usable for. This package does not use Git LFS.

---

## 4. Shape of a Record

**MOD-11. A Record is opaque UTF-8 bytes.** `document` and `singleton`:
the file bytes. `journal`: the payload after the first `:` (colon, then space) on the line
(see MOD-16). Scheda does not parse those bytes as YAML, Markdown, JSON,
or any other document format. Empty bytes are allowed (empty file / empty
payload). Non-UTF-8 on `add` / `replace` is `invalid`. A `.` in `id` is
not a format signal. *Check:* `add` of `foo.md` and `foo.yaml` in one
collection stores two Records; `get` of each returns the bytes that were
written; neither file is parsed.

**MOD-12 (retired).** Missing vs default was a Schema-field rule.

**MOD-18 (retired).** JSON `null` vs a missing key was a Schema-field
rule.

**MOD-13 (retired).** Header `id` matching the path. For `document` /
`singleton`, identity is the path (MOD-5). For `journal`, identity is the
envelope `id` (MOD-16). `check` (LIB-15) does not parse Record bytes for a
consumer `id` key. It still reports duplicate ids, broken journal
envelopes, non-UTF-8 files, conflict markers, declaration errors,
isolation gitignore, Index modes, and a leftover `.schema.yaml`.

**MOD-14 (retired).** Empty body vs journal. Kind is the collection's
write mode, not whether a file has a header.

---

## 5. Writes implied by kind

**MOD-15. `document`.** `add` creates the file with the caller's bytes.
`replace` replaces the whole file. `remove` unlinks the file. Partial
patch of contents is not a Scheda operation. *Check:* replace changes
bytes of that path only. *Check:* `remove` then `get` → `not_found`; the
file is gone.

**MOD-16. `journal`.** `add(id, payload)` appends `{id}: {payload}\n` to
the open segment (colon, then space). `id` is a single slug (MOD-6; `:`
is not in the slug set). `payload` is UTF-8 and must not contain `\n` or
`\r` (`invalid` otherwise). Empty payload is `{id}: \n`. `Record.raw` and
`lines` `raw` are the payload, not the envelope. Scheda splits on the
first `:` (colon, then space); it does not parse the payload. The segment is not a YAML
document. `replace` and `remove` do not
exist (`unsupported`). A journal has no remove: rewriting a segment would
break the append-only guarantee a caller may rest a checksum chain on. A
truncated line, a missing `:` delimiter (colon, then space), or an empty envelope `id` is detected on
read/`check` and is not silently repaired. *Check:* `replace` → error;
`remove` → error; mid-file corruption → `check` fails; valid lines before
the damage remain readable as specified in `04`. *Check:* wipe
`{vault}/.scheda/`; `get` still finds the line by scanning envelopes.

**MOD-17. `singleton`.** Replace is the whole file (the declared `id`
path). No content schema is applied. `remove` unlinks that file; a later
`add` of the declared `id` is allowed again (MOD-7). The file must remain
small for the same git reasons as a segment. *Check:* `add` of a second
logical Record is rejected (see MOD-7). *Check:* `remove` then `get` →
`not_found`.

---

## 6. What this document does not decide

Git commit, Remote, `auto_sync`, `sync`, write lock, and working-tree
durability — `04`.

Collection declaration keys (kind, isolation, encryption, journal caps)
— `05`. Index columns and `Projector` — `06` / `13`.

How `get` locates a journal `id` without scanning every segment — `06`
(Index or equivalent lookup). Until then, a correct but slow scan of
envelopes is allowed for **core** `get`. Order of `find` on a `journal` —
`06` (QRY-7: append order). Walking every journal line without the Index
— `07` LIB-19 (`lines`).
