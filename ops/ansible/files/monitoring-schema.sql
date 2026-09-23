\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '5s';
SELECT pg_advisory_xact_lock(1935892841);
CREATE SCHEMA IF NOT EXISTS operations;
REVOKE ALL ON SCHEMA operations FROM PUBLIC;
CREATE TABLE IF NOT EXISTS operations.schema_version (version INTEGER PRIMARY KEY CHECK (version = 1));
INSERT INTO operations.schema_version VALUES (1) ON CONFLICT DO NOTHING;
DO $$ BEGIN
    IF (SELECT count(*) FROM operations.schema_version) != 1 OR
       (SELECT version FROM operations.schema_version) != 1 THEN
        RAISE EXCEPTION 'Unsupported operational schema version';
    END IF;
END $$;
-- Private operational incident history. Runtime application grants exclude these tables.
CREATE TABLE IF NOT EXISTS operations.alerts (
    id BIGSERIAL PRIMARY KEY,
    incident_key CHAR(64) NOT NULL CHECK (incident_key ~ '^[a-f0-9]{64}$'),
    rule VARCHAR(64) NOT NULL CHECK (rule ~ '^[A-Za-z][A-Za-z0-9]{0,63}$'),
    severity TEXT NOT NULL CHECK (severity IN ('warning', 'critical')),
    resource JSONB NOT NULL CHECK (jsonb_typeof(resource) = 'object' AND octet_length(resource::text) <= 1024),
    first_seen TIMESTAMPTZ NOT NULL,
    last_seen TIMESTAMPTZ NOT NULL,
    resolved_at TIMESTAMPTZ,
    release_revision CHAR(40) CHECK (release_revision ~ '^[a-f0-9]{40}$'),
    observations BIGINT NOT NULL DEFAULT 1 CHECK (observations > 0),
    CHECK (last_seen >= first_seen),
    CHECK (resolved_at IS NULL OR resolved_at >= last_seen)
);
CREATE UNIQUE INDEX IF NOT EXISTS alerts_active_key ON operations.alerts(incident_key)
    WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS alerts_history ON operations.alerts(last_seen DESC);
CREATE TABLE IF NOT EXISTS operations.alert_cursor (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    observed_at TIMESTAMPTZ NOT NULL
);
REVOKE ALL ON ALL TABLES IN SCHEMA operations FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA operations FROM PUBLIC;
COMMIT;
