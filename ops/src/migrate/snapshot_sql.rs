//! The statements of a database snapshot, carried over byte for byte from the
//! helper this replaces: both ends of a move must describe a database the same
//! way, and the replayed recordings pin every one of them.

/// Relations outside PostgreSQL's own schemas.
macro_rules! namespaces {
    () => {
        "n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'"
    };
}

/// A constraint's definition. A logical restore can move a varchar enum's
/// array-to-text cast onto its literals: only that complete grammar is
/// normalized, and every literal and every other cast is kept.
macro_rules! constraint_definition {
    () => {
        concat!(
            r#"CASE WHEN c.contype='c' AND d.definition ~
 '^CHECK \([a-z_][a-z0-9_]*::text = ANY \(ARRAY\[''[a-z_][a-z0-9_]*"#,
            r#"''::character varying(, ''[a-z_][a-z0-9_]*''::character varying)*\]::text\[\]\)\)$'
 THEN replace(replace(d.definition,'::character varying','::character varying::text'),
              ']::text[]))',']))')
 ELSE d.definition END"#,
        )
    };
}

/// One read-only transaction with fixed session settings.
pub(super) const SETUP: &str = r#"
BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL statement_timeout='30s';
SET LOCAL lock_timeout='3s';
SET LOCAL idle_in_transaction_session_timeout='45s';
SET LOCAL application_name='simplestchat-migration-readonly-snapshot';
SET LOCAL timezone='UTC';
SET LOCAL datestyle='ISO, YMD';
SET LOCAL intervalstyle='postgres';
SET LOCAL extra_float_digits=3;
SET LOCAL bytea_output='hex';
SET LOCAL search_path=pg_catalog;
SET LOCAL client_encoding='UTF8';
SET LOCAL work_mem='8MB';
SET LOCAL temp_file_limit='256MB';
"#;

/// The database's identity and its relations, as one JSON value.
pub(super) const CATALOG: &str = concat!(
    r#"
SELECT jsonb_build_object(
 'database',current_database(),
 'serverVersion',current_setting('server_version_num'),
 'databaseEncoding',current_setting('server_encoding'),
 'readOnly',current_setting('transaction_read_only'),
 'isolation',current_setting('transaction_isolation'),
 'systemIdentifier', (pg_control_system()).system_identifier::text,
 'tables',COALESCE((
   SELECT jsonb_agg(jsonb_build_object('schema',n.nspname,'name',c.relname,
     'kind',c.relkind,'isPartition',c.relispartition)
     ORDER BY n.nspname COLLATE "C", c.relname COLLATE "C")
   FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE "#,
    namespaces!(),
    r#" AND c.relkind IN ('r','p')
 ),'[]'::jsonb),
 'sequences',COALESCE((
   SELECT jsonb_agg(jsonb_build_object('schema',n.nspname,'name',c.relname)
     ORDER BY n.nspname COLLATE "C",c.relname COLLATE "C")
   FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE "#,
    namespaces!(),
    r#" AND c.relkind='S'
 ),'[]'::jsonb)
);
"#,
);

/// The constraint normalizer on its own.
#[cfg(test)]
pub(super) const CONSTRAINT_DEFINITION: &str = constraint_definition!();

