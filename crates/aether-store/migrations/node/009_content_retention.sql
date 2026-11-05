ALTER TABLE tasks ADD COLUMN input_expires_at INTEGER;
ALTER TABLE tasks ADD COLUMN content_expired_at INTEGER;

CREATE INDEX tasks_input_expiry_index ON tasks(input_expires_at);

UPDATE tasks
SET input_expires_at = result_expires_at
WHERE state IN ('succeeded', 'failed', 'rejected', 'cancelled')
  AND result_expires_at IS NOT NULL;
