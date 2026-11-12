//! Private, deterministic evidence of a database from one read-only
//! snapshot. Every physical table is read with ONLY, partition leaves
//! included, and reduced to a count and a digest while it streams: no row
//! reaches a log, a file or more of this process than the line being hashed.
//! Sequence state needs quiesced writers, because sequences are not
//! transactional.

use super::snapshot_sql::{CATALOG, METADATA, SETUP};
use super::value::{field, text};
use crate::host::dialogue::{Dialogue, Heard, PIECE};
use crate::host::evidence::ENVIRONMENT;
use crate::host::failure::{Failure, Result, require};
use crate::host::fault::{object, set, string};
use crate::host::{Clock, DOCKER, Host, domain, hexadecimal, state};
use crate::json::Json;
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Where the database listens inside its container.
pub(crate) const SOCKET: &str = "/run/simplestchat-postgres";
const MAX_RELATIONS: usize = 512;
const MAX_IDENTIFIER_BYTES: usize = 63;
const MAX_STATEMENT_BYTES: usize = 128 * 1024;
/// How long one statement may take, and a whole session.
const QUERY: Duration = Duration::from_secs(40);
const WHOLE: Duration = Duration::from_secs(300);
const SECOND: Duration = Duration::from_secs(1);
const FAREWELL: &[u8] = b"ROLLBACK;\n\\quit\n";
const LEDGER: &str = "SELECT COALESCE(jsonb_agg(jsonb_build_object(
 'version',version::text,'success',success,'checksum',encode(checksum,'hex'))
 ORDER BY version),'[]'::jsonb) FROM public._sqlx_migrations;";
const SCOPE: &str = "ONLY-each-relation-including-partition-leaves";

/// How much a session may return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bounds {
    /// The longest single line.
    pub line: usize,
    /// The most one statement may return.
    pub section: usize,
    /// The most the schema's description may be.
    pub metadata: usize,
    /// The most a whole session may return.
    pub total: usize,
}

/// The bounds of every real snapshot.
pub(crate) const BOUNDS: Bounds = Bounds {
    line: 4 * 1024 * 1024,
    section: 256 * 1024 * 1024,
    metadata: 16 * 1024 * 1024,
    total: 512 * 1024 * 1024,
};

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn hexadecimal_text(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A catalog name as a quoted identifier: it never becomes a fragment of a
/// statement.
fn identifier(value: &str) -> Result<String> {
    let sized = (1..=MAX_IDENTIFIER_BYTES).contains(&value.len());
    require(sized && !value.contains('\0'), "invalid_identifier")?;
    Ok(format!("\"{}\"", value.replace('"', "\"\"")))
}

/// The quoted name of one relation the catalog listed.
fn qualified(relation: &Json) -> Result<String> {
    let schema = identifier(text(field(relation, "schema")?)?)?;
    let name = identifier(text(field(relation, "name")?)?)?;
    Ok(format!("{schema}.{name}"))
}

/// `base` with every field of `added`, replaced where it stands or appended.
fn merged(base: &Json, added: &Json) -> Json {
    let mut whole = base.clone();
    if let Json::Object(entries, order) = added {
        for name in order {
            set(&mut whole, name, entries[name].clone());
        }
    }
    whole
}

/// One bounded client transaction whose output is hashed line by line.
struct Session<'a> {
    child: Box<dyn Dialogue + 'a>,
    clock: &'a dyn Clock,
    bounds: Bounds,
    /// When the whole session must be over, on the monotonic clock.
    deadline: Duration,
    total: usize,
    pending: Vec<u8>,
    marker: Vec<u8>,
}

