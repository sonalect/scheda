# 13. Find and search

Document 13 of the Scheda package. Version 0.69, 24 October 2026.

How a query and a search must work. Messages: `api/scheda/v1/query.proto`, `ports.proto`, `index.proto`. Index columns: `Projector.Columns(collection)`, not collection-declaration keys (`05`). This document is the target. Implementations that concatenate all columns into one vector, or treat a search as a query string only, are wrong.

Files are source of truth. The Index is disposable (`{vault}/.scheda/`, gitignored). Wipe it, `Index.Reindex`, same Documents.

---

## 1. Two calls: a query selects, a search ranks

| | Query | Search |
|---|---|---|
| Call | `Collection.Query` and `Collection.Count`; `Vault.QueryDocuments` and `Vault.CountDocuments` over a set of collection names (`CollectionNames`) | `Collection.Search`; `Vault.SearchDocuments` over the same set |
| In | `DocumentQuery` (optional `filter`; `order_by`, `offset`, `limit`, `start_after`, `include_raw`, `not_before`); `VaultQuery` adds the set and a `DocumentKey` for `start_after` | `DocumentSearch` (exactly one of `text`, `like`, or `vector`; `filter`, `k`, `lexical_weight`, `vector_weight`, `exact`, `min_vector_score`, `rerank_limit`, `include_raw`, `offset`, `not_before`); `VaultSearch` adds the set |
| Out | a stream of `QueryItem`: the `QuerySummary` (`total`) first, then the `Document`s of the page (`raw` only if `include_raw`); `Count` gives the number of matches | a stream of `SearchItem`: the `SearchSummary` first, then `SearchHit`s best first, each a `Document` with `lexical_score`, `vector_score` and the text that gave it (`vector_match`), `score`, `rerank_score` |
| Promise | the exact set the `Filter` matches, in a stable order, without scores | the best `k` by relevance, no order promised beyond that |
| Engine | Lance: the typed `Filter` run as an expression on the columns | BM25 (Lance's inverted index) **and** nearest neighbours (Lance's IVF_RQ index), fuse, then optional rerank |
| Vectors | no | yes, if an embedder is set and the Projector marks vector columns |

Names are a set on the Vault form, never a field of `DocumentQuery` or `DocumentSearch`. No Document path on the request. `Document.collection` is output only. No `scan`: every indexed Document in the selected collections is an unset `filter`. An unknown name → `NOT_FOUND`. An isolated name on this Vault → an ordinary query. An encrypted name → `UNIMPLEMENTED`, and so is an `INDEX_SCOPE_IDS` collection (the Index holds its identity only) named in a query that filters or orders, or named in a search (`tests/search.rs`: `a_collection_without_vectors_is_ranked_by_the_lexical_leg_and_an_ids_one_is_unimplemented`). In a Vault query or search such collections are left out of an empty set instead. An empty set is every collection that is neither isolated nor encrypted. A collection the Index has not reached yet is empty. In a build without the feature `index` the query and search calls are `UNIMPLEMENTED` (`tests/query.rs`: `without_the_index_the_four_calls_and_the_index_object_are_unimplemented`; `tests/search.rs`: `without_the_index_the_two_calls_are_unimplemented`).

