# DuckDB FTS implementation notes

Source: [DuckDB Full-Text Search extension documentation](https://duckdb.org/docs/stable/core_extensions/full_text_search.html)

The DuckDB FTS extension is not part of the core SQL engine and must be loaded. Its `create_fts_index` macro creates an FTS schema for a table; `match_bm25(document_id, query)` returns BM25 scores. The index does **not** update automatically when input rows change; the documented refresh workaround is to recreate the index. The service therefore must refresh the derived FTS projection synchronously after accepted writes/deletes or use the canonical-text lexical fallback until refresh succeeds.

The extension's default stemmer is Porter and its default stopword set is English. It supports Dutch and Russian stemmers, but one index uses one stemmer; to avoid inappropriate English defaults for multilingual memories, use a no-stemming/no-stopword configuration or separate language-specific indexes. Search queries and candidate results must still be scoped to owner and namespace before returning data.

The service startup path must only `LOAD` a locally provisioned extension and must never run an implicit `INSTALL`/download. Provision the FTS binary explicitly with the same pinned DuckDB engine/build, then rebuild all derived scope indexes at service startup. If loading or rebuilding fails, retain canonical records and lexical search and report lexical mode in health/tool output.
