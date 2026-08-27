# 06. Index and search

Document 6 of the Scheda package. Version 0.32, 27 August 2026.

Normative rules for the derived Index and `get`. **`find` and `search` (how they must work) are `13`.** Terms are in `02`. Marks and edges in Schema are in `05`. The caller's method names are in `07`. Codes: `IDX`, `QRY`. Phase **core** or **search** as marked.

An assertion without a check is not a requirement. The Index is never source of truth (STO-1, GIT-13).

---

## 1. The Index is disposable

**IDX-1. Rebuild from files.** The Index directory is `{vault}/.scheda/` (gitignored; GIT-13). It is not part of a Vault commit or backup. `reindex` reads Schema and Records and rebuilds it. *Check (**core**):* delete `.scheda/`, `get` and `history` of a fixture `document` still return that file; `lines` of a fixture `journal` still yields every line; `find` / `replace` of that id fail until `reindex` (IDX-10). After `reindex`, a fixture `find` matches the files.

**IDX-2. Versioned format.** The Index carries its own format version. **Core** writes `{vault}/.scheda/index/format`: one decimal integer (currently `3`) and a newline. An incompatible or corrupt file is not read “as best effort”: `reindex` or `index_unusable`. `get`, `history`, and `lines` still read Vault / git files. Methods that resolve `id` through the Index (`find`, `search`, `replace`) fail `index_unusable` or empty, never another Vault's ids. *Check (**core**):* corrupt or unknown version → `search` / `find` return no hits or a failure, never another Vault's ids (QRY-4); `get` and `lines` still match the files.

**IDX-3. Invalid Records stay out of search.** Files that fail current Schema (SCH-12) are omitted from lexical, vector, and edge indexes until they validate. `get` still returns them with a validity flag (`07`). *Check (**search**):* tightened Schema; old file is absent from `search` and present in `get`.

**IDX-4. Sealed fields stay out (**later**).** Isolated collections and sealed fields (SCH-15) are not copied into `{vault}/.scheda/` (ISO-4). Isolated Index is `{vault}/.isolated/.scheda/`. *Check (**later**):* grep of `{vault}/.scheda/` does not contain a fixture sealed value.

**IDX-5. Journal `id` lookup.** After **core** Index exists, `get` on a `journal` must not scan every segment. The Index maps `(collection, id)` → segment path. File: `{vault}/.scheda/index/journal/{name}/ids` (the collection name, same directory as the collection files, MOD-8). JSONL, one object per Record, keys `id` and `path` (vault-relative POSIX path of the segment). Format version stays `1` (IDX-2). `reindex` rewrites the files from Vault text. Missing or unreadable map: `get` scans segment files (IDX-2). Until that map exists, a correct full scan is allowed (MOD / `03` §6). *Check (**core**, once the map is implemented):* `get` cost does not grow linearly with unrelated segments.

Writes (`add` / `replace`) update this map in the same process when the writer holds the lock. Human edits in an editor are not watched by default: the caller runs `reindex` or `check`. A file watcher is **later**.

**IDX-10. Caller `id` on the filesystem: `get`, `add`, `history`.** Those three validate the slug, then use that identity against the tree (MOD-8). `get` / `history`: `document` `{name}/{id}.md`; `singleton` `{name}/record.yaml` when `id` matches Schema; `journal` the IDX-5 map to the segment (the segment is not named `{id}`). `add` creates the file or line and updates the Index. `lines` does not take a caller `id`: it walks every journal line in Schema sequence without the Index (`07` LIB-19). Every other method that needs a Record (`find`, `search`, `replace`) takes `id`s **from the Index**, then opens those files. A caller `id` on `replace` is an Index lookup, not a path. Unknown to the Index → `not_found` (do not probe `{id}.md` from the raw argument). An `id` the Index returns has already been accepted as a Record identity. *Check (**core**):* `find` `id =` an unindexed orphan file is empty; `get` and `history` of that id still use the file. *Check:* `replace` of an id not in the Index is `not_found` even if the file exists.

---

## 2. Point read and filter

**QRY-1. `get`.** Validate the slug, then read the file for that `id` (IDX-10). Returns fields, body (if any), path, and file bytes. Missing Record is a failure, or a documented empty result if `07` chooses that shape — one shape per implementation. It is not a blank document. Encrypted isolated: returned bytes are decrypted (LIB-8). That clause is **later**. *Check (**core**):* round-trip `add` / `get` matches stored fields under MOD-12. *Check:* empty / illegal slug → `invalid` before any file open.

**QRY-2. `find`.** Run a Tantivy 0.26 `Query` on the Index (IDX-10, `07` LIB-10), then open files for the `(collection, id)`s it returns. Names are a Vault method argument (empty = this Vault, non-isolated); `Collection.find` passes one name. Query argument is `api.v1.FindQuery`: required `Filter` (the same kinds as `tantivy::query`: `boolean` must / should / must_not, `term`, `term_set`, `phrase`, `range`, `exists`, …) plus `include_raw` (unset or false omits `Record.raw`; `get` always returns `raw`). Paging is `(Count, TopDocs::with_limit(count).and_offset(offset))`. Page order is QRY-7. Returns `api.v1.FindResult`: `records` (one page) and `total` (indexed hits; a missing file does not decrease it). Field `id` is Record identity, not a Schema field. No path query: the caller never supplies a Record path. A Schema field with `exclude_find` cannot appear. `id` cannot be excluded. `Filter` with `all` matches every **indexed** Record in the selected collections; the page is still `offset` / `count`. *Check (**core**):* a Record that fails the query is absent from the result, not only from a later top-k; `all` on a three-Record fixture with `count` 3 returns three and `total` 3; with `count` 1, one Record and `total` 3. *Check:* `should` of two `term`s is the union; `must` of two `term`s is the intersection. *Check:* `term` `id = "alpha"` returns that Record only when it is in the Index. *Check:* `count` `0` → `invalid`; `offset` past the last match → empty `records` and `total` still the match count. *Check:* default `find` omits `raw`; `include_raw` true fills `Record.raw`.

