# 13. Find and search

Document 13 of the Scheda package. Version 0.42, 4 October 2026.

How `find` and `search` must work. Messages: `api/v1/find.proto`, `api/v1/search.proto`, `api/v1/index.proto`. Index columns: `Projector.schema(collection)` at `open`, not collection-declaration keys (`05`). This document is the target. Implementations that concatenate all columns into one vector, or treat `search` as a query string only, are wrong.

Files are source of truth. The Index is disposable (`{vault}/.scheda/`, gitignored). Wipe it, `reindex`, same Records.

---

## 1. Two methods

| | `find` | `search` |
|---|---|---|
| Call | Vault `find(collections, query, offset, count)`. Collection `find(query, offset, count)` calls that with its name. | Vault `search(collections, query, reranker?)`. Collection `search(query, reranker?)` calls that with its name. |
| In | `api.v1.FindQuery` (`Filter` required; `include_raw` default false) plus a set of collection names (empty = this Vault, non-isolated and non-encrypted) | `api.v1.SearchQuery` (`text` required; `include_raw` default false) plus the same name set |
| Out | `api.v1.FindResult` (`records` plus `total`; `Record.collection` on each hit; `Record.raw` only if `include_raw`; each `Record` as LIB-8) | `api.v1.SearchHits` (`collection`, `id`, scores; `rerank_score` if reranker; `raw` only if `include_raw`) |
| Engine | Tantivy Query only | BM25 (Tantivy) **and** ANN (USearch), fuse, then optional rerank |
| Vectors | no | yes, if an embedder is set and the Projector marks vector columns |

Names are a method argument, not a field on `FindQuery` / `SearchQuery`. No Record path on the request. `Record.collection` and `Hit.collection` are output only. No `scan`: every indexed Record in the selected collections is `Filter` `all`. Unknown name → `not_found`. Isolated name on this handle → an ordinary query. Encrypted name → `unsupported`. Empty set stays non-isolated and non-encrypted.

`get` / `add` / `history` / `lines` / `replace` / `remove` do not wait on the Index: they use the Record file (a journal `get`, the `id` map, which writes update at once). `find` / `search` take `id`s from the Index, then open files. The Index follows the last commit in the background (`06` IDX-9), so `find` / `search` are eventually consistent: a write is found after its commit has been indexed. A caller that must see its own write calls `wait_indexed` first; `index_status` says how far behind the Index is. Inside a transaction, queries do not see its uncommitted writes. Orphan or uncommitted file: `get` works; `find` / `search` skip it until a commit holds it and the indexer has applied it. Ordered journal read without the Index is `lines` (`07` LIB-19), not `find` and not `scan`.

---

## 2. Who writes the Index

The **library** calls the engine APIs. The caller never writes Tantivy or USearch files.

- **Tantivy** creates and owns `{vault}/.scheda/index/<generation>/lexical/` (`Index::create_in_dir` / open). Scheda names identity columns and Projector columns, then adds documents. On-disk layout is Tantivy’s (crate pin), not a Scheda spec.
- **USearch** creates and owns the vector dump under `{vault}/.scheda/`. Scheda only passes dimensions, keys, and `f32` arrays. On-disk layout is USearch’s.
- Scheda writes `{vault}/.scheda/index/<generation>/format` (decimal version) and `{vault}/.scheda/index/current` (live generation name). Wrong version → `index_version` / empty `find`/`search`, not foreign ids.

The background indexer (`06` IDX-9) or `reindex` creates the engines if missing and keeps them on `HEAD`; writes do not touch them. No LLM is used to create either index.

---

## 3. `find`

### Projector

Not a model inside the library. Not an `api/v1` field. Same injection class as Embedder: the program that links the library passes it at `open`.

```text
schema(collection) → api.v1.IndexSchema   // columns of that collection:
                                   // name, kind (text / integer / number /
                                   // boolean / text list), find / fts /
                                   // vector marks
project(collection, id, raw) → IndexDoc
                                   // values for schema(collection);
                                   // omit a column = missing, not null
                                   // unless the value is present
```

Unknown collection on `schema`: empty columns or an error; both are allowed. Same column `name` may differ in kind across collections. `add` does not call `project` to accept or reject the write. `project` error or empty omits the Record from lexical and vector indexes; `get` still returns the file; a journal id map entry remains (IDX-3).

