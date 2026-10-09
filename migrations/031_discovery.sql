-- Account-owned room shortcuts and explicitly accepted contact relationships.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';
CREATE TABLE contacts (
    low_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    high_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    requester_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('pending', 'accepted', 'declined')),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (low_id, high_id),
    CHECK (low_id < high_id),
    CHECK (requester_id IN (low_id, high_id))
);
CREATE INDEX contacts_high ON contacts (high_id);
CREATE INDEX contacts_requester ON contacts (requester_id, requested_at);
CREATE TABLE saved_rooms (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE
        CHECK (char_length(room_id) <= 128),
    favorite BOOLEAN NOT NULL DEFAULT false,
    last_visited TIMESTAMPTZ,
    PRIMARY KEY (user_id, room_id),
    CHECK (favorite OR last_visited IS NOT NULL)
);
CREATE INDEX saved_rooms_room ON saved_rooms (room_id);
