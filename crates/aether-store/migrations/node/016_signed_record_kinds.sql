-- Directory records get ids of their own, apart from the `direct:` ids of
-- contacts, so that a directory scope named `direct` cannot replace one.
UPDATE signed_records
SET id = 'directory:' || scope_id || ':' || agent_id
WHERE id NOT LIKE 'direct:%';