No Projector: the Index holds identity (`collection`, `id`, path, ordinal) only. `find` `all` and `term` on `id` work; any other field is `invalid`. `search` has no FTS/vector slots (lexical empty, vector off).

Identity columns are the library's. There is no magic `body` key unless the Projector names a column `body`.

### Filter field names

A Projector column is `(collection, name)`. Docs write `collection1.foo`. The wire does **not** parse a dotted `field` string: collection slugs may contain `.` (MOD-6). Each Filter leaf carries column `name` (`foo`) and, when needed, `collection` (a proto field on the leaf, not a dotted parse of `field`). Identity `id` / `collection` are library columns and are not qualified this way.

- **Vault `find` / `search`:** every Projector-column leaf must set `collection`. Bare `foo` is `invalid`. A qualifier that names no collection is `not_found`. A qualifier that names a collection whose `schema` has no such column, or whose kind cannot accept that leaf, is `invalid`. `schema` error on a **named** collection is that error (or `invalid` if the Projector has no code). Empty collection set: omit collections that cannot accept the Filter (empty `schema` or `schema` error counts as cannot accept).
- **Collection `find` / `search`:** `foo` means this handle's `foo`. `collection1.foo` is the same if this handle is `collection1`. `collection2.foo` on a `collection1` handle is `invalid`. A qualifier that does not exist is `invalid` on the Collection handle.

A column without the `find` mark cannot appear in a Filter leaf. `id` cannot be excluded.

### Steps

1. Validate `FindQuery` (`Filter.kind` required). `count == 0` → `invalid`. Resolve `collections` (empty → non-isolated on this handle). Apply Filter field names above. Explicit name that cannot accept the `Filter` → `invalid`. Empty set: omit collections that reject it.
2. Run that Query on the vault Tantivy index with `MUST` collection in the resolved names.
3. Collector: `(Count, TopDocs::with_limit(count).and_offset(offset))` in one `search()`. `Count` is indexed hits (`FindResult.total`).
4. Order: exactly one resolved collection and it is `journal` → append order (`order_by_fast_field` on segment+line ordinal). Otherwise score descending, equal scores broken by that same ordinal ascending (`tweak_score`), never by `DocAddress` — an address depends on segment layout, which varies between runs, so paging on it could repeat or skip a Record. Rotation does not reorder old lines.
5. Open files for those `(collection, id)`s. Return one `FindResult` page. Past the end → empty `records`; `total` is still the match count. A missing file is omitted from `records` and does not decrease `total`. Unset or false `include_raw` omits `Record.raw`; true fills it (`get` always returns `raw`).

`Filter` is a Tantivy 0.26 Query tree (`all`, `empty`, `term`, `term_set`, `phrase`, `phrase_prefix`, `fuzzy`, `regex`, `regex_phrase`, `range`, `exists`, `boolean` with `must` / `should` / `must_not`, `boost`, `const_score`, `disjunction_max`, `more_like_this`). No QueryParser string. AND = `OCCUR_MUST`, OR = `OCCUR_SHOULD`. Field `id` is Record identity (`term`, `term_set`, `range` only).

`more_like_this` has two source forms and they must produce comparable queries. With `fields`, the caller supplies the text. With `id`, Scheda reads that Record and builds the terms from the **same** full-text the Index holds for it (Projector columns marked `fts`), then hands it to the `fields` path. It does not ask Tantivy to recover the source from the Index: Scheda's full-text is indexed and not stored, so a stored-field extraction would find only Scheda's own bookkeeping — `key`, `path`, `collection` — and "similar" would collapse into "in the same collection", every Record of it tying on one score. An unknown source `id` is `not_found`. A Record that exists but yields no full-text (no `fts` columns, or `project` omitted them) matches nothing, which is an empty page rather than an error.

`find` does not call the embedder and does not use USearch.

---

## 4. Embedder

Not a model inside the library. Not a proto field.

The **program that links the library** passes a function at `open`:

```text
embed_many(texts) → [][]float32   required
embed(text) → []float32           provided: embed_many with one text
embed_query(text) → []float32     provided: embed(text); search-query role
many_limit → non-zero size
```

