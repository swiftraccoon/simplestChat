\set ON_ERROR_STOP on
BEGIN READ ONLY;
SET LOCAL statement_timeout = '30s';
-- These reads fail if a supposedly complete archive omitted required objects/columns.
SELECT id,email,auth_version,recovery_key_hash FROM public.users LIMIT 0;
SELECT id,user_id,credential_json,credential_id FROM public.webauthn_credentials LIMIT 0;
SELECT id,user_id FROM public.sessions LIMIT 0;
SELECT id,owner_id,description FROM public.rooms LIMIT 0;
SELECT room_id,user_id FROM public.room_roles LIMIT 0;
SELECT room_id,user_id FROM public.room_states LIMIT 0;
SELECT id,room_id,status FROM public.room_reports LIMIT 0;
SELECT version,success,checksum FROM public._sqlx_migrations LIMIT 0;
SELECT incident_key,rule,severity,resource,first_seen,last_seen,resolved_at,
       release_revision,observations FROM operations.alerts LIMIT 0;
SELECT singleton,observed_at FROM operations.alert_cursor LIMIT 0;
DO $$ BEGIN
    IF (SELECT count(*) FROM operations.schema_version) != 1 OR
       (SELECT version FROM operations.schema_version) != 1 THEN
        RAISE EXCEPTION 'Unsupported operational schema';
    END IF;
    IF EXISTS (SELECT FROM pg_constraint c JOIN pg_namespace n ON n.oid=c.connamespace
               WHERE n.nspname IN ('public','operations') AND NOT c.convalidated) OR
       EXISTS (SELECT FROM pg_index i JOIN pg_class c ON c.oid=i.indrelid
               JOIN pg_namespace n ON n.oid=c.relnamespace
               WHERE n.nspname IN ('public','operations')
                 AND NOT (i.indisvalid AND i.indisready AND i.indislive)) THEN
        RAISE EXCEPTION 'Unvalidated constraints or invalid indexes';
    END IF;
    IF (SELECT nspowner FROM pg_namespace WHERE nspname='operations') !=
       (SELECT oid FROM pg_roles WHERE rolname='postgres') OR
       EXISTS (SELECT FROM pg_roles r WHERE r.rolname IN
                  ('simplestchat_app','simplestchat_migrate')
               AND has_schema_privilege(r.oid,'operations','USAGE,CREATE')) OR
       EXISTS (SELECT FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
               CROSS JOIN pg_roles r WHERE n.nspname='operations' AND c.relkind='r'
               AND r.rolname IN ('simplestchat_app','simplestchat_migrate')
               AND has_table_privilege(r.oid,c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE')) OR
       has_table_privilege('simplestchat_app','public._sqlx_migrations',
                           'INSERT,UPDATE,DELETE,TRUNCATE') THEN
        RAISE EXCEPTION 'Restored role privacy differs';
    END IF;
    IF NOT EXISTS (SELECT FROM pg_index WHERE indexrelid=
                    to_regclass('operations.alerts_active_key') AND indisunique)
       OR (SELECT last_value FROM operations.alerts_id_seq) <
          COALESCE((SELECT max(id) FROM operations.alerts),0) THEN
        RAISE EXCEPTION 'Incident uniqueness or sequence differs';
    END IF;
    -- Older version-1 archives legitimately predate external evidence. A partial
    -- newer schema is never treated as an old archive and silently ignored.
    IF (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname='operations' AND c.relkind='r' AND c.relname IN
        ('external_schema_version','external_runs','external_status')) NOT IN (0,3) THEN
        RAISE EXCEPTION 'Incomplete external operations schema';
    END IF;
    IF to_regclass('operations.external_schema_version') IS NOT NULL THEN
        IF (SELECT count(*) FROM operations.external_schema_version) != 1 OR
           (SELECT version FROM operations.external_schema_version) != 1 THEN
            RAISE EXCEPTION 'Unsupported external operations schema';
        END IF;
        PERFORM workflow,run_id,attempt,source_revision,deployed_revision_at_import,
            started_at,completed_at,result,conclusion,checks,imported_at
            FROM operations.external_runs LIMIT 0;
        PERFORM workflow,observed_at,last_complete_at,window_start,api_ok,complete,pending,
            truncated,failure_since,history_gap_since FROM operations.external_status LIMIT 0;
    END IF;
END $$;
-- Aggregate-only private evidence: no emails, tokens, credential material or incident rows.
SELECT json_build_object(
    'users',(SELECT count(*) FROM public.users),
    'rooms',(SELECT count(*) FROM public.rooms),
    'sessions',(SELECT count(*) FROM public.sessions),
    'credentials',(SELECT count(*) FROM public.webauthn_credentials),
    'activeIncidents',(SELECT count(*) FROM operations.alerts WHERE resolved_at IS NULL),
    'resolvedIncidents',(SELECT count(*) FROM operations.alerts WHERE resolved_at IS NOT NULL));
COMMIT;
