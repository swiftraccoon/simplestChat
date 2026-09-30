-- Replace plaintext invitation capabilities with SHA-256 lookup digests and
-- unrelated UUID management identifiers. This project is in active buildout:
-- invalidate outstanding invitations and their receipts instead of carrying a
-- legacy capability format. Previously granted room memberships are unchanged.
DELETE FROM invites;
ALTER TABLE invite_redemptions DROP CONSTRAINT invite_redemptions_code_fkey;
ALTER TABLE invites ALTER COLUMN code TYPE VARCHAR(64);
ALTER TABLE invite_redemptions ALTER COLUMN code TYPE VARCHAR(64);
ALTER TABLE invites RENAME COLUMN code TO code_hash;
ALTER TABLE invite_redemptions RENAME COLUMN code TO invite_hash;
ALTER TABLE invites ADD COLUMN id UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE invites ADD CONSTRAINT invites_id_unique UNIQUE (id);
ALTER TABLE invites ADD CONSTRAINT invites_hash_shape CHECK (code_hash ~ '^[0-9a-f]{64}$');
ALTER TABLE invite_redemptions ADD CONSTRAINT invite_redemptions_invite_hash_fkey
    FOREIGN KEY (invite_hash) REFERENCES invites(code_hash) ON DELETE CASCADE;
