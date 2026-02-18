-- Role assignments (deviations from default "user" role)
-- role values: 1=member, 2=moderator, 3=admin
-- Owner is the rooms.owner_id, not stored here
CREATE TABLE IF NOT EXISTS room_roles (
    room_id VARCHAR(128) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role SMALLINT NOT NULL CHECK (role BETWEEN 1 AND 3),
    granted_by UUID REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_room_roles_room ON room_roles(room_id);
