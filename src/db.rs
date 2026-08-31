#![forbid(unsafe_code)]

use anyhow::{Context, bail};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;
use tracing::info;

pub async fn connect() -> anyhow::Result<Option<PgPool>> {
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

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(3))
        .connect_with(options)
        .await?;

    info!("Connected to PostgreSQL");

    if run_migrations {
        sqlx::migrate::Migrator::new(Path::new("./migrations"))
            .await?
            .run(&pool)
            .await?;
        info!("Database migrations applied");
    }

    Ok(Some(pool))
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
    // Several room mutations deliberately retain a per-room lock until the
    // durable write succeeds. Bound server and lock waits so a stalled
    // database cannot freeze a room indefinitely. Production migrations use
    // the separate `sqlx migrate run` path documented in README.
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
