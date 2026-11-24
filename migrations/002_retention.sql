CREATE TABLE IF NOT EXISTS event_log_watermarks (
    origin_replica_id VARCHAR PRIMARY KEY,
    pruned_through BIGINT NOT NULL,
    updated_at VARCHAR NOT NULL
);
