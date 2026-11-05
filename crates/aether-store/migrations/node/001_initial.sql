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

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL UNIQUE,
    idempotency_key TEXT NOT NULL UNIQUE,
    caller_agent_id TEXT NOT NULL,
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    capability_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    access_mode TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS task_events (
    task_id TEXT NOT NULL REFERENCES tasks(id),
    sequence INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, sequence)
);

CREATE TABLE IF NOT EXISTS execution_leases (
    task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(id),
    owner TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_changes (
    id TEXT PRIMARY KEY NOT NULL,
    change_type TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_records (
    id INTEGER PRIMARY KEY,
    actor_type TEXT NOT NULL,
    action TEXT NOT NULL,
    result TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
