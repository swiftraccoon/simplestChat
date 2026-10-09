#![forbid(unsafe_code)]

use anyhow::{Context, bail};
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;
use tracing::info;

// Process-wide counters let error conversions classify a SQLx error before its
// private details are redacted, without threading a metrics handle through all
// transactions. These count instrumented boundaries, not statements or retries.
const ERROR_KINDS: [&str; 9] = [
    "pool_timeout",
    "pool_closed",
    "connection",
    "lock_timeout",
    "cancelled",
    "serialization",
    "deadlock",
    "constraint",
    "other",
];
static DATABASE_ERRORS: [std::sync::atomic::AtomicU64; 9] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 9];

fn error_kind(error: &sqlx::Error) -> usize {
    match error {
        sqlx::Error::PoolTimedOut => 0,
        sqlx::Error::PoolClosed => 1,
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::Protocol(_) => 2,
        sqlx::Error::Database(error) => match error.code().as_deref() {
            Some("55P03") => 3,
            Some("57014") => 4,
            Some("40001") => 5,
            Some("40P01") => 6,
            Some(code) if code.starts_with("23") => 7,
            Some(code) if code.starts_with("08") => 2,
            _ => 8,
        },
        _ => 8,
    }
}

/// Count a failure at one error-conversion boundary, before redacting details.
/// Do not call this twice while propagating the same error.
pub fn record_error(error: &sqlx::Error) {
    DATABASE_ERRORS[error_kind(error)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn append_error_metrics(out: &mut String) {
    use std::fmt::Write as _;
    let _ = writeln!(
        out,
        "# HELP simplestchat_database_errors_total Database errors at instrumented API conversion boundaries, not all statements; cancelled includes statement deadlines\n# TYPE simplestchat_database_errors_total counter"
    );
    for (kind, counter) in ERROR_KINDS.iter().zip(&DATABASE_ERRORS) {
        let _ = writeln!(
            out,
            "simplestchat_database_errors_total{{kind=\"{kind}\"}} {}",
            counter.load(std::sync::atomic::Ordering::Relaxed)
        );
    }
}

pub async fn connect() -> anyhow::Result<Option<PgPool>> {
    let limits = crate::configuration::numeric_settings()?;
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            info!("DATABASE_URL not set — running without database (anonymous-only mode)");
            return Ok(None);
        }
    };

    let options = PgConnectOptions::from_str(&url).context("invalid DATABASE_URL")?;
    let options = with_runtime_timeouts(options);
    require_verified_remote_database(&options)?;
    let run_migrations = run_migrations()?;

    // A small warm floor keeps the first request after an idle period from
    // paying a full connect (TLS on remote hosts) inside the acquire budget.
    // The default pre-acquire ping is retained deliberately: one extra
    // round trip is cheaper than a user-facing failure on a stale connection
    // after a database restart.
    let pool = PgPoolOptions::new()
        .max_connections(limits["DATABASE_MAX_CONNECTIONS"] as u32)
        .min_connections(limits["DATABASE_MIN_CONNECTIONS"] as u32)
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .acquire_timeout(Duration::from_secs(
            limits["DATABASE_ACQUIRE_TIMEOUT_SECS"] as u64,
        ))
        .acquire_slow_threshold(Duration::from_millis(500))
        .connect_with(options)
        .await?;

    info!("Connected to PostgreSQL");

    if run_migrations {
        // An interrupted concurrent build may leave an invalid named index. Never
        // let IF NOT EXISTS mark its migration complete without repairing it.
        verify_chat_indexes(&pool, false).await?;
        // Maintenance budgets, not the runtime's 10 s statement deadline: a large
        // index build or backfill must not fail at a limit meant for requests.
        let maintenance = PgConnectOptions::from_str(&url)
            .context("invalid DATABASE_URL")?
            .options([("statement_timeout", "10min"), ("lock_timeout", "1min")]);
        let mut connection = sqlx::postgres::PgConnection::connect_with(&maintenance).await?;
        sqlx::migrate::Migrator::new(Path::new("./migrations"))
            .await?
            .run(&mut connection)
            .await?;
        sqlx::Connection::close(connection).await?;
        info!("Database migrations applied");
    }

    verify_schema(&pool).await?;
    Ok(Some(pool))
}

