//! Fixed-plan and owned path fixtures; no database/container/browser work.

use super::*;
use std::collections::BTreeMap;

struct Fake {
    commands: Vec<Command>,
    output: Vec<u8>,
    fail: Option<(usize, u8)>,
    clean: bool,
}
impl Executor for Fake {
    async fn ready(&mut self) -> tooling::Result<()> {
        Ok(())
    }
    async fn run(&mut self, command: Command) -> tooling::Result<Vec<u8>> {
        self.commands.push(command);
        if let Some((index, code)) = self.fail
            && self.commands.len() == index
        {
            return Err(Failure::new(code, "Owned fixture failure."));
        }
        Ok(self.output.clone())
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
}

/// The captured environment of a check whose restore runs in containers.
fn containers() -> Environment {
    Environment::from([
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from(CONTAINER_SELECTOR), OsString::from("1")),
    ])
}

#[tokio::test]
async fn every_check_has_its_exact_command_and_restore_pull_precedes_discovery() {
    let root =
        std::env::temp_dir().join(format!("database check $(literal) {}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let environment = containers();
    let monitoring = [
        "cargo",
        "test",
        "--locked",
        "-p",
        "simplestchat-ops",
        "--lib",
        "--",
        "--ignored",
        "--exact",
        "--test-threads=1",
        "monitor::database_tests::an_ambiguous_committed_response_retains_the_outage_after_recovery",
        "monitor::database_tests::external_history_keeps_gaps_privacy_and_retention_through_partial_recovery",
        "monitor::database_tests::incident_lifecycle_replay_privileges_and_retention",
    ];
    // The same owned selection, naming the restore check's tests.
    let mut restore: Vec<String> = monitoring[..10].iter().map(ToString::to_string).collect();
    restore.extend(
        [
            "a_pre_upgrade_backup_verifies_against_its_own_recorded_ledger",
            "a_current_backup_keeps_incidents_and_refuses_privacy_and_schema_drift",
            "a_truncated_backup_never_verifies",
            "a_real_container_of_the_pinned_image_verifies_and_refuses",
        ]
        .map(|name| format!("release::attestation::restore::database_tests::{name}")),
    );
    let migration = [
        "cargo",
        "test",
        "--locked",
        "-p",
        "simplestchat-ops",
        "--lib",
        "--",
        "--ignored",
        "--exact",
        "--test-threads=1",
        "migrate::snapshot::database_tests::only_the_lossless_enum_cast_relocation_is_made_canonical",
        "migrate::snapshot::database_tests::a_real_restore_keeps_rows_partitions_grants_and_the_schema_digest",
    ];
    for (check, command) in [
        (Check::Monitoring, monitoring.map(str::to_owned).to_vec()),
        (Check::Restore, restore),
        (Check::Migration, migration.map(str::to_owned).to_vec()),
    ] {
        assert_eq!(check.command(), command);
        let mut owned = environment.clone();
        check.select(&mut owned);
        // Each check selects only its own tests.
        for (other, name) in [
            (Check::Monitoring, "MONITORING_DATABASE_CHECK"),
            (Check::Restore, "RESTORE_DATABASE_CHECK"),
            (Check::Migration, "MIGRATION_DATABASE_CHECK"),
        ] {
            let selector = owned.get(OsStr::new(name));
            let selector = selector.map(|value| value.to_str().unwrap());
            assert_eq!(selector, (check == other).then_some("1"), "{check:?}");
        }
        assert_eq!(owned.len(), environment.len() + 1);
        let mut executor = Fake {
            commands: vec![],
            output: format!("{}\n", root.display()).into_bytes(),
            fail: None,
            clean: true,
        };
        let result = prepare(check, &environment, &mut executor).await.unwrap();
        // The container selector reaches the tests in the check's cluster.
        assert_eq!(
            result.get(OsStr::new(CONTAINER_SELECTOR)),
            Some(&OsString::from("1"))
        );
        assert_eq!(
            executor.commands.len(),
            if check == Check::Restore { 2 } else { 1 }
        );
        if check == Check::Restore {
            let pull = &executor.commands[0];
            assert_eq!(pull.program, Program::Docker);
            assert_eq!(pull.arguments, ["pull", RESTORE_IMAGE].map(OsString::from));
            assert!(pull.capture.is_none());
            assert_eq!(pull.timeout, Duration::from_secs(600));
        }
        let command = executor.commands.last().unwrap();
        assert_eq!(command.program, Program::PgConfig);
        assert_eq!(command.arguments, [OsString::from("--bindir")]);
        assert_eq!(command.capture, Some(4096));
        assert_eq!(command.timeout, Duration::from_secs(30));
        assert_eq!(
            std::env::split_paths(result.get(OsStr::new("PATH")).unwrap()).next(),
            Some(root.clone())
        );
    }
    std::fs::remove_dir(root).unwrap();
}

/// Without the container selector, or with another value, the restore check
/// pulls nothing and runs its cluster tests only.
#[tokio::test]
async fn an_unselected_restore_pulls_nothing() {
    for value in [None, Some("0"), Some("true")] {
        let mut environment = Environment::new();
        if let Some(value) = value {
            environment.insert(CONTAINER_SELECTOR.into(), value.into());
        }
        let mut executor = Fake {
            commands: vec![],
            output: b"/".to_vec(),
            fail: None,
            clean: true,
        };
        prepare(Check::Restore, &environment, &mut executor)
            .await
            .unwrap();
        let programs: Vec<_> = executor.commands.iter().map(|c| c.program).collect();
        assert_eq!(programs, [Program::PgConfig], "{value:?}");
    }
}

#[tokio::test]
async fn preparation_failures_preserve_status_and_never_continue() {
    for code in [23, 124, 130, 143, 127] {
        let mut executor = Fake {
            commands: vec![],
            output: vec![],
            fail: Some((1, code)),
            clean: true,
        };
        assert_eq!(
            prepare(Check::Restore, &containers(), &mut executor)
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(executor.commands.len(), 1);
    }
}

#[tokio::test]
async fn failed_restore_discovery_does_not_hide_its_status_after_successful_pull() {
    let mut executor = Fake {
        commands: vec![],
        output: vec![],
        fail: Some((2, 37)),
        clean: true,
    };
    assert_eq!(
        prepare(Check::Restore, &containers(), &mut executor)
            .await
            .unwrap_err()
            .code,
        37
    );
    assert_eq!(executor.commands.len(), 2);
    assert_eq!(executor.commands[0].program, Program::Docker);
    assert_eq!(executor.commands[1].program, Program::PgConfig);
}

/// The pulled image is the one the deployment runs, read from its group
/// variables, and the Ops tests select their container mode by the same
/// variable this check reads.
#[test]
fn the_pinned_image_and_container_selector_are_the_deployments_and_the_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let variables = root.join("ops/ansible/group_vars/benchmark_hosts.yml");
    let variables: BTreeMap<String, serde_json::Value> =
        serde_saphyr::from_slice(&std::fs::read(variables).unwrap()).unwrap();
    assert_eq!(variables["scpub_postgres_image"], RESTORE_IMAGE);
    let tests = root.join("ops/src/release/attestation/restore/container_tests.rs");
    let tests = std::fs::read_to_string(tests).unwrap();
    let declared = format!("pub(super) const SELECTOR: &str = \"{CONTAINER_SELECTOR}\";");
    assert_eq!(tests.matches(&declared).count(), 1);
}

#[test]
fn path_admission_and_closed_selection_reject_ambiguous_inputs() {
    for args in [
        vec![],
        vec!["other".into()],
        vec!["restore".into(), "extra".into()],
    ] {
        assert!(Check::parse(&args).is_none());
    }
    for output in [
        b"".as_slice(),
        b"relative",
        b"/tmp\nsecond",
        b"/tmp\r\n",
        b"/tmp\0",
        b"\xff",
    ] {
        assert!(postgres_path(output, &Environment::new()).is_err());
    }
}

struct Owned(PathBuf);
impl Owned {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "pg config native {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let program = directory.join("pg_config");
        std::fs::write(
            &program,
            format!("#!/bin/sh\n{}{script}\n", crate::fake_shell::PRELUDE),
        )
        .unwrap();
        std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(directory)
    }
    fn environment(&self) -> Environment {
        Environment::from([("PATH".into(), self.0.clone().into_os_string())])
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn actual_owned_pg_config_preserves_paths_status_and_same_signal_registration() {
    let fixture = Owned::new(
        "root=${0%/*}\njson_strings \"$@\" >\"$root/args.json\"\nprintf '%s\\n' \"$root\"",
    );
    let environment = fixture.environment();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    let result = prepare(Check::Monitoring, &environment, &mut native)
        .await
        .unwrap();
    assert_eq!(
        std::env::split_paths(result.get(OsStr::new("PATH")).unwrap()).next(),
        Some(fixture.0.clone())
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("args.json")).unwrap(),
        "[\"--bindir\"]"
    );
    assert!(native.cleanup_passed());
    // The original receiver survives transfer; no second listener is installed.
    let mut signals = native.into_signals().await.unwrap();
    sender.send(130).unwrap();
    assert_eq!(signals.pending().await, Some(130));

    let fixture = Owned::new("exit 37");
    let environment = fixture.environment();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    assert_eq!(
        prepare(Check::Monitoring, &environment, &mut native)
            .await
            .unwrap_err()
            .code,
        37
    );
    assert!(native.cleanup_passed());
    std::fs::remove_file(fixture.0.join("pg_config")).unwrap();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    assert_eq!(
        prepare(Check::Monitoring, &environment, &mut native)
            .await
            .unwrap_err()
            .code,
        127
    );
    assert!(native.cleanup_passed());
}

#[tokio::test]
async fn actual_discovery_overflow_or_cancellation_never_transfers_clean_success() {
    let fixture = Owned::new("repeat x 4097\nprintf '\\n'");
    let environment = fixture.environment();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    assert!(
        prepare(Check::Migration, &environment, &mut native)
            .await
            .is_err()
    );

    let fixture = Owned::new(
        r#"root=${0%/*}
trap 'printf retired >"$root/term"; exit 0' TERM
printf ready >"$root/ready.pending"
/bin/mv -f "$root/ready.pending" "$root/ready"
while :; do /bin/sleep 0.01; done"#,
    );
    let environment = fixture.environment();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    let cancel = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.0.join("ready").is_file() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(
        prepare(Check::Monitoring, &environment, &mut native),
        cancel
    );
    assert_eq!(result.unwrap_err().code, 143);
    assert!(fixture.0.join("term").is_file());
    assert!(native.cleanup_passed());
    assert_eq!(native.into_signals().await.err().unwrap().code, 143);
}

#[tokio::test]
async fn unproven_cleanup_and_late_cancellation_fence_the_harness_handoff() {
    let fixture = Owned::new("printf '%s\\n' \"${0%/*}\"");
    let environment = fixture.environment();
    let mut fake = Fake {
        commands: vec![],
        output: fixture.0.to_string_lossy().as_bytes().to_vec(),
        fail: None,
        clean: false,
    };
    assert!(
        prepare(Check::Monitoring, &environment, &mut fake)
            .await
            .unwrap_err()
            .message
            .contains("cleanup is unproven")
    );
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    prepare(Check::Monitoring, &environment, &mut native)
        .await
        .unwrap();
    sender.send(143).unwrap();
    assert_eq!(native.into_signals().await.err().unwrap().code, 143);
}

/// Each owned check names every database test of its module, exactly: a
/// test missing from the list would never run, and a name that matched
/// nothing would be filtered out and the check would pass having run
/// nothing.
#[test]
fn the_owned_checks_name_exactly_their_database_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent);
    for (check, file, module, selector, count) in [
        (
            Check::Monitoring,
            "ops/src/monitor/database_tests.rs",
            "monitor::database_tests",
            "MONITORING_DATABASE_CHECK",
            3,
        ),
        (
            Check::Restore,
            "ops/src/release/attestation/restore/database_tests.rs",
            "release::attestation::restore::database_tests",
            "RESTORE_DATABASE_CHECK",
            4,
        ),
        (
            Check::Migration,
            "ops/src/migrate/snapshot_database_tests.rs",
            "migrate::snapshot::database_tests",
            "MIGRATION_DATABASE_CHECK",
            2,
        ),
    ] {
        let source = std::fs::read_to_string(root.unwrap().join(file)).unwrap();
        let mut declared = Vec::new();
        let mut lines = source.lines();
        while let Some(line) = lines.next() {
            if line != "#[test]" {
                continue;
            }
            let ignored = lines.next().unwrap();
            assert!(ignored.starts_with("#[ignore = \""), "{ignored}");
            let function = lines.next().unwrap().strip_prefix("fn ").unwrap();
            let name = function.strip_suffix("() {").unwrap();
            assert_eq!(
                lines.next(),
                Some("    let Some(url) = database() else { return };")
            );
            declared.push(format!("{module}::{name}"));
        }
        assert_eq!(source.matches("#[test]").count(), declared.len());
        let command = check.command();
        assert_eq!(command[10..], declared);
        assert_eq!(declared.len(), count);
        assert!(source.contains(&format!("const SELECTOR: &str = \"{selector}\";")));
    }
}