impl<'a> Session<'a> {
    /// Start the private client of an already verified database container.
    fn open(host: &Host<'a>, container: &str, bounds: Bounds) -> Result<Self> {
        require(hexadecimal(container, 64), "invalid_database_container")?;
        let deadline = host.clock.monotonic() + WHOLE;
        let mut random = [0_u8; 24];
        getrandom::fill(&mut random).map_err(|_| Failure::Os)?;
        let marker = format!("SNAPSHOT_END_{}", hexadecimal_text(&random)).into_bytes();
        let client = [
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
        ];
        let argv: Vec<String> = DOCKER
            .iter()
            .chain(&client)
            .map(|word| (*word).to_owned())
            .collect();
        let child = host.commands.converse(&argv, &ENVIRONMENT)?;
        Ok(Self {
            child,
            clock: host.clock,
            bounds,
            deadline,
            total: 0,
            pending: Vec::new(),
            marker,
        })
    }

    /// Hand the client all of `payload` before `deadline`.
    fn send(&mut self, payload: &[u8], deadline: Duration) -> Result<()> {
        let budget = deadline.saturating_sub(self.clock.monotonic());
        require(!budget.is_zero(), "postgres_input_deadline")?;
        let sent = self.child.send(payload, budget);
        sent.map_err(|failure| match failure {
            Failure::Timeout => Failure::Refused("postgres_input_deadline"),
            Failure::Command => Failure::Refused("postgres_session_exited"),
            other => other,
        })
    }

    /// Run one group of statements and give `consume` each complete line it
    /// prints, up to `limit` bytes, until the client echoes this session's
    /// marker as the very last thing it says.
    fn query(
        &mut self,
        sql: &str,
        limit: usize,
        consume: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        let deadline = self.deadline.min(self.clock.monotonic() + QUERY);
        let mut payload = sql.as_bytes().to_vec();
        payload.extend_from_slice(b"\n\\echo ");
        payload.extend_from_slice(&self.marker);
        payload.push(b'\n');
        require(payload.len() <= MAX_STATEMENT_BYTES, "sql_limit")?;
        self.send(&payload, deadline)?;
        let mut section = 0_usize;
        loop {
            let pending = std::mem::take(&mut self.pending);
            let mut start = 0;
            while let Some(length) = pending[start..].iter().position(|byte| *byte == b'\n') {
                let end = start + length + 1;
                let line = &pending[start..end];
                require(line.len() <= self.bounds.line, "row_limit")?;
                if line[..length] == self.marker[..] {
                    return require(end == pending.len(), "unexpected_output_after_marker");
                }
                section += line.len();
                self.total += line.len();
                require(section <= limit, "section_limit")?;
                require(self.total <= self.bounds.total, "snapshot_byte_limit")?;
                consume(line)?;
                start = end;
            }
            self.pending = pending[start..].to_vec();
            let budget = deadline.saturating_sub(self.clock.monotonic());
            require(!budget.is_zero(), "postgres_query_deadline")?;
            match self.child.hear(budget.min(SECOND))? {
                Heard::Quiet => require(self.child.alive(), "postgres_session_exited")?,
                Heard::Closed => return Err(Failure::Refused("postgres_output_closed")),
                Heard::Bytes(piece) => {
                    self.pending.extend_from_slice(&piece);
                    let bounded = self.pending.len() <= self.bounds.line + PIECE;
                    require(bounded, "row_limit")?;
                }
            }
        }
    }

    /// Statements that must print nothing.
    fn silent(&mut self, sql: &str) -> Result<()> {
        let refuse = &mut |_: &[u8]| Err(Failure::Refused("unexpected_transaction_output"));
        self.query(sql, self.bounds.section, refuse)
    }

    /// The one JSON value a redacted catalog query prints; never rows.
    fn value(&mut self, sql: &str) -> Result<Json> {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let keep = &mut |line: &[u8]| {
            lines.push(line.to_vec());
            Ok(())
        };
        self.query(sql, self.bounds.line, keep)?;
        require(lines.len() == 1, "json_result_shape")?;
        state::decode(&lines[0])
    }

