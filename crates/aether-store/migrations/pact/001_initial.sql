-- One conversation between a personal agent's user and one Brand (PACT §4.2).
-- `brand_user` is set the first time a turn runs under a delegation token,
-- and a token for another Brand user may not continue it (§5.5).
CREATE TABLE pact_contexts (
    context_id TEXT PRIMARY KEY,
    brand_id TEXT NOT NULL,
    pa_issuer TEXT NOT NULL,
    pa_subject TEXT NOT NULL,
    brand_user TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    closed_at INTEGER
) STRICT;

CREATE INDEX pact_contexts_open_index
    ON pact_contexts(updated_at) WHERE closed_at IS NULL;

-- One `message:send` in a context. `response` is the stored A2A reply that a
-- retry with the same `messageId` gets back (§4.3); it is NULL while the
-- turn runs. The text columns feed the transcript of later turns.
CREATE TABLE pact_messages (
    context_id TEXT NOT NULL REFERENCES pact_contexts(context_id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    request_text TEXT NOT NULL,
    reply_text TEXT,
    response TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (context_id, message_id),
    UNIQUE (context_id, sequence)
) STRICT;

-- RFC 8628 device authorizations (§5.3). The device code itself is never
-- stored, only its digest.
CREATE TABLE pact_device_authorizations (
    device_code_hash TEXT PRIMARY KEY,
    user_code TEXT NOT NULL UNIQUE,
    brand_id TEXT NOT NULL,
    pa_issuer TEXT NOT NULL,
    pa_subject TEXT NOT NULL,
    scopes TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('pending', 'approved', 'denied', 'consumed')),
    grant_id TEXT,
    interval_seconds INTEGER NOT NULL,
    last_polled_at INTEGER,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

-- What the owner approved. The same id names the signed Aether grant, which
-- is the authority on revocation.
CREATE TABLE pact_grants (
    grant_id TEXT PRIMARY KEY,
    brand_id TEXT NOT NULL,
    pa_issuer TEXT NOT NULL,
    pa_subject TEXT NOT NULL,
    brand_user TEXT NOT NULL,
    scopes TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

-- Single-use refresh tokens, stored as digests.
CREATE TABLE pact_refresh_tokens (
    token_hash TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL REFERENCES pact_grants(grant_id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
) STRICT;

-- Signed receipts (§5.6), kept for the trust layer. They hold no message
-- text, so they outlive the context they came from.
CREATE TABLE pact_receipts (
    context_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    jws TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (context_id, message_id)
) STRICT;
