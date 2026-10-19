# 13. Find and search

Document 13 of the Scheda package. Version 0.45, 19 October 2026.

How a query and a search must work. Messages: `api/scheda/v1/query.proto`, `ports.proto`, `index.proto`. Index columns: `Projector.Columns(collection)`, not collection-declaration keys (`05`). This document is the target. Implementations that concatenate all columns into one vector, or treat a search as a query string only, are wrong.

Files are source of truth. The Index is disposable (`{vault}/.scheda/`, gitignored). Wipe it, `Index.Reindex`, same Documents.

---

## 1. Two calls: a query selects, a search ranks

| | Query | Search |
|---|---|---|
| Call | `Collection.Query` and `Collection.Count`; `Vault.QueryDocuments` and `Vault.CountDocuments` over a set of collection names (`CollectionNames`) | `Collection.Search`; `Vault.SearchDocuments` over the same set |
| In | `DocumentQuery` (`filter` required; `include_raw`, `offset`, `limit`, `start_after`); `VaultQuery` adds the set and a `DocumentKey` for `start_after` | `DocumentSearch` (exactly one of `text` or `like`; `filter`, `k`, `lexical_weight`, `vector_weight`, `rerank_limit`, `include_raw`, `offset`); `VaultSearch` adds the set |
| Out | a lazy stream of `Document` (`raw` only if `include_raw`); `Count` gives the number of matches | a lazy stream of `SearchItem`: the `SearchSummary` first, then `SearchHit`s best first, each a `Document` with `lexical_score`, `vector_score`, `score`, `rerank_score` |
| Promise | the exact set the `Filter` matches, in a stable order, without scores | the best `k` by relevance, no order promised beyond that |
| Engine | Tantivy query only | BM25 (Tantivy) **and** ANN (USearch), fuse, then optional rerank |
| Vectors | no | yes, if an embedder is set and the Projector marks vector columns |

Names are a set on the Vault form, never a field of `DocumentQuery` or `DocumentSearch`. No Document path on the request. `Document.collection` is output only. No `scan`: every indexed Document in the selected collections is `Filter` `all`. An unknown name → `NOT_FOUND`. An isolated name on this Vault → an ordinary query. An encrypted name → `UNIMPLEMENTED`, and so is a collection that leaves the lexical Index out (`exclude_lexical_index`) named in a query; a search covers it by the vector leg (`tests/search.rs`: `a_collection_that_leaves_the_lexical_index_out_is_searched_by_the_vector_leg`), and a collection that leaves both legs out, named in a search, is `UNIMPLEMENTED` (`tests/search.rs`: `a_collection_with_both_legs_left_out_is_unimplemented_when_named`). An empty set stays non-isolated and non-encrypted. In a build without `find` the query calls are `UNIMPLEMENTED`, and without `search` the search calls are (`tests/query.rs`: `without_find_the_four_calls_are_unimplemented_naming_the_feature`; `tests/search.rs`: `without_search_the_two_calls_are_unimplemented_naming_the_feature`).

`Collection.Get` / `Create` / `History` / `GetMany` / `Update` / `Upsert` / `Delete` do not wait on the Index: they use the Document file (a journal `Get` uses the `id` map, and looks in the segments changed since the map for a line it does not list, `06` IDX-5). `Query` / `Search` take `id`s from the Index, then open files. The Index follows the last commit in the background (`06` IDX-9), so `Query` / `Search` are eventually consistent: a write is found after its commit has been indexed. `Index.Status` and `Index.Watch` report the state the Index reached as an opaque `version`; there is no token for `HEAD` to compare it with. Inside a batch, queries do not see its uncommitted writes. Orphan or uncommitted file: `Get` works; `Query` / `Search` skip it until a commit holds it and the indexer has applied it. Ordered journal read without the Index is `Collection.GetMany` with a listing, not `Query` and not `scan`.

---

## 2. Who writes the Index

The **library** calls the engine APIs. The caller never writes Tantivy or USearch files.

