CREATE TABLE IF NOT EXISTS configuration (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scopes (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS signed_records (
    id TEXT PRIMARY KEY NOT NULL,
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    agent_id TEXT NOT NULL,
    record BLOB NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS grants (
    id TEXT PRIMARY KEY NOT NULL,
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    subject_agent_id TEXT NOT NULL,
    subject_endpoint_id TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS invitations (
    id TEXT PRIMARY KEY NOT NULL,
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    expires_at INTEGER NOT NULL,
    redeemed_at INTEGER
);

CREATE TABLE IF NOT EXISTS audit_records (
    id INTEGER PRIMARY KEY,
    actor_type TEXT NOT NULL,
    action TEXT NOT NULL,
    result TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
