ALTER TABLE tasks ADD COLUMN skill TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN remote_text TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN final_result TEXT;
ALTER TABLE tasks ADD COLUMN result_expires_at INTEGER;

CREATE INDEX tasks_result_expiry_index ON tasks(result_expires_at);
