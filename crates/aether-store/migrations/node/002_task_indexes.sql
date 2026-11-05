CREATE INDEX IF NOT EXISTS tasks_queue_index
ON tasks(state, caller_agent_id, created_at);

CREATE INDEX IF NOT EXISTS task_events_task_index
ON task_events(task_id, sequence);