/// Every object definition as sorted records. No catalog identifiers, heap
/// statistics, dropped-column holes or cluster-local values: those differ
/// after a logical restore.
pub(super) const METADATA: &str = concat!(
    r#"
COPY (
WITH relations AS (
 SELECT c.*, n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
 WHERE "#,
    namespaces!(),
    r#" AND c.relkind IN ('r','p','S')
), records AS (
 SELECT jsonb_build_object('type','schema','name',n.nspname,
   'owner',pg_get_userbyid(n.nspowner),'acl',
   (SELECT jsonb_agg(a::text ORDER BY a::text COLLATE "C")
     FROM unnest(COALESCE(n.nspacl,acldefault('n',n.nspowner))) a)) AS value
 FROM pg_namespace n WHERE "#,
    namespaces!(),
    r#"
 UNION ALL
 SELECT jsonb_build_object('type','relation','schema',r.nspname,'name',r.relname,
   'kind',r.relkind,'persistence',r.relpersistence,'owner',pg_get_userbyid(r.relowner),
   'rowSecurity',r.relrowsecurity,'forceRowSecurity',r.relforcerowsecurity,
   'replicaIdentity',r.relreplident,'partitioned',r.relispartition,
   'partitionBound',pg_get_expr(r.relpartbound,r.oid,true),
   'partitionKey',CASE WHEN r.relkind='p' THEN pg_get_partkeydef(r.oid) END,
   'parents',(SELECT jsonb_agg(jsonb_build_array(pn.nspname,p.relname)
       ORDER BY i.inhseqno) FROM pg_inherits i JOIN pg_class p ON p.oid=i.inhparent
       JOIN pg_namespace pn ON pn.oid=p.relnamespace WHERE i.inhrelid=r.oid),
   'acl',(SELECT jsonb_agg(a::text ORDER BY a::text COLLATE "C")
     FROM unnest(COALESCE(r.relacl,acldefault(
       (CASE WHEN r.relkind='S' THEN 's' ELSE 'r' END)::"char",r.relowner))) a),
   'options',(SELECT jsonb_agg(o ORDER BY o COLLATE "C") FROM unnest(r.reloptions) o))
 FROM relations r
 UNION ALL
 SELECT jsonb_build_object('type','column','schema',r.nspname,'relation',r.relname,
   'name',a.attname,'position',row_number() OVER (PARTITION BY r.oid ORDER BY a.attnum),
   'dataType',format_type(a.atttypid,a.atttypmod),'notNull',a.attnotnull,
   'identity',a.attidentity,'generated',a.attgenerated,
   'storage',a.attstorage,'compression',a.attcompression,
   'collation',CASE WHEN a.attcollation<>0 THEN
       jsonb_build_array(cn.nspname,co.collname) END,
   'default',pg_get_expr(d.adbin,d.adrelid,true),
   'acl',(SELECT jsonb_agg(x::text ORDER BY x::text COLLATE "C")
     FROM unnest(COALESCE(a.attacl,acldefault('c',r.relowner))) x))
 FROM relations r JOIN pg_attribute a ON a.attrelid=r.oid
 LEFT JOIN pg_attrdef d ON d.adrelid=r.oid AND d.adnum=a.attnum
 LEFT JOIN pg_collation co ON co.oid=a.attcollation
 LEFT JOIN pg_namespace cn ON cn.oid=co.collnamespace
 WHERE a.attnum>0 AND NOT a.attisdropped
 UNION ALL
 SELECT jsonb_build_object('type','constraint','schema',r.nspname,'relation',r.relname,
   'name',c.conname,'definition',"#,
    constraint_definition!(),
    r#",
   'validated',c.convalidated,'deferrable',c.condeferrable,'deferred',c.condeferred)
 FROM relations r JOIN pg_constraint c ON c.conrelid=r.oid
 CROSS JOIN LATERAL (SELECT pg_get_constraintdef(c.oid,true) AS definition) d
 UNION ALL
 SELECT jsonb_build_object('type','index','schema',r.nspname,'relation',r.relname,
   'name',ci.relname,'definition',pg_get_indexdef(i.indexrelid),
   'valid',i.indisvalid,'ready',i.indisready,'live',i.indislive,
   'unique',i.indisunique,'primary',i.indisprimary)
 FROM relations r JOIN pg_index i ON i.indrelid=r.oid
 JOIN pg_class ci ON ci.oid=i.indexrelid
 UNION ALL
 SELECT jsonb_build_object('type','trigger','schema',r.nspname,'relation',r.relname,
   'name',t.tgname,'definition',pg_get_triggerdef(t.oid,true),'enabled',t.tgenabled)
 FROM relations r JOIN pg_trigger t ON t.tgrelid=r.oid WHERE NOT t.tgisinternal
 UNION ALL
 SELECT jsonb_build_object('type','policy','schema',r.nspname,'relation',r.relname,
   'name',p.polname,'command',p.polcmd,'permissive',p.polpermissive,
   'roles',(SELECT jsonb_agg(CASE WHEN role=0 THEN 'PUBLIC' ELSE pg_get_userbyid(role) END
       ORDER BY CASE WHEN role=0 THEN 'PUBLIC' ELSE pg_get_userbyid(role) END)
       FROM unnest(p.polroles) role),
   'using',pg_get_expr(p.polqual,p.polrelid,true),
   'check',pg_get_expr(p.polwithcheck,p.polrelid,true))
 FROM relations r JOIN pg_policy p ON p.polrelid=r.oid
 UNION ALL
 SELECT jsonb_build_object('type','sequenceDefinition','schema',r.nspname,'name',r.relname,
   'dataType',format_type(s.seqtypid,NULL),'start',s.seqstart,'increment',s.seqincrement,
   'maximum',s.seqmax,'minimum',s.seqmin,'cache',s.seqcache,'cycle',s.seqcycle,
   'ownedBy',(SELECT jsonb_agg(jsonb_build_array(pn.nspname,p.relname,a.attname,d.deptype)
       ORDER BY pn.nspname COLLATE "C",p.relname COLLATE "C",a.attname COLLATE "C")
     FROM pg_depend d JOIN pg_class p ON p.oid=d.refobjid
     JOIN pg_namespace pn ON pn.oid=p.relnamespace
     JOIN pg_attribute a ON a.attrelid=p.oid AND a.attnum=d.refobjsubid
     WHERE d.classid='pg_class'::regclass AND d.objid=r.oid
       AND d.refclassid='pg_class'::regclass AND d.deptype IN ('a','i')))
 FROM relations r JOIN pg_sequence s ON s.seqrelid=r.oid
 UNION ALL
 SELECT jsonb_build_object('type','extension','name',e.extname,'version',e.extversion,
   'schema',n.nspname,'owner',pg_get_userbyid(e.extowner))
 FROM pg_extension e JOIN pg_namespace n ON n.oid=e.extnamespace
 UNION ALL
 SELECT jsonb_build_object('type','databasePermissions','owner',pg_get_userbyid(d.datdba),
   'acl',(SELECT jsonb_agg(a::text ORDER BY a::text COLLATE "C")
     FROM unnest(COALESCE(d.datacl,acldefault('d',d.datdba))) a))
 FROM pg_database d WHERE d.datname=current_database()
 UNION ALL
 SELECT jsonb_build_object('type','defaultPrivileges','owner',pg_get_userbyid(d.defaclrole),
   'schema',n.nspname,'kind',d.defaclobjtype,
   'acl',(SELECT jsonb_agg(a::text ORDER BY a::text COLLATE "C") FROM unnest(d.defaclacl) a))
 FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid=d.defaclnamespace
 UNION ALL
 SELECT jsonb_build_object('type','routine','schema',n.nspname,'name',p.proname,
   'arguments',pg_get_function_identity_arguments(p.oid),'definition',pg_get_functiondef(p.oid),
   'owner',pg_get_userbyid(p.proowner),
   'acl',(SELECT jsonb_agg(a::text ORDER BY a::text COLLATE "C")
     FROM unnest(COALESCE(p.proacl,acldefault('f',p.proowner))) a))
 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
 WHERE "#,
    namespaces!(),
    r#" AND p.prokind IN ('f','p')
   AND NOT EXISTS(SELECT FROM pg_depend d WHERE d.classid='pg_proc'::regclass
                  AND d.objid=p.oid AND d.deptype='e')
 UNION ALL
 SELECT jsonb_build_object('type','runtimeRole','name',r.rolname,'superuser',r.rolsuper,
   'inherit',r.rolinherit,'createRole',r.rolcreaterole,'createDatabase',r.rolcreatedb,
   'login',r.rolcanlogin,'replication',r.rolreplication,'bypassRls',r.rolbypassrls,
   'connectionLimit',r.rolconnlimit,'validUntil',r.rolvaliduntil,
   'configuration',(SELECT jsonb_agg(x ORDER BY x COLLATE "C") FROM unnest(r.rolconfig) x))
 FROM pg_roles r WHERE r.rolname IN ('simplestchat_app','simplestchat_migrate')
)
SELECT value::text FROM records ORDER BY value::text COLLATE "C"
) TO STDOUT WITH (FORMAT text, ENCODING 'UTF8');
"#,
);
