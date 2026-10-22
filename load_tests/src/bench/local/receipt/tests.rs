//! Owned fake children and private files test receipt failures without workloads.

use super::*;
use crate::bench::process::Sink;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, symlink},
    path::PathBuf,
};

const ID: &str = "12345678-1234-4234-8234-123456789abc";
struct Fake {
    known: bool,
    group: bool,
    streams: bool,
    fail_spawn: bool,
    fail_stop: bool,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            known: true,
            group: true,
            streams: true,
            fail_spawn: false,
            fail_stop: false,
        }
    }
}
struct Process {
    facts: Option<Retirement>,
    fail: bool,
}
#[async_trait::async_trait]
impl Child for Process {
    fn pid(&self) -> u32 {
        1001
    }
    fn retirement(&self) -> Option<Retirement> {
        self.facts.clone()
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        Ok(Some(exit()))
    }
    async fn stop(&mut self, _: Duration) -> Result<Exit> {
        if std::mem::take(&mut self.fail) {
            anyhow::bail!("inert stop failure");
        }
        Ok(exit())
    }
    fn output(&self) -> &[u8] {
        b"inert output"
    }
    fn errors(&self) -> &[u8] {
        &[]
    }
}
fn exit() -> Exit {
    Exit {
        code: Some(0),
        signal: None,
    }
}
#[async_trait::async_trait]
impl Host for Fake {
    fn spawn(&self, _: Command) -> Result<Box<dyn Child>> {
        ensure!(!self.fail_spawn, "inert admission failure");
        Ok(Box::new(Process {
            facts: self.known.then_some(Retirement {
                term_attempted: true,
                term_sent: true,
                kill_attempted: true,
                kill_sent: false,
                leader_terminal_before_cleanup: Some(true),
                leader_grace_expired: Some(false),
                wait_code: Some(0),
                wait_signal: None,
                group_cleanup_passed: self.group,
                streams_closed: self.streams,
            }),
            fail: self.fail_stop,
        }))
    }
    async fn cancelled(&self) -> u8 {
        std::future::pending().await
    }
}
fn command() -> Command {
    Command {
        executable: "inert-never-executed".into(),
        arguments: Vec::new(),
        directory: "/".into(),
        environment: BTreeMap::new(),
        timeout: Duration::from_secs(1),
        stdout_limit: 1,
        stderr_limit: 1,
        input: Vec::new(),
        stdout: Sink::Capture,
        stderr: Sink::Capture,
        cleanup: false,
    }
}
fn six(observed: &ObservedHost<'_>) {
    for _ in 0..6 {
        observed.completed_run();
    }
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("local-receipt-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn run_identity_is_optional_and_canonical_before_any_resources() {
    assert!(run_id(&BTreeMap::new()).unwrap().is_none());
    for value in [
        "",
        "secret",
        "12345678123442348234123456789abc",
        "12345678-1234-4234-8234-123456789ABC",
    ] {
        assert!(run_id(&[("BENCH_CI_RUN_ID".into(), value.into())].into()).is_err());
    }
    assert_eq!(
        run_id(&[("BENCH_CI_RUN_ID".into(), ID.into())].into())
            .unwrap()
            .as_deref(),
        Some(ID)
    );
}

#[test]
fn typed_primary_status_survives_late_signal_and_success_observes_pending_signal() {
    use crate::owned_process::{Interrupted, child::CommandTimedOut};
    for code in [42, 124, 130, 143] {
        let outcome = Err(Interrupted(code).into());
        assert_eq!(primary(&outcome, 143), code);
    }
    assert_eq!(primary(&Err(CommandTimedOut.into()), 143), 124);
    assert_eq!(primary(&Ok(serde_json::json!({"passed":true})), 143), 143);
    assert_eq!(primary(&Ok(serde_json::json!({"passed":true})), 0), 0);
    assert_eq!(primary(&Err(anyhow::anyhow!("unknown")), 0), 1);
}

#[test]
fn changed_selected_executable_cannot_retain_positive_final_provenance() {
    let directory = Directory::new();
    let executable = directory.0.join("inert executable bytes");
    fs::write(&executable, b"before").unwrap();
    let arguments = [
        "--baseline-root",
        directory.0.to_str().unwrap(),
        "--baseline-bin",
        executable.to_str().unwrap(),
        "--candidate-root",
        directory.0.to_str().unwrap(),
        "--candidate-bin",
        executable.to_str().unwrap(),
        "--generator",
        executable.to_str().unwrap(),
        "--output",
        directory.0.join("output").to_str().unwrap(),
    ]
    .map(OsString::from);
    let options = crate::bench::options::parse(&arguments, &directory.0).unwrap();
    let inputs = files::Inputs::capture(&options, &executable).unwrap();
    fs::write(executable, b"after").unwrap();
    assert!(inputs.finish().is_err());
}

#[tokio::test]
async fn all_actual_retirement_facts_are_required_even_after_successful_stop() {
    for fake in [
        Fake {
            known: false,
            ..Fake::default()
        },
        Fake {
            group: false,
            ..Fake::default()
        },
        Fake {
            streams: false,
            ..Fake::default()
        },
    ] {
        let observed = ObservedHost::new(&fake);
        six(&observed);
        let mut child = observed.spawn(command()).unwrap();
        child.stop(Duration::ZERO).await.unwrap();
        assert!(!observed.receipt(ID, true, true).passed);
    }
}

#[tokio::test]
async fn auxiliary_admission_errors_unretired_children_and_earlier_stop_errors_are_sticky() {
    let fake = Fake {
        fail_spawn: true,
        ..Fake::default()
    };
    let observed = ObservedHost::new(&fake);
    six(&observed);
    assert!(observed.spawn(command()).is_err());
    assert!(!observed.receipt(ID, true, true).passed);
    let fake = Fake::default();
    let observed = ObservedHost::new(&fake);
    six(&observed);
    drop(observed.spawn(command()).unwrap());
    assert!(!observed.receipt(ID, true, true).passed);
    let fake = Fake {
        fail_stop: true,
        ..Fake::default()
    };
    let observed = ObservedHost::new(&fake);
    six(&observed);
    let mut child = observed.spawn(command()).unwrap();
    assert!(child.stop(Duration::ZERO).await.is_err());
    child.stop(Duration::ZERO).await.unwrap();
    assert!(!observed.receipt(ID, true, true).passed);
}

#[tokio::test]
async fn six_completed_runs_publish_only_after_positive_owned_retirement() {
    let fake = Fake::default();
    let observed = ObservedHost::new(&fake);
    let directory = Directory::new();
    observed.bind_output(&directory.0).unwrap();
    six(&observed);
    let mut child = observed.spawn(command()).unwrap();
    assert!(!observed.receipt(ID, true, true).passed);
    child.stop(Duration::ZERO).await.unwrap();
    assert!(!observed.receipt(ID, false, true).passed);
    assert!(!observed.receipt(ID, true, false).passed);
    let receipt = observed.receipt(ID, true, true);
    assert!(receipt.passed);
    observed.publish(&receipt).unwrap();
    let path = directory.0.join("comparison-owner.json");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    let bytes = fs::read(path).unwrap();
    assert!(bytes.len() <= 16 * 1024);
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"schemaVersion":1,"runId":ID,"completed":true,
        "passed":true,"runs":6,"groupCleanupPassed":true,"streamsClosed":true})
    );
    observed.completed_run();
    assert!(!observed.receipt(ID, true, true).passed);
}

