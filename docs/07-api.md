# 07. Library API

Document 7 of the Scheda package. Version 0.8, 23 August 2026.

Language-neutral contract: operations, arguments, outcomes, and error codes.
Message field names and constraints live in `api/v1` (Protobuf edition 2024,
protovalidate rules). Bindings (Rust first; later Python, Java, Go,
TypeScript, and others) use these names or a documented 1:1 mapping. A
caller-supplied message that fails those rules is `invalid`, before Schema,
git, or the write lock. Agent transport (`08`) is proto3 JSON of those
messages. Codes: `LIB`. Phase **core** unless marked **search** or **agent**.

Idioms of one language appear only in a sentence that names that language (`02`).

An assertion without a check is not a requirement.

---

## 1. Shape of a binding

**LIB-1. In-process library.** The caller loads Scheda in its own process. A Scheda daemon or HTTP server is not required to read or write. *Check (**core**):* a test writes and reads a local Vault (`auto_sync` off) with no second process and no network.

**LIB-2. Same operations in every language.** Required names: `open`, `init`, `clone`, `collection`, `insert_one`, `find_one`, `replace_one`, `find`, `scan`, `search`, `commit`, `push_remote`, `pull_remote`, `sync_remote`, `check`, `reindex`. Optional later: `history`, one-step `walk` / `walk_inverse` (**search**). *Check:* a binding that omits a required name is incomplete; aliases are allowed only if the docs of that binding list the mapping.

**LIB-3. Whole-record writes.** Mutation is `insert_one` or `replace_one` of the whole Record. There is no partial field patch. SQL against the Index is not a caller API. *Check:* `replace_one` on `journal` is a failure (`unsupported`).

**LIB-4. Observable `open`.** After `open`, the caller can read: Vault path, Index path (`{vault}/.scheda/index`), whether this handle holds the write lock, Schema version(s), whether a Remote exists. **Later:** whether this isolated root is encrypted (ISO-5). No process-global singleton that forbids a second Vault. Options at `open` include read vs write, `auto_sync` (GIT-10), and an embedder (IDX-8). The Index directory is not a caller option (GIT-13). **Later:** for an isolated root the encryption **secret** when `encrypt` was set at `init` (ISO-5). *Check (**core**):* two `open`s on different roots in one process. *Check:* `init` writes `.gitignore` with `.scheda/`; Index files are ignored; deleting `.scheda/` does not delete Records.

**LIB-5. Failures are coded.** Every failure has a stable code plus an optional human message. The message must not replace the code. Codes in **core** include at least: `not_found`, `invalid`, `conflict` (git markers / unique `id`), `not_fast_forward` (GIT-10, GIT-11), `locked` (STO-4), `io`, `git`, `index_unusable` (QRY-4), `unsupported` (`replace_one` on `journal`). **Later:** `crypto` (ISO-5). *Check:* negative tests assert codes, not only message text.

How a language surfaces a code is that language's affair. Examples: Rust `Result` with an error type that carries the code; Go `(value, error)`; Python exception or a result object; Java exception or a result type; TypeScript throw or a result object. The code string is what tests and `08` share.

**LIB-6. Sync is enough for core.** `insert_one`, `replace_one`, `find_one`, `find`, `scan`, `commit` must not require an async runtime. `search` with a local embedder may be synchronous in the same process. *Check (**core**):* a binding's core tests run without starting an async executor. (Rust: no Tokio required for those tests.)

---

## 2. Record I/O

**LIB-7. `insert_one(collection, record)`.** Creates a `document` file, appends a journal line, or creates a `singleton` if absent (MOD-15–17). Returns the `id`. The Record envelope must satisfy `api/v1` (`id` and `collection` present and non-empty) before Schema (SCH-12). Does not `commit`, `push_remote`, or `pull_remote`. *Check:* `insert_one` with empty `id` → `invalid`. *Check:* round-trip with `find_one`.

