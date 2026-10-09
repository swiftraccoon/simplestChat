-- Account policy gates alerts only; message delivery and unread cursors are unchanged.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';
CREATE TABLE account_notification_preferences (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    private_messages BOOLEAN NOT NULL DEFAULT TRUE,
    mentions BOOLEAN NOT NULL DEFAULT TRUE,
    quiet_start SMALLINT,
    quiet_end SMALLINT,
    quiet_timezone TEXT,
    CHECK ((quiet_start IS NULL AND quiet_end IS NULL AND quiet_timezone IS NULL)
        OR (quiet_start IS NOT NULL AND quiet_end IS NOT NULL AND quiet_timezone IS NOT NULL
            AND quiet_start BETWEEN 0 AND 1439 AND quiet_end BETWEEN 0 AND 1439
            AND quiet_start <> quiet_end AND char_length(quiet_timezone) BETWEEN 1 AND 64))
);
CREATE TABLE conversation_notification_preferences (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    peer_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    muted BOOLEAN NOT NULL DEFAULT FALSE,
    snoozed_until TIMESTAMPTZ,
    PRIMARY KEY (user_id, peer_id),
    CHECK (user_id <> peer_id),
    CHECK (muted OR snoozed_until IS NOT NULL)
);
CREATE INDEX conversation_notification_preferences_peer ON conversation_notification_preferences(peer_id);
