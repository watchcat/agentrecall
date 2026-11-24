# DuckDB VSS implementation notes

Source: [DuckDB VSS extension documentation](https://duckdb.org/docs/stable/core_extensions/vss.html)

DuckDB VSS adds HNSW indexing for fixed-size `FLOAT[N]` arrays. The implementation keeps embeddings as model/version-tagged derived rows in the disk-backed database, then rebuilds fixed-size vector tables and HNSW indexes in a separate in-memory DuckDB connection. It does not enable `hnsw_enable_experimental_persistence`.

The extension's documented pattern is `CREATE INDEX ... USING HNSW (embedding) WITH (metric='cosine')`, followed by nearest-neighbor queries ordered by `array_cosine_distance(embedding, <fixed-size-vector>)` and a `LIMIT`. This service uses a distinct in-memory table for each owner/namespace, so a global top-k cannot return another scope's candidates. It checks canonical live records again before returning results.

DuckDB documents that deletes are marked in HNSW and can make the graph stale. This implementation synchronously rebuilds the affected owner/namespace HNSW index after vector writes and updates; deletes remove the vector and recreate the index as well. Search also purges newly expired records from the persisted embedding cache and FTS projection, removes their in-memory vector rows, and rebuilds the affected HNSW scope before candidate retrieval. This keeps the next search current instead of relying on index update behavior, at the cost of mutation/search latency that still needs production measurement. If VSS or index creation is unavailable, semantic retrieval uses an exact vector scan filtered to the authenticated owner/namespace and canonical live records. Without a configured embedding adapter, search remains lexical.

Service startup only attempts `LOAD vss`; it never downloads extensions. Operators provision a matching extension explicitly with `agentrecall install-vss` for the pinned bundled DuckDB build.
