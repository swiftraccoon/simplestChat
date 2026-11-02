SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';

-- Opaque access tokens: one row per token a sign-in or refresh issued. Only
-- the hex SHA-256 digest of a token's text is stored, computed by the server
-- (`session::hash_token`, the rule refresh tokens use). Browser tabs share a
-- session but each holds its own token, so a session has several rows; the
-- issuer prunes a session's expired rows and keeps at most 32 live ones
-- (`MAX_ACCESS_TOKENS_PER_SESSION` in src/auth/access_tokens.rs). A token is
-- accepted while it and its session are unexpired and its account still has
-- the version recorded at issue. Deleting a session deletes its tokens.
CREATE TABLE session_access_tokens (
    token_hash TEXT PRIMARY KEY CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    auth_version BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX session_access_tokens_session
    ON session_access_tokens (session_id, created_at);
