# 07. Library API

Document 7 of the Scheda package. Version 0.63, 27 August 2026.

Language-neutral contract: operations, arguments, outcomes, and error codes.
Message field names and constraints live in `api/v1` (Protobuf edition 2024,
protovalidate rules). Bindings (Rust first; later Python, Java, Go,
TypeScript, and others) use these names or a documented 1:1 mapping. A
caller-supplied message that fails those rules is `invalid`, before Schema,
git, or the write lock. Agent transport (`08`) is the Protobuf JSON mapping
of those messages. Codes: `LIB`. Phase **core** unless marked **search** or **agent**.

Idioms of one language appear only in a sentence that names that language (`02`).

An assertion without a check is not a requirement.

---

## 1. Shape of a binding

**LIB-1. In-process library.** The caller loads Scheda in its own process. A Scheda daemon or HTTP server is not required to read or write. *Check (**core**):* a test writes and reads a local Vault (`auto_sync` off) with no second process and no network.

**LIB-2. Same operations in every language.** Required names: `open`, `info`, `collection`, `add`, `get`, `replace`, `find`, `search`, `begin`, `commit`, `discard`, `push`, `pull`, `sync`, `check`, `reindex`. **Agent** also requires `create_collection` (LIB-21). AGT-1 listing is `info` (LIB-4, LIB-20), not a separate `catalog` method. Optional: `history` (core in this binding). There is no `init`, `clone`, `catalog`, `walk`, or `walk_inverse` as a public name: creating or cloning a Vault is `open` (LIB-13, GIT-9). *Check:* a binding that omits a required name is incomplete; aliases are allowed only if the docs of that binding list the mapping.

**LIB-3. Whole-record writes.** Mutation is `add` or `replace` of the whole Record. There is no partial field patch. SQL against the Index is not a caller API. *Check:* `replace` on `journal` is a failure (`unsupported`).

**LIB-4. Observable `open`.** After `open`, `info` returns `api.v1.VaultInfo`: Vault path, Index path (`{vault}/.scheda/index`), `remote` (GIT-7: `origin` or empty), `auto_sync` on this handle, and the collection list. The Vault root is the only filesystem path the caller supplies. **Later:** isolated collections listed with `schema.isolated` (ISO-3); whether `{vault}/.isolated/` is encrypted (ISO-5). No process-global singleton that forbids a second Vault. Options at `open` include `auto_sync` (GIT-10), `remote_repo` (GIT-9), and an embedder (IDX-8). **Later:** `encrypt` / `secret` for `.isolated/` (ISO-5, ISO-7). The Index directory is not a caller option (GIT-13). `open` does not take the write lock; mutating calls take it for that write. *Check (**core**):* two `open`s on different roots in one process. *Check:* creating a Vault writes `.gitignore` with `.scheda/` and `.isolated/`; Index files are ignored; deleting `.scheda/` does not delete Records. *Check:* `open` with network disabled still reports `origin` on `info.remote` when that remote is configured (GIT-7).

**LIB-5. Failures are coded.** Every failure has a stable code plus an optional human message. The message must not replace the code. Codes in **core** include at least: `not_found`, `invalid`, `conflict` (git markers / unique `id`), `not_fast_forward` (GIT-10, GIT-11), `locked` (STO-4), `io`, `git`, `index_unusable` (QRY-4), `unsupported` (`replace` on `journal`). **Later:** `crypto` (ISO-5). *Check:* negative tests assert codes, not only message text.

How a language surfaces a code is that language's affair. Examples: Rust `Result` with an error type that carries the code; Go `(value, error)`; Python exception or a result object; Java exception or a result type; TypeScript throw or a result object. The code string is what tests and `08` share.

**LIB-6. Sync is enough for core.** `add`, `replace`, `get`, `find`, `begin`, `commit`, `discard` must not require an async runtime. `search` with a local embedder or reranker may be synchronous in the same process. *Check (**core**):* a binding's core tests run without starting an async executor. (Rust: no Tokio required for those tests.)

---

## 2. Record I/O

