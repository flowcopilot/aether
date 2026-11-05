CREATE TABLE access_groups (
    name TEXT PRIMARY KEY NOT NULL
);

CREATE TABLE access_group_members (
    group_name TEXT NOT NULL REFERENCES access_groups(name) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    PRIMARY KEY (group_name, agent_id, endpoint_id)
);

CREATE INDEX access_group_members_identity
    ON access_group_members(agent_id, endpoint_id);

INSERT INTO access_groups(name) VALUES ('friends');
