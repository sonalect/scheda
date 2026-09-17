# 16. Opaque records

Document 16 of the Scheda package. Version 0.6, 17 September 2026.

Work order so Scheda stops owning the format of Record contents. Records
become opaque UTF-8 bytes. Collection declaration keeps write mode and
perimeter. Field Schema, YAML frontmatter, and a forced `.md` suffix go.
A `Projector` callback (same injection idea as `Embedder`) is how `find`
and `search` learn columns and text. F0–F17 are done; codes and
*Check:* lines in `03`–`09`, `13`, and `14` as amended are the spec.
This file remains the work order for F18–F19.

This is a breaking 0.x change of the Vault layout and of `api/v1`. There
is no compatibility adapter for `{id}.md` YAML headers or for
`.schema.yaml` field lists. Existing Vaults fail closed (`invalid`) rather
than being guessed into the new layout.

The first implementation is the crate `scheda`. Other language bindings
are out of the F stages. Consumers migrate after F19; they are not a
stage here. C0–C10, A0–A7, P0–P16, and R0–R8 stay closed. Do not reopen
them as new stages; F boxes amend the codes those plans already accepted.

---

## 1. How to track progress

**Stop after every stage.** The owner reviews that stage's result before
the next stage starts. Spec stages (F1–F8): the review artefact is the
document diff. Implementation stages (F9–F19): the review artefact is the
code + test diff, and `bazel test //...` is green. Do not start the next F stage
from a verbal claim that the previous one is ready.

Tick a box only when the cited *Check:* exists (implementation) or the
owner has accepted the document diff (spec). Do not tick from a verbal
claim.

| Mark  | Meaning |
| ----- | ------- |
| `[ ]` | Not done |
| `[x]` | Spec: owner accepted the diff. Code: the check lives in the crate tests |

Each stage has **Status:** `not started` | `in progress` | `done`. Set
`done` only when every box in that stage is `[x]`. For F9–F19, Clippy is
clean and `bazel test //...` has exited 0 on that tree.

If F1–F8 have not yet decided a path, a filename, a proto field, or a
`code`, stop. Amend the spec; do not invent a silent default in `src/`.
Do not implement F9 against this charter alone.

Order: F0 → F1 → … → F19. Do not interleave a later F stage with an
earlier one. F10 may be sketched in `src/` as a trait during F9 only if
it is unused; wiring is F12–F13.

---

## 2. Dashboard

