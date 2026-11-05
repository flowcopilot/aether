CREATE TABLE managed_invitations (
    id TEXT PRIMARY KEY NOT NULL,
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    token_hash BLOB NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL,
    redeemed_at INTEGER,
    redeemed_agent_id TEXT
);

CREATE TABLE managed_members (
    scope_id TEXT NOT NULL REFERENCES scopes(id),
    agent_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(length(role) > 0),
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    PRIMARY KEY(scope_id, agent_id),
    UNIQUE(scope_id, endpoint_id)
);

CREATE INDEX managed_members_roster_index
    ON managed_members(scope_id, revoked_at, expires_at, agent_id);