#[test]
fn existing_or_redirected_receipts_and_replaced_output_directories_are_refused() {
    let directory = Directory::new();
    let outside = directory.0.join("outside");
    fs::write(&outside, b"keep existing bytes").unwrap();
    for linked in [false, true] {
        let leaf = directory.0.join(if linked { "linked" } else { "existing" });
        fs::DirBuilder::new().mode(0o700).create(&leaf).unwrap();
        if linked {
            symlink(&outside, leaf.join("comparison-owner.json")).unwrap();
        } else {
            fs::write(leaf.join("comparison-owner.json"), b"keep existing bytes").unwrap();
        }
        let output = files::Output::open(&leaf).unwrap();
        assert!(output.publish(&serde_json::json!({"passed":true})).is_err());
        assert_eq!(
            fs::read(leaf.join("comparison-owner.json")).unwrap(),
            b"keep existing bytes"
        );
    }
    let leaf = directory.0.join("original");
    fs::DirBuilder::new().mode(0o700).create(&leaf).unwrap();
    let output = files::Output::open(&leaf).unwrap();
    fs::rename(&leaf, directory.0.join("renamed")).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&leaf).unwrap();
    assert!(output.publish(&serde_json::json!({"passed":true})).is_err());
    assert!(!leaf.join("comparison-owner.json").exists());
    assert!(!directory.0.join("renamed/comparison-owner.json").exists());
    assert_eq!(fs::read(outside).unwrap(), b"keep existing bytes");
}
