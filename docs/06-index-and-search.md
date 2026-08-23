# 06. Index and search

Document 6 of the Scheda package. Version 0.7, 23 August 2026.

Normative rules for the derived Index, `find_one` / `find` / `search`, embedders, and edge walks. Terms are in `02`. Marks and edges in Schema are in `05`. The caller's method names are in `07`. Codes: `IDX`, `QRY`, `REL`. Phase **core** or **search** as marked.

An assertion without a check is not a requirement. The Index is never source of truth (STO-1, GIT-13).

---

## 1. The Index is disposable

**IDX-1. Rebuild from files.** The Index directory is `{vault}/.scheda/` (gitignored; GIT-13). It is not part of a Vault commit or backup. `reindex` reads Schema and Records and rebuilds it. *Check (**core**):* delete `.scheda/`, `reindex`; `find_one` and (when **search** is on) a fixture query set match the files.

**IDX-2. Versioned format.** The Index carries its own format version. **Core** writes `{vault}/.scheda/index/format`: one decimal integer (currently `1`) and a newline. An incompatible or corrupt file is not read “as best effort”: `reindex` or `index_unusable`. `find_one` / `find` still read Vault files. *Check (**core**):* corrupt or unknown version → `search` returns no hits or a failure, never another Vault's ids (QRY-4); `find_one` still matches the files.

**IDX-3. Invalid Records stay out of search.** Files that fail current Schema (SCH-12) are omitted from lexical, vector, and edge indexes until they validate. `find_one` still returns them with a validity flag (`07`). *Check (**search**):* tightened Schema; old file is absent from `search` and present in `find_one`.

**IDX-4. Sealed fields stay out (**later**).** Isolated collections and sealed fields (SCH-15) are not copied into the main Vault Index. *Check (**later**):* grep of Index files does not contain a fixture sealed value.

**IDX-5. Journal `id` lookup.** After **core** Index exists, `find_one` on a `journal` must not scan every segment. The Index (or an equivalent map rebuilt by `reindex`) maps `(collection, id)` → segment path. Until that map exists, a correct full scan is allowed (MOD / `03` §6). *Check (**core**, once the map is implemented):* `find_one` cost does not grow linearly with unrelated segments.

Writes (`insert_one` / `replace_one`) update this map in the same process when the writer holds the lock. Human edits in an editor are not watched by default: the caller runs `reindex` or `check`. A file watcher is **later**.

---

## 2. Point read and filter

**QRY-1. `find_one`.** Read by `(collection, id)` returns fields, body (if any), path, and file bytes. Missing Record is a failure, or a documented empty result if `07` chooses that shape — one shape per implementation. It is not a blank document. Encrypted isolated: returned bytes are decrypted (LIB-8). That clause is **later**. *Check (**core**):* round-trip `insert_one` / `find_one` matches stored fields under MOD-12.

**QRY-2. `find`.** Field filters declared in SCH-8: equality (including `null`), set membership, existence, and `document` / `singleton` path-prefix (`07` LIB-10). Applied before any ranking. Unmarked fields cannot be filtered. An empty filter (no constraints) returns every Record in the collection. *Check (**core**):* a Record that fails the filter is absent from the result list, not only from a later top-k; empty filter on a three-Record fixture returns three.

`find` does not use vectors or FTS. That is `search`.

**QRY-7. Journal append order.** On a `journal`, `find` / `scan` returns matching Records in the order they were appended: closed segments in Schema sequence, each file from first line to last, then the open segment. Rotation does not reorder earlier lines. `document` / `singleton` order is the binding's, documented, and stable for a given tree. *Check (**core**):* insert A, B; rotate; insert C; empty `find` → A, B, C in that order.

---

## 3. Lexical and vector (**search**)

**IDX-6. Lexical index.** Full-text over fields and body marked `fts` (SCH-9). An embedded engine in-process is enough; a separate search server is not required for **search**. The first implementation (Rust) may pin an FTS library in its workspace; the contract is not SQL. *Check:* an exact token in a marked field is found; the same token only in an unmarked field is not.

**IDX-7. Vector index.** Nearest neighbours over slots marked vector (SCH-9, SCH-10). Same Index directory as lexical. No required external vector database. *Check:* a query that shares no tokens with the Record but is a synonym (fixture) still ranks that Record inside the requested `k` when an embedder is present.

**IDX-8. Embedder is injected.** The caller passes an embedder (a function or interface; in Rust, typically a trait). The library does not require the network to embed. No embedder → vector leg off; lexical, `find`, and writes still work. An embedder that calls the network is the caller's; core git with `auto_sync` off still GIT-8. *Check:* build and query with no embedder; a stub that returns a fixed vector fills the vector table; those runs make no unexpected connections.

**IDX-9. Incremental update.** After a successful write, the Index for that Record is updated (lexical, vector if embedder present, edges, journal map). `reindex` remains the way to repair or to pick up editor edits. *Check:* `insert_one` then `search` without a full `reindex` finds the new Record when **search** is on.

---

## 4. Hybrid `search`

**QRY-3. One call, several legs.** `search` may combine lexical hits, vector hits, and (REL) edge expansions, then merge ranks. Merge weights are a request parameter or Versioned data in the Vault, not constants in implementation source. The result is ordered `(collection, id)` plus raw per-leg scores — not a generated answer. *Check:* a fixture query set; at given `k`, all expected ids appear in the result (order of a consumer's extra reranker is out of scope).

**QRY-4. Fail closed.** If the Index is missing, corrupt, or being rebuilt, `search` returns no hits or a failure, not stale ids. *Check:* overwrite Index with junk; `search` does not return foreign ids.

**QRY-5. No pair reranker in Scheda.** Cross-encoder, listwise models, and consumer scoring stay outside. The public API does not load them. *Check:* `search` does not pull an ONNX/cross-encoder plugin by itself; no consumer ranking types in Scheda source.

**QRY-6. Filters on `search`.** The same SCH-8 filters as `find` apply before ranking (IDX / QRY-2). *Check:* a filtered-out Record is not in the hybrid candidate set.

---

## 5. Edges (**search**, walk **later**)

**REL-1.** Declared in SCH-11. A dangling `id` is not a hit on a walk.

**REL-2. One-step walk.** “Records in B referenced by field F of records matching a `find` on A.” *Check:* a three-document fixture; the walk matches a hand list.

**REL-3. Inverse.** Given B, list A that point at it without scanning all files. *Check:* on a synthetic set, inverse time does not grow linearly with unrelated Records (index, not full pass).

**REL-4. Multi-step.** Path length > 1, depth cap, no cycles. *Check:* A→B→C found at depth 2, not at depth 1. **Later.**

---

## 6. What this document does not decide

SQL as a caller API — forbidden as the main surface (`07`). An implementation may use SQL internally.

Human-edit watcher — **later**; default is `reindex`.

Second vector copy on model change — rebuild (SCH-10); keeping two generations at once is **later**.

Server-side Index for huge Vaults — replacement behind this API (`10`), not a second primary layer.