`find` does not use vectors. That is `search`. Markdown `body` is a lexical find key unless Schema sets `exclude_fts` (SCH-7). The concatenated BM25 blob is still `search` only.

**QRY-7. `find` order.** Exactly one selected collection and it is `journal`: append order, not score. Closed segments in Schema sequence, each file from first line to last, then the open segment. Rotation does not reorder earlier lines. Implemented as `order_by_fast_field` on an Index ordinal (segment + line, or a monotonic counter). Any other selection (`document` / `singleton`, several collections, mixed kinds): Tantivy score (`order_by_score()`; equal scores use `DocAddress`). Paging is skip/take in that order. Ordered journal read without the Index is `lines` (`07` LIB-19), not `find`. *Check (**core**):* insert A, B; rotate; insert C; `find` with `all`, `offset` 0 and `count` 3 → A, B, C. *Check:* `count` `0` → `invalid`.

---

## 3. Lexical and vector (**search**)

The Index is a **retrieval** engine an agent can recall from, not a substring demo. Grep, SQL `LIKE`, and scanning Vault files at query time are not `search`.

**IDX-6. Lexical index.** Ranked full-text (BM25 or equivalent) over `string` / `string_list` fields and body that are not `exclude_fts` (SCH-9). In-process; a separate search server is not required. Identifiers, rare tokens, and exact phrases must surface. The first implementation (Rust) pins a BM25 library (for example Tantivy); the contract is the ranking class, not a crate name, and is not SQL. *Check:* an exact token in an included field ranks that Record inside the requested `k` among distractors that lack the token; the same token only in an `exclude_fts` field is not a hit.

**IDX-7. Vector index.** In-process ANN nearest neighbours (HNSW or equivalent) over slots that are not `exclude_vector` (SCH-9, SCH-10). Same Index directory as lexical. No external vector database. At OPS-1, brute-force over every stored vector is not the implementation. The first implementation may pin an ANN library (for example USearch). *Check:* a query that shares no tokens with the Record but is a synonym (fixture) still ranks that Record inside the requested `k` when an embedder is present.

**IDX-8. Embedder is injected.** The caller passes an embedder (a function or interface; in Rust, typically a trait). The library does not require the network to embed. No embedder → vector leg off; lexical, `find`, and writes still work. An embedder that calls the network is the caller's; core git with `auto_sync` off still GIT-8. *Check:* build and query with no embedder; a stub that returns a fixed vector fills the vector table; those runs make no unexpected connections.

**IDX-9. Incremental update.** After a successful write, the Index for that Record is updated (lexical, vector if embedder present, edges, journal map). `reindex` remains the way to repair or to pick up editor edits. *Check:* `add` then `search` without a full `reindex` finds the new Record when **search** is on.

---

## 4. Hybrid `search`

**QRY-3. One call, several legs.** When both lexical and vector legs are available, `search` **must** run both and fuse ranks. Default fusion is Reciprocal Rank Fusion (RRF). The caller may pass `k` and merge weights; those weights are a request parameter or Versioned data in the Vault, not constants in implementation source. A binding may expose another fusion as an option; RRF must work without a consumer-specific constant in library source. The result is ordered `id` plus scores — not a generated answer. `Hit.collection` is output only. With a reranker, order is that score (`13`); without it, RRF. *Check:* (1) token query: expected id in `k` from the lexical leg even if the embedder is a stub; (2) synonym query with no shared tokens: expected id in `k` from the vector leg; (3) hybrid fixture: all expected ids appear at the given `k`, and a hit retrieved by both legs carries both scores.

**QRY-4. Fail closed.** If the Index is missing, corrupt, or being rebuilt, `search` returns no hits or a failure, not stale ids. *Check:* overwrite Index with junk; `search` does not return foreign ids.

**QRY-5. No ranking model in Scheda.** The crate does not load a ranking model, a cross-encoder, or an LLM. Optional: `search` takes a caller `Reranker` callback (`13`); then returned hits are already in that order. *Check:* `search` without a callback does not pull a plugin; with a stub that swaps two ids, those ids swap in `SearchHits`.

**QRY-6. Filters on `search`.** Optional `api.v1.Filter` on `SearchQuery`. When set, Tantivy runs it first on the selected collections; BM25 and USearch search only that `(collection, id)` set (`Must` + text, `filtered_search`). A filtered-out Record is not a hybrid candidate. Unset `filter` → both legs on the selected collections. *Check:* a Record that fails the filter is absent from both legs.

---

## 5. Edges

Schema may declare `edge` fields (SCH-11). Values are Record `id`s. The library has no `walk` / `walk_inverse`. The caller `get`s the source and follows ids.

---

## 6. What this document does not decide

SQL as a caller API — forbidden as the main surface (`07`). An implementation may use SQL internally.

Human-edit watcher — **later**; default is `reindex`.

Second vector copy on model change — rebuild (SCH-10); keeping two generations at once is **later**.

Server-side Index for huge Vaults — replacement behind this API (`10`), not a second primary layer.