**LIB-7. `add(id, record)`.** On the collection handle. `id` is a method argument; the library checks the slug (Unicode letter or digit, then letters, digits, combining marks, `.`, `_`, `-`, `+`, `(`, `)`, space, or `@`; at most 255 bytes; not a Windows device name such as `CON`; no leading/trailing whitespace; no trailing `.`) before Schema (SCH-12). Argument `record` is `api.v1` `RecordAdd` (`fields`, `body`). Creates a `document` file, appends a journal line, or creates a `singleton` if absent (MOD-15–17), then updates the Index (IDX-10). Success is `()`. Same `id` already present → `conflict`. Collection is the handle, not a field. Outside a transaction, then one git commit with message `scheda` (GIT-2). Inside a transaction, does not `commit`. Does not `pull`. *Check:* `add` with empty `id` → `invalid`. *Check:* `id` starting with `.` or `-`, or containing `$` / `;` → `invalid`. *Check:* trailing space → `invalid`. *Check:* round-trip with `get`. *Check:* without `begin`, `git log` gains one commit.

**LIB-8. `get(id)`.** On the collection handle. Validate the slug (same rules as LIB-7), then open the Record file for that identity (IDX-10, QRY-1). Does not consult the Index for `document` / `singleton` (journal uses the IDX-5 map to find the segment). Returns `api.v1` `Record`: `collection` (output only), `id`, fields, body, path, raw bytes, and `valid` (false if the file exists but fails current Schema). Missing Record → code `not_found`, not an empty Record. **Later:** on an encrypted isolated collection, fields/body/raw bytes are the **decrypted** Record (STO-2 plaintext), not the age armor on disk. *Check:* missing id → `not_found`; invalid old file → `valid: false` and bytes still present (SCH-12). *Check:* illegal slug → `invalid` and no file open. *Check (**later**):* encrypted fixture → `get` bytes match `age -d` of the path.

**LIB-9. `replace(id, record)`.** On the collection handle. Resolve `id` in the Index first (IDX-10); unknown → `not_found`. Do not open `{id}.md` from the raw argument. Argument is `api.v1` `RecordReplace` (`fields`, `body`); `id` is the method argument, not a field. Whole `document` or `singleton` only. `journal` → `unsupported`. Autocommit rules are the same as LIB-7 (GIT-2). *Check:* empty `id` → `invalid`. *Check:* journal call fails; `get` matches the new record. *Check:* file on disk but not in the Index → `not_found`. **Later:** on an encrypted isolated collection, the file at the path is age armor, not plaintext YAML.

