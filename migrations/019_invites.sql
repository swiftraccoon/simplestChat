-- Invitations: a registration code lets someone in while registration is
-- closed (single use, minted by any account, a few live at a time); a room
-- code grants a role in one room to whoever redeems it (multi-use, minted by
-- the room's admins). Codes are random 20-character strings; a redeemed code
-- counts down and an expired or exhausted one is refused. Rooms and accounts
-- take their invitations with them when they go.
CREATE TABLE IF NOT EXISTS invites (
    code VARCHAR(32) PRIMARY KEY,
    kind VARCHAR(16) NOT NULL CHECK (kind IN ('registration', 'room')),
    room_id VARCHAR(128) REFERENCES rooms(id) ON DELETE CASCADE,
    role SMALLINT CHECK (role BETWEEN 1 AND 3),
    created_by UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    uses_left INTEGER NOT NULL CHECK (uses_left >= 0),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT invites_kind_fields CHECK (
        (kind = 'room') = (room_id IS NOT NULL) AND (kind = 'room') = (role IS NOT NULL)
    )
);
CREATE INDEX IF NOT EXISTS invites_room ON invites (room_id) WHERE room_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS invites_creator ON invites (created_by);

-- Who let an account in, for moderation lineage; cleared when the inviter goes.
ALTER TABLE users
    ADD COLUMN invited_by UUID REFERENCES users(id) ON DELETE SET NULL;
