ALTER TABLE memories ADD COLUMN IF NOT EXISTS current_version_event_json VARCHAR;

UPDATE memories
SET current_version_event_json = mutation_log.event_json
FROM mutation_log
WHERE memories.version_event_id = mutation_log.event_id
  AND memories.current_version_event_json IS NULL;

-- Older compaction retained current-version rows even below an advanced watermark.
-- Their canonical envelopes are now backfilled, so those history rows are safe to remove.
DELETE FROM mutation_log
WHERE EXISTS (
    SELECT 1 FROM event_log_watermarks
    WHERE event_log_watermarks.origin_replica_id = mutation_log.origin_replica_id
      AND mutation_log.origin_seq <= event_log_watermarks.pruned_through
);

DELETE FROM applied_events
WHERE EXISTS (
    SELECT 1 FROM event_log_watermarks
    WHERE event_log_watermarks.origin_replica_id = applied_events.origin_replica_id
      AND applied_events.origin_seq <= event_log_watermarks.pruned_through
);