- **Tantivy** creates and owns `{vault}/.scheda/index/lexical/` (`Index::create_in_dir` / open). Scheda names identity columns and Projector columns, then adds documents, and commits once per indexer round with the state it reached as the commit payload. On-disk layout is Tantivy’s (crate pin), not a Scheda spec.
- **USearch** serializes the HNSW index into `{vault}/.scheda/index/vector/vectors`, which Scheda writes whole after the model record and the key map. Scheda only passes dimensions, keys, and `f32` arrays; the bytes of the HNSW part are USearch’s.
- Scheda writes `{vault}/.scheda/index/format` (the layout number, now `2`), `commit` (the state the whole Index reflects), `columns`, `stale`, the journal `id` maps, and `vector/state` (`06` IDX-2). Another `format` or no `commit` → `INDEX_REBUILDING` while the indexer rebuilds; never foreign ids.

The indexer (`06` IDX-9) creates the engines when missing and keeps them at `HEAD`; writes do not touch them. No LLM is used to create either index.

---

## 3. Query

### Projector

Not a model inside the library. A port (`Projector`, `ports.proto`), the same class as `Embedder`: the program that links the library gives the Vault a reference to it, at `Scheda.Open` or with `Vault.SetProjector`.

```text
Describe()           → ProjectorDescriptor   // projection_id, read once
Columns(collection)  → IndexSchema           // columns of that collection:
                                             // name, kind (text / integer /
                                             // number / boolean / text list),
                                             // find / fts / vector marks
Project(RawDocument) → IndexDoc              // values for Columns(collection);
                                             // omit a column = missing, not
                                             // null unless the value is
                                             // present
```

A collection for which `Columns` gives none is indexed by identity only. The same column `name` may differ in kind across collections. A write does not call `Project` to accept or reject it. A Document `Project` refuses (`REFUSED`) is left out of the lexical and vector indexes and its file stays; one for which it gives no values is indexed by identity only; `UNAVAILABLE` fails the indexer round, which is tried again (`06` IDX-3).

No Projector: the Index holds identity (`collection`, `id`, ordinal) only. `all` and `term` on `id` work; any other field is `INVALID_ARGUMENT`. A search has no FTS/vector slots (lexical empty, vector off).

Identity columns are the library's. There is no magic `body` key unless the Projector names a column `body`.

### Filter field names

A Projector column is `(collection, name)`. Docs write `collection1.foo`. The wire does **not** parse a dotted `field` string: collection slugs may contain `.` (MOD-6). Each Filter leaf carries column `name` (`foo`) and, when needed, `collection` (a proto field on the leaf, not a dotted parse of `field`). Identity `id` is a library column and is not qualified this way.

- **`Vault.QueryDocuments` / `Vault.SearchDocuments`:** every Projector-column leaf must set `collection`. Bare `foo` is `INVALID_ARGUMENT`. A qualifier that names no declared collection is `NOT_FOUND`. A qualifier that names a collection whose columns have no such `find` column, or whose kind cannot accept that leaf, is `INVALID_ARGUMENT`. The Filter is compiled once for each collection scanned: a leaf that names another collection matches nothing there, so `a.title = x OR b.title = y` selects the Documents of `a` with that title and those of `b` with the other; a negated leaf of another collection does not exclude (`tests/query.rs`: `a_leaf_on_another_collection_is_false_where_it_does_not_apply_and_each_kind_ranges_on_its_own`, `a_negated_leaf_of_another_collection_does_not_exclude_here`). Every leaf is checked against the columns of the collection it names, scanned or not.
- **`Collection.Query` / `Collection.Search`:** `foo` means this collection's `foo`. `collection1.foo` is the same if this is `collection1`. `collection2.foo` on a `collection1` object is `INVALID_ARGUMENT`, and so is a qualifier that does not exist.

A column is checked against the columns the Index was built with, not what the Projector says now. A column without the `find` mark cannot appear in a Filter leaf. `id` cannot be excluded.

### Steps

