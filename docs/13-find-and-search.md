# 13. Find and search

Document 13 of the Scheda package. Version 0.22, 1 September 2026.

How `find` and `search` must work. Messages: `api/v1/find.proto`, `api/v1/search.proto`. Schema marks: `05` SCH-8–10. This document is the target. Implementations that concatenate all fields into one vector, or treat `search` as a query string only, are wrong.

Files are source of truth. The Index is disposable (`{vault}/.scheda/`, gitignored). Wipe it, `reindex`, same Records.

---

## 1. Two methods

| | `find` | `search` |
|---|---|---|
| Call | Vault `find(collections, query, offset, count)`. Collection `find(query, offset, count)` calls that with its name. | Vault `search(collections, query, reranker?)`. Collection `search(query, reranker?)` calls that with its name. |
| In | `api.v1.FindQuery` (`Filter` required; `include_raw` default false) plus a set of collection names (empty = this Vault, non-isolated) | `api.v1.SearchQuery` (`text` required; `include_raw` default false) plus the same name set |
| Out | `api.v1.FindResult` (`records` plus `total`; `Record.collection` on each hit; `Record.raw` only if `include_raw`) | `api.v1.SearchHits` (`collection`, `id`, `path`, scores; `rerank_score` if reranker; `raw` only if `include_raw`) |
| Engine | Tantivy Query only | BM25 (Tantivy) **and** ANN (USearch), RRF, then optional rerank |
| Vectors | no | yes, if an embedder is set |

Names are a method argument, not a field on `FindQuery` / `SearchQuery`. No Record path on the request. `Record.collection` and `Hit.collection` are output only. No `scan`: every indexed Record in the selected collections is `Filter` `all`. Unknown name → `not_found`. Isolated name on this handle → `unsupported`.

`get` / `add` / `history` / `lines` do not wait on the Index. `find` / `search` take `id`s from the Index, then open files. `replace` / `remove` do the same when `find` is on, isolated collections included; when `find` is off, and on encrypted collections either way, they use the Record file. Orphan file: `get` works; `find` / `search` skip it until indexed. Ordered journal read without the Index is `lines` (`07` LIB-19), not `find` and not `scan`.

---

## 2. Who writes the Index

The **library** calls the engine APIs. The caller never writes Tantivy or USearch files.

- **Tantivy** creates and owns `{vault}/.scheda/index/lexical/` (`Index::create_in_dir` / open). Scheda only names fields and adds documents. On-disk layout is Tantivy’s (crate pin), not a Scheda spec.
- **USearch** creates and owns the vector dump under `{vault}/.scheda/`. Scheda only passes dimensions, keys, and `f32` arrays. On-disk layout is USearch’s.
- Scheda writes `{vault}/.scheda/index/format` (decimal version). Wrong version → `index_version` / empty `find`/`search`, not foreign ids.

First write or `reindex` creates the engines if missing. `add` / `replace` update them. No LLM is used to create either index.

---

## 3. `find`

1. Validate `FindQuery` (`Filter.kind` required). `count == 0` → `invalid`. Resolve `collections` (empty → non-isolated on this handle). Explicit name whose Schema rejects the `Filter` → `invalid`. Empty set: omit collections that reject it.
2. Run that Query on the vault Tantivy index with `MUST` collection in the resolved names.
3. Collector: `(Count, TopDocs::with_limit(count).and_offset(offset))` in one `search()`. `Count` is indexed hits (`FindResult.total`).
4. Order: exactly one resolved collection and it is `journal` → append order (`order_by_fast_field` on segment+line ordinal). Otherwise score descending, equal scores broken by that same ordinal ascending (`tweak_score`), never by `DocAddress` — an address depends on segment layout, which varies between runs, so paging on it could repeat or skip a Record. Rotation does not reorder old lines.
5. Open files for those `(collection, id)`s. Return one `FindResult` page. Past the end → empty `records`; `total` is still the match count. A missing file is omitted from `records` and does not decrease `total`. Unset or false `include_raw` omits `Record.raw`; true fills it (`get` always returns `raw`).

`Filter` is a Tantivy 0.26 Query tree (`all`, `empty`, `term`, `term_set`, `phrase`, `phrase_prefix`, `fuzzy`, `regex`, `regex_phrase`, `range`, `exists`, `boolean` with `must` / `should` / `must_not`, `boost`, `const_score`, `disjunction_max`, `more_like_this`). No QueryParser string. AND = `OCCUR_MUST`, OR = `OCCUR_SHOULD`. Field `id` is Record identity (`term`, `term_set`, `range` only). Field `body` is Markdown prose on `document` (unless `no_body` or `exclude_fts`). `exclude_find` fields cannot appear. `id` cannot be excluded.