/// One column from each table the newest migrations shaped. A deployment that ran
/// the binary against an unmigrated database fails here, at startup, with a message
/// naming the gap, instead of on the first request that touches it.
const EXPECTED_COLUMNS: [(&str, &str); 26] = [
    ("attachments", "data"),
    ("contacts", "status"),
    ("saved_rooms", "favorite"),
    ("account_notification_preferences", "private_messages"),
    ("conversation_notification_preferences", "snoozed_until"),
    ("chat_pins", "message_id"),
    ("push_keys", "private_key"),
    ("push_subscriptions", "generation"),
    ("rooms", "history_retention_days"),
    ("chat_messages", "body"),
    ("chat_inbox", "last_message_id"),
    ("chat_read_cursors", "message_id"),
    ("users", "preferences"),
    ("users", "profile_style"),
    ("rooms", "name_style"),
    ("rooms", "topic_style"),
    ("sessions", "refresh_token_family_hash"),
    ("rooms", "updated_at"),
    ("room_states", "ip_address"),
    ("room_reports", "resolved_by"),
    ("moderation_events", "report_id"),
    ("invites", "uses_left"),
    ("invites", "code_hash"),
    ("invites", "id"),
    ("invite_redemptions", "invite_hash"),
    ("webauthn_credentials", "credential_json"),
];

/// Concurrent indexes are checked both before migration retry and before readiness.
async fn verify_chat_indexes(pool: &PgPool, required: bool) -> anyhow::Result<()> {
    let indexes: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT c.relname::text, i.indisvalid, i.indisready
         FROM pg_catalog.pg_index i
         JOIN pg_catalog.pg_class c ON c.oid=i.indexrelid
         JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
         WHERE n.nspname='public' AND c.relname IN
           ('chat_messages_incoming_unread','chat_messages_conversation_reply')",
    )
    .fetch_all(pool)
    .await
    .context("could not inspect concurrent chat indexes")?;
    for (name, valid, ready) in &indexes {
        if !valid || !ready {
            bail!("database index {name} is incomplete; repair it before rerunning migrations");
        }
    }
    if required && indexes.len() != 2 {
        bail!(
            "database schema is behind this build (missing chat lookup indexes); run the migrations first"
        );
    }
    Ok(())
}

async fn verify_schema(pool: &PgPool) -> anyhow::Result<()> {
    // Fixed identifiers only; no runtime value enters the SQL text.
    let expected = EXPECTED_COLUMNS
        .iter()
        .map(|(table, column)| format!("('{table}', '{column}')"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT c.table_name::text, c.column_name::text, c.is_nullable::text
         FROM information_schema.columns c
         JOIN (VALUES {expected}) AS e(table_name, column_name)
           ON c.table_name = e.table_name AND c.column_name = e.column_name
         WHERE c.table_schema = 'public'"
    );
    let present: Vec<(String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .context("could not inspect the database schema")?;
    let missing = schema_gaps(&present);
    if !missing.is_empty() {
        bail!(
            "database schema is behind this build (missing {}); run the migrations first",
            missing.join(", ")
        );
    }
    verify_chat_indexes(pool, true).await
}

fn schema_gaps(present: &[(String, String, String)]) -> Vec<String> {
    let mut missing: Vec<String> = EXPECTED_COLUMNS
        .iter()
        .filter(|(table, column)| !present.iter().any(|(t, c, _)| t == table && c == column))
        .map(|(table, column)| format!("{table}.{column}"))
        .collect();
    if present.iter().any(|(table, column, nullable)| {
        table == "sessions" && column == "refresh_token_family_hash" && nullable != "NO"
    }) {
        missing.push("sessions.refresh_token_family_hash NOT NULL".to_string());
    }
    missing
}

fn require_verified_remote_database(options: &PgConnectOptions) -> anyhow::Result<()> {
    let host = options.get_host();
    // URL parsing preserves brackets around IPv6 literals in this accessor.
    let host_ip = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    let local = options.get_socket().is_some()
        || host.eq_ignore_ascii_case("localhost")
        || host_ip
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());

    if !local && !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull) {
        bail!(
            "remote DATABASE_URL host {host:?} must set sslmode=verify-full (and sslrootcert when the CA is not in the system trust store)"
        );
    }

    Ok(())
}

