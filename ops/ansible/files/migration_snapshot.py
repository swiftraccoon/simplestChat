"""Stream private, deterministic migration evidence from one read-only snapshot.

Every physical table is read with ONLY, including partition leaves. Values never
reach logs or retained files. Sequence state requires quiesced writers because
PostgreSQL sequences are not MVCC-protected.
"""

from __future__ import annotations

import hashlib
import os
import re
import selectors
import subprocess
import time
from contextlib import suppress
from typing import TYPE_CHECKING, cast

import release_public as public
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import BinaryIO

SOCKET = "/run/simplestchat-postgres"
MAX_RELATIONS = 512
MAX_IDENTIFIER_BYTES = 63
MAX_LINE_BYTES = 4 * 1024 * 1024
MAX_SECTION_BYTES = 256 * 1024 * 1024
MAX_TOTAL_BYTES = 512 * 1024 * 1024
MAX_METADATA_BYTES = 16 * 1024 * 1024
QUERY_SECONDS = 40
TOTAL_SECONDS = 300
NAMESPACE_FILTER = "n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'"
SETUP = """
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
"""


def identifier(value: str) -> str:
    """Quote catalog identifiers; they never become executable SQL fragments."""
    public.require(
        0 < len(value.encode()) <= MAX_IDENTIFIER_BYTES and "\x00" not in value,
        "invalid_identifier",
    )
    return '"' + value.replace('"', '""') + '"'


def no_output(_line: bytes) -> None:
    """Reject unexpected setup or transaction output."""
    message = "unexpected_transaction_output"
    raise public.ReleaseError(message)


