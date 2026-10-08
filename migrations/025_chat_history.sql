-- Room history is an explicit owner choice. Account PM history lasts 90 days.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';
ALTER TABLE rooms ADD COLUMN history_retention_days BIGINT NOT NULL DEFAULT 0
    CHECK (history_retention_days IN (0, 1, 7, 30, 90));

CREATE TABLE chat_messages (
    id UUID PRIMARY KEY,
    conversation TEXT NOT NULL,
    room_id TEXT REFERENCES rooms(id) ON DELETE CASCADE CHECK (char_length(room_id) <= 128),
    sender_account UUID REFERENCES users(id) ON DELETE CASCADE,
    recipient_account UUID REFERENCES users(id) ON DELETE CASCADE,
    sender_session UUID NOT NULL,
    client_message_id TEXT NOT NULL CHECK (char_length(client_message_id) <= 128),
    sent_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    body JSONB NOT NULL,
    CHECK ((room_id IS NOT NULL AND recipient_account IS NULL) OR
           (room_id IS NULL AND sender_account IS NOT NULL AND recipient_account IS NOT NULL)),
    UNIQUE (conversation, sender_session, client_message_id)
);
CREATE INDEX chat_messages_conversation_page ON chat_messages (conversation, sent_at DESC, id DESC);
CREATE INDEX chat_messages_expiry ON chat_messages (expires_at);
CREATE INDEX chat_messages_inbox_sender ON chat_messages (sender_account, sent_at DESC)
    WHERE recipient_account IS NOT NULL;
CREATE INDEX chat_messages_public_reply ON chat_messages (room_id, (body->'replyTo'->>'messageId'))
    WHERE room_id IS NOT NULL;

CREATE TABLE chat_inbox (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    peer_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    conversation TEXT NOT NULL,
    last_message_id UUID NOT NULL,
    last_sent_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, peer_id),
    CHECK (user_id <> peer_id)
);
CREATE INDEX chat_inbox_page ON chat_inbox (user_id, last_sent_at DESC, last_message_id DESC);
CREATE INDEX chat_inbox_expiry ON chat_inbox (expires_at);

CREATE TABLE chat_read_cursors (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    conversation TEXT NOT NULL,
    message_id UUID NOT NULL,
    sent_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (user_id, conversation)
);
CREATE INDEX chat_read_cursors_expiry ON chat_read_cursors (expires_at);

ALTER TABLE moderation_events DROP CONSTRAINT moderation_events_action_check;
ALTER TABLE moderation_events ADD CONSTRAINT moderation_events_action_check CHECK (action IN (
    'kick', 'ban', 'unban', 'cam_ban', 'cam_unban', 'text_mute', 'text_unmute',
    'report_resolved', 'report_dismissed', 'message_removed'
)) NOT VALID;