    /// Count and hash every line of a statement's output, keeping none.
    fn digest(&mut self, sql: &str, limit: usize) -> Result<Json> {
        let mut hash = Sha256::new();
        let (mut records, mut size) = (0_usize, 0_usize);
        let count = &mut |line: &[u8]| {
            hash.update(line);
            records += 1;
            size += line.len();
            Ok(())
        };
        self.query(sql, limit, count)?;
        Ok(object(vec![
            ("rows", Json::Integer(records.to_string())),
            ("copyBytes", Json::Integer(size.to_string())),
            ("sha256", string(&hexadecimal_text(&hash.finalize()))),
        ]))
    }

    /// Roll back and leave; the server's own statement and idle bounds stay
    /// in force whatever the client does.
    fn close(self) {
        self.child.end(FAREWELL);
    }
}

/// Fingerprint every relation's physical rows exactly once.
fn tables(session: &mut Session<'_>, catalog: &Json) -> Result<Vec<Json>> {
    let selected = field(catalog, "tables")?.array().ok_or(Failure::Value)?;
    require((1..=MAX_RELATIONS).contains(&selected.len()), "table_count")?;
    let mut evidence = Vec::new();
    for table in selected {
        let name = qualified(table)?;
        let statement = format!(
            "COPY (SELECT to_jsonb(t)::text FROM ONLY {name} AS t \
             ORDER BY to_jsonb(t)::text COLLATE \"C\") \
             TO STDOUT WITH (FORMAT text, ENCODING 'UTF8');"
        );
        let result = session.digest(&statement, session.bounds.section)?;
        evidence.push(merged(table, &result));
    }
    Ok(evidence)
}

/// The exact state of every sequence, which only a window without writers
/// makes meaningful.
fn sequences(session: &mut Session<'_>, catalog: &Json) -> Result<Vec<Json>> {
    let selected = field(catalog, "sequences")?.array().ok_or(Failure::Value)?;
    require(selected.len() <= MAX_RELATIONS, "sequence_count")?;
    let mut evidence = Vec::new();
    for sequence in selected {
        let name = qualified(sequence)?;
        let statement = format!(
            "SELECT jsonb_build_object('lastValue',last_value::text,'isCalled',is_called) \
             FROM {name};"
        );
        let value = session.value(&statement)?;
        let last = text(field(&value, "lastValue")?)?;
        let last = last.strip_prefix('-').unwrap_or(last);
        require(digits(last), "sequence_value")?;
        let called = matches!(field(&value, "isCalled")?, Json::Bool(_));
        require(called, "sequence_called")?;
        evidence.push(merged(sequence, &value));
    }
    Ok(evidence)
}

/// Every applied schema migration, each of which must have succeeded.
fn ledger(session: &mut Session<'_>) -> Result<Json> {
    let recorded = session.value(LEDGER)?;
    let entries = recorded.array().ok_or(Failure::Value)?;
    require((1..=MAX_RELATIONS).contains(&entries.len()), "ledger_shape")?;
    for entry in entries {
        require(digits(text(field(entry, "version")?)?), "ledger_version")?;
        let succeeded = matches!(entry.get("success"), Some(Json::Bool(true)));
        require(succeeded, "migration_failed")?;
        let checksum = text(field(entry, "checksum")?)?;
        require(hexadecimal(checksum, 96), "ledger_checksum")?;
    }
    Ok(recorded)
}

