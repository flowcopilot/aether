CREATE TABLE replay_entries (
    caller_endpoint_id TEXT NOT NULL,
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    replay_key TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY(caller_endpoint_id, scope_id, replay_key)
);

CREATE INDEX replay_entries_expiry_index ON replay_entries(expires_at);
