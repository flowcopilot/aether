CREATE TABLE managed_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type TEXT NOT NULL CHECK(event_type IN ('joined', 'revoked')),
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    agent_id TEXT NOT NULL,
    endpoint_id TEXT,
    role TEXT,
    expires_at INTEGER,
    created_at INTEGER NOT NULL,
    CHECK(
        (event_type = 'joined' AND endpoint_id IS NOT NULL AND role IS NOT NULL AND expires_at IS NOT NULL)
        OR
        (event_type = 'revoked' AND endpoint_id IS NULL AND role IS NULL AND expires_at IS NULL)
    )
);

CREATE INDEX managed_events_scope_sequence_index
    ON managed_events(scope_id, sequence);
