//! Configuration preflight exercises the real binary without a runtime, socket,
//! media worker or database connection. Environment changes stay in the child.

use std::process::Command;

fn check() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_simplestChat"));
    command
        .env_clear()
        .env("MEDIA_WORKERS", "1")
        .arg("--check-config");
    command
}

#[test]
fn preflight_is_offline_and_reports_only_public_settings() {
    let output = check()
        .env(
            "DATABASE_URL",
            "postgres://private-user:private-password@127.0.0.1:1/missing",
        )
        .env("JWT_SECRET", "private-jwt-secret-never-print")
        .env("MAX_CONNECTIONS", "123")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["numeric"]["MAX_CONNECTIONS"], 123);
    assert_eq!(value["mediaWorkers"], 1);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private"));
}

#[test]
fn malformed_limits_and_inconsistent_pool_bounds_fail_before_startup() {
    for value in ["", "0", "mistyped", "1000001"] {
        let output = check().env("MAX_CONNECTIONS", value).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("MAX_CONNECTIONS must"));
    }
    let output = check()
        .env("DATABASE_MAX_CONNECTIONS", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("DATABASE_MIN_CONNECTIONS"));
    let output = check().env("MEDIA_WORKERS", "65").output().unwrap();
    assert!(!output.status.success());
}