fn with_runtime_timeouts(options: PgConnectOptions) -> PgConnectOptions {
    // Bound every pool user's server-side waits. Room writes additionally bound
    // the whole persistence phase while retaining their control gate, not their
    // chat/media state lock. Production migrations use a separate connection.
    options.options([
        ("statement_timeout", "10s"),
        ("lock_timeout", "5s"),
        ("idle_in_transaction_session_timeout", "15s"),
    ])
}

fn run_migrations() -> anyhow::Result<bool> {
    let value = match std::env::var("RUN_MIGRATIONS") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(false),
        Err(error) => return Err(error).context("RUN_MIGRATIONS must be valid UTF-8"),
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" | "" => Ok(false),
        _ => bail!("RUN_MIGRATIONS must be true or false"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_requires_nonnullable_refresh_families() {
        let mut columns: Vec<_> = EXPECTED_COLUMNS
            .iter()
            .map(|(table, column)| (table.to_string(), column.to_string(), "NO".to_string()))
            .collect();
        assert!(schema_gaps(&columns).is_empty());
        let family = columns
            .iter_mut()
            .find(|(table, column, _)| table == "sessions" && column == "refresh_token_family_hash")
            .unwrap();
        family.2 = "YES".into();
        assert_eq!(
            schema_gaps(&columns),
            vec!["sessions.refresh_token_family_hash NOT NULL"]
        );
        columns.retain(|(table, _, _)| table != "sessions");
        assert_eq!(
            schema_gaps(&columns),
            vec!["sessions.refresh_token_family_hash"]
        );
    }

    #[test]
    fn pool_failures_are_classified_without_error_text() {
        assert_eq!(
            ERROR_KINDS[error_kind(&sqlx::Error::PoolTimedOut)],
            "pool_timeout"
        );
        assert_eq!(
            ERROR_KINDS[error_kind(&sqlx::Error::PoolClosed)],
            "pool_closed"
        );
        assert_eq!(
            ERROR_KINDS[error_kind(&sqlx::Error::Protocol("private details".to_owned()))],
            "connection"
        );
        assert_eq!(ERROR_KINDS[error_kind(&sqlx::Error::RowNotFound)], "other");
        let mut snapshot = String::new();
        append_error_metrics(&mut snapshot);
        assert!(!snapshot.contains("private details"));
    }

    #[test]
    fn rejects_remote_database_without_full_verification() {
        for url in [
            "postgres://user@example.com/chat",
            "postgres://user@example.com/chat?sslmode=require",
            "postgres://user@example.com/chat?sslmode=verify-ca",
            // Query parameters override the URL authority in SQLx; validate the
            // effective destination rather than trusting the apparent host.
            "postgres://user@localhost/chat?hostaddr=203.0.113.10&sslmode=disable",
        ] {
            let options = PgConnectOptions::from_str(url).unwrap();
            assert!(require_verified_remote_database(&options).is_err(), "{url}");
        }
    }

    #[test]
    fn accepts_remote_database_with_full_verification() {
        let options =
            PgConnectOptions::from_str("postgres://user@example.com/chat?sslmode=verify-full")
                .unwrap();
        assert!(require_verified_remote_database(&options).is_ok());
    }

    #[test]
    fn accepts_unencrypted_loopback_database() {
        for url in [
            "postgres://user@localhost/chat?sslmode=disable",
            "postgres://user@127.0.0.1/chat?sslmode=disable",
            "postgres://user@[::1]/chat?sslmode=disable",
        ] {
            let options = PgConnectOptions::from_str(url).unwrap();
            assert!(require_verified_remote_database(&options).is_ok(), "{url}");
        }
    }

    #[test]
    fn runtime_connections_have_bounded_database_waits() {
        let options = with_runtime_timeouts(
            PgConnectOptions::from_str("postgres://user@localhost/chat").unwrap(),
        );
        let startup_options = options.get_options().expect("bounded startup options");
        assert!(startup_options.contains("-c statement_timeout=10s"));
        assert!(startup_options.contains("-c lock_timeout=5s"));
        assert!(startup_options.contains("-c idle_in_transaction_session_timeout=15s"));
    }
}
