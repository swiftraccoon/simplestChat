-- Each login session has one stable, high-entropy family secret and one
-- rotating token secret. Only hashes are stored. A previously consumed token
-- still carries the family secret, allowing its replay to revoke the current
-- successor without retaining an unbounded token-history table.
ALTER TABLE sessions
    ADD COLUMN refresh_token_family_hash VARCHAR(64),
    ADD COLUMN previous_refresh_token_hash VARCHAR(64),
    ADD COLUMN refresh_token_rotated_at TIMESTAMPTZ;

-- Existing UUID refresh tokens become the family secret for their session.
-- The first refresh wraps that secret in the versioned opaque-token format;
-- existing users therefore keep their sessions across this migration.
UPDATE sessions
SET refresh_token_family_hash = refresh_token_hash;

ALTER TABLE sessions
    ADD CONSTRAINT sessions_refresh_token_family_hash_format
    CHECK (
        refresh_token_family_hash IS NULL
        OR refresh_token_family_hash ~ '^[0-9a-f]{64}$'
    ),
    ADD CONSTRAINT sessions_previous_refresh_token_hash_format
    CHECK (
        previous_refresh_token_hash IS NULL
        OR previous_refresh_token_hash ~ '^[0-9a-f]{64}$'
    ),
    ADD CONSTRAINT sessions_previous_refresh_token_is_rotated
    CHECK (
        (previous_refresh_token_hash IS NULL) = (refresh_token_rotated_at IS NULL)
    );

-- NULL remains permitted only for compatibility with an older application
-- binary during a rolling deployment. The new binary always writes a family
-- hash and fails closed if this migration has not run.
CREATE UNIQUE INDEX idx_sessions_refresh_token_family_hash_unique
    ON sessions (refresh_token_family_hash)
    WHERE refresh_token_family_hash IS NOT NULL;

CREATE UNIQUE INDEX idx_sessions_previous_refresh_token_hash_unique
    ON sessions (previous_refresh_token_hash)
    WHERE previous_refresh_token_hash IS NOT NULL;
