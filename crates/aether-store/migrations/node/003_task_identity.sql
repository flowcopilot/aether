CREATE TABLE tasks_new (
    id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL UNIQUE,
    idempotency_key TEXT NOT NULL,
    caller_agent_id TEXT NOT NULL,
    caller_endpoint_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    capability_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    access_mode TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(caller_endpoint_id, idempotency_key)
);

INSERT INTO tasks_new(id,request_id,idempotency_key,caller_agent_id,caller_endpoint_id,request_fingerprint,scope_id,capability_id,workspace_id,access_mode,state,created_at)
SELECT id,request_id,idempotency_key,caller_agent_id,'','',scope_id,capability_id,workspace_id,access_mode,state,created_at FROM tasks;

CREATE TABLE task_events_new AS SELECT * FROM task_events;
CREATE TABLE execution_leases_new AS SELECT * FROM execution_leases;

DROP TABLE execution_leases;
DROP TABLE task_events;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;

ALTER TABLE task_events_new RENAME TO task_events;
ALTER TABLE execution_leases_new RENAME TO execution_leases;

CREATE INDEX tasks_queue_index ON tasks(state, caller_agent_id, created_at);
CREATE INDEX task_events_task_index ON task_events(task_id, sequence);
