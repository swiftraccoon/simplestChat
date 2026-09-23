\set ON_ERROR_STOP on
BEGIN;
SET LOCAL statement_timeout = '5s';
SELECT pg_advisory_xact_lock(1935892841);
CREATE TEMP TABLE alert_snapshot ON COMMIT DROP AS
SELECT :'payload'::jsonb AS value
WHERE NOT EXISTS (SELECT 1 FROM operations.alert_cursor WHERE observed_at >= (:'payload'::jsonb->>'observedAt')::timestamptz);
INSERT INTO operations.alerts (incident_key, rule, severity, resource, first_seen, last_seen, release_revision)
SELECT alert->>'key', alert->>'rule', alert->>'severity', alert->'resource',
       (value->>'observedAt')::timestamptz, (value->>'observedAt')::timestamptz, value->>'revision'
FROM alert_snapshot, jsonb_array_elements(value->'alerts') AS elements(alert)
ON CONFLICT (incident_key) WHERE resolved_at IS NULL
DO UPDATE SET last_seen = EXCLUDED.last_seen, observations = operations.alerts.observations + 1,
              release_revision = EXCLUDED.release_revision;
UPDATE operations.alerts SET resolved_at = (snapshot.value->>'observedAt')::timestamptz
FROM alert_snapshot AS snapshot
WHERE resolved_at IS NULL AND (snapshot.value->>'complete')::boolean
AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements(snapshot.value->'alerts') AS elements(alert)
                WHERE alert->>'key' = incident_key);
INSERT INTO operations.alert_cursor (singleton, observed_at)
SELECT TRUE, (value->>'observedAt')::timestamptz FROM alert_snapshot
ON CONFLICT (singleton) DO UPDATE SET observed_at = EXCLUDED.observed_at;
DELETE FROM operations.alerts WHERE resolved_at IS NOT NULL
AND (resolved_at < now() - interval '30 days' OR id IN
     (SELECT id FROM operations.alerts WHERE resolved_at IS NOT NULL ORDER BY last_seen DESC OFFSET 10000));
COMMIT;