1. Validate the request (`filter.kind` required; the rules of the messages, such as `fuzzy.distance` at most 2, are protovalidate). `limit` unset is every match. Resolve the collections (empty → every one that is neither isolated nor encrypted). Apply the Filter field names above.
2. Compile the Filter for each collection scanned and run it on the vault Tantivy index with `MUST` collection.
3. Take a snapshot of the Index (a Tantivy searcher) and of the repositories at `HEAD`, once, when the call is made. Read the keys in portions of the `order` field of the Index, each after the last one seen, starting after `start_after`; `offset` counts the keys passed over and `limit` the Documents handed out. A huge `limit` or `offset` reserves nothing (`tests/query.rs`: `a_huge_limit_or_offset_is_not_a_reservation`). Nothing written after the call is seen, and no Document comes twice or is missed (`tests/query.rs`: `a_stream_is_read_from_one_snapshot_and_none_is_repeated_or_skipped`).
4. Order (`06` QRY-7): `id` for a `standard` or `singleton` collection, append order for a `journal`, collection name first across collections. Never score, never `DocAddress`: an address depends on segment layout, which varies between runs.
5. Open the Document of each key as the stream reaches it (`include_raw` fills `Document.raw`). A key the Index holds for a file `HEAD` no longer has is skipped. `Count` applies `start_after`, `offset`, and `limit` and opens no file, so it equals the stream (`queries::tests::a_count_opens_no_file_and_a_stream_opens_one_as_it_reaches_a_document`; `tests/query.rs`: `limit_and_offset_hold_and_count_equals_the_stream`).

`Filter` is a condition tree: it only selects, so it has no score construct. Kinds: `all`, `empty`, `term`, `term_set`, `phrase`, `phrase_prefix`, `fuzzy`, `regex`, `regex_phrase`, `range`, `exists`, `boolean` (clauses `OCCUR_MUST` / `OCCUR_SHOULD` / `OCCUR_MUST_NOT`, with `minimum_should_match`). No QueryParser string. AND = `OCCUR_MUST`, OR = `OCCUR_SHOULD`. Field `id` is Document identity (`term`, `term_set`, `range` only). `range` on a Projector column of kind `integer` or `number` compares by numeric value, negatives and fractions included; the bounds must be finite JSON numbers (a string or boolean bound is `INVALID_ARGUMENT`), and a fractional bound on an `integer` column rounds inwards (`n > 9.5` is `n >= 10`). `integer` columns are exact over the whole `i64` range. `term` and `term_set` on those columns match the number (`10` matches `Integer(10)` and `Number(10.0)`). `range` on a `text` / `text list` column and on `id` compares as text, byte order. The kind is the column's own in each collection, so one Filter over collections that name a column alike but declare it differently compiles per collection. The Index keeps a sortable key per numeric value beside the text term (`tests/query.rs`: `a_range_on_an_integer_column_compares_by_value`, `a_range_on_a_number_column_orders_negatives_and_floats`). A value of another type than the column, and a phrase, fuzzy term, or pattern on a column without tokens, is `INVALID_ARGUMENT` (`tests/query.rs`: `a_filter_breaks_the_rules_of_the_columns_it_names`).

Finding documents similar to one is a search by `like` (§6), not a Filter kind.

A query does not call the embedder and does not use USearch.

---

## 4. Embedder

Not a model inside the library. A port (`Embedder`, `ports.proto`): the **program that links the library** gives the Vault a reference to it, at `Scheda.Open` or with `Vault.SetEmbedder`.

```text
Describe()          → EmbedderDescriptor    // model_id, dimensions; read once
Embed(stream Text)  → stream Vector         // one vector per text, in order
EmbedQuery(Query)   → Vector                // the search-text role
```

`model_id` and `dimensions` are non-zero (`tests/ports.rs`: `a_descriptor_that_breaks_its_rules_fails_open_and_set_embedder`). No embedder → vector leg off; queries, BM25, and writes still work. Network inside the embedder is the program's problem.

