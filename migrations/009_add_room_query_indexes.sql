-- Keep per-owner room quota checks from scanning the entire rooms table.
CREATE INDEX IF NOT EXISTS idx_rooms_owner_id ON rooms (owner_id);

-- The public room list filters on non-secret rooms and orders newest-first.
CREATE INDEX IF NOT EXISTS idx_rooms_public_created_at
    ON rooms (created_at DESC)
    WHERE secret = false;

-- Support opportunistic expired-session cleanup without a table scan.
CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions (expires_at);

-- Refresh-token hashes identify a single-use session. Build the unique index
-- under a new name first so a duplicate-data failure leaves the existing
-- lookup index intact, then remove the redundant non-unique index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_refresh_token_hash_unique
    ON sessions (refresh_token_hash);
DROP INDEX IF EXISTS idx_sessions_token_hash;
