-- Grants from access requests join their own group, apart from friends.
INSERT OR IGNORE INTO access_groups(name) VALUES ('requests');

-- The endpoints that the owner blocked on the hub. The host signs no grant
-- for them, whatever the hub sends.
CREATE TABLE hub_blocks (
    endpoint TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