**LIB-8. `find_one(collection, id)`.** Returns fields, body, path, raw bytes, and `valid` (false if the file exists but fails current Schema). Missing Record → code `not_found`, not an empty Record. **Later:** on an encrypted isolated root, fields/body/raw bytes are the **decrypted** Record (STO-2 plaintext), not the age armor on disk. *Check:* missing id → `not_found`; invalid old file → `valid: false` and bytes still present (SCH-12). *Check (**later**):* encrypted fixture → `find_one` bytes match `age -d` of the path.

**LIB-9. `replace_one(collection, id, record)`.** Whole `document` or `singleton` only. `journal` → `unsupported`. The Record envelope must satisfy `api/v1` before Schema (SCH-12). *Check:* `replace_one` with empty `id` on the Record → `invalid`. *Check:* journal call fails; `find_one` matches the new record. **Later:** on an encrypted isolated root, the file at the path is age armor, not plaintext YAML.

**LIB-10. `find(collection, filter)` / `scan(collection)`.** SCH-8 filters. List of Records (or ids plus optional bodies per request flag). No ranking. Empty filter or `scan` = every Record in the collection. Journal order is append order (QRY-7). `scan` is `find` with an empty filter; a binding may expose only one name if the other is a documented alias. *Check:* QRY-2, QRY-7.

**LIB-11. `search(query)`.** Hybrid retrieval (QRY-3–6). Input: text, optional collection scope, optional `find` filter, `k`, optional merge weights. Output: ordered hits with `(collection, id)`, path, per-leg scores. **Search.** *Check:* fixture ids all appear at the given `k`.

---

## 3. Git and maintenance

**LIB-12. `commit(message)`.** GIT-2. If `auto_sync` is on, then `push_remote` (GIT-10, GIT-12).

**LIB-13. `init(path)` / `clone(url, path)` / `open(path)`.** GIT-9. `clone` may be spelled `init(..., remote_repo=url)` in a binding if the path is not already a repository. **Later:** isolated roots: `init` / `open` only, never `clone` (ISO-6). Isolated `init` may set `encrypt`; then `open` requires `secret` (ISO-5, ISO-7).

**LIB-14. `pull_remote` / `push_remote` / `sync_remote`.** GIT-11–12. Remote name optional (default `origin`). **Later:** on an isolated handle these fail (ISO-6).

**LIB-15. `check`.** Validates Schema, encodings (STO-2), journal lines (STO-3), `id` uniqueness, conflict markers (GIT-4). **Later:** on an encrypted isolated root, validates age armor (ISO-8) and, when `secret` is present, that files decrypt. Does not write Records. Reports each problem with a code and path.

**LIB-16. `reindex`.** IDX-1. Returns counts: Records read, skipped invalid, segments seen.

**LIB-17. `history(path)`.** GIT-3. **Core** for `document` / `singleton` / segment files. Line-level journal blame is **later**.

**LIB-18. Canonical journal line.** `insert_one` on `journal` writes one object per line whose object key order is RFC 8785 (JSON Canonicalization Scheme) so a consumer can hash bytes. *Check:* two inserts of equal fields produce equal line bytes aside from field values that differ.

---

## 4. What this API refuses

- Partial field patch.
- Delete of a journal line; delete of a `document` may exist later as an explicit `delete_one` (not **core**).
- Cross-Vault query unless the caller passes two handles into a combined helper (**later**).
- Loading a pair reranker (QRY-5).

---

## 5. First implementation (Rust)

The crate `scheda` is the first binding. It must:

- map every LIB operation above;
- use `Result` with a typed error that includes LIB-5 codes (no `panic!` / `unwrap` / `expect` / `assert!` on the library path; tests may assert);
- carry `[lints] workspace = true` as in this repository.

That panic policy is **Rust**, not a requirement on other languages (Python, Java, Go, TypeScript, and others use their usual error conventions, still with LIB-5 codes).

---

## 6. What this document does not decide

How a binding wraps generated types (builders, `Result`, exceptions) as long
as proto field names and LIB-2 operation names hold.

Isolated `encrypt` / `secret` / age armor — `09` (**later**).
