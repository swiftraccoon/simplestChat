SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';

-- Profiles name an avatar by a digest of the stored picture instead of carrying
-- its bytes, and the avatar endpoint answers only the exact current digest.
-- The profile update writes the column with the same rule (`avatar_digest` in
-- src/auth/account.rs): the first 16 hex characters of the SHA-256 of the
-- stored text. TEXT like rooms.image_version: squawk refuses varchar(n).
ALTER TABLE users ADD COLUMN avatar_digest TEXT;
UPDATE users SET avatar_digest = left(encode(sha256(convert_to(avatar_url, 'UTF8')), 'hex'), 16) WHERE avatar_url IS NOT NULL;
