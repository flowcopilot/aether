-- The hub invites that this node's own page sent or approved. The node
-- signs a grant for a hub row only when a row here names it.
CREATE TABLE hub_approvals (
    invite_id TEXT PRIMARY KEY NOT NULL,
    side TEXT NOT NULL CHECK (side IN ('sender', 'receiver')),
    kind TEXT NOT NULL CHECK (kind IN ('invite', 'request')),
    peer_name TEXT NOT NULL,
    peer_endpoint TEXT,
    days INTEGER NOT NULL CHECK (days BETWEEN 1 AND 30),
    access_group TEXT NOT NULL DEFAULT 'friends',
    bundle TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX hub_approvals_peer_endpoint ON hub_approvals(peer_endpoint);