/// The comparable description of the database a session is connected to.
fn described(session: &mut Session<'_>) -> Result<Json> {
    session.silent(SETUP)?;
    let catalog = session.value(CATALOG)?;
    catalog.object().ok_or(Failure::Value)?;
    let named = |name: &str| catalog.get(name).and_then(Json::string);
    require(
        named("database") == Some("simplestchat"),
        "database_identity",
    )?;
    require(named("readOnly") == Some("on"), "transaction_not_readonly")?;
    let isolated = named("isolation") == Some("repeatable read");
    require(isolated, "transaction_isolation")?;
    require(
        named("databaseEncoding") == Some("UTF8"),
        "database_encoding",
    )?;
    let cluster = text(field(&catalog, "systemIdentifier")?)?;
    require(digits(cluster), "system_identity")?;
    let tables = tables(session, &catalog)?;
    let sequences = sequences(session, &catalog)?;
    let ledger = ledger(session)?;
    let metadata = session.digest(METADATA, session.bounds.metadata)?;
    session.silent("ROLLBACK;")?;
    let mut rows = 0_i64;
    for table in &tables {
        let counted = field(table, "rows")?.integer().ok_or(Failure::Value)?;
        rows = rows.checked_add(counted).ok_or(Failure::Value)?;
    }
    Ok(object(vec![
        ("schemaVersion", Json::Integer("1".into())),
        ("passed", Json::Bool(true)),
        ("database", field(&catalog, "database")?.clone()),
        ("postgresVersion", field(&catalog, "serverVersion")?.clone()),
        (
            "databaseEncoding",
            field(&catalog, "databaseEncoding")?.clone(),
        ),
        (
            "systemIdentifier",
            field(&catalog, "systemIdentifier")?.clone(),
        ),
        ("rowScope", string(SCOPE)),
        ("tables", Json::Array(tables)),
        ("sequences", Json::Array(sequences)),
        ("migrationLedger", ledger),
        ("schemaMetadata", metadata),
        ("totalPhysicalRows", Json::Integer(rows.to_string())),
    ]))
}

/// The snapshot of `container` within `bounds`.
fn snapshot(host: &Host<'_>, container: &str, bounds: Bounds) -> Result<Json> {
    let mut session = Session::open(host, container, bounds)?;
    let outcome = described(&mut session);
    session.close();
    outcome
}

/// Comparable evidence of the database in `container`, without any row.
pub(crate) fn collect(host: &Host<'_>, container: &str) -> Result<Json> {
    snapshot(host, container, BOUNDS)
}

/// Require two snapshots that differ in nothing but the identity of their
/// clusters, which must differ.
pub(crate) fn equivalent(source: &Json, destination: &Json) -> Result<()> {
    let passed = |snapshot: &Json| matches!(snapshot.get("passed"), Some(Json::Bool(true)));
    require(passed(source) && passed(destination), "snapshot_not_passed")?;
    let cluster = |snapshot: &Json| snapshot.get("systemIdentifier").cloned();
    let same = match (cluster(source), cluster(destination)) {
        (Some(one), Some(other)) => one.semantic_eq(&other),
        (None, None) => true,
        _ => false,
    };
    require(!same, "same_database_cluster")?;
    let rest = |snapshot: &Json| {
        let mut rest = snapshot.clone();
        if let Json::Object(entries, order) = &mut rest {
            entries.remove("systemIdentifier");
            order.retain(|name| name != "systemIdentifier");
        }
        rest
    };
    let alike = rest(source).semantic_eq(&rest(destination));
    require(alike, "database_snapshot_mismatch")
}

/// What the users table's digest becomes when only the managed owner's
/// address changes from `old` to `new`: computed from the stream, without
/// changing or keeping any row.
pub(crate) fn renamed_users(
    host: &Host<'_>,
    container: &str,
    old: &str,
    new: &str,
) -> Result<Json> {
    for address in [old, new] {
        let managed = address.strip_prefix("owner@").is_some_and(domain);
        require(managed, "invalid_managed_owner_email")?;
    }
    // Both addresses are canonical managed identities: no quote, backslash
    // or other statement syntax can occur in them.
    let statement = format!(
        "COPY (\nSELECT value::text FROM (\n SELECT CASE WHEN t.email='{old}'\n \
         THEN to_jsonb(t)||jsonb_build_object('email','{new}')\n \
         ELSE to_jsonb(t) END AS value FROM ONLY public.users t\n\
         ) rewritten ORDER BY value::text COLLATE \"C\"\n\
         ) TO STDOUT WITH(FORMAT text,ENCODING 'UTF8');\n"
    );
    let mut session = Session::open(host, container, BOUNDS)?;
    let prepared = session.silent(SETUP);
    let outcome = prepared.and_then(|()| session.digest(&statement, session.bounds.section));
    session.close();
    outcome
}

#[cfg(test)]
#[path = "snapshot_database_tests.rs"]
mod database_tests;
#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