Scheda sends the texts one by one. The implementation gathers them into the portions its model wants (a size, a pause, several in flight) and sends its last partial portion when the texts end. The same text must give the same vector whether it travelled alone or inside a padded batch — a wrong attention mask under padding splits the vector space and raises nothing (`vector::tests::the_portions_an_embedder_forms_do_not_change_the_vectors`). An `Err` ends the stream: Scheda keeps the vectors that came before it and sends the other texts again later. `EmbedQuery` is `Embed` of one text for a symmetric encoder, and does something extra for a model that encodes the query role differently (a prefix, a task type). A search calls `EmbedQuery` for the query text and `Embed` for the texts of Projector vector columns.

Zero is not representable in either size. Absence of a vector leg is no embedder at all, not a zero `dimensions`.

CLI hook (`14`): `--embedder-openai-url` plus `--embedder-model` and
`--embedder-dimensions` (OpenAI `POST` `{URL}embeddings`), or
`--model2vec-embedder DIR`. All three HTTP flags or none. The CLI's `model_id` is the `--embedder-model` name (HTTP) or an identity of the model files (Model2Vec), never a URL or a path (`14`). Unset → lexical
only. The CLI is that program; Scheda still does not ship weights.

Scheda does not ship weights or a chat LLM.

---

## 5. Building vectors (the indexer)

In each indexer round (`06` IDX-9), for each changed Document, for **each** Projector column marked `vector` that `Projector.Project` filled:

