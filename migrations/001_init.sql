CREATE TABLE IF NOT EXISTS service_meta (
    key VARCHAR PRIMARY KEY,
    value VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS memories (
    owner_id VARCHAR NOT NULL,
    memory_id VARCHAR NOT NULL,
    namespace VARCHAR NOT NULL,
    memory_json VARCHAR,
    content VARCHAR,
    title VARCHAR,
    tags VARCHAR,
    expires_at VARCHAR,
    deleted_at VARCHAR,
    revision BIGINT NOT NULL,
    version_event_id VARCHAR,
    PRIMARY KEY (owner_id, memory_id)
);
CREATE INDEX IF NOT EXISTS memories_owner_namespace_live_idx
    ON memories (owner_id, namespace, deleted_at);

-- Optional rebuildable search data; canonical memories and replication events remain authoritative.
CREATE TABLE IF NOT EXISTS memory_embeddings (
    owner_id VARCHAR NOT NULL,
    namespace VARCHAR NOT NULL,
    memory_id VARCHAR NOT NULL,
    model_name VARCHAR NOT NULL,
    model_version VARCHAR NOT NULL,
    dimension INTEGER NOT NULL,
    revision BIGINT NOT NULL,
    embedding_json VARCHAR NOT NULL,
    PRIMARY KEY (owner_id, namespace, memory_id, model_name, model_version)
);

CREATE TABLE IF NOT EXISTS mutation_log (
    event_id VARCHAR PRIMARY KEY,
    origin_replica_id VARCHAR NOT NULL,
    origin_seq BIGINT NOT NULL,
    owner_id VARCHAR NOT NULL,
    client_id VARCHAR NOT NULL,
    memory_id VARCHAR NOT NULL,
    event_json VARCHAR NOT NULL,
    namespace VARCHAR NOT NULL,
    operation VARCHAR NOT NULL,
    base_event_id VARCHAR,
    payload_json VARCHAR NOT NULL,
    idempotency_json VARCHAR,
    created_at VARCHAR NOT NULL,
    UNIQUE (origin_replica_id, origin_seq)
);

CREATE TABLE IF NOT EXISTS replica_state (
    peer_replica_id VARCHAR PRIMARY KEY,
    inbound_cursor BIGINT NOT NULL DEFAULT 0,
    outbound_ack BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS applied_events (
    event_id VARCHAR PRIMARY KEY,
    origin_replica_id VARCHAR NOT NULL,
    origin_seq BIGINT NOT NULL,
    applied_at VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS idempotency_ledger (
    owner_id VARCHAR NOT NULL,
    client_id VARCHAR NOT NULL,
    client_request_id VARCHAR NOT NULL,
    request_fingerprint VARCHAR NOT NULL,
    result_json VARCHAR NOT NULL,
    event_id VARCHAR NOT NULL,
    PRIMARY KEY (owner_id, client_id, client_request_id)
);

CREATE TABLE IF NOT EXISTS replication_conflicts (
    owner_id VARCHAR NOT NULL,
    memory_id VARCHAR NOT NULL,
    namespace VARCHAR NOT NULL,
    branches_json VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL,
    PRIMARY KEY (owner_id, memory_id)
);

CREATE TABLE IF NOT EXISTS conflict_audit (
    audit_id VARCHAR PRIMARY KEY,
    owner_id VARCHAR NOT NULL,
    memory_id VARCHAR NOT NULL,
    action VARCHAR NOT NULL,
    branch_event_ids_json VARCHAR NOT NULL,
    actor_id VARCHAR NOT NULL,
    created_at VARCHAR NOT NULL
);
