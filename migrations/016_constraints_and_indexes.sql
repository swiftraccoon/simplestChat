-- Guarantees the application already enforces, so scripts, later services and
-- hand edits cannot create states it assumes impossible, and indexes for the
-- paths the server takes. Every rule below held for existing rows when written.
ALTER TABLE rooms
    ADD CONSTRAINT rooms_max_participants_range
        CHECK (max_participants IS NULL OR max_participants BETWEEN 1 AND 10000),
    ADD CONSTRAINT rooms_max_broadcasters_range
        CHECK (max_broadcasters IS NULL OR max_broadcasters BETWEEN 1 AND 1000),
    ADD CONSTRAINT rooms_broadcasters_within_participants
        CHECK (max_participants IS NULL OR max_broadcasters IS NULL OR max_broadcasters <= max_participants);

-- A sanction restricts an account, an address, or both; never nothing.
ALTER TABLE room_states
    ADD CONSTRAINT room_states_identity CHECK (user_id IS NOT NULL OR ip_address IS NOT NULL);

-- An open report has no resolution; a closed one has both its time and its resolver.
ALTER TABLE room_reports
    ADD CONSTRAINT room_reports_resolution
        CHECK ((status = 'open') = (resolved_at IS NULL) AND (status = 'open') = (resolved_by IS NULL));

-- Directory pages order by created_at then id, so the index does too.
CREATE INDEX IF NOT EXISTS idx_rooms_public_created_id
    ON rooms (created_at DESC, id DESC)
    WHERE secret = false;
DROP INDEX IF EXISTS idx_rooms_public_created_at;

-- These duplicated the leading column of a primary or unique key.
DROP INDEX IF EXISTS idx_room_roles_room;
DROP INDEX IF EXISTS idx_room_states_room;

-- Referencing sides of foreign keys: account-oriented lookups and cascading deletes.
CREATE INDEX IF NOT EXISTS idx_room_roles_user ON room_roles (user_id);
CREATE INDEX IF NOT EXISTS idx_room_roles_granted_by ON room_roles (granted_by) WHERE granted_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_room_states_user ON room_states (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_room_states_applied_by ON room_states (applied_by);
