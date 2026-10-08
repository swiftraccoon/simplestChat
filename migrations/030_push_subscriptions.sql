-- Empty-body Web Push: no message text or sender identity enters this queue.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';
CREATE TABLE push_keys (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    private_key BYTEA NOT NULL CHECK (octet_length(private_key) BETWEEN 64 AND 1024)
);
CREATE TABLE push_subscriptions (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    auth_version BIGINT NOT NULL,
    endpoint TEXT NOT NULL CHECK (char_length(endpoint) BETWEEN 1 AND 2048),
    endpoint_hash TEXT NOT NULL UNIQUE CHECK (char_length(endpoint_hash) = 64),
    generation BIGINT NOT NULL DEFAULT 0,
    pending_since TIMESTAMPTZ,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts BIGINT NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 5)
);
CREATE INDEX push_subscriptions_user ON push_subscriptions (user_id);
CREATE INDEX push_subscriptions_due ON push_subscriptions (next_attempt_at)
    WHERE pending_since IS NOT NULL;
