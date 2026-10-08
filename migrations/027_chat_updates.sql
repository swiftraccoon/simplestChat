-- Pins share the message's retention; deleting a message always removes its pin.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';
CREATE TABLE chat_pins (
    room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    message_id UUID NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    slot BIGINT NOT NULL CHECK (slot BETWEEN 1 AND 3),
    pinned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, message_id),
    UNIQUE (room_id, slot)
);
