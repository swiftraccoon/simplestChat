-- Persistent rooms (survive server restarts)
CREATE TABLE IF NOT EXISTS rooms (
    id VARCHAR(128) PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users(id),
    display_name VARCHAR(128) NOT NULL,
    password_hash VARCHAR(255),
    require_registration BOOLEAN NOT NULL DEFAULT false,
    max_participants INTEGER,
    max_broadcasters INTEGER,
    allow_screen_sharing BOOLEAN NOT NULL DEFAULT true,
    allow_chat BOOLEAN NOT NULL DEFAULT true,
    allow_video BOOLEAN NOT NULL DEFAULT true,
    moderated BOOLEAN NOT NULL DEFAULT false,
    invite_only BOOLEAN NOT NULL DEFAULT false,
    secret BOOLEAN NOT NULL DEFAULT false,
    lobby_enabled BOOLEAN NOT NULL DEFAULT false,
    push_to_talk BOOLEAN NOT NULL DEFAULT false,
    guests_allowed BOOLEAN NOT NULL DEFAULT true,
    guests_can_broadcast BOOLEAN NOT NULL DEFAULT true,
    topic VARCHAR(512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
