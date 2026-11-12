-- The caller chooses the task ID and the request ID, and `tasks.id` and
-- `tasks.request_id` are unique for the whole table. When another endpoint
-- already has a row with one of them, the new row gets IDs that the node
-- makes, and these columns keep the IDs that the caller sent. NULL: the row
-- has the caller's IDs.
ALTER TABLE tasks ADD COLUMN caller_task_id TEXT;
ALTER TABLE tasks ADD COLUMN caller_request_id TEXT;

CREATE INDEX tasks_caller_task_index ON tasks(caller_endpoint_id, caller_task_id);
