-- ClickHouse table for Clawpot events.
-- Ingest via: clickhouse-client --query "INSERT INTO clawpot_events FORMAT JSONEachLine" < events.jsonl

CREATE TABLE IF NOT EXISTS clawpot_events
(
    id              UInt64,
    session_id      String,
    timestamp       DateTime64(3, 'UTC'),
    category        LowCardinality(String),
    event_type      LowCardinality(String),
    vm_id           Nullable(String),
    correlation_id  Nullable(String),
    duration_ms     Nullable(Int64),
    success         Nullable(Bool),
    data            String  -- JSON string
)
ENGINE = MergeTree()
ORDER BY (session_id, timestamp, id)
PARTITION BY toYYYYMM(timestamp);
