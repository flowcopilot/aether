CREATE TABLE task_events_new (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK(sequence > 0),
    event_type TEXT NOT NULL CHECK(length(event_type) > 0),
    data TEXT NOT NULL CHECK(json_valid(data)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, sequence)
);

INSERT INTO task_events_new(task_id,sequence,event_type,data,created_at)
SELECT task_id,sequence,event_type,data,created_at FROM task_events;

CREATE TABLE execution_leases_new (
    task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    owner TEXT NOT NULL CHECK(length(owner) > 0),
    execution_id TEXT NOT NULL CHECK(length(execution_id) > 0),
    expires_at INTEGER NOT NULL
);

INSERT INTO execution_leases_new(task_id,owner,execution_id,expires_at)
SELECT task_id,owner,execution_id,expires_at FROM execution_leases;

DROP TABLE execution_leases;
DROP TABLE task_events;
ALTER TABLE task_events_new RENAME TO task_events;
ALTER TABLE execution_leases_new RENAME TO execution_leases;
CREATE INDEX task_events_task_index ON task_events(task_id, sequence);
