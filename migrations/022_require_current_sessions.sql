-- The current authentication contract requires every access token to name a
-- live session and every refresh token to carry a 256-bit family capability.
-- Existing hashes cannot identify which earlier refresh format issued a row.
-- Deliberately invalidate all sign-ins once instead of retaining format parsers.
-- Users, password/passkey credentials, recovery keys and memberships are untouched.
DELETE FROM sessions;

ALTER TABLE sessions
    ALTER COLUMN refresh_token_family_hash SET NOT NULL;
