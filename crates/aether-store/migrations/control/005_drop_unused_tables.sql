-- The control service keeps managed invitations in `managed_invitations`
-- and has no configuration, grants or audit log; nothing used these tables.
DROP TABLE configuration;
DROP TABLE grants;
DROP TABLE invitations;
DROP TABLE audit_records;