| Stage | Status | What closes |
| ----- | ------ | ----------- |
| [F0](#f0-charter) | done | Locked decisions; this file; package index |
| [F1](#f1-concept) | done | `01` does not own Markdown/YAML as the Record |
| [F2](#f2-glossary) | done | `02` Record, path, collection declaration, Projector |
| [F3](#f3-model) | done | `03` path = `id`; no fields/body/header `id` |
| [F4](#f4-collection-declaration) | done | `05` is write mode + perimeter, not field types |
| [F5](#f5-library-api) | done | `07` `add`/`get` bytes; no `Record.fields` |
| [F6](#f6-projector-and-query) | done | `06` / `13` Projector; `Filter` vs Projector schema |
| [F7](#f7-agent-and-cli-spec) | done | `08` / `14` no field catalog; `--file` |
| [F8](#f8-files-and-perimeter) | done | `04` / `09` opaque UTF-8; encryption still whole-file |
| [F9](#f9-document-bytes) | done | `document` I/O is bytes; path is `id` |
| [F10](#f10-singleton-bytes) | done | `singleton` I/O is bytes; file is the declared `id` |
| [F11](#f11-journal-envelope) | done | Journal line envelope; payload opaque |
| [F12](#f12-projector-and-find) | done | `Projector` wired into the lexical Index and `find` |
| [F13](#f13-projector-and-search) | done | BM25 / vectors from Projector slots |
| [F14](#f14-drop-field-schema) | done | No `Field` / edges in proto, disk, or `src/` |
| [F15](#f15-cli) | done | CLI `--file`; default whole-file Projector |
| [F16](#f16-agent-surface) | done | `info` / `create_collection` without fields |
| [F17](#f17-check-reindex-dead-code) | done | `check` / `reindex` / YAML codec gone |
| [F18](#f18-documents-once) | not started | One consistency pass over `docs/` + README |
| [F19](#f19-acceptance) | not started | Mixed-format fixture; wipe Index; same files |

---

## 3. Locked decisions

F1–F8 copy these into the normative documents. Do not reopen them in a
later F stage without amending this section first.

1. **Opaque UTF-8, not binary.** `add` / `replace` reject bytes that are
   not UTF-8 (`invalid`). A Record file is text a person can open and
   that `git diff` can show. Arbitrary binary (images, protobuf wire,
   age armor aside from SCH-16) is **later**, not this plan. Encrypted
   collections stay whole-file age armor of the UTF-8 plaintext (F8).
2. **Collection declaration stays. Field Schema goes.** Kind,
   `isolated`, `encrypted`, journal caps, `exclude_lexical_index`,
   `exclude_vector_index`, and singleton `id` remain Vault data.
   `version` on the declaration is the **collection generation**: how
   the declaration file is shaped **and** how Record files of that
   collection sit on disk (not field-schema `read_as` / migrate; that
   SCH-13 is still retired). It is required. No default: missing,
   non-integer, or any value other than the generations the library
   implements is `invalid`. Generation **1** is the only one in this
   plan (opaque UTF-8, `{name}/{id}` for `document`, journal envelope,
   singleton declared `id`). Generation 2 may change layout; the
   library will keep reading 1. `create_collection` writes `1`; the
   proto declaration does **not** carry `version` (the library sets
   it). Kind does not change in place (MOD-2); flipping `encrypted` on
   a non-empty collection is already `invalid` (ISO-11); journal caps
   may change inside generation 1. Field names, types, `required` /
   `nullable` / `default`, `exclude_find`, body marks, and edges are
   not Scheda's. The on-disk file is `{name}/.collection.yaml` (also
   `.yml` / `.json`). A leftover `{name}/.schema.yaml` is `invalid`,
   not ignored.
3. **Identity is a vault-relative POSIX path for `document` /
   `singleton`.** The caller supplies `id`. For `document`, the file is
   `{collection}/{id}` (`/` is a directory). The library does not append
   `.md`. An extension, if any, is part of `id` (`foo.md`, `note.yaml`,
   `work/meeting.toml`). Scheda does not interpret the extension. There
   is no header/`id` key inside the file that must match the path.
   Renaming the file is changing `id`.

   `id` must stay inside the collection directory. Reject (`invalid`)
   before any `open`: empty; NUL; `\`; a leading `/`; a trailing `/`;
   `//`; a `.` or `..` component; a Windows device stem (`CON`, `NUL`,
   …, as today). Do **not** add a path crate. `Path::normalize_lexically`
   is nightly (`normalize_lexically`, not in rustc 1.98). `relative-path`
   applies `..` instead of refusing it, which is the opposite of this
   rule. Host `std::path` is the wrong model (Windows `\`). The library
   already has this check (`require_document_id` plus `join_inside`);
   keep it as POSIX-string rules plus join of `Normal` components only
   under the collection directory. Do not `canonicalize` for containment
   (`..` is already gone; canonicalize follows symlinks and needs the
   file to exist). Journal `id` stays a single slug (no `/`); it lives
   in the envelope, not in a filename.
4. **Journal identity needs a Scheda-owned envelope.** A journal `id` is
   not a filename (segments are packed). Wipe-Index `get` (IDX-1) must
   still work without a `Projector`. Each line is `{id}: {payload}\n`
   (colon U+003A, then space U+0020). `id` is the slug already used as
   the method argument (no `:`, no newline). `:` is not in the slug
   character set, so the first `:` cannot appear inside `id`. `payload`
   is the bytes `add` received, UTF-8, no `\n` and no `\r` (`invalid`
   otherwise). Empty payload is `{id}: \n` (the space is the delimiter).
   `Record.raw` / `lines` `raw` is the **payload**, not the envelope.
   Scheda splits on the first `:`; it does not parse the payload. A
   segment of these lines is **not** a YAML document. RFC 8785 canonical
   JSON of consumer fields is no longer a library rule (LIB-18 retires).
5. **`Projector` is a code callback at `open`, not Vault data.** Same
   class as `Embedder`: not an `api/v1` field. The name is `Projector`
   (opaque bytes → declared Index columns). Not `Indexer` (collides with
   the product noun Index), not `Extractor` (too generic), not `Lexier` /
   `Lexer` (`Lexier` is not an English word; a lexer tokenizes a
   language). It declares typed Index columns per collection
   (`schema(collection)`) and maps `(collection, id, raw) →` those
   columns (`project`). The same column name may have different kinds in
   different collections (`collection1.foo` text, `collection2.foo`
   integer). `Filter` / BM25 / vectors use that declaration. An unknown
   `Filter` field is `invalid`. `project` error or empty → the Record
   stays out of the Index; `get` still returns the file (IDX-3 without
   field Schema). No `Projector` → the Index still holds identity
   (`collection`, `id`, path, ordinal) so `find` `all` and `term` on `id`
   work; any other field is `invalid`; `search` has no FTS/vector slots
   (lexical empty / vector off). `add` does **not** call `project` to
   accept or reject the write.

   **Filter field names.** A Projector column is identified by
   `(collection, name)`. Docs write that as `collection1.foo`. The wire
   does **not** parse a dotted `field` string: collection slugs may
   contain `.` (MOD-6), so `a.b.c` is ambiguous. Each Filter leaf carries
   column `name` (`foo`) and, when needed, `collection`. Identity `id` /
   `collection` stay library columns and are not qualified this way.

   - **Vault `find` / `search`:** every Projector-column leaf must name
     the collection (`collection1.foo`). Bare `foo` is `invalid`.
   - **Collection `find` / `search`:** `foo` means this collection's
     `foo`. `collection1.foo` is the same if this handle is
     `collection1`. `collection2.foo` on a `collection1` handle is
     `invalid` (this method is one collection; use Vault `find` /
     `search` to query another). A collection qualifier that does not
     exist is `not_found` on Vault, `invalid` on the Collection handle.
6. **`info` does not catalog fields.** `info` still returns
   `api.v1.VaultInfo` (same proto object as today). Agent listing is
   collections + kind / isolated / encrypted / caps. Queryable field
   names live in the consumer's `Projector` (and may be mirrored at
   runtime later; not a Vault file). `create_collection` takes a proto
   collection declaration (today `api.v1.Schema`; rename is F14), not a
   field list and not a language-native hashmap.
7. **Kinds stay.** `document` / `journal` / `singleton` are write mode
   and disk layout, not content codecs. `replace` / `remove` on `journal`
   stay `unsupported`.
8. **Mixed files in one collection are allowed.** A `document` collection
   may hold `a.yaml` and `b.toml`. That is the caller's problem. The
   `Projector` may sniff `id` or bytes. Scheda does not.

---

## 4. Out of F0–F19

These must not block a stage in §2. Writes in F9–F19 must not depend on
them.

- Binary Records (locked decision 1).
- A compatibility reader for `.schema.yaml` fields or `{id}.md` YAML
  headers.
- A consumer's migration onto the new API. F19 does not wait on one.
- Extra language bindings (`10` §5).
- A path-validation crate (`relative-path`, `path_normalizer`, …).
  Locked decision 3.
- File watcher; journal line `remove`; REL-4; `walk` / `walk_inverse` /
  `scan`.
- Shipping an `Projector` implementation inside the crate other than the
  CLI's default whole-file one (F15).
- Reintroducing edges, `strict` dangling checks, or Schema `default`.
- Changing isolation, encryption, autocommit, or online `reindex` (R0–R8)
  except where a renamed declaration file or opaque bytes force a
  mechanical edit.

---

## F0. Charter

**Status:** done

**Codes:** none yet. This stage does not amend `03`–`09`.

Record the locked decisions (§3) in this file and point the package at
it. Do not change Record I/O.

- [x] `docs/16-opaque-records.md` exists and §3 matches the owner's
      agreement (opaque UTF-8; collection declaration without fields or
      `version`; POSIX path `id` that cannot leave the collection;
      journal `:` envelope; `Projector` at `open`; `info` without
      fields).
- [x] `01` §6, `10` (next crate work), and README Documents table name
      `16`.
- [x] Owner has reviewed this file. Next stage is F1, not F9.

**Review:** this document. Stop.

---

## F1. Concept

**Status:** done

**Codes:** Challenge 2, Challenge 3 (`01`).

Close only after F0. Amend `01` only.

Scheda is a library over git files. It does not own YAML-frontmatter +
Markdown as the Record, and it does not own field types. Challenge 2
stays: files are UTF-8 a person can open; `git diff` shows the change.
Challenge 3 is stronger: consumer types are not Vault Schema fields
either — they live in the consumer (and in its `Projector`). Collection
kind / isolation / encryption remain Vault data so `open` can discover
collections.

- [x] `01` §2–§3 no longer present YAML header + Markdown body, JSONL
      fields, or `record.yaml` as the product's Record shape.
- [x] `01` says Records are opaque UTF-8; the library places the file;
      format and extension are the caller's.
- [x] `01` still refuses a general-purpose database, a second brain, and
      treating the Index as truth.
- [x] Binary Records are named as later, not day one.
- [x] Owner has reviewed the `01` diff.

**Review:** `01`. Stop. Do not edit `02` in this stage.

---

## F2. Glossary

**Status:** done

**Codes:** `02` articles for Record, Schema, Collection, path, Projector.

Close only after F1. Amend `02` only.

- [x] **Schema** as field types is retired as a product noun, or confined
      to a historical sentence. The live noun is **collection
      declaration** (kind, perimeter, caps).
- [x] **Record** is `(collection, id)` plus UTF-8 bytes at the library
      path. No Fields, Body, Header articles as Scheda concepts.
- [x] **Path** for `document` is `{collection}/{id}`; for `singleton`
      `{collection}/{declared id}`; for `journal` the segment, with the
      envelope in §3 decision 4.
- [x] **Projector** is defined next to Embedder / Reranker: caller code,
      not Vault data, not proto.
- [x] **Consumer** still owns type names; it no longer declares them as
      Schema fields in the Vault.
- [x] Owner has reviewed the `02` diff.

**Review:** `02`. Stop.

---

## F3. Model

**Status:** done

**Codes:** MOD-5, MOD-8, MOD-11–18 (amend or retire).

Close only after F2. Amend `03` only.

- [x] MOD-5 / MOD-8: `document` file is `{name}/{id}`; the library does
      not *append* `.md`. `/` in `id` is a directory. `id` is a
      vault-relative POSIX path that cannot leave the collection (locked
      decision 3: no `..` component, no `.` component, no absolute, no
      `\\`). A `.` *inside* a component is allowed: `foo.md` and
      `work/meeting.toml` are valid ids and those files are the Records.
      `ids()` / `documents()` list regular files except dotfiles; they
      do not require a `.md` suffix and they do not skip `.md` files.
- [x] MOD-11–14: no Scheda fields, body, or header `id`. MOD-13 (header
      matches path) is retired. `check` (LIB-15) **stays**: declarations,
      UTF-8, journal envelopes, `id` uniqueness, conflict markers,
      isolation gitignore, Index modes, leftover `.schema.yaml`. It does
      **not** parse Record bytes for consumer fields. Without `check`, a
      hand-edit that breaks an envelope or duplicates an `id` has no
      library report except a later `get` / `find` surprise.
- [x] MOD-12 / MOD-18 (missing vs `null`) are retired as library rules;
      they were Schema-field rules.
- [x] Journal: envelope `{id}: {payload}\n`; payload opaque; `raw` is
      payload. `replace` / `remove` still `unsupported` (MOD-16).
- [x] Singleton: one file named by the declaration's `id` (may include
      an extension). Not always `record.yaml`.
- [x] Slug rules stay (MOD-6) so dotfiles and `.collection.yaml` are not
      Records. Trailing `.` still forbidden; `.yaml` as a suffix is fine
      (`note.yaml` has no trailing dot).
- [x] Owner has reviewed the `03` diff.

**Review:** `03`. Stop.

---

## F4. Collection declaration

**Status:** done

**Codes:** SCH-1–16 (keep perimeter/caps; retire field SCH-4–13 and
SCH-2 `version`).

Close only after F3. Amend `05` only. Do not generate proto yet (F9).

`05` becomes the collection declaration: where it lives, discovery,
kind, journal caps, singleton `id`, `isolated`, `encrypted`,
`exclude_lexical_index`, `exclude_vector_index`. No `fields` key and no
`version` key. Either is `invalid` (unknown / retired).

- [x] Discovery: directories at the Vault root that are not dot-dirs and
      that contain exactly one of `.collection.yaml` / `.yml` / `.json`.
      Two declaration files, or a leftover `.schema.yaml`, → `invalid`.
- [x] SCH-4–13 (field types, required, body marks, edges, write-time
      field validation) are retired or marked historical. SCH-12's
      "old file fails Schema" becomes: `get` returns bytes if the path
      exists; Index omission is the `Projector`'s `project` failure (F6),
      not a Schema type check.
- [x] SCH-14 journal caps, SCH-15 isolation, SCH-16 encryption stay.
      Encrypted `journal` stays `invalid`.
- [x] §8 on-disk keys match the thinner file. Unknown keys refused
      (SCH-3: no executable blob as an extra key).
- [x] Owner has reviewed the `05` diff.

**Review:** `05`. Stop.

---

## F5. Library API

**Status:** done

**Codes:** LIB-7, LIB-8, LIB-9, LIB-18, LIB-20, LIB-21, LIB-25, LIB-26.

Close only after F4. Amend `07` only. Proto edits wait for F9.

- [x] `add(id, raw, message?)` / `replace(id, raw, message?)`. `raw` is
      bytes. No `RecordAdd.fields` / `body`. Empty `raw` is allowed (empty
      file / empty payload).
- [x] `get` returns `api.v1.Record` (same proto object as today). The
      message keeps `collection`, `id`, `path`, `raw`. No `fields`, no
      `body`. `valid` is removed (it meant Schema match). Malformed UTF-8
      on disk is reported by `check`, not by a flag on `get`; `get` still
      returns the bytes if the file exists.
- [x] LIB-18 (RFC 8785 journal line) is retired. Journal `add` writes the
      envelope (§3 decision 4).
- [x] LIB-21 `create_collection(name, decl, message?)`. `decl` is proto
      (today `api.v1.Schema`; rename is F14), not a YAML string and not
      `Schema` with `fields`.
- [x] LIB-20 `info` returns `api.v1.VaultInfo` (same proto object as
      today). Lists declarations without field lists. ISO-3 still omits
      sealed material; there are no field values to omit.
- [x] LIB-25 / LIB-26: skip dotfiles and non-files; do not require `.md`.
      `id` is the relative path under the collection, including extension.
- [x] `open` takes an optional `Projector` the same way it takes an
      `Embedder` (not proto). Signature named in `07`; Rust shape is the
      binding's affair as long as it is not an `api/v1` field.
- [x] Owner has reviewed the `07` diff.

**Review:** `07`. Stop.

---

## F6. Projector and query

**Status:** done

**Codes:** IDX-3, IDX-6, IDX-8, IDX-9, SCH-8–10, QRY-2, QRY-3, QRY-6, `13`.

Close only after F5. Amend `06` and `13` only.

Target `Projector` (the trait name is locked in §3):

```text
schema(collection) → IndexSchema // columns of that collection:
                                // name, kind (text / integer / number /
                                // boolean / text list), find / fts /
                                // vector marks
project(collection, id, raw) → IndexDoc
                                // values for schema(collection);
                                // omit a column = missing, not null
                                // unless the value is present
```

Unknown collection on `schema`: empty columns or an error; both are
allowed. Same column `name` in two collections may differ in kind.

Tantivy columns are `IndexSchema` per collection, not inferred from a
`Struct` and not declared in `.collection.yaml`. One vector slot per
marked column, not a concatenation (`13` §5). `id` / `collection` / path
/ ordinal remain library columns. `body` as a magic find key is gone
unless the `Projector` names a column `body`.

Filter naming is locked in §3 decision 5: Vault leaves are
`collection.column`; Collection handle accepts `column` or its own
`collection.column`; another collection's qualifier is `invalid`. Wire
is proto (`field` + `collection` on the leaf), not a dotted parse. Proto
edits wait for F9; `13` names the rule.

- [x] `13` §3: `Filter` is validated against `schema(collection)` plus
      identity columns. Unknown field → `invalid`. No `exclude_find` in
      the Vault. Vault vs Collection field naming as §3 decision 5.
- [x] `13` §5: embedder texts come from Projector slots, not Schema marks.
      Digest skip is unchanged (git blob hash of that slot's text).
- [x] IDX-3: `project` failure omits the Record from lexical, vector, and
      (for journal) does **not** drop the id map — the envelope still
      has `id`. Encrypted collections still enter no Index.
- [x] No `Projector`: identity `find` only; `search` has no content slots.
- [x] `more_like_this` with source `id` still builds text from what the
      Index holds for that Record (`13` §3), now Projector full-text, not
      Schema fields.
- [x] Edges / `walk` stay absent (`13` §7).
- [x] Owner has reviewed the `06` / `13` diff.

**Review:** `06`, `13`. Stop.

---

## F7. Agent and CLI spec

**Status:** done

**Codes:** AGT-1, AGT-3, `08`, `14`.

Close only after F6. Amend `08` and `14` only.

- [x] AGT-1 listing is collections + declaration (kind, isolated,
      encrypted, caps). No field names, marks, or edges.
- [x] `create_collection` JSON is the thin declaration. A `fields` key is
      `invalid`.
- [x] CLI `add` / `replace` take file bytes (`--file` or stdin), not a
      JSON `fields` object. `get` writes `raw` (default: bytes to stdout;
      JSON envelope when the command already prints Protobuf JSON — pick
      one in this stage and write it in `14`, do not leave both implied).
- [x] CLI `search` / `find` / `reindex`: if no program-supplied Projector,
      the CLI uses a **default whole-file Projector**: one text column
      `body` (FTS + vector) whose value is the entire UTF-8 Record.
      `find` on other field names is `invalid`. This default is CLI-only;
      the library does not install it when the program passes `None`.
- [x] Owner has reviewed the `08` / `14` diff.

**Review:** `08`, `14`. Stop.

---

## F8. Files and perimeter

**Status:** done

**Codes:** STO-2, STO-3, ISO-8, ISO-9, GIT-13.

Close only after F7. Amend `04` and `09` only.

- [x] STO-2: Vault Record files are UTF-8 text. The library does not
      require YAML 1.2 or JSON (except the collection declaration file,
      which stays YAML/JSON as today). `git diff` of a `document` is a
      diff of those bytes.
- [x] STO-3: journal damage is a broken line (missing newline, missing
      `:`, empty `id`). Not "JSON object failed Schema".
- [x] Encryption (ISO-8, ISO-9) is still whole-file age armor of the
      UTF-8 plaintext. No field-level encryption. Encrypted `journal`
      stays refused.
- [x] Owner has reviewed the `04` / `09` diff.

**Review:** `04`, `09`. Stop. Spec is complete; F9 may start.

---

## F9. Document bytes

**Status:** done

**Codes:** MOD-8, LIB-7, LIB-8, LIB-9, LIB-24, LIB-25, LIB-26.

Close only after F8. First code stage. Write is method `raw` bytes
(`add(id, raw, message?)` / `replace(id, raw, message?)`). Proto `Record`
is output-only (`get` / `find` / `search`). `RecordAdd` and `RecordReplace`
are gone. `Record` keeps `bytes raw` and drops `fields` / `body` / `valid` /
`path`. `Schema` may still exist in proto until F14 so journal/singleton
code still compiles; do not add new field-Schema features. `Schema.version`
is not on the proto; the library writes generation `1` on disk.

`bazel run //api/v1:generate` before tests (`run-commands.mdc`).

- [x] `document` `add(id, raw)` writes `{name}/{id}` (no `.md` suffix).
      `get` / `replace` / `remove` / `history` use that path.
- [x] `id` `work/meeting.toml` → `{name}/work/meeting.toml`. `id`
      `a.yaml` and `a.toml` are different Records.
- [x] Bytes are not parsed as YAML. Two files of different formats in one
      collection round-trip.
- [x] Non-UTF-8 `add` → `invalid`; no file.
- [x] `documents()` / `ids()`: regular files, skip dotfiles, do not
      require `.md`. `.collection.yaml` is not a Record. Until F14,
      skip `.schema.yaml` as a dotfile too.
- [x] `find` on a `document` matches identity only (`all`, `term` `id=`)
      until F12. No panic if Schema fields still exist in proto.
- [x] Journal and singleton tests still pass (their on-disk shape may
      still be the pre-F11/F10 one as long as writes pass bytes: tests
      pass the old JSON/YAML as `raw`).
- [x] `bazel test //...` green. Owner has reviewed the diff.

**Review:** proto + `document` I/O. Stop.

---

## F10. Singleton bytes

**Status:** done

**Codes:** MOD-7, MOD-8, MOD-17.

Close only after F9.

- [x] Singleton file is `{name}/{declaration.id}`, not always
      `record.yaml`. `add` / `replace` / `remove` / `get` / `history` /
      `documents` / `ids` use that name.
- [x] Contents are opaque UTF-8. No Schema field validation.
- [x] Second `add` still `duplicate_id`. `id` on the method must equal
      the declared singleton `id` (`invalid` otherwise).
- [x] `bazel test //...` green. Owner has reviewed the diff.

**Review:** singleton I/O. Stop.

---

## F11. Journal envelope

**Status:** done

**Codes:** MOD-9, MOD-10, MOD-16, SCH-14, LIB-18 (retired), LIB-19, IDX-5.

Close only after F10.

- [x] `add(id, payload)` appends `{id}: {payload}\n` to the open segment.
      Payload with `\n` / `\r` or non-UTF-8 → `invalid`.
- [x] `get` / `lines`:`raw` is the payload. Wipe `{vault}/.scheda/`:
      `get` still finds the line by scanning the envelope (IDX-1);
      `lines` still yields every payload in append order.
- [x] Duplicate `id` across segments → `duplicate_id`. The library reads
      `id` from the envelope, not from JSON keys in the payload.
- [x] `replace` / `remove` remain `unsupported`. Rotation caps unchanged.
- [x] IDX-5 map still `(collection, id) →` segment path. After F11 it is
      rebuilt from envelopes, not from JSON `id` fields.
- [x] `bazel test //...` green. Owner has reviewed the diff.

**Review:** journal I/O. Stop.

---

## F12. Projector and `find`

**Status:** done

**Codes:** IDX-8 analogue for Projector, QRY-2, QRY-7, LIB-10, `13` §3.

Close only after F11. The `Projector` trait stays handwritten in `src/`.
`Projector.schema` returns `api.v1.IndexSchema` (`index.proto`). `IndexDoc`
stays a Rust ingest type. Wire `add` / `replace` / `remove` / `reindex` /
`find`.

- [x] `Projector` is exported next to `Embedder`. `open` accepts
      `Option<Arc<dyn Projector>>` (or the binding's equivalent).
- [x] Tantivy schema is library identity columns plus `Projector.schema()`.
      Changing Projector schema is an Index format change (`reindex`).
- [x] `project` runs on index, not as a write gate. Failure → omit from
      lexical Index; file remains; journal id map still updated.
- [x] `Filter` on a name absent from Projector schema + identity →
      `invalid`. Typed leaves match the declared kind (range on text is
      `invalid` unless `13` already allowed it for that Tantivy type).
- [x] Fixture: two Records, Projector that exposes one string column;
      `term` on that column hits one; `term` on `id` still hits; unknown
      column → `invalid`.
- [x] No Projector: `all` / `id` still work; other columns `invalid`.
- [x] `bazel test //...` green. Owner has reviewed the diff.

**Review:** `Projector` + `find`. Stop. Do not wire `search` in this stage.

---

## F13. Projector and `search`

**Status:** done

**Codes:** IDX-6–9, QRY-3, QRY-6, LIB-11, `13` §5–6.

Close only after F12.

- [x] FTS columns are Projector slots marked fts, not Schema `exclude_fts`.
- [x] Vector slots are Projector slots marked vector; one slot, one vector;
      digest skip unchanged. No Projector → no vector slots; embedder is
      not called on write.
- [x] Optional `Filter` on `search` uses the same column rules as `find`.
- [x] Reranker still receives `Record`; it now has `raw`, not `fields`.
- [x] Fixture: token query hits FTS slot; synonym query hits vector slot
      when an embedder is present (existing C9 quality class).
- [x] `bazel test //...` green. Owner has reviewed the diff.

**Review:** `search`. Stop.

---

## F14. Drop field Schema

**Status:** done

**Codes:** SCH-4–13 retired; LIB-21; `api/v1` `Schema` → declaration.

Close only after F13.

- [x] On disk: `create_collection` writes `.collection.yaml` without
      `fields`. `open` / `check` refuse `.schema.yaml` and refuse a
      `fields` key (`invalid`).
- [x] Proto: collection declaration message without `Field`, `Edge`,
      `Body`, `Object`, `Text`. `CollectionInfo` nests that message.
      Generate. `Collection.schema()` is renamed or replaced (`decl` /
      the declaration message — pick in this stage, write rustdoc, no
      `docs/` citation in comments).
- [x] Grep of `rust/scheda/src` has no YAML frontmatter splitter, no
      field-type match, no edge `strict` write check.
- [x] Isolated / encrypted / journal caps / exclude_* index flags still
      round-trip on the declaration.
- [x] `bazel test //...` green. Owner has reviewed the diff.

**Review:** Schema removal. Stop.

---

## F15. CLI

**Status:** done

**Codes:** `14`, AGT-2–5 as they read after F7.

Close only after F14.

- [x] `add` / `replace` read bytes from `--file` or stdin. No `--fields`
      JSON object as the Record body.
- [x] `get` output matches F7's choice in `14`.
- [x] Default whole-file Projector on CLI `find` / `search` / `reindex` as
      F7. Library `open` from other programs does not get that default.
- [x] `create-collection` JSON matches the thin declaration. `--schema`
      is renamed or documented as declaration JSON in this stage (`14`).
- [x] `bazel test //...` green (includes `rust/cli/tests`). Owner has
      reviewed the diff.

**Review:** CLI. Stop.

---

## F16. Agent surface

**Status:** done

**Codes:** AGT-1, LIB-20, ISO-3.

Close only after F15.

- [x] `Vault.info` JSON has no field lists. Isolated collections still
      marked isolated; encrypted still marked encrypted.
- [x] Library vs CLI JSON (and `code`) match on `info` and
      `create_collection` (AGT-4).
- [x] Owner has reviewed the diff.

**Review:** `info` / agent listing. Stop.

---

## F17. `check`, `reindex`, dead code

**Status:** done

**Codes:** LIB-15, LIB-16, IDX-1, IDX-3.

Close only after F16.

- [x] `check` does not type-check consumer contents. It still checks
      declaration files, encodings (UTF-8), journal envelopes (STO-3),
      `id` uniqueness, conflict markers, Index freshness, isolation
      gitignore, encrypted armor when the handle can read it.
- [x] `reindex` walks files as bytes, calls `Projector.project` when
      present, rebuilds journal maps from envelopes.
- [x] Dead YAML/JSON Record codec, Schema field testdata, and unused
      `schema_from_*` field helpers are gone.
- [x] `bazel test //...` green. Owner has reviewed the diff.

**Review:** maintenance paths and deletions. Stop.

---

## F18. Documents once

**Status:** not started

Close only after F17 and green tests. One pass over documents that still
contradict the code. Do not polish twice.

- [ ] `01`–`10`, `13`, `14`, README, this file's dashboard statuses, and
      glossary terms match shipped behaviour (filenames, method names,
      error `code`s, defaults).
- [ ] CHANGELOG `## [Unreleased]` states the break (opaque Records,
      collection declaration, Projector, no field Schema) when this work is
      committed; this box is the note, not a separate changelog-only
      commit.
- [ ] Owner has reviewed the docs diff.

**Review:** document consistency. Stop.

---

## F19. Acceptance

**Status:** not started

Close only after F18.

- [ ] Fixture collection with four `document` files: `.yaml`, `.md`,
      `.toml`, `.txt`. `add` / `get` / `ids` round-trip. `git diff` after
      `replace` of one file shows that file's text.
- [ ] Delete `.scheda/`:`get` / `history` / `lines` / `documents` still
      match the files; `find` / `search` fail closed until `reindex`.
- [ ] With an `Projector` that exposes one string column from a YAML key,
      `find` hits that Record; a `.txt` file in the same collection is
      omitted from that column and still `get`s.
- [ ] CLI default Projector: `search` text finds a token in the `.txt`
      file.
- [ ] Encrypted and isolated collections still round-trip as after P16.
      `find` naming encrypted is `unsupported`.
- [ ] `bazel test //...` green. Owner has reviewed the fixture tests.

**Review:** acceptance. After this, `16` is done for the crate.

---

## 5. What this plan does not decide

- Crate version number for the break (`versioning` skill at release).
- Whether `open`'s Rust signature adds a fourth argument or a builder.
  F5 names the option; F12 picks the shape and rustdoc.
- Whether the collection declaration proto message is still called
  `Schema` until F14 or is renamed in F9. F14 is the rename gate.
- Default column name for the CLI whole-file Projector: F7 locked
  `body`. F15 implements that name.

---

## 6. Open questions

None that block F9. Spec F1–F8 is closed. Remaining items in §5 are
implementation choices for later F stages. If the owner rejects a locked
decision in §3, amend §3 and this file before that later stage.

---

## 7. Consumers

Out of this plan. After F19 a consumer writes `raw`, supplies an
`Projector` if it needs `find` / `search` by field, and uses the thin
collection declaration. How any one consumer does that is not an F
stage.
