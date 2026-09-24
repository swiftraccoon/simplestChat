\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout='5s';
SELECT pg_advisory_xact_lock(1935892842);
CREATE TEMP TABLE incoming_external ON COMMIT DROP AS
SELECT :'payload'::jsonb AS value;
-- This always contains one snapshot. Default temp-table cardinality estimates
-- multiply through both JSON expansions and can trigger costly LLVM JIT work
-- that consumes the statement budget before these small batches are inserted.
ANALYZE incoming_external;
INSERT INTO operations.external_runs(workflow,run_id,attempt,source_revision,
    deployed_revision_at_import,started_at,completed_at,result,conclusion,checks,imported_at)
SELECT batch->>'workflow',(run->>'runId')::bigint,(run->>'attempt')::integer,
       run->>'sourceRevision',value->>'revision',(run->>'startedAt')::timestamptz,
       (run->>'completedAt')::timestamptz,run->>'result',run->>'conclusion',run->'checks',
       (value->>'observedAt')::timestamptz
FROM incoming_external,jsonb_array_elements(value->'workflows') AS batches(batch),
     jsonb_array_elements(batch->'runs') AS runs(run)
ON CONFLICT(run_id,attempt) DO NOTHING;
INSERT INTO operations.external_status(workflow,observed_at,last_complete_at,window_start,
    api_ok,complete,pending,truncated)
SELECT batch->>'workflow',(value->>'observedAt')::timestamptz,
       CASE WHEN (batch->>'complete')::boolean THEN (value->>'observedAt')::timestamptz END,
       (value->>'windowStart')::timestamptz,(batch->>'apiOk')::boolean,
       (batch->>'complete')::boolean,(batch->>'pending')::integer,(batch->>'truncated')::boolean
FROM incoming_external,jsonb_array_elements(value->'workflows') AS batches(batch)
ON CONFLICT(workflow) DO UPDATE SET
    observed_at=EXCLUDED.observed_at,
    last_complete_at=COALESCE(EXCLUDED.last_complete_at,external_status.last_complete_at),
    window_start=EXCLUDED.window_start,api_ok=EXCLUDED.api_ok,complete=EXCLUDED.complete,
    pending=EXCLUDED.pending,truncated=EXCLUDED.truncated,
    history_gap_since=COALESCE(external_status.history_gap_since,
      CASE WHEN external_status.observed_at>'epoch' AND external_status.observed_at<EXCLUDED.window_start
           THEN external_status.observed_at END)
WHERE EXCLUDED.observed_at>external_status.observed_at;
-- Only a complete pull with a newer successful check may clear a known failure.
WITH latest AS (
    SELECT DISTINCT ON(workflow) workflow,result,started_at,completed_at
    FROM operations.external_runs WHERE result IN ('success','failure')
    ORDER BY workflow,completed_at DESC,run_id DESC,attempt DESC
)
UPDATE operations.external_status s SET failure_since=
    CASE WHEN l.result='failure' THEN COALESCE(s.failure_since,l.started_at)
         WHEN s.complete AND l.result='success'
              AND s.observed_at=(SELECT (value->>'observedAt')::timestamptz FROM incoming_external)
              AND l.completed_at>=COALESCE(s.failure_since,'epoch') THEN NULL
         ELSE s.failure_since END
FROM latest l WHERE l.workflow=s.workflow;
DELETE FROM operations.external_runs WHERE completed_at<now()-interval '30 days'
   OR (run_id,attempt) IN (SELECT run_id,attempt FROM operations.external_runs
       ORDER BY completed_at DESC,run_id DESC,attempt DESC OFFSET 10000);
COMMIT;