**LIB-10. `find(collections, query, offset, count)`.** On the Vault, and on the collection handle as `find(query, offset, count)` (that form calls the Vault method with that collection's name). `collections` is a set of names. Empty → every non-isolated collection on this handle. A named set: unknown name → `not_found`; isolated → `unsupported` (same as `collection(name)`). An explicit name whose Schema cannot accept the `Filter` (SCH-8) → `invalid`. Empty set: collections that cannot accept the `Filter` are omitted. Run a Tantivy 0.26 `Query` on the Index, then open files for the matching `(collection, id)`s (IDX-10). Query argument is `api.v1` `FindQuery` (`find.proto`): required `Filter` (the same kinds as `tantivy::query`: `all`, `empty`, `term`, `term_set`, `phrase`, `phrase_prefix`, `fuzzy`, `regex`, `regex_phrase`, `range`, `exists`, `boolean` with `must` / `should` / `must_not`, `boost`, `const_score`, `disjunction_max`, `more_like_this`) plus `include_raw` (unset or false omits `Record.raw`; `get` always returns `raw`). Collector is `(Count, TopDocs::with_limit(count).and_offset(offset))` (not a field on `FindQuery`). Order: one `journal` collection → append order (QRY-7: `order_by_fast_field` on the indexed ordinal); otherwise Tantivy score (`order_by_score()`). `count` `0` is `invalid` (Tantivy panics). Returns `api.v1` `FindResult` (same file): `records` (length at most `count`) and `total` (Tantivy `Count`: indexed hits matching the Filter). A missing file is omitted from the page and does not decrease `total`. Past the end → empty `records`; `total` is still the match count. Every indexed Record in the selected collections is `Filter` `all`. There is no `scan` method. Collection is not a field on `Filter` / `FindQuery`. `Record.collection` is output only. *Check:* `should` of two `term`s on different fields returns the union. *Check:* `must` of two `term`s returns the intersection. *Check:* `count` `0` → `invalid`; `offset` beyond the last match → empty `records`. *Check:* empty `collections` on a two-collection fixture returns both; a one-name set returns only that collection. *Check:* `all` on a three-Record fixture with `count` 1 returns one Record and `total` 3. *Check:* default `find` omits `raw`; `include_raw` true fills `Record.raw`.

**LIB-11. `search(collections, query, reranker?)`.** Hybrid retrieval (`13`): BM25 and ANN when both are on, RRF, then optional rerank. On the Vault, and on the collection handle as `search(query, reranker?)` (that form calls the Vault method with that collection's name). `collections` is the same set as LIB-10. Argument is `api.v1` `SearchQuery` plus an optional caller `Reranker` (`score(query_text, record)` → float; not proto). `k` unset or `0` → 10. Weights unset → `1`; **zero turns that leg off**. `filter` set → Tantivy first on the selected collections; both legs on that `(collection, id)` set. Fusion pool is at least `max(k, 50)`. With a reranker the library opens those Records, scores, sorts, returns `k` with `rerank_score`. Without it, RRF order and no `rerank_score`. `include_raw` true fills `Hit.raw` (file bytes); unset or false omits it. Output is `api.v1` `SearchHits`. `Hit.collection` is output only (same as `Record.collection`). Scheda does not load a ranking model. **Search.** *Check:* QRY-3 fixture ids all appear at the given `k`; a hit from both legs carries both scores. *Check:* negative or non-finite weight → `invalid`. *Check:* `filter` that excludes a Record keeps it out of both legs. *Check:* reranker that inverts two hits is reflected in hit order. *Check:* empty `collections` on a two-collection fixture can hit both; a one-name set does not. *Check:* default `search` omits `Hit.raw`; `include_raw` true fills it.

**LIB-20. Catalog is `info`.** AGT-1 listing is `Vault.info` (`api.v1.VaultInfo`: nested `Schema` per collection). Same content as Schema files (`05` §8): collections, kinds, fields, index marks, edges. Not inferred from directories. No `catalog()` method and no request envelope. Isolated collections are listed with `schema.isolated` and without `schema.fields` (ISO-3). **Agent.** *Check:* AGT-1.

**LIB-21. `create_collection(name, schema)`.** Write one collection declaration. Argument is the collection name plus `api.v1` `Schema`. Schema has no layout and no Record path. A message that fails `api/v1` is `invalid` before Schema. Then SCH-2–SCH-14 (same rules as load). Needs the write lock. Writes `{name}/.schema.yaml`, or replaces an existing `.schema.yaml` / `.schema.yml` / `.schema.json` in that directory. `name` is the collection name and the on-disk directory (MOD-8). Until nested `{vault}/.isolated/` I/O (ISO-2), `schema.isolated` is `unsupported` and no file is written. **Later:** `schema.isolated` → Record files under `{vault}/.isolated/{name}/`; creates the nested git if needed; then `collection(name)` is the handle for those Records. After a successful write, `info` and `collection` on this handle see the new Schema (no reopen). Returns the live `Collection` handle. Autocommit rules are the same as LIB-7 (GIT-2). **Agent.** *Check:* `create_collection` then `add` on that collection without reopen; a leftover `layout` key or an extra Schema key → `invalid` and no file. *Check:* `isolated: true` → `unsupported` and no file.

---

## 3. Git and maintenance

**LIB-12. `commit(message)`.** GIT-2. Ends the transaction. `invalid` if none is open, or if the message is empty, or if there is nothing to commit. If `auto_sync` is on, then `push` (GIT-10, GIT-12).

**LIB-22. `begin()`.** GIT-2. Enter a transaction. Needs the write lock. Discards leftover Scheda writes first. `invalid` if a transaction is already open.

**LIB-23. `discard()`.** GIT-2. Restore Scheda-recorded paths to HEAD, end the transaction if any, reload Schema, and rebuild the Index. Needs the write lock. Allowed without an open transaction (crash leftovers).

**LIB-13. `open(path)`.** GIT-9. One public call. Path exists as a Vault → open (`remote_repo` ignored). Path missing and `remote_repo` set → `git clone`. Path missing → `git init`, no Remote. Path exists but is not a Vault → `invalid`. **Later:** `{vault}/.isolated/` is created by Scheda, never cloned (ISO-6). `OpenOptions.encrypt` applies when that nested tree is first created; isolated Record I/O then requires `secret` (ISO-5, ISO-7). `open` of the Vault does not require `secret`.

**LIB-14. `pull` / `push` / `sync`.** GIT-11–12. Always `origin`; there is no remote-name argument. **Later:** these never publish `{vault}/.isolated/` (ISO-6).

**LIB-15. `check`.** Validates Schema, encodings (STO-2), journal lines (STO-3), `id` uniqueness, conflict markers (GIT-4) for non-isolated collections. Isolated Record files are not on this handle until ISO-2; `check` does not scan them yet. **Later:** parent git must not track `.isolated/`; nested isolated git must have no Remote (ISO-2, ISO-6); if `.isolated/` is encrypted, validates age armor (ISO-8) and, when `secret` is present, that files decrypt. Does not write Records. Reports each problem with a code and path.

**LIB-16. `reindex`.** IDX-1. Returns `api/v1` `ReindexResult`: Records read, skipped invalid, segments seen. Needs the write lock. Writes Index format version `3` as `{vault}/.scheda/index/format` (one decimal integer and a newline) and the journal `id` map (IDX-5). **Later:** if `{vault}/.isolated/` exists, also rebuilds `{vault}/.isolated/.scheda/`; main Index never copies isolated Records (ISO-4).

**LIB-17. `history(id, offset, count)`.** GIT-3. On the collection handle. Same identity path as `get` (IDX-10): validate the slug, then git history of that file. Does not ask the Index which ids exist. **Core** for `document` / `singleton`. For `journal`, IDX-5 map then git history of the **segment file** (the whole file in `Entry.raw`); line-level blame is **later**. The caller passes Record `id`, not a file path. Missing file → `not_found`. Paged, newest first: `offset` skips that many commits, `count` is the page size (`invalid` if `count` is 0). Returns `api/v1` `History` with `repeated Entry`: commit hash, `google.protobuf.Timestamp` `time` (UTC; git stores whole seconds since epoch), author, message, and file bytes (`raw`). Isolated collections use the nested git. Past the end of history → empty `entries`. *Check:* two commits editing one `document`; snapshots match `git show` of the file Scheda stores. *Check:* `count` `0` → `invalid`; `offset` beyond the last commit → empty `entries`; unknown `id` → `not_found`. *Check:* delete `.scheda/`; `history` of a fixture `document` still matches `git show`.

**LIB-18. Canonical journal line.** `add` on `journal` writes one RFC 8785 object per line (JSON Canonicalization Scheme: keys in UTF-16 code unit order, numbers as ECMAScript `NumberToString`, strings escaped as the RFC states) so a consumer can hash bytes. `RecordAdd.fields` is `google.protobuf.Struct`; a JSON number there is `number_value` (IEEE 754 binary64). An `integer` field is accepted only when that double is a finite integer whose magnitude is at most 2^53. A larger magnitude is `invalid`; this API does not carry a wider integer. On disk the integer is the decimal digits (`1`), not `1.0`. *Check:* two inserts of equal fields produce equal line bytes aside from field values that differ. *Check:* `integer` `1` is stored as `1`; `1.5` and magnitude above 2^53 are `invalid`.

---

## 4. What this API refuses

- Partial field patch.
- A Record file path from the caller. `get` / `add` / `history` take a validated `id` and the library opens, creates, or gits the file. `find` / `replace` / `search` take `id`s from the Index (IDX-10).
- Delete of a journal line; delete of a `document` may exist later as an explicit `delete_one` (not **core**).
- Cross-Vault query unless the caller passes two handles into a combined helper (**later**).
- Shipping a ranking model in the library (QRY-5). An optional reranker **callback** on `search` is allowed (`13`).

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