1. Take each value of that column (a `text list` gives one text per element).
2. Hash that exact text as a git blob (SHA-1 of `blob {len}\0{bytes}`). That digest is stored with the slot in the key map inside `vectors`.
3. A text whose digest the Index already holds keeps its stored vector: the embedder is not called for it. Vectors the embedder returned before a failed try of the round are kept in the indexing Vault's memory the same way, by digest, until a round succeeds.
4. Every other text goes to `Embedder.Embed` in one stream per round, in the order of the Documents; the embedder forms its own portions. A failure ends the stream: the vectors that came before it are kept (step 3), and a `REFUSED` text leaves its Document out of the vector side.
5. Each vector is stored in USearch under a fresh `u64` key. The key map gives the key its collection, `id`, Vault-relative file, column, place in the column, and digest, so a hit leads back to its Document and a changed or deleted Document (or journal segment) drops exactly its own vectors.
6. A slot that is gone (`Columns` no longer marks the column `vector`, or this Document's `Project` omits it) is removed. USearch reuses the room of removed vectors, so the file does not grow across incremental rounds.

Do not glue several columns into one blob. One value, one vector. Do not hash the Document file or a git tree: the digest is of the embedder text, so a commit that changes other bytes of the Document does not re-embed it. A journal segment that changed is projected again line by line, and its unchanged lines keep their vectors by digest.

Without an embedder there is no vector side; Tantivy still indexes `fts` columns. A collection with a `vector` column needs an embedder for the Index to move at all (`06` IDX-9, who indexes).

`vectors` records `model_id` and `dimensions`. Only a Vault whose embedder matches them indexes. A Vault with another model indexes nothing, does not embed its queries, and its `search` runs on the lexical leg alone; `IndexState.model_mismatch` is true. Moving to a new model is `Index.Reindex` from a Vault with the new embedder: the rebuild removes the Index and embeds every text again. A `Reindex` with the same model embeds every text again too, since the stored vectors go with the rest of the Index.

The embedder runs on the indexer's thread, never inside a write: a slow, blocking, or failing embedder delays the Index and is retried with backoff (`IndexState.last_error`), but fails no write. While it fails, `commit` and the reported `version` do not move for the whole Index.

---

## 6. Search

A search has exactly one subject. `text` is the query, not a vector **instead of** text: the lexical leg matches its words and the library embeds it (`Embedder.EmbedQuery`) when the vector leg is on. `like` names a Document of any collection (`DocumentKey`): others like it.

`k` unset or `0` → 10; there is no upper limit beyond the Index. Each leg's pool is also clamped to the documents or vector slots that exist, so a large `k` costs nothing extra on a small corpus (`tests/search.rs`: `k_has_no_upper_limit_and_the_first_item_is_the_summary`). `lexical_weight` / `vector_weight` unset → 1; **0 turns that leg off**; both 0, a negative one, or a non-finite one is `INVALID_ARGUMENT` (`tests/search.rs`: `the_weights_are_checked_and_zero_turns_a_leg_off`). The weights scale Reciprocal Rank Fusion. `offset` skips the first hits of the ranking and opens none of their files. A search ranks again on each call (a model may answer a little differently, the Index may have moved), so page 2 of a second call need not continue page 1; pages that must agree come from one search with a large enough `k`, read as a stream (`tests/search.rs`: `offset_skips_hits_and_pages_agree_with_one_large_search`).

Without a Projector, there are no FTS/vector slots: step 2 has nothing to rank, step 3 is off.

Steps:

1. Resolve the collections as in a query. If `filter` is set: the Filter (same field-name rules as §3) narrows both legs, which search **only** the Documents it matches. Unset `filter` → the selected collections, not other collections in the same Index.
2. Lexical (weight > 0): Tantivy `Must(collections)` plus BM25 on Projector columns marked `fts`, and `Must(filter)` if filtered. For `text` the words are parsed as a Tantivy query, and a text the parser refuses is searched again with its syntax characters replaced by spaces. For `like` it is Tantivy's `MoreLikeThis` over the `fts` text of that Document, which the Vault's Projector projects again (the Index does not store the text); without a Projector a `like` has no lexical leg. The Filter is a clause of the query, wrapped in a constant score of zero so it narrows the candidates without changing the ranking; the top pool is taken from the Documents that match both, never from a larger top list filtered afterwards (`tests/search.rs`: `a_filter_selects_before_the_lexical_pool_is_cut`).
3. Vector (weight > 0, and an embedder with vectors in the Index): for `text`, `EmbedQuery(text)` once; for `like`, the stored vectors of that Document, and no model is called (`tests/search.rs`: `like_finds_the_neighbours_of_a_document_without_calling_the_embedder`). The nearest vectors the scope and the Filter admit, tested during the walk, not dropped after the top `k`. A Document with several vectors counts once, by its nearest. The HNSW walk width is a binding floor (search expansion), not the caller's `k`; the engine retrieves at least that many slots, then truncates to the depth of the leg, so asking for a deeper page does not reshuffle the head of a shallower one (`06` IDX-7).
4. Depth and fusion. Each leg is asked for `max(offset + k, rerank_limit, 50)` hits, so a Document one leg ranks low can still be promoted by the other. Reciprocal Rank Fusion: a hit at rank `r` (from 0) of a leg of weight `w` adds `w / (60 + r + 1)`; ties break by collection, then `id`. `lexical_score` is the BM25 score, `vector_score` is `1 / (1 + cosine distance)`, `score` the fused one. The Document `like` names is never a hit of its own search.
5. Rerank. When `rerank_limit` is set, the first `rerank_limit` fused hits go to the Vault's `Reranker`: `Score` gets the query text first (for `like`, the `fts` text of its Document), then the Documents with their `raw`. The hits are ordered by its score (higher first) and carry `rerank_score`; the hits after them keep the fused order. `offset` and `k` then cut that order. Without `rerank_limit`, or without a reranker, the fused order stands and there is no `rerank_score` (`tests/search.rs`: `a_reranker_orders_the_first_hits_and_sees_the_query_first`, `a_reranked_like_search_scores_against_the_text_of_its_document`).
6. The ranking is computed whole, but it holds keys and scores only. The stream's first item is the `SearchSummary`; the Documents are opened as the stream reaches them, so a large `k` reads no file ahead; `include_raw` true fills `raw` of each hit's Document, unset or false omits it (`tests/search.rs`: `the_documents_carry_their_raw_only_when_asked`). A hit the Index holds for a file `HEAD` no longer has is skipped.
7. `SearchSummary.vector_leg` is the first item of every reply, early empty ones included. It is decided before step 3 from what the Vault already holds (no extra I/O, no diff), the first that applies: `VECTOR_LEG_NOT_ASKED` (`vector_weight` 0); `VECTOR_LEG_NO_VECTOR_COLLECTION` (no selected collection has vector slots: excluded or encrypted, no Projector, or no Projector column marked `vector`); `VECTOR_LEG_NO_EMBEDDER`; `VECTOR_LEG_NO_VECTORS` (the Index holds none yet); `VECTOR_LEG_MODEL_MISMATCH` (the embedder's `model_id` or dimensions differ from the Index vectors; the query is not embedded); else `VECTOR_LEG_RAN`. A vector-only query (`lexical_weight` 0) with no hits and any value but `RAN` did not search vectors at all, which is how a caller tells that from "nothing matched" without `Index.Status` (`tests/search.rs`: `the_summary_names_why_the_vector_leg_did_not_run`, `the_summary_names_a_vault_without_a_vector_collection_or_without_vectors`).

Index not built, or of another format → `INDEX_REBUILDING` (retryable); a file of the Index that does not read → `INDEX_BROKEN` until `Index.Reindex`; another projection, or a collection whose columns changed → `INDEX_STALE` (`06` IDX-2, IDX-9, QRY-4). Never another Vault’s ids, and never an empty answer in place of those codes. A query Tantivy refuses because of what the caller put in it (a `regex_phrase` pattern, a `fuzzy.distance` above 2) is `INVALID_ARGUMENT`, never an Index code.

### Reranker

Not a model inside Scheda. A port (`Reranker`, `ports.proto`), like the embedder: the **program that links the library** gives the Vault a reference to it, at `Scheda.Open` or with `Vault.SetReranker`; it is the Vault's, not an argument of a search.

```text
Score(stream ScoreItem) → stream DocumentScore
                        // the first item is the query text, the rest are the
                        // Documents (their raw set); one score per Document,
                        // in their order; higher is better
```

The implementation gathers the Documents into the portions its model wants, as `Embed` does. No reranker → order is RRF. A search sets `rerank_limit` to ask for a rerank; set without a reranker, also after `Vault.RemoveReranker`, it is `INVALID_ARGUMENT` (`tests/search.rs`: `rerank_limit_needs_a_reranker_also_after_it_was_removed`). Network inside the reranker is the program's problem. The library does not load a ranking model (QRY-5).

**How many candidates it sees is the caller's choice.** Fusion depth and rerank depth answer two different questions: fusing is cheap and wants to be wide, while a cross-encoder costs tens of milliseconds per pair on a CPU and wants to be as narrow as the caller can accept. Unset `rerank_limit` means no rerank. Set to N, both legs retrieve `max(offset + k, N, 50)` and the reranker scores the first N of the fused list (or every hit that exists if the corpus is smaller), before the library returns those Documents. A large N is more BM25 and ANN retrieval and more Document reads, not only more model calls. `rerank_limit` is at least 1 (protovalidate), because scoring nothing is what passing no reranker already means. A value below the effective `k` is `INVALID_ARGUMENT`, because a page where some hits carry `rerank_score` and some do not cannot be told from a truncated one (`tests/search.rs`: `rerank_limit_below_k_is_invalid`).

CLI hook (`14`): global flags `--reranker-cohere-url` plus
`--reranker-model` (Cohere/Jina `POST` `{URL}rerank`), used by a search that sets `rerankLimit`. Both HTTP flags
or none. Unset → RRF.

---

## 7. What this is not

- SeekStorm query string (`+ - ""`) as `Filter`.
- One vector per Document from concatenated columns.
- A search whose only input is a raw query vector (the subject is `text` or `like`; text stays).
- Embedder invoked only at "create index" and never again (it runs on every vector write and every vector search).
- Shipping a cross-encoder or LLM inside the crate (the `Reranker` is the caller's).
- `walk` / `walk_inverse` calls.
- A dotted `field` string (`collection1.foo` as one proto string). Qualifier is a separate `collection` on the leaf.
