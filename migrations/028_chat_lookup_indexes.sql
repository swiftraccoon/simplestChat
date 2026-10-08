-- no-transaction
-- One concurrent index per SQLx migration keeps this outside implicit transactions.
CREATE INDEX CONCURRENTLY IF NOT EXISTS chat_messages_incoming_unread
    ON chat_messages (recipient_account, sent_at, id)
    WHERE recipient_account IS NOT NULL;
