-- A successful room redemption is replayable by its account without spending
-- another use or restoring a subsequently removed role. Revocation deletes the
-- invitation and its receipts. Expiry cleanup retains both for seven extra days.
CREATE TABLE invite_redemptions (
    code VARCHAR(32) NOT NULL REFERENCES invites(code) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    granted_role SMALLINT NOT NULL CHECK (granted_role BETWEEN 0 AND 5),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (code, user_id)
);
CREATE INDEX invite_redemptions_user ON invite_redemptions (user_id);
CREATE INDEX invites_expiration ON invites (expires_at, code);

-- Global retention batches must not scan room-leading indexes for old rows.
CREATE INDEX moderation_events_retention ON moderation_events (created_at, id);
CREATE INDEX moderation_events_address_retention ON moderation_events (created_at, id)
    WHERE target_ip IS NOT NULL;
CREATE INDEX room_states_expiration ON room_states (expires_at, id)
    WHERE expires_at IS NOT NULL;
CREATE INDEX room_reports_resolution_retention ON room_reports (resolved_at, id)
    WHERE status <> 'open';