plus `model_id` and `dimensions` (also non-zero). No function → vector leg off; `find`, BM25, and writes still work. Network inside that function is the program’s problem.

Only the batch function is required, and the single-text one is the batch path with one element. A program may override it, but then the same text must give the same vector whether it travelled alone or inside a padded batch — a wrong attention mask under padding splits the vector space and raises nothing. `embed_query` defaults to `embed`: a symmetric encoder does nothing extra. `search` calls `embed_query` for the query string and still uses `embed` / `embed_many` for Projector vector-column texts.

Zero is not representable in either size. Absence of a vector leg is no embedder at all, not a zero `dimensions`.

CLI hook (`14`): `--embedder-openai-url` plus `--embedder-model` and
`--embedder-dimensions` (OpenAI `POST` `{URL}embeddings`), or
`--model2vec-embedder DIR`. All three HTTP flags or none. Unset → lexical
only. The CLI is that program; Scheda still does not ship weights.

Scheda does not ship weights or a chat LLM.

---

## 5. Building vectors (background indexer and `reindex`)

For each Record, for **each** Projector column marked `vector` that `project` filled:

1. Take that column’s text (`text list`: join values for that column).
2. Hash that exact string as a git blob (SHA-1 of `blob {len}\0{bytes}`, the same digest git stores for file content). Compare it to the digest already stored for that `(collection, id, column)` slot in `keys.json`.
3. If the digest matches and the Index `model_id` and dimensions match the current embedder, leave the USearch entry untouched — do not call the embedder.
4. Otherwise call `embed` when that Record has one non-empty vector column that needs a new vector, or when `many_limit` is 1. Otherwise call `embed_many` in chunks of `many_limit` for the slots that changed or are new. A leftover chunk of one text uses `embed`.
5. Store one vector in USearch. Key: `(collection, id, column name)`. Write the digest next to the slot in `keys.json`. An Index written before digests existed loads with an empty digest (`serde` default) and re-embeds each slot once.
6. A slot that is gone — `schema` no longer marks the column `vector`, or this Record's `project` omits it — is removed from USearch. `search` must not return that content. USearch `remove` leaves tombstones, so the dump can grow across many incremental cycles; this release does not compact it.

Do not glue several columns into one blob. One column, one vector. Do not hash the Record file or a git tree: the digest is of the embedder text, so a commit that changes other bytes of the Record does not re-embed it.

If `embed` is missing, skip all vector writes. Tantivy still indexes `fts` columns.

Index remembers `model_id` and `dimensions`. Only a handle whose embedder matches them writes vectors (the `vector.lock` role, `06` IDX-9). A handle with another model writes none, does not embed its queries, and its `search` runs on the lexical leg alone; `index_status().model_mismatch` is true. Moving to a new model is the caller's `reindex` with the new embedder: the new generation starts with an empty vector table and every slot is embedded. With the same model, `reindex` copies the previous generation's vector dump (when it loads) and applies the skip/remove rules above, so only changed text is embedded; without an embedder it carries the previous vectors over unchanged. The lexical Index of a new generation is always built in full.

The embedder runs on the background indexer's thread, never inside `add` / `replace` / `commit`: a slow, blocking, or failing embedder delays vectors and is retried with backoff (`index_status().last_error`), but fails no write.

---

## 6. `search`

`SearchQuery.text` is the query. Not a vector **instead of** text. The library embeds that text when the vector leg is on.

`k` unset or `0` → 10. `lexical_weight` / `vector_weight` unset → 1; **0 turns that leg off**. Without a reranker those weights also scale Reciprocal Rank Fusion (order and truncation). With a reranker they no longer decide pool membership: each running leg's ranks fill the pool by alternation.

Without a Projector, there are no FTS/vector slots: step 2 has nothing to rank, step 3 is off.

Steps:

