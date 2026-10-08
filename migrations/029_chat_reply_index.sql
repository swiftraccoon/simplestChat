-- no-transaction
-- Keep quote repair bounded without blocking chat-message writes.
CREATE INDEX CONCURRENTLY IF NOT EXISTS chat_messages_conversation_reply
    ON chat_messages (conversation, (body->'replyTo'->>'messageId'));
