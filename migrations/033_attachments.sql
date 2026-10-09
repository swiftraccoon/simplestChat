SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';
CREATE TABLE attachments (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
    content_type TEXT NOT NULL CHECK (content_type IN ('image/png','image/jpeg','image/webp','application/octet-stream')),
    size BIGINT NOT NULL CHECK (size BETWEEN 1 AND 5242880),
    data BYTEA NOT NULL CHECK (octet_length(data) = size),
    message_id UUID REFERENCES chat_messages(id) ON DELETE CASCADE,
    ephemeral_message_id UUID,
    room_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now()+INTERVAL '1 hour',
    CHECK (message_id IS NULL OR ephemeral_message_id IS NULL),
    CHECK (ephemeral_message_id IS NULL OR room_id IS NOT NULL)
);
CREATE INDEX attachments_owner ON attachments(owner_id);
CREATE INDEX attachments_message ON attachments(message_id) WHERE message_id IS NOT NULL;
CREATE INDEX attachments_ephemeral_message ON attachments(ephemeral_message_id) WHERE ephemeral_message_id IS NOT NULL;
CREATE INDEX attachments_expiry ON attachments(expires_at);
