-- Migration 009 only backfilled `input_expires_at` for rows whose
-- `result_expires_at` was already set, which is true only for `succeeded`
-- rows whose result had never been cleared. `failed`, `rejected`, and
-- `cancelled` rows, and any `succeeded` row already cleared by the older
-- `clear_expired_results` (which predates the `content_expired_at` column),
-- were left with no expiry at all and would keep their input forever.
--
-- Backfill every terminal row still missing an expiry using its own
-- completion evidence: the latest recorded event for that task, or its
-- admission time if no event exists. This intentionally does not start a
-- fresh 24-hour window from the time this migration runs — these rows
-- become immediately eligible for the next cleanup pass, exactly as if
-- migration 009 had backfilled them correctly the first time.
UPDATE tasks
SET input_expires_at = COALESCE(
        (SELECT MAX(task_events.created_at)
         FROM task_events
         WHERE task_events.task_id = tasks.id),
        tasks.created_at
    ) + 86400
WHERE input_expires_at IS NULL
  AND content_expired_at IS NULL
  AND state IN ('succeeded', 'failed', 'rejected', 'cancelled', 'unknown_result');