`more_like_this` has two source forms and they must produce comparable queries. With `fields`, the caller supplies the text. With `id`, Scheda reads that Record and builds the terms from the **same** full-text string the Index holds for it, then hands it to the `fields` path. It does not ask Tantivy to recover the source from the Index: Scheda's full-text field is indexed and not stored, so a stored-field extraction would find only Scheda's own bookkeeping — `key`, `path`, `collection` — and "similar" would collapse into "in the same collection", every Record of it tying on one score. An unknown source `id` is `not_found`. A Record that exists but yields no full-text (every field excluded from search) matches nothing, which is an empty page rather than an error.

`find` does not call the embedder and does not use USearch.

---

## 4. Embedder

Not a model inside the library. Not a proto field.

The **program that links the library** passes a function at `open`:

```text
embed(text) → []float32
embed_many(texts) → [][]float32
many_limit → integer > 0
```

plus `model_id` and `dimensions`. No function → vector leg off; `find`, BM25, and writes still work. Network inside that function is the program’s problem. `many_limit` of 0 is `invalid`.

CLI hook (`14`): `--embedder-openai-url` plus `--embedder-model` and
`--embedder-dimensions` (OpenAI `POST` `{URL}embeddings`), or
`--model2vec-embedder DIR`. All three HTTP flags or none. Unset → lexical
only. The CLI is that program; Scheda still does not ship weights.

Scheda does not ship weights or a chat LLM.

---

## 5. Building vectors (`add` / `replace` / `reindex`)

For each Record, for **each** field (and body) that is vector-eligible (`string` / `string_list` / body / edge text; not `exclude_vector`):

1. Take that field’s text (`string_list`: join values for that field).
2. Call `embed` when that Record has one non-empty vector field, or when `many_limit` is 1. Otherwise call `embed_many` in chunks of `many_limit`. A leftover chunk of one text uses `embed`. `many_limit` of 0 is `invalid`.
3. Store one vector in USearch. Key: Record `id` + field name (body → `body`).

Do not glue title+body+other fields into one blob. One field, one vector.

If `embed` is missing, skip all vector writes. Tantivy still indexes lexical fields (`exclude_fts` stays out of BM25).

Index remembers `model_id` and `dimensions`. Mismatch with the current function → rebuild vectors or error on the vector leg; lexical still works.

---

## 6. `search`

`SearchQuery.text` is the query. Not a vector **instead of** text. The library embeds that text when the vector leg is on.

`k` unset or `0` → 10. `lexical_weight` / `vector_weight` unset → 1; **0 turns that leg off**.

Steps:

1. Resolve `collections` as in `find`. If `filter` is set: Tantivy runs `Filter` → allow-set of `(collection, id)`s. Both legs search **only** that set. Unset `filter` → the selected collections, not other collections in the same Index.
2. Lexical (weight > 0): Tantivy `Must(collections)` plus BM25 on `text` (`Must(filter)` if filtered). Top pool by score.
3. Vector (weight > 0 and embedder set): `embed(text)` once. `many_limit` of 0 is `invalid`. USearch `filtered_search` on the allow-set and selected collections (predicate during HNSW, not drop-after-top-k). Tiny allow-set may brute-force those vectors only, never the whole table at OPS-1.
4. Fuse ranks with RRF. Candidate pool is **larger than `k`**: at least `max(k, 50)` from the fused list (or all fused hits if fewer).
5. If a **reranker** is passed to `search`: `get` each pooled Record; call `score` when there is one candidate or `many_limit` is 1; otherwise `score_many` in chunks of `many_limit` (a leftover chunk of one Record uses `score`). Sort by that score (higher first); keep `k`. Set `Hit.rerank_score`. `many_limit` of 0 is `invalid`. If no reranker: return the fused list truncated to `k` (no `rerank_score`).
6. `include_raw` true: fill `Hit.raw` from the Record file (after truncate to `k` when there is no reranker). Unset or false → omit `raw`.

Invalid / missing Index → no hits or an Index code (`index_version`, `index_lexical`, `index_missing`, `index_corrupt`), never another Vault’s ids.

### Reranker

Not a model inside Scheda. Not a proto field. Same idea as the embedder: the **program that links the library** passes a function on `search`:

```text
score(query_text, record) → float
score_many(query_text, records) → []float
many_limit → integer > 0
```

`None` → order is RRF. Network inside that function is the program’s problem. The library does not load a ranking model (QRY-5).

CLI hook on `search` (`14`): `--reranker-cohere-url` plus
`--reranker-model` (Cohere/Jina `POST` `{URL}rerank`). Both HTTP flags
or none. Unset → RRF.

---

## 7. What this is not

- SeekStorm query string (`+ - ""`) as `Filter`.
- One vector per Record from concatenated fields.
- `search` input = raw query vector only (optional extra vector later; text stays).
- Embedder invoked only at “create index” and never again (it runs on every vector write and every vector `search`).
- Shipping a cross-encoder or LLM inside the crate (the reranker callback is the caller’s).
- `walk` / `walk_inverse` methods.
