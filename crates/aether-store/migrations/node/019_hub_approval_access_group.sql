-- Deleting a group cancels its approvals. Never move an approval to a
-- different group: that would change the access that the owner approved.
CREATE TABLE hub_approvals_new (
    invite_id TEXT PRIMARY KEY NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('sender', 'receiver')),
    kind TEXT NOT NULL CHECK (kind IN ('invite', 'request')),
    peer_name TEXT NOT NULL,
    peer_endpoint TEXT,
    days INTEGER NOT NULL CHECK (days BETWEEN 1 AND 30),
    access_group TEXT NOT NULL DEFAULT 'friends'
        REFERENCES access_groups(name) ON DELETE CASCADE,
    bundle TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    listing TEXT,
    listing_signature TEXT,
    listing_expires_at INTEGER
);

-- Old approvals for missing groups cannot complete. Discard them.
INSERT INTO hub_approvals_new
    (invite_id, side, kind, peer_name, peer_endpoint, days, access_group,
     bundle, created_at, expires_at, listing, listing_signature, listing_expires_at)
SELECT invite_id, side, kind, peer_name, peer_endpoint, days, access_group,
       bundle, created_at, expires_at, listing, listing_signature, listing_expires_at
FROM hub_approvals
WHERE access_group IN (SELECT name FROM access_groups);

DROP TABLE hub_approvals;
ALTER TABLE hub_approvals_new RENAME TO hub_approvals;
CREATE INDEX hub_approvals_peer_endpoint ON hub_approvals(peer_endpoint);
CREATE INDEX hub_approvals_access_group ON hub_approvals(access_group);
