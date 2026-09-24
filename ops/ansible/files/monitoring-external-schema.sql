\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout='5s';
SELECT pg_advisory_xact_lock(1935892841);
DO $$ BEGIN
    IF (SELECT count(*) FROM operations.schema_version) != 1 OR
       (SELECT version FROM operations.schema_version) != 1 THEN
        RAISE EXCEPTION 'Unsupported base operations schema';
    END IF;
END $$;
CREATE TABLE operations.external_schema_version(version INTEGER PRIMARY KEY CHECK(version=1));
INSERT INTO operations.external_schema_version VALUES(1);
CREATE TABLE operations.external_runs (
    workflow TEXT NOT NULL CHECK(workflow IN ('availability','canary')),
    run_id BIGINT NOT NULL CHECK(run_id>0),
    attempt INTEGER NOT NULL CHECK(attempt BETWEEN 1 AND 1000000),
    source_revision CHAR(40) NOT NULL CHECK(source_revision ~ '^[a-f0-9]{40}$'),
    deployed_revision_at_import CHAR(40) NOT NULL CHECK(deployed_revision_at_import ~ '^[a-f0-9]{40}$'),
    started_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ NOT NULL CHECK(completed_at>=started_at),
    result TEXT NOT NULL CHECK(result IN ('success','failure','incomplete')),
    conclusion TEXT NOT NULL CHECK(conclusion IN
        ('success','failure','cancelled','timed_out','skipped','neutral','action_required','stale','unknown')),
    checks JSONB NOT NULL CHECK(jsonb_typeof(checks)='object' AND octet_length(checks::text)<=512),
    imported_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(run_id,attempt)
);
CREATE INDEX external_runs_workflow_completed ON operations.external_runs(workflow,completed_at DESC);
CREATE TABLE operations.external_status (
    workflow TEXT PRIMARY KEY CHECK(workflow IN ('availability','canary')),
    observed_at TIMESTAMPTZ NOT NULL,
    last_complete_at TIMESTAMPTZ,
    window_start TIMESTAMPTZ NOT NULL,
    api_ok BOOLEAN NOT NULL,
    complete BOOLEAN NOT NULL,
    pending INTEGER NOT NULL CHECK(pending BETWEEN 0 AND 4000),
    truncated BOOLEAN NOT NULL,
    failure_since TIMESTAMPTZ,
    history_gap_since TIMESTAMPTZ
);
INSERT INTO operations.external_status(workflow,observed_at,window_start,api_ok,complete,pending,truncated)
VALUES ('availability','epoch','epoch',false,false,0,false),('canary','epoch','epoch',false,false,0,false);
REVOKE ALL ON TABLE operations.external_schema_version,operations.external_runs,
    operations.external_status FROM PUBLIC,simplestchat_app,simplestchat_migrate;
COMMIT;
