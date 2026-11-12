-- For each agent ID and signing key: the latest expiry of a record that the
-- directory took, and the latest expiry at the last revocation. A record
-- that does not expire after `revoked_through` was public before the
-- revocation, so anyone could send it again; the directory refuses it.
CREATE TABLE directory_record_marks (
    agent_id TEXT NOT NULL,
    signing_key BLOB NOT NULL,
    latest_expires_at INTEGER NOT NULL,
    revoked_through INTEGER,
    PRIMARY KEY(agent_id, signing_key)
);
