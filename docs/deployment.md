# Deployment and client wiring

## Cloud instance

Run one service process owning one disk-backed DuckDB file on durable storage. Bind the service privately behind a reverse proxy that terminates TLS; the public URL must be stable and end in `/mcp`. Configure the proxy to preserve `Authorization`, `Origin`, `Accept`, `Content-Type`, and `MCP-Protocol-Version`. Restrict ingress to MCP, health checks and the separate replication path; keep operator paths private. Never mount the same DuckDB file into another writer process.

Example environment (replace placeholders using a secret manager):

```sh
AGENTRECALL_ROLE=cloud
AGENTRECALL_BIND_ADDR=127.0.0.1:8787
AGENTRECALL_DATABASE_PATH=/var/lib/agentrecall/cloud.duckdb
AGENTRECALL_TOKENS_FILE=/etc/agentrecall/tokens.json
AGENTRECALL_REPLICA_ID=cloud-<stable-id>
AGENTRECALL_ALLOWED_HOSTS=memory.example.net
AGENTRECALL_ALLOWED_ORIGINS=https://<approved-client-origin>
AGENTRECALL_CURSOR_HMAC_SECRET=<at-least-32-random-bytes>
# During rotation only; configure both together. Deadline must be RFC 3339.
# AGENTRECALL_CURSOR_HMAC_PREVIOUS_SECRET=<previous-cursor-secret>
# AGENTRECALL_CURSOR_HMAC_PREVIOUS_UNTIL=<RFC3339-UTC-deadline>
AGENTRECALL_REPLICATION_TOKEN=<cloud-accepts-local-replication-token>
AGENTRECALL_TRUSTED_REPLICA_ID=local-<stable-id>
```

Provision a distinct Cue bearer credential in the cloud token file. Do not use the local Hermes credential on the cloud instance. Configure the reverse proxy's TLS certificate verification and do not publish the service on plaintext HTTP.

## Local service and replication

Run a separate local service process with its own DuckDB path and Hermes client credential. Keep it bound to loopback by default. The local service initiates the exchange, so it works behind a home NAT/firewall:

```sh
AGENTRECALL_ROLE=local
AGENTRECALL_BIND_ADDR=127.0.0.1:8787
AGENTRECALL_DATABASE_PATH=$HOME/.local/share/agentrecall/local.duckdb
AGENTRECALL_TOKENS_FILE=$HOME/.config/agentrecall/tokens.json
AGENTRECALL_REPLICA_ID=local-<stable-id>
AGENTRECALL_ALLOWED_HOSTS=localhost,127.0.0.1,::1
AGENTRECALL_PEER_URL=https://memory.example.net/_internal/replication/exchange
AGENTRECALL_PEER_TOKEN=<credential-accepted-by-cloud-replication-endpoint>
AGENTRECALL_PEER_REPLICA_ID=cloud-<stable-id>
AGENTRECALL_CURSOR_HMAC_SECRET=<at-least-32-random-bytes>
# During rotation only; configure both together. Deadline must be RFC 3339.
# AGENTRECALL_CURSOR_HMAC_PREVIOUS_SECRET=<previous-cursor-secret>
# AGENTRECALL_CURSOR_HMAC_PREVIOUS_UNTIL=<RFC3339-UTC-deadline>
```

The cloud side sets `AGENTRECALL_REPLICATION_TOKEN` to the credential sent by the local service and `AGENTRECALL_TRUSTED_REPLICA_ID` to that local service's stable ID. The service rejects an authenticated exchange if its declared source ID differs from this trusted ID. The token is separate from Cue/Hermes client tokens. Use different secrets from MCP credentials, rotate them deliberately, and never place them in MCP tool arguments. `AGENTRECALL_REPLICA_ID` must remain stable for the life of a database file; if omitted at first startup, the service creates and persists one, which is returned in readiness metadata. Obtain each replica ID from its operator/deployment configuration, not from a client-supplied argument.

Each side commits local operations before responding. Replication exchanges durable sequence-ordered events and resumes after restart. Cross-replica visibility is eventual. Do not copy DuckDB files to synchronize them.

## Semantic search

Provision matching DuckDB extensions explicitly for the pinned build and service account with `agentrecall install-vss` (and `agentrecall install-fts` for BM25). Startup only loads installed extensions; it does not download them. VSS HNSW indexes are rebuildable, per-owner/per-namespace in-memory projections. Persisted embeddings carry model/version/dimension/revision metadata; do not enable experimental HNSW disk persistence. If VSS cannot load or an index/query fails, the service uses exact vector scoring over persisted embeddings filtered by owner and namespace; if no embedding provider is active, search stays lexical.

The CLI includes an opt-in local BGE-M3 provider. Set `AGENTRECALL_EMBEDDING_PROVIDER=bge-m3` to enable semantic retrieval; first use may download model assets to the local cache, while embeddings run in process and memory text stays local. Without this setting, search remains lexical. Readiness reports `search_mode` and `search_engine`, and should show hybrid/HNSW or hybrid/exact mode only while semantic configuration is active. See [VSS notes](vss-reference.md).

## Hermes local-first router

On the Hermes machine, run `agentrecall serve-router`. It is loopback-only and defaults to `127.0.0.1:8788`. Supply these secrets through a secure local environment mechanism:

```sh
AGENTRECALL_ROUTER_BIND_ADDR=127.0.0.1:8788
AGENTRECALL_ROUTER_LOCAL_URL=http://127.0.0.1:8787/mcp
AGENTRECALL_ROUTER_CLOUD_URL=https://memory.example.net/mcp
AGENTRECALL_ROUTER_HERMES_TOKEN=<router-ingress-token>
AGENTRECALL_ROUTER_LOCAL_SERVICE_TOKEN=<local-Hermes-service-token>
AGENTRECALL_ROUTER_CLOUD_SERVICE_TOKEN=<cloud-fallback-token>
```

Configure Hermes with one Streamable HTTP MCP endpoint: `http://127.0.0.1:8788/mcp`. Give Hermes only the router-ingress token; the router injects separate service credentials and never returns them to Hermes. A confirmed connection-establishment failure to local may be routed to cloud for that request. A timeout, reset or lost response after sending is ambiguous and returns a temporary error; never automatically retry/fail over such a write. The router never queries DuckDB.

## Cue and active memory behavior

Register Cue to the cloud HTTPS endpoint `/mcp` with Cue's own read/write credential. Install the policy in [memory-policy.md](memory-policy.md). Register Hermes only to the local router, not simultaneously to local and cloud endpoints. Install the same policy in Hermes. Verify real discovery and calls from both clients, a tagged save in each direction, and eventual cross-replica retrieval. Tool registration alone is not proof that either agent actively uses memory.
