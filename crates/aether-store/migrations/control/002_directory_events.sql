CREATE TABLE directory_events (
    sequence INTEGER PRIMARY KEY,
    event_type TEXT NOT NULL CHECK(event_type IN ('published', 'renewed', 'revoked')),
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    agent_id TEXT NOT NULL,
    record BLOB,
    created_at INTEGER NOT NULL,
    CHECK(
        (event_type IN ('published', 'renewed') AND record IS NOT NULL)
        OR (event_type = 'revoked' AND record IS NULL)
    )
);

CREATE INDEX directory_events_sequence_index ON directory_events(sequence);