1. Resolve `collections` as in `find`. If `filter` is set: Tantivy runs `Filter` (same field-name rules as §3) → allow-set of `(collection, id)`s. Both legs search **only** that set. Unset `filter` → the selected collections, not other collections in the same Index.
2. Lexical (weight > 0): Tantivy `Must(collections)` plus BM25 on Projector columns marked `fts` (`Must(filter)` if filtered). Top pool by score.
3. Vector (weight > 0 and embedder set): `embed_query(text)` once. USearch `filtered_search` on the allow-set and selected collections (predicate during HNSW, not drop-after-top-k). The HNSW walk width is a binding floor (search expansion), not the caller's `k`; the engine retrieves at least that many slots, then truncates to the fusion pool, so asking for a deeper page does not reshuffle the head of a shallower one (IDX-7). Tiny allow-set may brute-force those vectors only, never the whole table at OPS-1.
4. Fuse. Candidate pool is **larger than `k`**: at least `max(k, 50)` (or all fused hits if fewer). When a reranker is passed and `rerank_limit` is set above that floor, the pool is `max(k, 50, rerank_limit)` and both Index legs retrieve to that depth. Reciprocal Rank Fusion scores are always computed and reported on each hit.
   - **No reranker:** weighted RRF both orders the fused list and truncates it to the pool (ties: collection, then id). That order is the page.
   - **Reranker present:** RRF does not decide membership. Fill the pool by alternating legs — vector rank 1, lexical rank 1, vector rank 2, lexical rank 2, and so on — skipping a document already taken, until the pool is full or both legs are exhausted. A short leg does not leave the pool short while the other still has hits. When both weights are non-zero the interleave is one-for-one; the weights no longer choose who enters the pool.
5. If a **reranker** is passed to `search`: cut the fused list to `rerank_limit` when it is set, then `get` each remaining Record; call `score` when there is one candidate or `many_limit` is 1; otherwise `score_many` in chunks of `many_limit` (a leftover chunk of one Record uses `score`). Sort by that score (higher first); keep `k`. Set `Hit.rerank_score`. If no reranker: return the fused list truncated to `k` (no `rerank_score`); `rerank_limit` does not change the pool.
6. `include_raw` true: fill `Hit.raw` from the Record file (after truncate to `k` when there is no reranker). Unset or false → omit `raw`.

Invalid / missing Index → no hits or an Index code (`index_version`, `index_lexical`, `index_missing`, `index_corrupt`), never another Vault’s ids.

### Reranker

Not a model inside Scheda. Not a proto field. Same idea as the embedder: the **program that links the library** passes a function on `search`:

```text
score_many(query_text, records) → []float   required
score(query_text, record) → float           provided: score_many with one Record
many_limit → non-zero size
```

`None` → order is RRF. Network inside that function is the program’s problem. The library does not load a ranking model (QRY-5).

**How many candidates it sees is the caller’s choice.** Fusion depth and rerank depth answer two different questions: fusing is cheap and wants to be wide, while a cross-encoder costs tens of milliseconds per pair on a CPU and wants to be as narrow as the caller can accept. Unset `SearchQuery.rerank_limit` keeps the pool at `max(k, 50)` and scores every fused hit. Set to N, both legs retrieve `max(k, 50, N)` and the reranker scores the first N of that fused list (or every hit that exists if the corpus is smaller), before the library opens those Records. With a reranker that fused list is the alternating-leg pool, not the RRF ranking: RRF rewards agreement between legs, which can bury a document only one leg ranked first, and the reranker never sees a document that missed the pool. A large N is more BM25 and ANN retrieval and more Record reads, not only more model calls. `0` is `invalid`, because scoring nothing is what passing no reranker already means. A value below `k` is `invalid`, because a page where some hits carry `rerank_score` and some do not cannot be told from a truncated one. Without a reranker the field is ignored and the pool stays `max(k, 50)`.

CLI hook on `search` (`14`): `--reranker-cohere-url` plus
`--reranker-model` (Cohere/Jina `POST` `{URL}rerank`). Both HTTP flags
or none. Unset → RRF.

---

## 7. What this is not

- SeekStorm query string (`+ - ""`) as `Filter`.
- One vector per Record from concatenated columns.
- `search` input = raw query vector only (optional extra vector later; text stays).
- Embedder invoked only at “create index” and never again (it runs on every vector write and every vector `search`).
- Shipping a cross-encoder or LLM inside the crate (the reranker callback is the caller’s).
- `walk` / `walk_inverse` methods.
- A dotted `field` string (`collection1.foo` as one proto string). Qualifier is a separate `collection` on the leaf.
