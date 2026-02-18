-- Punitive states (camban, mute, ban) — orthogonal to roles
CREATE TABLE IF NOT EXISTS room_states (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    room_id VARCHAR(128) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    state VARCHAR(16) NOT NULL CHECK (state IN ('cambanned', 'muted', 'banned')),
    ip_address INET,
    reason VARCHAR(256),
    expires_at TIMESTAMPTZ,
    applied_by UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (room_id, user_id, state)
);

CREATE INDEX IF NOT EXISTS idx_room_states_room ON room_states(room_id);
CREATE INDEX IF NOT EXISTS idx_room_states_ip ON room_states(ip_address) WHERE ip_address IS NOT NULL;
