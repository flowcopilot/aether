-- The tasks that this node sent to other agents: the outbox while they
-- wait or work, and the inbox when they end.
CREATE TABLE sent_tasks (
    task_id TEXT PRIMARY KEY NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    agent_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    authorization_json TEXT NOT NULL,
    text TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK (state IN ('waiting', 'working', 'answered', 'failed', 'not_delivered')),
    reason TEXT,
    host_state TEXT,
    answer TEXT,
    last_event_sequence INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    next_attempt_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    finished_at INTEGER,
    read_at INTEGER,
    lease_until INTEGER
);

CREATE INDEX sent_tasks_due ON sent_tasks(state, next_attempt_at);
CREATE INDEX sent_tasks_endpoint ON sent_tasks(endpoint_id, state);
CREATE INDEX sent_tasks_created ON sent_tasks(created_at, task_id);
