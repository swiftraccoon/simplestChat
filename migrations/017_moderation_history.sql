-- A room's moderation history: one append-only row per sanction, removal or
-- report decision, written in the same transaction as the change it records.
-- The target's address is kept for the owner only; listings never serialize it
-- to moderators. A report a sanction answers is linked and resolved with it.
CREATE TABLE IF NOT EXISTS moderation_events (
    id UUID PRIMARY KEY,
    room_id VARCHAR(128) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    action VARCHAR(24) NOT NULL CHECK (action IN (
        'kick', 'ban', 'unban', 'cam_ban', 'cam_unban', 'text_mute', 'text_unmute',
        'report_resolved', 'report_dismissed'
    )),
    actor_id VARCHAR(64) NOT NULL,
    actor_name VARCHAR(64) NOT NULL,
    target_id VARCHAR(64) NOT NULL,
    target_name VARCHAR(64) NOT NULL,
    target_authenticated BOOLEAN NOT NULL,
    target_ip INET,
    reason VARCHAR(256),
    expires_at TIMESTAMPTZ,
    report_id UUID REFERENCES room_reports(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS moderation_events_room_created
    ON moderation_events (room_id, created_at DESC, id);
CREATE INDEX IF NOT EXISTS moderation_events_report
    ON moderation_events (report_id) WHERE report_id IS NOT NULL;
