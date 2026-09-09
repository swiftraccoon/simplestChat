CREATE TABLE IF NOT EXISTS room_reports (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    room_id VARCHAR(128) NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    reporter_id VARCHAR(64) NOT NULL,
    reporter_name VARCHAR(64) NOT NULL,
    target_participant_id VARCHAR(64) NOT NULL,
    target_name VARCHAR(64) NOT NULL,
    reason VARCHAR(1024) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','dismissed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ,
    resolved_by VARCHAR(64)
);
CREATE INDEX IF NOT EXISTS room_reports_room_created ON room_reports(room_id,created_at DESC,id);
