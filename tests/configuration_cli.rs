//! Configuration preflight exercises the real binary without a runtime, socket,
//! media worker or database connection, and a start without the browser bundle
//! stops before any socket, media worker or database connection. Environment
//! changes stay in the child.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The real binary with an empty environment apart from one media worker.
fn server() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_simplestChat"));
    command.env_clear().env("MEDIA_WORKERS", "1");
    command
}

fn check() -> Command {
    let mut command = server();
    command.arg("--check-config");
    command
}

#[test]
fn preflight_is_offline_and_reports_only_public_settings() {
    let output = check()
        .env(
            "DATABASE_URL",
            "postgres://private-user:private-password@127.0.0.1:1/missing",
        )
        .env("JWT_SECRET", "private-jwt-secret-never-printed-anywhere")
        .env("MAX_CONNECTIONS", "123")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["version"], 2);
    assert_eq!(value["settings"]["MAX_CONNECTIONS"], 123);
    assert_eq!(value["settings"]["MEDIA_WORKERS"], 1);
    assert_eq!(value["settings"]["DATABASE_URL"], "configured");
    assert_eq!(value["settings"]["JWT_SECRET"], "configured");
    assert_eq!(value["settings"]["METRICS_TOKEN"], serde_json::Value::Null);
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

#[test]
fn preflight_passes_and_startup_refuses_without_the_browser_bundle() {
    let directory = std::env::temp_dir().join(format!(
        "simplestchat-startup-without-bundle-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory).unwrap();
    let preflight = check().current_dir(&directory).output().unwrap();
    assert!(
        preflight.status.success(),
        "{}",
        String::from_utf8_lossy(&preflight.stderr)
    );
    let mut child = server()
        .current_dir(&directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A start that ignored the missing bundle would serve until stopped.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (mut stdout, mut stderr) = (String::new(), String::new());
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
    let status = status.expect("startup must refuse a missing bundle, not serve");
    assert!(!status.success());
    assert!(
        stderr.contains("web/dist/index.html is missing; run npm --prefix web run build"),
        "{stderr}"
    );
    // The refusal comes right after the startup line, before the sizing and
    // address announcements that precede media workers and the listener.
    assert!(stdout.contains("simplestChat server starting"), "{stdout}");
    assert!(!stdout.contains("Announcing"), "{stdout}");
}
