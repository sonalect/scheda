# 13. Find and search

Document 13 of the Scheda package. Version 0.25, 6 September 2026.

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

Names are a method argument, not a field on `FindQuery` / `SearchQuery`. No Record path on the request. `Record.collection` and `Hit.collection` are output only. No `scan`: every indexed Record in the selected collections is `Filter` `all`. Unknown name → `not_found`. Isolated name on this handle → an ordinary query. Encrypted name → `unsupported`. Empty set stays non-isolated.

`get` / `add` / `history` / `lines` do not wait on the Index. `find` / `search` take `id`s from the Index, then open files. `replace` / `remove` do the same when `find` is on, isolated collections included; when `find` is off, and on encrypted collections either way, they use the Record file. Orphan file: `get` works; `find` / `search` skip it until indexed. Ordered journal read without the Index is `lines` (`07` LIB-19), not `find` and not `scan`.

---

## 2. Who writes the Index

The **library** calls the engine APIs. The caller never writes Tantivy or USearch files.

- **Tantivy** creates and owns `{vault}/.scheda/index/<generation>/lexical/` (`Index::create_in_dir` / open). Scheda only names fields and adds documents. On-disk layout is Tantivy’s (crate pin), not a Scheda spec.
- **USearch** creates and owns the vector dump under `{vault}/.scheda/`. Scheda only passes dimensions, keys, and `f32` arrays. On-disk layout is USearch’s.
- Scheda writes `{vault}/.scheda/index/<generation>/format` (decimal version) and `{vault}/.scheda/index/current` (live generation name). Wrong version → `index_version` / empty `find`/`search`, not foreign ids.

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
embed_many(texts) → [][]float32   required
embed(text) → []float32           provided: embed_many with one text
many_limit → non-zero size
```

plus `model_id` and `dimensions` (also non-zero). No function → vector leg off; `find`, BM25, and writes still work. Network inside that function is the program’s problem.

Only the batch function is required, and the single-text one is the batch path with one element. A program may override it, but then the same text must give the same vector whether it travelled alone or inside a padded batch — a wrong attention mask under padding splits the vector space and raises nothing.

Zero is not representable in either size. Absence of a vector leg is no embedder at all, not a zero `dimensions`.

CLI hook (`14`): `--embedder-openai-url` plus `--embedder-model` and
`--embedder-dimensions` (OpenAI `POST` `{URL}embeddings`), or
`--model2vec-embedder DIR`. All three HTTP flags or none. Unset → lexical
only. The CLI is that program; Scheda still does not ship weights.

Scheda does not ship weights or a chat LLM.

---

## 5. Building vectors (`add` / `replace` / `reindex`)

For each Record, for **each** field (and body) that is vector-eligible (`string` / `string_list` / body / edge text; not `exclude_vector`):

1. Take that field’s text (`string_list`: join values for that field).
2. Hash that exact string as a git blob (SHA-1 of `blob {len}\0{bytes}`, the same digest git stores for file content). Compare it to the digest already stored for that `(collection, id, field)` slot in `keys.json`.
3. If the digest matches and the Index `model_id` and dimensions match the current embedder, leave the USearch entry untouched — do not call the embedder.
4. Otherwise call `embed` when that Record has one non-empty vector field that needs a new vector, or when `many_limit` is 1. Otherwise call `embed_many` in chunks of `many_limit` for the slots that changed or are new. A leftover chunk of one text uses `embed`.
5. Store one vector in USearch. Key: Record `id` + field name (body → `body`). Write the digest next to the slot in `keys.json`. An Index written before digests existed loads with an empty digest (`serde` default) and re-embeds each slot once.
6. A slot that is gone — Schema unmarked the field, or this Record no longer has a value for it — is removed from USearch. `search` must not return that content. USearch `remove` leaves tombstones, so the dump can grow across many incremental cycles; this release does not compact it.

Do not glue title+body+other fields into one blob. One field, one vector. Do not hash the Record file or a git tree: a hand edit that is not yet committed must still change the digest.

If `embed` is missing, skip all vector writes. Tantivy still indexes lexical fields (`exclude_fts` stays out of BM25).

Index remembers `model_id` and `dimensions`. Mismatch with the current function → rebuild vectors from an empty table (every slot is embedded) or skip mixing spaces on a live write; lexical still works. `reindex` copies the previous generation’s vector dump into the new generation when model and dimensions match, then applies the skip/remove rules above. The lexical Index is always rebuilt in full.

Catch-up of writes that finished while `reindex/active` exists copies that Record’s vector slots from live `N` into `N+1` when the two generations share `model_id` and dimensions — including the digest stored in `keys.json`. It does not call the embedder: `add` already paid for those vectors on `N`. If the digest already matches in `N+1`, skip. If the models differ, `N` did not mix spaces; catch-up embeds into `N+1` with `embed_many` **before** the publish `write.lock`. The locked tail is empty or those few slots copy from `N` (same-model) or were already embedded in the unlocked pass.

---

## 6. `search`

`SearchQuery.text` is the query. Not a vector **instead of** text. The library embeds that text when the vector leg is on.

`k` unset or `0` → 10. `lexical_weight` / `vector_weight` unset → 1; **0 turns that leg off**.

Steps:

1. Resolve `collections` as in `find`. If `filter` is set: Tantivy runs `Filter` → allow-set of `(collection, id)`s. Both legs search **only** that set. Unset `filter` → the selected collections, not other collections in the same Index.
2. Lexical (weight > 0): Tantivy `Must(collections)` plus BM25 on `text` (`Must(filter)` if filtered). Top pool by score.
3. Vector (weight > 0 and embedder set): `embed(text)` once. USearch `filtered_search` on the allow-set and selected collections (predicate during HNSW, not drop-after-top-k). Tiny allow-set may brute-force those vectors only, never the whole table at OPS-1.
4. Fuse ranks with RRF. Candidate pool is **larger than `k`**: at least `max(k, 50)` from the fused list (or all fused hits if fewer). When a reranker is passed and `rerank_limit` is set above that floor, the pool is `max(k, 50, rerank_limit)` and both Index legs retrieve to that depth, so a candidate at fused rank N can actually be scored.
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

**How many candidates it sees is the caller’s choice.** Fusion depth and rerank depth answer two different questions: fusing is cheap and wants to be wide, while a cross-encoder costs tens of milliseconds per pair on a CPU and wants to be as narrow as the caller can accept. Unset `SearchQuery.rerank_limit` keeps the pool at `max(k, 50)` and scores every fused hit. Set to N, both legs retrieve `max(k, 50, N)` and the reranker scores the top N of that fused list (or every hit that exists if the corpus is smaller), before the library opens those Records. A large N is more BM25 and ANN retrieval and more Record reads, not only more model calls. `0` is `invalid`, because scoring nothing is what passing no reranker already means. A value below `k` is `invalid`, because a page where some hits carry `rerank_score` and some do not cannot be told from a truncated one. Without a reranker the field is ignored and the pool stays `max(k, 50)`.

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