class Session:
    """Keep one bounded psql transaction while hashing COPY lines in memory."""

    def __init__(self, container: str) -> None:
        """Start only the already-verified PostgreSQL container's private client."""
        public.require(re.fullmatch("[a-f0-9]{64}", container), "invalid_database_container")
        self.deadline: float = time.monotonic() + TOTAL_SECONDS
        self.total: int = 0
        self.pending: bytearray = bytearray()
        self.marker: bytes = ("SNAPSHOT_END_" + os.urandom(24).hex()).encode()
        self.process: subprocess.Popen[bytes] = subprocess.Popen(  # noqa: S603 -- fixed local Docker client, exact validated container, no shell.
            [
                *public.DOCKER,
                "exec",
                "--interactive",
                "--user",
                "999:999",
                container,
                "psql",
                "--no-psqlrc",
                "--quiet",
                "--tuples-only",
                "--no-align",
                "--set",
                "ON_ERROR_STOP=on",
                "--pset",
                "pager=off",
                "--host",
                SOCKET,
                "--username",
                "postgres",
                "--dbname",
                "simplestchat",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=public.ENV,
            bufsize=0,
            start_new_session=True,
        )
        if self.process.stdin is None or self.process.stdout is None:
            public.Runner.stop(self.process)
            message = "postgres_pipe_missing"
            raise public.ReleaseError(message)
        self.input: BinaryIO = cast("BinaryIO", self.process.stdin)
        self.output: BinaryIO = cast("BinaryIO", self.process.stdout)
        os.set_blocking(self.input.fileno(), False)
        self.selector: selectors.BaseSelector = selectors.DefaultSelector()
        _ = self.selector.register(self.output, selectors.EVENT_READ)

    def send(self, payload: bytes, deadline: float) -> None:
        """Handle partial pipe writes without an unbounded blocking stdin write."""
        remaining = memoryview(payload)
        with selectors.DefaultSelector() as writer:
            _ = writer.register(self.input, selectors.EVENT_WRITE)
            while remaining:
                budget = deadline - time.monotonic()
                public.require(budget > 0, "postgres_input_deadline")
                public.require(self.process.poll() is None, "postgres_session_exited")
                if not writer.select(min(budget, 1)):
                    continue
                try:
                    count = os.write(self.input.fileno(), remaining)
                except BlockingIOError:
                    continue
                public.require(count > 0, "postgres_input_closed")
                remaining = remaining[count:]

    def query(
        self, sql: str, consume: Callable[[bytes], None], *, limit: int = MAX_SECTION_BYTES
    ) -> None:
        """Bound one statement group, stream each complete line, and require its marker."""
        deadline = min(self.deadline, time.monotonic() + QUERY_SECONDS)
        payload = sql.encode() + b"\n\\echo " + self.marker + b"\n"
        public.require(len(payload) <= 128 * 1024, "sql_limit")
        self.send(payload, deadline)
        section_bytes = 0
        while True:
            while b"\n" in self.pending:
                end = self.pending.index(b"\n") + 1
                line = bytes(self.pending[:end])
                del self.pending[:end]
                public.require(len(line) <= MAX_LINE_BYTES, "row_limit")
                if line[:-1] == self.marker:
                    public.require(not self.pending, "unexpected_output_after_marker")
                    return
                section_bytes += len(line)
                self.total += len(line)
                public.require(section_bytes <= limit, "section_limit")
                public.require(self.total <= MAX_TOTAL_BYTES, "snapshot_byte_limit")
                consume(line)
            budget = deadline - time.monotonic()
            public.require(budget > 0, "postgres_query_deadline")
            if not self.selector.select(min(budget, 1)):
                public.require(self.process.poll() is None, "postgres_session_exited")
                continue
            chunk = os.read(self.output.fileno(), 65536)
            public.require(bool(chunk), "postgres_output_closed")
            self.pending.extend(chunk)
            public.require(len(self.pending) <= MAX_LINE_BYTES + 65536, "row_limit")

    def value(self, sql: str) -> JsonValue:
        """Decode only explicitly redacted catalog queries, never application rows."""
        lines: list[bytes] = []
        self.query(sql, lines.append, limit=MAX_LINE_BYTES)
        public.require(len(lines) == 1, "json_result_shape")
        return decode_json(lines[0])

    def digest(self, sql: str, *, limit: int = MAX_SECTION_BYTES) -> JsonObject:
        """Discard every row after updating a SHA-256 and a physical row count."""
        digest = hashlib.sha256()
        records = 0
        size = 0

        def consume(line: bytes) -> None:
            nonlocal records, size
            public.require(line.endswith(b"\n"), "copy_record_unterminated")
            digest.update(line)
            records += 1
            size += len(line)

        self.query(sql, consume, limit=limit)
        return {"rows": records, "copyBytes": size, "sha256": digest.hexdigest()}

    def close(self) -> None:
        """Roll back and reap only this client; server query/idle bounds remain active."""
        try:
            if self.process.poll() is None:
                with suppress(OSError, public.ReleaseError):
                    self.send(b"ROLLBACK;\n\\quit\n", time.monotonic() + 1)
            self.input.close()
            try:
                _ = self.process.wait(timeout=4)
            except subprocess.TimeoutExpired:
                public.Runner.stop(self.process)
        finally:
            self.selector.close()
            self.output.close()


CATALOG_SQL = f"""
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
   WHERE {NAMESPACE_FILTER} AND c.relkind IN ('r','p')
 ),'[]'::jsonb),
 'sequences',COALESCE((
   SELECT jsonb_agg(jsonb_build_object('schema',n.nspname,'name',c.relname)
     ORDER BY n.nspname COLLATE "C",c.relname COLLATE "C")
   FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE {NAMESPACE_FILTER} AND c.relkind='S'
 ),'[]'::jsonb)
);
"""  # noqa: S608 -- only a fixed module-owned namespace predicate is interpolated.

# No catalog OIDs, heap statistics, dropped-column holes or cluster-local IDs:
# they necessarily differ after a logical restore. All object definitions remain
# inside the process and are reduced directly to a digest.
METADATA_SQL = f"""
COPY (
WITH relations AS (
 SELECT c.*, n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
 WHERE {NAMESPACE_FILTER} AND c.relkind IN ('r','p','S')
), records AS (
 SELECT jsonb_build_object('type','schema','name',n.nspname,
   'owner',pg_get_userbyid(n.nspowner),'acl',
   (SELECT jsonb_agg(a::text ORDER BY a::text COLLATE "C")
     FROM unnest(COALESCE(n.nspacl,acldefault('n',n.nspowner))) a)) AS value
 FROM pg_namespace n WHERE {NAMESPACE_FILTER}
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
   'name',c.conname,'definition',pg_get_constraintdef(c.oid,true),
   'validated',c.convalidated,'deferrable',c.condeferrable,'deferred',c.condeferred)
 FROM relations r JOIN pg_constraint c ON c.conrelid=r.oid
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
 WHERE {NAMESPACE_FILTER} AND p.prokind IN ('f','p')
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
"""  # noqa: S608 -- only a fixed module-owned namespace predicate is interpolated.


def tables(session: Session, catalog: JsonObject) -> list[JsonValue]:
    """Fingerprint every relation's physical rows exactly once."""
    selected = array_value(catalog["tables"])
    public.require(0 < len(selected) <= MAX_RELATIONS, "table_count")
    evidence: list[JsonValue] = []
    for item in selected:
        table = object_value(item)
        qualified = (
            identifier(string_value(table["schema"]))
            + "."
            + identifier(string_value(table["name"]))
        )
        result = session.digest(
            f"COPY (SELECT to_jsonb(t)::text FROM ONLY {qualified} AS t "  # noqa: S608 -- individually quoted catalog identifiers.
            + 'ORDER BY to_jsonb(t)::text COLLATE "C") '
            + "TO STDOUT WITH (FORMAT text, ENCODING 'UTF8');"
        )
        evidence.append({**table, **result})
    return evidence


def sequences(session: Session, catalog: JsonObject) -> list[JsonValue]:
    """Retain exact sequence state, relying on the enforced no-writer window."""
    selected = array_value(catalog["sequences"])
    public.require(len(selected) <= MAX_RELATIONS, "sequence_count")
    evidence: list[JsonValue] = []
    for item in selected:
        sequence = object_value(item)
        qualified = (
            identifier(string_value(sequence["schema"]))
            + "."
            + identifier(string_value(sequence["name"]))
        )
        value = object_value(
            session.value(
                "SELECT jsonb_build_object('lastValue',last_value::text,'isCalled',is_called) "
                + f"FROM {qualified};"
            )
        )
        public.require(
            re.fullmatch(r"-?[0-9]+", string_value(value["lastValue"])), "sequence_value"
        )
        public.require(type(value["isCalled"]) is bool, "sequence_called")
        evidence.append({**sequence, **value})
    return evidence


def collect(container: str) -> JsonObject:
    """Return comparable evidence without retaining any row contents."""
    session = Session(container)
    try:
        session.query(SETUP, no_output)
        catalog = object_value(session.value(CATALOG_SQL))
        public.require(catalog.get("database") == "simplestchat", "database_identity")
        public.require(catalog.get("readOnly") == "on", "transaction_not_readonly")
        public.require(catalog.get("isolation") == "repeatable read", "transaction_isolation")
        public.require(catalog.get("databaseEncoding") == "UTF8", "database_encoding")
        public.require(
            re.fullmatch(r"[0-9]+", string_value(catalog["systemIdentifier"])), "system_identity"
        )
        table_evidence = tables(session, catalog)
        sequence_evidence = sequences(session, catalog)
        ledger = array_value(
            session.value(
                """SELECT COALESCE(jsonb_agg(jsonb_build_object(
 'version',version::text,'success',success,'checksum',encode(checksum,'hex'))
 ORDER BY version),'[]'::jsonb) FROM public._sqlx_migrations;"""
            )
        )
        public.require(0 < len(ledger) <= MAX_RELATIONS, "ledger_shape")
        for value in ledger:
            entry = object_value(value)
            public.require(
                re.fullmatch(r"[0-9]+", string_value(entry["version"])), "ledger_version"
            )
            public.require(entry.get("success") is True, "migration_failed")
            public.require(
                re.fullmatch(r"[0-9a-f]{96}", string_value(entry["checksum"])), "ledger_checksum"
            )
        metadata = session.digest(METADATA_SQL, limit=MAX_METADATA_BYTES)
        session.query("ROLLBACK;", no_output)
        return {
            "schemaVersion": 1,
            "passed": True,
            "database": catalog["database"],
            "postgresVersion": catalog["serverVersion"],
            "databaseEncoding": catalog["databaseEncoding"],
            "systemIdentifier": catalog["systemIdentifier"],
            "rowScope": "ONLY-each-relation-including-partition-leaves",
            "tables": table_evidence,
            "sequences": sequence_evidence,
            "migrationLedger": ledger,
            "schemaMetadata": metadata,
            "totalPhysicalRows": sum(
                integer_value(object_value(table)["rows"]) for table in table_evidence
            ),
        }
    finally:
        session.close()


def equivalent(source: JsonObject, destination: JsonObject) -> None:
    """Permit only the expected independent cluster identity to differ."""
    public.require(
        source.get("passed") is True and destination.get("passed") is True, "snapshot_not_passed"
    )
    public.require(
        source.get("systemIdentifier") != destination.get("systemIdentifier"),
        "same_database_cluster",
    )
    public.require(
        {key: value for key, value in source.items() if key != "systemIdentifier"}
        == {key: value for key, value in destination.items() if key != "systemIdentifier"},
        "database_snapshot_mismatch",
    )


def renamed_users(container: str, old_email: str, new_email: str) -> JsonObject:
    """Hash the expected email-only change without mutating or retaining any user row."""
    for email in (old_email, new_email):
        public.require(
            re.fullmatch(r"owner@(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}", email),
            "invalid_managed_owner_email",
        )
    statement = f"""COPY (
SELECT value::text FROM (
 SELECT CASE WHEN t.email='{old_email}'
 THEN to_jsonb(t)||jsonb_build_object('email','{new_email}')
 ELSE to_jsonb(t) END AS value FROM ONLY public.users t
) rewritten ORDER BY value::text COLLATE "C"
) TO STDOUT WITH(FORMAT text,ENCODING 'UTF8');
"""  # noqa: S608 -- both emails are canonical managed identities without SQL metacharacters.
    session = Session(container)
    try:
        session.query(SETUP, no_output)
        return session.digest(statement)
    finally:
        session.close()
