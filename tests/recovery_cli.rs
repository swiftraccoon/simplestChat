//! The recovery-key issuance mode of the real binary: malformed command
//! lines and selections stop before any database, a database that cannot be
//! reached changes nothing, and (with a migrated disposable
//! `TEST_DATABASE_URL`) an eligible account gets exactly the key written out.

use sha2::{Digest, Sha256};
use std::process::{Command, Output};

const REFUSED: &str = "account_recovery_refused_nothing_changed\n";

/// The real binary with an empty environment apart from one media worker.
fn issue(arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_simplestChat"));
    command.env_clear().env("MEDIA_WORKERS", "1");
    command.arg("issue-account-recovery").args(arguments);
    command
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

#[test]
fn a_malformed_command_line_is_a_usage_error_before_anything_is_read() {
    for arguments in [
        &[][..],
        &["--email"],
        &["owner@example.test"],
        &["--mail", "owner@example.test"],
        &["--email", "owner@example.test", "--email"],
        &["--email", "owner@example.test", "--output", "PRIVATE_VALUE"],
        &["--email=owner@example.test"],
    ] {
        // Even an invalid setting is not reached.
        let output = issue(arguments)
            .env("MAX_CONNECTIONS", "mistyped")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}");
        let stderr = text(&output.stderr);
        let usage = "simplestChat issue-account-recovery --email ADDRESS [--email ADDRESS]...";
        assert!(
            stderr.contains(usage) && !stderr.contains("PRIVATE_VALUE"),
            "{arguments:?}"
        );
    }
}

#[test]
fn an_invalid_selection_or_an_absent_database_changes_nothing() {
    let refused = |output: Output| {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(text(&output.stderr), REFUSED);
    };
    // No database is configured.
    refused(issue(&["--email", "owner@example.test"]).output().unwrap());
    // The selection is refused before the unreachable database is tried.
    let private = "postgres://private-user:private-phrase@127.0.0.1:1/missing";
    for arguments in [
        &["--email", "Owner@example.test"][..],
        &[
            "--email",
            "owner@example.test",
            "--email",
            "owner@example.test",
        ],
        &["--email", "owner"],
    ] {
        let began = std::time::Instant::now();
        refused(
            issue(arguments)
                .env("DATABASE_URL", private)
                .output()
                .unwrap(),
        );
        assert!(
            began.elapsed() < std::time::Duration::from_secs(2),
            "{arguments:?}"
        );
    }
    // A database that cannot be reached: nothing is written out at all.
    let output = issue(&["--email", "owner@example.test"])
        .env("DATABASE_URL", private)
        .env("DATABASE_ACQUIRE_TIMEOUT_SECS", "1")
        .output()
        .unwrap();
    assert!(!text(&output.stderr).contains("private"));
    refused(output);
}

/// One eligible account gets exactly the key the command wrote out; asking
/// again is refused and leaves that key in place.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_the_binary_issues_the_key_it_writes_out_and_never_replaces_it() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let email = format!("{}@recovery-cli.invalid", uuid::Uuid::new_v4().simple());
    sqlx::query("INSERT INTO users(email, display_name) VALUES($1, 'Recovery fixture')")
        .bind(&email)
        .execute(&pool)
        .await
        .unwrap();
    let run = || {
        issue(&["--email", &email])
            .env("DATABASE_URL", &url)
            .output()
            .unwrap()
    };
    let output = run();
    assert_eq!(text(&output.stderr), "account_recovery_issued\n");
    assert_eq!(output.status.code(), Some(0));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(document["accounts"][0]["email"], email.as_str());
    let handed = document["accounts"][0]["recoveryKey"].as_str().unwrap();
    let digest = hex::encode(Sha256::digest(handed.as_bytes()));
    let read = "SELECT recovery_key_hash FROM users WHERE email = $1";
    let stored: Option<String> = sqlx::query_scalar(read)
        .bind(&email)
        .fetch_one(&pool)
        .await
        .unwrap();
    // A key is never shown, only judged.
    assert!(stored.as_deref() == Some(digest.as_str()));
    let again = run();
    assert_eq!(
        (again.status.code(), text(&again.stderr)),
        (Some(1), REFUSED)
    );
    let kept: Option<String> = sqlx::query_scalar(read)
        .bind(&email)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(kept == stored);
    sqlx::query("DELETE FROM users WHERE email = $1")
        .bind(&email)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}