`Collection.Get` / `Create` / `History` / `GetMany` / `Update` / `Upsert` / `Delete` do not wait on the Index: they use the Document file (a journal `Get` uses the `id` map, and looks in the segments changed since the map for a line it does not list, `06` IDX-5). `Query` / `Search` take `id`s from the Index, then read those Documents from git at the state the Index reflects. The Index follows the last commit in the background (`06` IDX-9), so a call without `not_before` is eventually consistent. With `not_before` set to a state token (the `version` of a write's reply, of `Vault.Info`, or of `IndexState`), the call waits until the Index reflects a state that contains it: past `OpenOptions.index_wait_timeout_ms` (default 60 s) it is `DEADLINE_EXCEEDED` (retryable), `Vault.Close` ends the wait with `FAILED_PRECONDITION`, and a token whose commit is no longer in the history of `HEAD` does not wait (`06` QRY-8). No query sees a write whose call has not returned (`20` D6). A file in the work tree that no commit holds is seen by no call, `Get` included, since every call reads `HEAD` (`20` D22; `tests/documents.rs`: `a_persons_uncommitted_edit_is_seen_by_no_get_document_and_a_commit_by_hand_is`); a committed one is found by `Get` at once and by `Query` / `Search` once the indexer has applied its commit. Ordered journal read without the Index is `Collection.GetMany` with a listing, not `Query` and not `scan`.

---

## 2. What the Index holds

The **library** calls the engine. The caller never writes Index files.

- **Lance** holds, per indexed collection of a generation `{vault}/.scheda/index/g{n}/`, a rows table `t{n}d` and a files table `t{n}f` (`06` IDX-2). The rows table has one row per Document or journal line: `id`, `file`, `order`, the Projector's columns, typed (`text` → Utf8, `integer` → Int64, `number` → Float64, `boolean` → Boolean, `text list` → List<Utf8>), and for each `vector` column of kind `text` `_vector_{name}` and `_digest_{name}`. A `vector` column of kind `text list` has a texts table `t{n}x{k}`: one row per text with a vector (`id`, `file`, `position`, `digest`, `vector`). A column marked `scalar_index` has a B-tree, or a label list when its kind is `text list` (it answers `term` and `terms` on the list; `exists` and a negated leaf on a list read the column of every row), one marked `fts` an inverted index (BM25, no positions, no stemming), one marked `vector` an IVF_PQ index (cosine) from 10 000 rows, or from 10 000 texts in its texts table. The layout of a table is Lance's, not a Scheda spec.
- Scheda writes `{vault}/.scheda/index/format` (the layout number, now `3`), `current` (the live generation), the `commit` of each generation (the state it reflects and the Lance version of each table, written last), and the journal `id` maps (`06` IDX-2). An Index never built is `INDEX_REBUILDING`, never foreign ids.

The indexer (`06` IDX-9) creates the tables when missing and keeps them at `HEAD`; writes do not touch them. No LLM is used to create the Index.

---

## 3. Query

### Projector

Not a model inside the library. A port (`Projector`, `ports.proto`), the same class as `Embedder`: the program that links the library gives the Vault a reference to it, at `Scheda.Open` or with `Vault.SetProjector`.

```text
Describe()           → ProjectorDescriptor   // projection_id, read once
Columns(collection)  → IndexSchema           // columns of that collection:
                                             // name, kind (text / integer /
                                             // number / boolean / text list),
                                             // scalar_index / fts / vector marks
Project(RawDocument) → IndexDoc              // values for Columns(collection);
                                             // omit a column = missing, not
                                             // null unless the value is
                                             // present
```

The Vault asks `Columns` of a collection once for each installed Projector and state of `HEAD`, not at every look (`06` IDX-8). A collection for which `Columns` gives none is indexed by identity only. The same column `name` may differ in kind across collections. A key `Project` gives that no column declares is ignored; a value of another kind than its column's leaves the Document out. A write does not call `Project` to accept or reject it. A Document `Project` refuses (`REFUSED`) is left out of the Index and its file stays; one for which it gives no values is indexed by identity only; `UNAVAILABLE` fails the indexer round, which is tried again (`06` IDX-3).

No Projector: the Index holds identity (`id`, `file`, `order`) only. A filter may name `id`; any other column is `INVALID_ARGUMENT`. A search has no FTS/vector columns (lexical empty, vector off).

Identity columns are the library's. There is no magic `body` key unless the Projector names a column `body`.

### Filter field names

A `Filter` leaf names a `column`: `id`, or a column the Projector declares. It does not name a collection. It does not name `file`, a journal's `order` (a key of `order_by`, not a column of a filter), or an engine column such as `_rowid`, `_distance`, or `_score`: those are `INVALID_ARGUMENT` because no Projector declares them.

- **`Collection.Query` / `Collection.Search`:** `foo` means this collection's `foo`. A column this collection does not declare is `INVALID_ARGUMENT`.
- **`Vault.QueryDocuments` / `Vault.SearchDocuments`:** one filter applies to every collection in scope. A column a collection does not declare has no value in its Documents: a condition on it is false there and `none_of` it is true. A column no collection in scope declares is `INVALID_ARGUMENT`, so a typo is caught. A column the Projector declares that the Index does not hold yet for a collection (the Index has not reached it) is the same: no value, not an error.
- A value is of the kind of its column: a text, an integer, a number, a boolean (on a `text list`, a text). Another kind is `INVALID_ARGUMENT`.
- A filter deeper than `OpenOptions.filter_max_depth` (default 32, at most 100) is `INVALID_ARGUMENT`: a leaf is one level, each `all_of`, `any_of`, or `none_of` one more.

The columns are checked against what this Vault's Projector declares now, before the `index_scope` of the collection (SCH-9): a scope changed never makes the Index stale. An Index built for other columns or another projection is `INDEX_STALE` (`06` IDX-9).

### Steps

1. Validate the request (the rules of the messages are protovalidate: a `Filter` has exactly one `kind`, the lists are not empty, a `range` has an end, a number is finite, `order_by` names a column once). Resolve the collections (empty → every one that is neither isolated nor encrypted). Check the filter and `order_by` against the Projector's columns as above. Wait for `not_before` if it is set (`06` QRY-8). A call over the Vault reads up to four collections at once; the results, their order, and their scores are those of reading them one after another (`tests/search.rs`: `a_call_over_many_collections_answers_as_the_calls_over_each`).
2. Take one snapshot of the Index: the `commit` of the live generation, read once, and each table at the version it names. Nothing written after the call starts is seen.
3. In that snapshot, count the matches (`QuerySummary.total`) and read the keys of the page: the filter and the condition of `start_after` are typed expressions the library builds and the engine runs (`Scanner::filter_expr`); no text of a query language is made from values. `offset` skips matches after `start_after`, and `limit` stops the page. The page is read whole and held as keys, tens of bytes each, `offset + limit` of them at most (every match without `limit`; a `limit` or `offset` of `4294967295` reads the matches there are, `tests/query.rs`: `limit_and_offset_at_their_maximum_answer_with_the_documents_there_are`); the total and the page always agree.
4. Order (`06` QRY-7): the `order_by` keys (a Document without a value last, both ways), then, across collections, the collection name, then `id` for a `standard` or `singleton` collection and append order for a `journal`. Never score, and never an address that depends on file layout.
5. Open the Document of each key as the stream reaches it, from git (one snapshot of each repository for the page; the last journal segment read is kept by blob id, so a page of lines of one segment reads it once), at the state the snapshot reflects, or at the state now when that commit has left its repository (`include_raw` fills `Document.raw`). A key whose file that state lacks is skipped. `Count` gives the same number as the summary and opens no file.

`Filter` is a condition tree: it only selects, so it has no score construct. Kinds:

- `all_of`, `any_of`, `none_of`: every one, at least one, none of a list of at least one filter.
- `term`: the column equals the value; on a `text list`, it holds the text. `terms`: equals one of the values; holds one of them.
- `range`: between two ends, each inclusive or not, at least one set. Texts and `id` compare as UTF-8 bytes; numbers by value. Not on a `boolean` or a `text list`.
- `exists`: the column has a value (a `text list`: at least one text). Not on `id`.
- `prefix`: a `text` column or `id` begins with the text. Not on a `text list`.

The logic is two-valued: a condition on a Document without a value in the column is false, and `none_of` it is true. There is no QueryParser string, no phrase, fuzzy, or regex leaf. `id` is Document identity. *Check:* each kind of leaf selects the exact set, and a missing value is false (`tests/query.rs`: `each_kind_of_leaf_selects_the_exact_set_and_none_of_counts_a_missing_value_as_false`). *Check:* a column no collection declares, a value of another kind, and a filter deeper than the bound are `INVALID_ARGUMENT` (`tests/query.rs`: `a_column_no_collection_declares_a_value_of_another_kind_and_a_deep_filter_are_invalid`; `filter::tests::a_column_no_collection_declares_and_a_value_of_another_kind_are_invalid`, `filter::tests::a_leaf_is_one_level_and_each_list_one_more`); a filter of depth 100 runs on a 2 MiB stack (`tests/query.rs`: `a_filter_of_depth_100_runs_on_a_two_megabyte_stack`). *Check:* the order is stable and paging by `start_after` neither repeats nor skips; a query of the Vault orders by its keys, then the collection (`tests/query.rs`: `the_order_is_stable_and_paging_with_start_after_neither_repeats_nor_skips`, `a_vault_query_orders_by_its_keys_then_the_collection_and_pages_across_collections`).

Finding documents similar to one is a search by `like` (§6), not a Filter kind.

A query does not call the embedder and does not use the vector index.

---

## 4. Embedder

Not a model inside the library. A port (`Embedder`, `ports.proto`): the **program that links the library** gives the Vault a reference to it, at `Scheda.Open` or with `Vault.SetEmbedder`.

```text
Describe()          → EmbedderDescriptor    // model_id, dimensions; read once
Embed(stream Text)  → stream Vector         // one vector per text, in order
EmbedQuery(Query)   → Vector                // the search-text role
```

`model_id` and `dimensions` are non-zero (`tests/ports.rs`: `an_embedder_whose_descriptor_breaks_its_rules_is_invalid_argument_and_writes_nothing`). No embedder → vector leg off, and the Vault does not index a collection with a `vector` column (`06` IDX-9); queries, BM25, and writes still work. Network inside the embedder is the program's problem.

Scheda sends the texts one by one. The implementation gathers them into the portions its model wants (a size, a pause, several in flight) and sends its last partial portion when the texts end. The same text must give the same vector whether it travelled alone or inside a padded batch — a wrong attention mask under padding splits the vector space and raises nothing. An `Err` ends the stream: Scheda keeps the vectors that came before it and sends the other texts again later (`ports::tests::a_failure_after_the_seventh_vector_returns_the_seven_and_the_error`). A vector of the wrong length is `INVALID_ARGUMENT` (`ports::tests::a_vector_of_the_wrong_length_is_invalid_argument_and_keeps_the_ones_before_it`). `EmbedQuery` is `Embed` of one text for a symmetric encoder, and does something extra for a model that encodes the query role differently (a prefix, a task type). A search calls `EmbedQuery` for the query text and `Embed` for the texts of Projector vector columns.

Zero is not representable in either size. Absence of a vector leg is no embedder at all, not a zero `dimensions`.

*Fixed portions.* A model that wants no more than "these texts, those vectors" needs no stream code of its own: the library's fixed-portion adapters read the texts one by one, call one function of the program with portions of at most the size it gives, call it with the last partial portion when the texts end, and send the vectors back in order; `EmbedQuery` is a portion of one, unless the program gives a query function; a function that gives another count than its portion fails the stream with `INVALID_ARGUMENT`, and a failure of the function ends the stream; `Describe` gives the model id and dimensions the program gave, read once at install. `FixedEmbedder::new(model_id, dimensions, portion, f)` takes a blocking `f(Vec<String>) -> Result<Vec<Vec<f32>>, Error>` and is handed over as `EmbedderRef::from_sync(…)`: Scheda calls it on the blocking pool. `FixedEmbedderAsync::new` takes the same arguments with an `f` that returns a future of that result, and is handed over as `EmbedderRef::from_async(…)`: Scheda awaits it on the runtime the Vault was opened on, with no blocking pool, and no thread is held while the future waits. Both forms share one portioning (`ports::tests::the_adapter_calls_its_function_with_portions_and_flushes_the_last_one`, `the_adapter_fails_the_stream_when_its_function_miscounts_or_fails`, `a_failed_text_ends_the_adapter_stream_and_its_portion_is_not_run`, `the_async_adapters_await_their_futures_on_the_runtime_thread`; `tests/ports.rs`: `the_async_embedding_adapter_portions_as_the_blocking_one_and_awaits_its_function`). A model that embeds a search text otherwise than a stored one (an instruction before the text, another task type) gives a query function with `with_query(q)`: a blocking `q(String) -> Result<Vec<f32>, Error>` on `FixedEmbedder`, one that returns a future of that result on `FixedEmbedderAsync`. `EmbedQuery` then calls `q` with the text, not the portion function; a vector of another length than the declared dimensions is `INVALID_ARGUMENT`, and a failure of `q` is the failure of `EmbedQuery`. The texts of `Embed` still go to the portion function, and `new` alone keeps the portion of one (`tests/ports.rs`: `the_embedding_adapter_answers_a_query_with_its_query_function_when_given_one`, `the_async_embedding_adapter_answers_a_query_by_awaiting_its_query_function_when_given_one`; `tests/search.rs`: `an_async_fixed_embedder_with_a_query_function_embeds_the_search_text_with_it`).

CLI hook (`14`): `--embedder-openai-url` plus `--embedder-model` and
`--embedder-dimensions` (OpenAI `POST` `{URL}embeddings`), or
`--model2vec-embedder DIR`. All three HTTP flags or none. The CLI's `model_id` is the `--embedder-model` name (HTTP) or an identity of the model files (Model2Vec), never a URL or a path (`14`). Unset → the process indexes no collection with a `vector` column, and its searches run without the vector leg. The CLI is that program; Scheda still does not ship weights.

Scheda does not ship weights or a chat LLM.

---

## 5. Building vectors (the indexer)

In each indexer round (`06` IDX-9), for each changed Document, for **each** Projector column marked `vector` that `Projector.Project` filled (a `vector` column is a `text` column, one text, or a `text list` column, several; a collection that is not `INDEX_SCOPE_ALL` has none):

1. Take each text of that column: the text of a `text`, each text of a `text list` with its place in the list. A blank text (only whitespace) has no vector.
2. Hash that exact text as a git blob (SHA-1 of `blob {len}\0{bytes}`). That digest is stored beside the vector: in `_digest_{name}` of the row for a `text`, in the text's row of the texts table for a `text list`.
3. A text whose digest the table already holds for that Document, in any of its `vector` columns and at any place, keeps its stored vector: the embedder is not called for it. Moving texts within a list embeds nothing; adding or editing one embeds only that text (`tables::tests::each_text_of_a_list_has_its_vector_and_only_a_new_text_is_embedded`). Vectors the embedder returned before a failed try of the round are kept in the indexing Vault's memory the same way, by digest, until a round succeeds, and so are the Projector's answers, by Document and the digest of its bytes: a try after an embedder failure neither embeds nor projects again what the earlier tries got.
4. Every other text goes to `Embedder.Embed` in one stream per batch of 256 files a round reads, in the order of the Documents; the embedder forms its own portions. A failure ends the stream: the vectors that came before it are kept (step 3). A text the embedder refuses (`REFUSED`) leaves its Document without that vector, the other texts of the Document keep theirs, and the round goes on. The refusal is confirmed on the text alone, since an embedder that batches refuses a whole portion for one text: the first text left without a vector is sent alone; if it is embedded alone, the culprit is looked for among the texts after it by halving, and only the text refused alone keeps no vector.
5. Each vector of a `text` is stored in `_vector_{name}` of the Document's row, which the round upserts by `id`; each vector of a `text list` in its own row of the texts table, beside its place and digest. A changed Document replaces its row and its texts, and a deleted one loses them, vectors with them. There is no key map.

Do not glue several columns into one blob. One text, one vector: a Document found by any of several texts (a note by each of its phrasings, an answer by each question it answers) gives them as a `text list`, and a search ranks it once, by the closest. Do not hash the Document file or a git tree: the digest is of the embedder text, so a commit that changes other bytes of the Document does not re-embed it. A journal segment that grew by appending gives only its new lines; one changed otherwise is projected again line by line, and its unchanged lines keep their vectors by digest.

The Index records the embedder's `model_id` and `dimensions` in the `commit` of its generation, once a collection has a `vector` column. Only a Vault whose embedder matches them indexes. A Vault with another model or vector length finds the Index `INDEX_STALE`: it does not index, and its queries and searches fail with that code until a `Reindex` of a Vault with that embedder has switched the Index to it; there is no mode that indexes all but the vectors. Moving to a new model is that `Index.Reindex`: it builds a new generation beside the live one, which keeps answering, and embeds every text again, since vectors of two models are never mixed. A `Reindex` with the same model and vector length takes the vector the live generation holds for the same text of the same Document, and embeds only the rest.

The embedder runs inside the indexer task, never inside a write: a slow, blocking, or failing embedder delays the Index and is retried with backoff (`IndexState.last_error`), but fails no write. While it fails, `commit` and the reported `version` do not move for the whole Index, and a query sees the Index as `commit` names it. Closing the Vault waits for the portion being embedded, not for the whole stream (`tables::tests::closing_a_vault_in_the_middle_of_a_slow_embedding_returns_within_one_portion`).

---

## 6. Search

A search has exactly one subject. `text` is the query: the lexical leg matches its words and the library embeds it (`Embedder.EmbedQuery`) when the vector leg is on. `like` names a Document of any collection (`DocumentKey`): others like it. `vector` is a vector the caller made (the mean of stored vectors, a vector of the same model made elsewhere) and the `vector` column it is compared with: the vector leg alone ranks, in that column alone, and no model is called, so a Vault without an embedder searches by it too. Only its length can be checked against the column's vectors in the Index (another length, or a column no collection in scope keeps vectors in, is `INVALID_ARGUMENT`); that it comes from the column's embedding space is the caller's word. Its values are finite and not all 0, `vector_weight` is not 0, and `rerank_limit` is unset (the reranker scores against a text): otherwise `INVALID_ARGUMENT`, by the rules of the proto (`tests/vectors.rs`: `a_search_by_a_vector_ranks_as_the_brute_force_and_needs_no_embedder`).

`exact` makes the vector leg compare the query with every vector the filter admits, whatever index the table has, instead of asking the IVF_RQ index of a large table (Lance's flat search, `use_index(false)`): it finds the exact nearest, and the Documents at one distance in `id` order, so the hits depend only on the Documents the Index holds and on the request, not on how the index or the fragments moved. Lance orders the hits at one distance by where it stored them; when the last hit a leg takes shares its distance with others, the leg also reads every row at that distance (`distance_range`), so the cut by `id` is exact. The scan holds a bounded part of the table at once (one fragment, two batches of 1 024 rows, a read-ahead of 16 MiB): about 47 MB for 200 000 vectors of 256 values, as for 20 000 (`search::memory::an_exact_search_holds_as_much_of_a_large_table_as_of_a_small_one`, the memory target). With a filter, Lance first marks the rows it admits, some 20 bytes a row it admits, through the B-tree index of the column when it has one: a filter that admits 100 rows holds 0.4 MB in a table of 100 000 rows and 0.5 MB in one of 1 000 000 (`search::memory::an_exact_search_filtered_to_a_hundred_rows_holds_as_much_of_a_large_table`). `min_vector_score`, which needs `exact`, bounds the vector leg to the Documents whose `vector_score` is at least that, nearest first, paged by `offset` and `k`: a caller walks "every Document at least this close". The bound is Lance's `distance_range`, at the least distance whose score falls below it; through an IVF_PQ index of cosine vectors Lance 13.0.0 compares a bound with the index's own distances of the residuals, not with the cosine distance, which is why it needs `exact`. The lexical leg is not bounded; with `lexical_weight` 0, or a `vector` subject, every hit is (`tests/vectors.rs`: `an_exact_search_is_the_brute_force_ties_by_id_and_stays_so_after_writes_and_a_rebuild`).

`k` unset or `0` → 10; there is no upper limit: `k`, `offset`, and `rerank_limit` up to `4294967295` answer with the hits there are, because the library bounds the depth each leg asks of a table by the rows the table holds (`tests/search.rs`: `k_offset_and_rerank_limit_at_their_maximum_answer_with_the_hits_there_are`, `a_search_through_the_vector_index_finds_the_exact_nearest_and_bounds_k_at_its_maximum`; `search::tests::a_leg_asks_for_no_more_hits_than_the_table_holds`). `lexical_weight` / `vector_weight` unset → 1; **0 turns that leg off**; both 0, a negative one, or a non-finite one is `INVALID_ARGUMENT` (`tests/search.rs`: `the_weights_are_checked_and_zero_turns_a_leg_off`). The weights scale Reciprocal Rank Fusion. `offset` skips the first hits of the ranking and opens none of their files. A search ranks again on each call (a model may answer a little differently, the Index may have moved), so page 2 of a second call need not continue page 1; pages that must agree come from one search with a large enough `k`, read as a stream (`tests/search.rs`: `k_and_offset_cut_one_ranking_and_the_first_item_is_the_summary`).

Without a Projector, there are no FTS/vector columns: step 2 has nothing to rank, step 3 is off.

Steps:

1. Resolve the collections as in a query, and wait for `not_before` if it is set. A collection with `INDEX_SCOPE_NO_VECTORS` is ranked by the lexical leg only; one with `INDEX_SCOPE_IDS`, named, is `UNIMPLEMENTED`, and left out of a Vault search. If `filter` is set: the Filter (same rules as §3) narrows both legs, which search **only** the Documents it matches. Unset `filter` → the selected collections, not other collections in the same Index.
2. Lexical (weight > 0): BM25 over the Projector columns marked `fts`, in the table of each collection in scope, each by the statistics of its own table. For `text` the words of the text; for `like` the words of the `fts` columns of that Document as the Index holds them (a `text list` contributes all its texts); the Projector is not called. The Filter is applied before the legs rank (a prefilter): the top pool is taken from the Documents that match, never from a larger top list filtered afterwards (`tests/search.rs`: `the_filter_narrows_both_legs_before_they_rank`).
3. Vector (weight > 0, and vectors in the Index; for `text` and `like` an embedder too): for `text`, `EmbedQuery(text)` once; for `like`, the stored vectors of that Document, one per text of each of its `vector` columns, and no model is called (`tests/search.rs`: `like_finds_the_neighbours_of_a_document_without_calling_the_embedder`); for `vector`, the vector given, in its column only. Each `vector` column gives its nearest vectors by cosine distance: through its IVF_PQ index, the candidates ranked again with the full vectors, and by an exact scan of the rows no index covers yet (a table too small for an index, and the rows written since), so a write is found as soon as the Index holds it; with `exact`, by a scan of them all. A `text list` column does this in its texts table, and asks for twice as many texts while those it found belong to fewer Documents than the leg wants, up to every text (`06` IDX-7). The Filter admits the rows before the search, and in a texts table the texts of the Documents it matches. A Document counts once, by its closest text, or for `like` by its closest pair: the smallest distance between one text of the `like` Document and one of its own (`tests/search.rs`: `a_search_through_the_vector_index_finds_the_exact_nearest_and_bounds_k_at_its_maximum`; `tests/texts.rs`: `a_document_is_found_by_each_of_its_texts_once_and_scored_by_the_closest`, `like_ranks_the_others_by_their_closest_pair_of_texts`).
4. Depth and fusion. Each leg is asked for `max(offset + k, rerank_limit, 50)` hits, at most the rows of the table, so a Document one leg ranks low can still be promoted by the other. Reciprocal Rank Fusion: a hit at rank `r` (from 0) of a leg of weight `w` adds `w / (60 + r + 1)`; ties break by collection, then `id` (`search::tests::fusion_adds_the_weighted_reciprocal_ranks_and_breaks_ties_by_key`, `search::tests::the_depth_covers_the_page_the_rerank_and_the_floor`). `lexical_score` is the BM25 score, `vector_score` is `1 / (1 + cosine distance)` of the Document's closest text, `vector_match` names that text (its `vector` column, and its place in a `text list`, 0 in a `text`; of equal distances the first in the order of the columns, then of the list), `score` the fused one. The Document `like` names is never a hit of its own search.
5. Rerank. When `rerank_limit` is set (never with a `vector` subject), the first `rerank_limit` fused hits go to the Vault's `Reranker`: `Score` gets the query text first (for `like`, the `fts` text of its Document), then the Documents with their `raw`. The hits are ordered by its score (higher first) and carry `rerank_score`; the hits after them keep the fused order. `offset` and `k` then cut that order. Without `rerank_limit`, or without a reranker, the fused order stands and there is no `rerank_score` (`tests/search.rs`: `a_reranker_orders_the_first_hits_and_sees_the_query_first`).
6. The ranking is computed whole, but it holds keys and scores only. The stream's first item is the `SearchSummary`; the Documents are read from git as the stream reaches them, at the state the Index reflects, so a large `k` reads no file ahead; `include_raw` true fills `raw` of each hit's Document, unset or false omits it (`tests/search.rs`: `the_documents_carry_their_raw_only_when_asked_and_a_deleted_one_is_no_hit`). A hit whose file that state lacks is skipped.
7. `SearchSummary.vector_leg` is the first item of every reply, early empty ones included. It is decided before step 3 from what the Vault already holds (no diff; only the last test reads: a scan of the tables in scope that stops at the first row holding a vector), the first that applies: `VECTOR_LEG_NOT_ASKED` (`vector_weight` 0); `VECTOR_LEG_NO_VECTOR_COLLECTION` (no selected collection has a vector column: each is not `INDEX_SCOPE_ALL` or is encrypted, no Projector, or no Projector column marked `vector`; decided from the Projector's columns, not from what the Index holds); `VECTOR_LEG_NO_EMBEDDER` (no embedder, and the subject is a `text` or a `like`); `VECTOR_LEG_NO_VECTORS` (the Index holds none yet, in the column of a `vector` subject); else `VECTOR_LEG_RAN`. A vector-only search (`lexical_weight` 0) with no hits and any value but `RAN` did not search vectors at all, which is how a caller tells that from "nothing matched" without `Index.Status` (`tests/search.rs`: `the_summary_names_why_the_vector_leg_did_not_run`, `the_summary_names_a_vault_without_a_vector_collection_or_without_vectors`). An embedder of another model is not a reason of the summary: that Vault finds the Index `INDEX_STALE` and the search fails.

Index never built → `INDEX_REBUILDING` (retryable); a file of the Index that does not read → `INDEX_BROKEN` until `Index.Reindex`; another projection, other Projector columns, or another embedder model than this Vault's → `INDEX_STALE` (`06` IDX-2, IDX-9, QRY-4). A rebuild of a built Index is none of these: the live Index answers until the switch. Never another Vault’s ids, and never an empty answer in place of those codes. `not_before` that the Index does not reach in time → `DEADLINE_EXCEEDED`. What the caller put in the request, a filter or a weight, that the library refuses is `INVALID_ARGUMENT`, never an Index code.

### Reranker

Not a model inside Scheda. A port (`Reranker`, `ports.proto`), like the embedder: the **program that links the library** gives the Vault a reference to it, at `Scheda.Open` or with `Vault.SetReranker`; it is the Vault's, not an argument of a search.

```text
Score(stream ScoreItem) → stream DocumentScore
                        // the first item is the query text, the rest are the
                        // Documents (their raw set); one score per Document,
                        // in their order; higher is better
```

The implementation gathers the Documents into the portions its model wants, as `Embed` does. No reranker → order is RRF. A search sets `rerank_limit` to ask for a rerank; set without a reranker, also after `Vault.RemoveReranker`, it is `INVALID_ARGUMENT` (`tests/search.rs`: `rerank_limit_needs_a_reranker_and_covers_the_page`). Network inside the reranker is the program's problem. The library does not load a ranking model (QRY-5).

*Fixed portions.* As for the embedder: `FixedReranker::new(portion, f)` takes a blocking `f(&str, Vec<Document>) -> Result<Vec<f64>, Error>` (`RerankerRef::from_sync`), and `FixedRerankerAsync::new(portion, f)` an `f(Arc<str>, Vec<Document>)` that returns a future of that result (`RerankerRef::from_async`): the query is shared by every portion and the future may keep it. Each reads the query first and then the Documents, portions them, and checks the count as the embedder adapters do (`ports::tests::the_reranker_adapter_scores_in_portions_and_in_order`, `the_reranker_adapter_fails_the_stream_when_its_function_miscounts_or_fails`; `tests/ports.rs`: `the_async_reranking_adapter_reads_the_query_first_and_scores_in_order`). Async adapters installed on a Vault index a `vector` column and serve a reranked search on a current-thread and a multi-thread runtime (`tests/search.rs`: `async_fixed_adapters_index_a_vector_column_and_serve_a_search`).

**How many candidates it sees is the caller's choice.** Fusion depth and rerank depth answer two different questions: fusing is cheap and wants to be wide, while a cross-encoder costs tens of milliseconds per pair on a CPU and wants to be as narrow as the caller can accept. Unset `rerank_limit` means no rerank. Set to N, both legs retrieve `max(offset + k, N, 50)` and the reranker scores the first N of the fused list (or every hit that exists if the corpus is smaller), before the library returns those Documents. A large N is more BM25 and nearest-neighbour retrieval and more Document reads, not only more model calls. `rerank_limit` is at least 1 (protovalidate), because scoring nothing is what passing no reranker already means. A value below the effective `k` is `INVALID_ARGUMENT`, because a page where some hits carry `rerank_score` and some do not cannot be told from a truncated one (the same test).

CLI hook (`14`): global flags `--reranker-cohere-url` plus
`--reranker-model` (Cohere/Jina `POST` `{URL}rerank`), used by a search that sets `rerankLimit`. Both HTTP flags
or none. Unset → RRF.

---

## 7. Stored vectors

`Collection.GetVectors(VectorSelection)` gives back the vectors the Index holds in one `vector` column (`column`) for some Documents (`ids`, at least one; an id given twice comes twice), as they were stored: one `StoredVector` per vector, with the Document's `id`, the `position` of its text (0 in a `text` column), the `digest` of the text it was made from (its git blob id, the same as `git hash-object` of the text: a caller compares it with that of the text it holds now to tell a vector of the current text from one the Index has not caught up on), and its `values`. They come in the order of the ids, a `text list` in the order of its texts; an id the Index holds no vector of (no such Document in the snapshot, or no text of it with a vector) is one `absent` item. No model is called. A lazy stream: it reads portions of 64 ids, one Lance read each, as the stream reaches them, so its memory is that of one portion, however many ids it was given (about 0.5 MB for 1 000 ids as for 10 000, `vectors::memory::reading_ten_thousand_stored_vectors_holds_what_reading_a_thousand_holds`). It reads one snapshot, the `commit` the call read, for one hour: what a `commit` names stays on disk an hour from the moment before a reader read it (`06` IDX-9), so an item asked later, or a read that fails once that hour is over, is `FAILED_PRECONDITION`, and the caller asks again for the ids not given yet. A column the Projector does not mark `vector` for the collection is `INVALID_ARGUMENT`; a collection whose `index_scope` keeps no vectors, or an encrypted one, `UNIMPLEMENTED`; the Index codes are those of a query, and `not_before` waits as there (`tests/vectors.rs`: `stored_vectors_come_back_in_the_order_of_the_ids_with_the_digest_of_their_text`).

---

## 8. What this is not

- A query language string (SQL, SeekStorm, Lucene) as `Filter`, or SQL built by the library from a caller's text.
- Phrase, fuzzy, or regex leaves; the lexical leg has no positions (`06` §6).
- One vector per Document from concatenated columns.
- Embedder invoked only at "create index" and never again (it runs on every vector write and every vector search).
- Shipping a cross-encoder or LLM inside the crate (the `Reranker` is the caller's).
- `walk` / `walk_inverse` calls.
- A collection qualifier on a filter leaf: one filter applies to every collection in scope, and a column a collection lacks has no value there.
