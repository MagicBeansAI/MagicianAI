CREATE SEQUENCE IF NOT EXISTS event_id_seq;
CREATE TABLE IF NOT EXISTS events (
    id BIGINT DEFAULT nextval('event_id_seq'),
    timestamp TIMESTAMPTZ NOT NULL,
    event_type VARCHAR NOT NULL,
    source VARCHAR NOT NULL,
    payload JSON NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_type_ts ON events (event_type, timestamp);
