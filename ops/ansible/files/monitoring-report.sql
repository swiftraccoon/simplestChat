\set ON_ERROR_STOP on
BEGIN READ ONLY;
SET LOCAL statement_timeout='3s';
SELECT json_build_object(
 'generatedAt',now(),
 'scope','Retained observations; incident duration is observation span, not measured downtime.',
 'recorderObservedAt',(SELECT observed_at FROM operations.alert_cursor WHERE singleton),
 'incidents',COALESCE((SELECT json_agg(row_to_json(v)) FROM (
    SELECT rule,severity,resource,first_seen,last_seen,resolved_at,release_revision,observations,
      extract(epoch FROM COALESCE(resolved_at,now())-first_seen) AS observed_duration_seconds,
      greatest(count(*) OVER(PARTITION BY incident_key)-1,0) AS retained_recurrences
    FROM operations.alerts ORDER BY (resolved_at IS NULL) DESC,last_seen DESC LIMIT 50
 ) v),'[]'::json),
 'externalCoverage',COALESCE((SELECT json_agg(row_to_json(v)) FROM (
    SELECT s.*,extract(epoch FROM now()-s.observed_at) AS import_age_seconds,
      extract(epoch FROM now()-r.completed_at) AS probe_age_seconds,
      CASE WHEN r.completed_at IS NULL THEN 'missing'
           WHEN now()-s.observed_at>interval '30 minutes' THEN 'import_stale'
           WHEN now()-r.completed_at>CASE WHEN s.workflow='availability'
                THEN interval '30 minutes' ELSE interval '2 hours' END THEN 'stale'
           WHEN NOT s.api_ok OR NOT s.complete OR r.result='incomplete' THEN 'incomplete'
           ELSE 'current' END AS coverage,
      r.result AS latest_result,r.source_revision,r.deployed_revision_at_import,
      (SELECT count(*) FROM operations.external_runs f
         WHERE f.workflow=s.workflow AND f.result='failure') AS retained_failed_checks
    FROM operations.external_status s LEFT JOIN LATERAL (
      SELECT * FROM operations.external_runs r WHERE r.workflow=s.workflow
      ORDER BY completed_at DESC,run_id DESC,attempt DESC LIMIT 1
    ) r ON true ORDER BY s.workflow
 ) v),'[]'::json),
 'externalAttempts',COALESCE((SELECT json_agg(row_to_json(v)) FROM (
    SELECT workflow,run_id,attempt,source_revision,deployed_revision_at_import,
      started_at,completed_at,result,conclusion,checks,imported_at,
      extract(epoch FROM completed_at-started_at) AS job_duration_seconds
    FROM operations.external_runs ORDER BY completed_at DESC,run_id DESC,attempt DESC LIMIT 50
 ) v),'[]'::json)
);
COMMIT;
