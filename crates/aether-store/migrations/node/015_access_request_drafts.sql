-- Access requests that an agent prepared. The person confirms each one on
-- the caller page; until then the node sends nothing.
CREATE TABLE access_request_drafts (
    id TEXT PRIMARY KEY NOT NULL,
    target_name TEXT NOT NULL UNIQUE,
    listing TEXT NOT NULL,
    signature TEXT NOT NULL,
    listing_expires_at INTEGER NOT NULL,
    days INTEGER NOT NULL CHECK (days BETWEEN 1 AND 30),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

-- The listing that a request was sent for. The line that comes back must
-- be from the endpoint that signed it.
ALTER TABLE hub_approvals ADD COLUMN listing TEXT;
ALTER TABLE hub_approvals ADD COLUMN listing_signature TEXT;
ALTER TABLE hub_approvals ADD COLUMN listing_expires_at INTEGER;
