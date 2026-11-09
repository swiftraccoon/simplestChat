//! Whole local transactions through the real opaque acquisition, original
//! files, frames, bounded pipe drainers and report fences. Remote observations
//! are finite injected data; host policy and Native process ownership have
//! separate actual-runtime tests. No selected executable runs here.

use super::{
    execute::{self, Inputs},
    options::Options,
    plan::{self, Clock, Plan},
    sources,
};
use crate::ci::{
    command::{Command, Program, ReleaseRole},
    process::Executor,
};
use simplestchat_ops::capacity_vps::protocol::{
    Action, Build, Collect, Envelope, Header, Inspection, Lifecycle, Lookup, Mode, Preflight,
    Recovery, Residual, Residuals, Response, ResultData, Retirement, RetirementPhase, Start, Unit,
    UnitState,
};
use simplestchat_toolkit::tooling::Result;
use std::{
    fs,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

type Acquisition = super::super::acquire::fixture::Fixture;
type Acquirer = super::super::acquire::fixture::Fake;
type TestClock = super::super::acquire::fixture::Clock;
impl plan::Clock for TestClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

struct Fixture {
    acquisition: Acquisition,
    caller: PathBuf,
    inventory: PathBuf,
    output: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let acquisition = Acquisition::new(0);
        let caller = acquisition.root.join("checkout");
        fs::create_dir(&caller).unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repository = repository.parent().unwrap();
        for name in sources::NAMES {
            let path = caller.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::copy(repository.join(name), path).unwrap();
        }
        fs::write(
            caller.join("Cargo.toml"),
            b"[workspace.package]\nversion = '0.1.0'\n",
        )
        .unwrap();
        fs::create_dir_all(caller.join(".git/refs/heads")).unwrap();
        fs::write(caller.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(
            caller.join(".git/refs/heads/main"),
            format!("{}\n", "a".repeat(40)),
        )
        .unwrap();
        fs::write(caller.join(".git/config"), b"[core]\n bare=false\n").unwrap();
        fs::write(acquisition.root.join("git"), b"#!/bin/sh\nexit 99\n").unwrap();
        fs::set_permissions(
            acquisition.root.join("git"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        for name in ["key", "known_hosts"] {
            fs::write(acquisition.root.join(name), b"INERT-NOT-A-CREDENTIAL\n").unwrap();
            fs::set_permissions(
                acquisition.root.join(name),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let inventory = acquisition.root.join("inventory.yml");
        fs::write(
            &inventory,
            format!(
                concat!(
                    "benchmark_hosts:\n  hosts:\n    target:\n",
                    "      ansible_host: inert.invalid\n      ansible_user: root\n",
                    "      ansible_ssh_private_key_file: {}\n",
                    "      scbench_ssh_known_hosts_file: {}\n",
                    "      scbench_revision: .nan\n"
                ),
                acquisition.root.join("key").display(),
                acquisition.root.join("known_hosts").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&inventory, fs::Permissions::from_mode(0o600)).unwrap();
        let output = acquisition.root.join("controller");
        Self {
            acquisition,
            caller,
            inventory,
            output,
        }
    }
    fn options(&self, flags: &[&str]) -> Options {
        let mut arguments =
            super::tests::args(&["--ops-run-attempt", "2", "--runtime-seconds", "120"]);
        arguments.extend([
            "--inventory".into(),
            self.inventory.to_str().unwrap().into(),
            "--limit".into(),
            "target".into(),
        ]);
        arguments.extend(flags.iter().map(|value| (*value).to_owned()));
        Options::decode(&arguments, &self.caller, &self.acquisition.root).unwrap()
    }
    async fn run(&self, options: Options, fake: &mut Remote) -> Result<()> {
        let clock: Arc<dyn Clock> = fake.acquirer.clock.clone();
        let start = clock.now();
        let plan = Plan::new(clock, &options, start);
        let budget = super::super::namespace::Budget::default();
        let result = execute::transaction(
            Inputs {
                options,
                caller: self.caller.clone(),
                home: self.acquisition.root.clone(),
                environment: self.acquisition.environment.clone(),
                run: "1".repeat(32),
                output: self.output.clone(),
            },
            budget.clone(),
            &plan,
            fake,
        )
        .await;
        if result.is_err() && fake.actions.is_empty() {
            let names = fs::read_dir(&self.output)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            eprintln!(
                concat!(
                    "inert controller stopped before transport: acquisition calls {}, ",
                    "evidence names {:?}"
                ),
                fake.acquirer.calls, names
            );
        }
        assert_eq!(budget.count(), 0, "all local descriptor permits retire");
        assert_eq!(
            budget.close_counts().1,
            0,
            "all owned namespace files receive an explicit close attempt"
        );
        result
    }
    fn report(&self) -> String {
        fs::read_to_string(self.output.join("report.json")).unwrap()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Success,
    QualityFailure,
    LostStartAck,
    RecoverClean,
    RecoverUnknown,
    MissingOutcome,
    UnitGc,
    BuildFailure,
    LostRetireAck,
}
struct Remote {
    acquirer: Acquirer,
    caller: PathBuf,
    output: PathBuf,
    inventory: PathBuf,
    scenario: Scenario,
    saved: Option<Vec<u8>>,
    actions: Vec<Action>,
    status_requests: Vec<Vec<u8>>,
    reports: Vec<Vec<u8>>,
    transfer_members: Vec<String>,
    altered_collection: bool,
    unavailable: bool,
    source_after_replay: bool,
    dirty_after_replay: bool,
    inventory_after_replay: bool,
    subordinate_start: Option<u8>,
    owner_start: Option<u8>,
    retired: bool,
    mutate_before_status: Option<&'static str>,
    malformed_status: u8,
    damaged_archive: bool,
    incomplete_input: bool,
    missing_exit: bool,
    missing_capture: bool,
    unsettled_exit: bool,
    replay_code: Option<u8>,
    cleanup_after_replay: bool,
}
impl Remote {
    fn new(fixture: &Fixture, scenario: Scenario) -> Self {
        Self {
            acquirer: Acquirer::new(&fixture.acquisition),
            caller: fixture.caller.clone(),
            output: fixture.output.clone(),
            inventory: fixture.inventory.clone(),
            scenario,
            saved: None,
            actions: Vec::new(),
            status_requests: Vec::new(),
            reports: Vec::new(),
            transfer_members: Vec::new(),
            altered_collection: false,
            unavailable: false,
            source_after_replay: false,
            dirty_after_replay: false,
            inventory_after_replay: false,
            subordinate_start: None,
            owner_start: None,
            retired: false,
            mutate_before_status: None,
            malformed_status: 0,
            damaged_archive: false,
            incomplete_input: false,
            missing_exit: false,
            missing_capture: false,
            unsettled_exit: false,
            replay_code: None,
            cleanup_after_replay: false,
        }
    }
    fn terminal(&self, header: &Header, unit: Unit) -> UnitState {
        let failed = matches!(
            self.scenario,
            Scenario::QualityFailure
                | Scenario::RecoverClean
                | Scenario::RecoverUnknown
                | Scenario::MissingOutcome
        ) || (unit == Unit::Build && self.scenario == Scenario::BuildFailure);
        UnitState {
            load_state: "loaded".into(),
            active_state: if failed { "failed" } else { "inactive" }.into(),
            main_pid: "0".into(),
            sub_state: Some(if failed { "failed" } else { "dead" }.into()),
            result: Some(if failed { "exit-code" } else { "success" }.into()),
            exec_main_code: Some("1".into()),
            exec_main_status: Some(if failed { "1" } else { "0" }.into()),
            unit: Some(if unit == Unit::Capacity {
                header.capacity_unit()
            } else {
                "simplestchat-image-build.service".into()
            }),
            retained: (unit == Unit::Capacity && self.scenario == Scenario::UnitGc).then_some(true),
        }
    }
    fn residuals(&self, before: bool) -> Residuals {
        let mut residuals = Vec::new();
        if before
            && matches!(
                self.scenario,
                Scenario::RecoverClean | Scenario::RecoverUnknown
            )
        {
            residuals.push(residual('b', true));
        }
        if self.scenario == Scenario::RecoverUnknown {
            residuals.push(residual('c', false));
        }
        Residuals {
            containers_before: Vec::new(),
            containers_after: residuals.iter().map(|v: &Residual| v.id.clone()).collect(),
            clean: residuals.is_empty(),
            residuals,
        }
    }
    fn outcome(&self) -> Vec<u8> {
        if self.scenario == Scenario::MissingOutcome {
            return b"{\"passed\":false,\"finalized\":false,\
                     \"failureClass\":\"worker_outcome_missing\"}"
                .to_vec();
        }
        let passed = !matches!(
            self.scenario,
            Scenario::QualityFailure | Scenario::RecoverClean | Scenario::RecoverUnknown
        );
        let finalized = !matches!(
            self.scenario,
            Scenario::RecoverClean | Scenario::RecoverUnknown
        );
        let cleanup = serde_json::to_string(&self.residuals(false)).unwrap();
        let revision = "a".repeat(40);
        let status = i32::from(!passed);
        let passing = if passed {
            let workload = simplestchat_ops::capacity_vps::protocol::Workload::decode(
                self.saved.as_ref().unwrap(),
            )
            .unwrap();
            format!(
                r#"[{{"workload":"{}","size":30,"valid":true,"passed":true}}]"#,
                workload.workload()
            )
        } else {
            "[]".into()
        };
        format!(
            "{{\"schemaVersion\":1,\"revision\":\"{revision}\",\"passed\":{passed},\
             \"finalized\":{finalized},\"exitStatus\":{status},\"passingSteps\":{passing},\
             \"failedSteps\":[],\"cleanup\":{cleanup},\"opaque\":NaN,\
             \"large\":9007199254740993,\"signedZero\":-0,\"decimal\":1.00}}"
        )
        .into_bytes()
    }
    fn frame(&mut self, command: &mut Command) -> (Header, Envelope) {
        let input = Arc::try_unwrap(command.stdin.take().unwrap()).unwrap();
        let mut bytes = Vec::new();
        (&input).read_to_end(&mut bytes).unwrap();
        if self.incomplete_input {
            (&input).seek(SeekFrom::End(-1)).unwrap();
        }
        nix::unistd::close(input).unwrap();
        let newline = bytes.iter().position(|v| *v == b'\n').unwrap();
        let header = Header::decode(&bytes[..newline]).unwrap();
        let mut offset = newline + 1;
        if header.mode == Mode::Stage {
            let end = offset + usize::try_from(header.transfer.bytes).unwrap();
            let transfer = &bytes[offset..end];
            assert_eq!(super::super::sha(transfer), header.transfer.sha256);
            let mut archive = tar::Archive::new(Cursor::new(transfer));
            let expected = [
                ("ops-release.json", &header.members.manifest),
                ("ops-selection.json", &header.members.selection),
                ("simplestchat-ops", &header.members.binary),
            ];
            let mut entries = archive.entries().unwrap();
            for (name, pin) in expected {
                let mut entry = entries.next().unwrap().unwrap();
                assert_eq!(entry.path().unwrap().to_str().unwrap(), name);
                assert_eq!(entry.header().mode().unwrap(), 0o600);
                let mut member = Vec::new();
                entry.read_to_end(&mut member).unwrap();
                assert_eq!(member.len() as u64, pin.bytes);
                assert_eq!(super::super::sha(&member), pin.sha256);
                self.transfer_members.push(name.into());
            }
            assert!(entries.next().is_none());
            offset = end;
        }
        let envelope = &bytes[offset..];
        assert_eq!(envelope.len() as u64, header.request.bytes);
        assert_eq!(super::super::sha(envelope), header.request.sha256);
        let envelope = Envelope::decode(envelope).unwrap();
        envelope.bind(&header).unwrap();
        (header, envelope)
    }
    fn response(&mut self, header: &Header, envelope: &Envelope) -> (Vec<u8>, Vec<u8>, i32) {
        let action = envelope.action();
        self.actions.push(action);
        if action == Action::Lookup && header.mode == Mode::Invoke && self.unavailable {
            return (Vec::new(), b"ops_capacity_unavailable\n".to_vec(), 1);
        }
        if action == Action::Start {
            let intent: serde_json::Value = serde_json::from_slice(
                &fs::read(self.output.join("start-requested.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(intent["remoteOutcome"], "start_requested");
            self.saved = Some(envelope.original_request().unwrap().to_vec());
            if let Some(code) = self.owner_start {
                self.acquirer.cancelled.store(code, Ordering::SeqCst);
            }
            if self.scenario == Scenario::LostStartAck || self.subordinate_start.is_some() {
                return (
                    Vec::new(),
                    b"ops_capacity_io\n".to_vec(),
                    i32::from(self.subordinate_start.unwrap_or(1)),
                );
            }
        }
        if action == Action::Archive {
            let mut bytes = gzip(&self.saved.clone().unwrap(), &self.outcome());
            if self.damaged_archive {
                bytes.push(1);
            }
            return (bytes, Vec::new(), 0);
        }
        if action == Action::Retire
            && matches!(
                self.scenario,
                Scenario::RecoverUnknown | Scenario::MissingOutcome
            )
        {
            return (Vec::new(), b"ops_capacity_retirement\n".to_vec(), 1);
        }
        if action == Action::Retire {
            self.retired = true;
            if self.scenario == Scenario::LostRetireAck {
                return (Vec::new(), b"ops_capacity_io\n".to_vec(), 1);
            }
        }
        let result = match action {
            Action::Preflight => ResultData::Preflight(Preflight {
                containers_before: Vec::new(),
                private_host: true,
            }),
            Action::Build => ResultData::Build(Build {
                unit: "simplestchat-image-build.service".into(),
            }),
            Action::Start => ResultData::Start(Start {
                unit: header.capacity_unit(),
                evidence: header.evidence_path(),
                run: header.run.clone(),
            }),
            Action::Status => {
                self.status_requests
                    .push(envelope.original_request().unwrap().to_vec());
                ResultData::Status(self.terminal(header, envelope.unit().unwrap()))
            }
            Action::Lookup => {
                let saved = self.saved.clone().unwrap();
                ResultData::Lookup(Lookup {
                    request_json: String::from_utf8(saved.clone()).unwrap(),
                    request_sha256: super::super::sha(&saved),
                    unit: self.terminal(header, Unit::Capacity),
                })
            }
            Action::Collect => {
                let mut saved = self.saved.clone().unwrap();
                if self.altered_collection {
                    saved.insert(0, b' ');
                }
                ResultData::Collect(Collect {
                    passed: matches!(
                        self.scenario,
                        Scenario::Success | Scenario::UnitGc | Scenario::LostRetireAck
                    ),
                    unit: self.terminal(header, Unit::Capacity),
                    outcome_json: String::from_utf8(self.outcome()).unwrap(),
                    cleanup: self.residuals(false),
                    evidence: header.evidence_path(),
                    revision: header.selection.revision.clone(),
                    request_json: String::from_utf8(saved).unwrap(),
                })
            }
            Action::Recover => ResultData::Recover(Recovery {
                run: header.run.clone(),
                removed: vec!["b".repeat(64)],
                finalized: self.scenario == Scenario::RecoverClean,
                before: Some(self.residuals(true)),
                after: Some(self.residuals(false)),
            }),
            Action::Retire => ResultData::Retire(Retirement {
                retired: true,
                lifecycle: RetirementPhase::Terminal,
                request_sha256: super::super::sha(self.saved.as_ref().unwrap()),
            }),
            _ => panic!("unadmitted action in local fixture"),
        };
        let mut bytes = Response::encode(header, &result).unwrap();
        if action == Action::Status {
            if self.malformed_status == 1 {
                bytes.extend_from_slice(b"\n");
            }
            if self.malformed_status == 2 {
                bytes.resize(super::MAX_STATUS as usize + 1, b' ');
            }
        }
        (bytes, Vec::new(), 0)
    }
}
fn residual(digit: char, owned: bool) -> Residual {
    let id = digit.to_string().repeat(64);
    Residual {
        id: id.clone(),
        confirmed_owned: owned,
        inspection: Inspection {
            id,
            name: format!("/inert-{digit}"),
            image: Some(format!("sha256:{}", "d".repeat(64))),
            requested_image: Some("inert-image".into()),
            state: Lifecycle {
                status: Some("running".into()),
                running: Some(true),
                exit_code: Some(0),
                oom_killed: Some(false),
                started_at: None,
                finished_at: None,
            },
        },
    }
}
fn gzip(request: &[u8], outcome: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (name, body) in [("request.json", request), ("outcome.json", outcome)] {
        let mut header = tar::Header::new_ustar();
        header.set_size(body.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        archive
            .append_data(
                &mut header,
                format!("capacity-controller.{}/{name}", "1".repeat(32)),
                body,
            )
            .unwrap();
    }
    archive.finish().unwrap();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&archive.into_inner().unwrap()).unwrap();
    gzip.finish().unwrap()
}

impl Executor for Remote {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        if command.program == Program::OpsGh {
            return self.acquirer.run(command).await;
        }
        let proof = command
            .release
            .clone()
            .filter(|proof| proof.role() == ReleaseRole::Vps)
            .unwrap();
        if command.program == Program::OpsVpsSsh
            && self.actions.last() == Some(&Action::Start)
            && let Some(name) = self.mutate_before_status.take()
        {
            fs::write(
                self.output.join(name),
                b"changed original artifact before next effect\n",
            )
            .unwrap();
        }
        if let Err(error) = proof.begin(&command) {
            let _ = crate::ci::process::settle_unforwarded(command).await;
            return Err(error);
        }
        let (stdout, stderr, status) = match command.program {
            Program::OpsVpsGit => {
                let args = command
                    .arguments
                    .iter()
                    .map(|v| v.to_str().unwrap())
                    .collect::<Vec<_>>();
                let bytes = if args.ends_with(&["rev-parse", "HEAD"]) {
                    format!("{}\n", "a".repeat(40)).into_bytes()
                } else if args.ends_with(&["rev-parse", "--show-toplevel"]) {
                    format!("{}\n", self.caller.display()).into_bytes()
                } else if args.contains(&"status") {
                    if self.caller.join("untracked-after-replay").exists() {
                        b"?? untracked-after-replay\n".to_vec()
                    } else {
                        Vec::new()
                    }
                } else {
                    let name = args.last().unwrap().strip_prefix("HEAD:").unwrap();
                    assert!(sources::NAMES.contains(&name));
                    fs::read(self.caller.join(name)).unwrap()
                };
                (bytes, Vec::new(), 0)
            }
            Program::OpsVpsSsh => {
                let (header, envelope) = self.frame(&mut command);
                self.response(&header, &envelope)
            }
            _ => panic!("selected executable is never run by this fixture"),
        };
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
        let mut out = logs.stdout.writer()?;
        let mut err = logs.stderr.writer()?;
        let writer = tokio::task::spawn_blocking(move || {
            let written = out.write_all(&stdout).and_then(|()| err.write_all(&stderr));
            let out_closed = nix::unistd::close(out);
            let err_closed = nix::unistd::close(err);
            written.map_err(|_| super::failure(1))?;
            out_closed.and(err_closed).map_err(|_| super::failure(1))
        });
        let (written, out, err) = tokio::join!(writer, logs.stdout.finish(), logs.stderr.finish());
        let seal = |value: crate::ci::bounded_log::Finished| crate::ci::bounded_log::Seal {
            sha256: value.sha256,
            bytes: value.bytes,
            metadata: value.metadata,
        };
        let captures = match (out, err) {
            (Ok(out), Ok(err)) if out.complete && err.complete => Some(super::tools::Captures {
                stdout: seal(out),
                stderr: seal(err),
            }),
            _ => None,
        };
        let complete = captures.is_some();
        let remote = command.program == Program::OpsVpsSsh;
        if !self.missing_capture || !remote {
            proof.observed_io(captures);
        }
        if !self.missing_exit || !remote {
            proof.observed_exit(Some(status), !self.unsettled_exit || !remote, true);
        }
        written.map_err(|_| super::failure(1))??;
        if !complete {
            return Err(super::failure(1));
        }
        if status != 0 {
            return Err(super::failure(u8::try_from(status).unwrap_or(1)));
        }
        let invocation: &dyn std::any::Any = &*proof;
        invocation
            .downcast_ref::<super::tools::Invocation>()
            .unwrap()
            .settled()?;
        Ok(Vec::new())
    }
    async fn ready(&mut self) -> Result<()> {
        self.acquirer.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.acquirer.cleanup_passed()
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, end: Instant) -> Result<()> {
        if bytes
            .windows(b"remoteOutcome".len())
            .any(|v| v == b"remoteOutcome")
        {
            self.reports.push(bytes.to_vec());
            if self.source_after_replay {
                fs::write(
                    self.caller.join("build/capacity.py"),
                    b"changed source after replay\n",
                )
                .unwrap();
            }
            if self.dirty_after_replay {
                fs::write(
                    self.caller.join("untracked-after-replay"),
                    b"changed Git state\n",
                )
                .unwrap();
            }
            if self.inventory_after_replay {
                fs::write(&self.inventory, b"changed original inventory\n").unwrap();
            }
            if self.cleanup_after_replay {
                self.acquirer.cleanup = false;
            }
            self.replay_code
                .map_or(Ok(()), |code| Err(super::failure(code)))
        } else {
            self.acquirer.replay(bytes, stderr, end).await
        }
    }
}

#[tokio::test]
async fn whole_success_keeps_original_private_numbers_and_validates_archive_before_retirement() {
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::Success);
    fixture
        .run(fixture.options(&[]), &mut remote)
        .await
        .unwrap();
    assert_eq!(
        remote.actions,
        vec![
            Action::Preflight,
            Action::Start,
            Action::Status,
            Action::Collect,
            Action::Archive,
            Action::Retire
        ]
    );
    assert_eq!(remote.acquirer.calls, 9);
    assert!(remote.retired);
    assert_eq!(remote.reports.len(), 1);
    assert_eq!(
        remote.transfer_members,
        vec!["ops-release.json", "ops-selection.json", "simplestchat-ops"]
    );
    let report = fixture.report();
    for token in ["NaN", "9007199254740993", "-0", "1.00", "collected"] {
        assert!(report.contains(token));
    }
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.output.join("inventory.snapshot.json")).unwrap())
            .unwrap();
    assert_eq!(
        snapshot["benchmark_hosts"]["hosts"]["target"]["scbench_revision"],
        "a".repeat(40)
    );
    assert!(
        fs::read_to_string(&fixture.inventory)
            .unwrap()
            .contains(".nan")
    );
}
#[tokio::test]
async fn quality_failure_archives_and_retires_but_never_becomes_a_passing_measurement() {
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::QualityFailure);
    assert_eq!(
        fixture
            .run(fixture.options(&[]), &mut remote)
            .await
            .unwrap_err()
            .code,
        1
    );
    assert!(remote.retired);
    assert!(remote.actions.ends_with(&[Action::Archive, Action::Retire]));
    assert!(fixture.report().contains("collected"));
}
#[tokio::test]
async fn lost_start_ack_keeps_durable_uncertainty_and_never_retries_or_retires() {
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::LostStartAck);
    assert_eq!(
        fixture
            .run(fixture.options(&[]), &mut remote)
            .await
            .unwrap_err()
            .code,
        1
    );
    assert_eq!(remote.actions, vec![Action::Preflight, Action::Start]);
    assert!(!remote.retired);
    assert!(fixture.report().contains("start_requested"));
    assert!(remote.saved.is_some());
}
#[tokio::test]
async fn explicit_recovery_adopts_saved_runtime_and_preserves_unknown_or_clean_failure() {
    for scenario in [Scenario::RecoverClean, Scenario::RecoverUnknown] {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, scenario);
        let saved = fixture.options(&[]).request(&"1".repeat(32)).unwrap();
        remote.saved = Some(saved.clone());
        remote.unavailable = true;
        let selected =
            fixture.options(&["--recover", &"1".repeat(32), "--runtime-seconds", "7200"]);
        assert_eq!(
            fixture.run(selected, &mut remote).await.unwrap_err().code,
            1
        );
        assert_eq!(
            remote.actions,
            vec![
                Action::Lookup,
                Action::Lookup,
                Action::Status,
                Action::Recover,
                Action::Collect,
                Action::Archive,
                Action::Retire
            ]
        );
        assert_eq!(remote.status_requests, vec![saved]);
        assert_eq!(remote.retired, scenario == Scenario::RecoverClean);
        assert!(fixture.report().contains("collected"));
    }
}
#[tokio::test]
async fn missing_outcome_and_lost_retirement_ack_keep_failure_evidence() {
    for scenario in [Scenario::MissingOutcome, Scenario::LostRetireAck] {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, scenario);
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            1
        );
        assert!(remote.actions.ends_with(&[Action::Archive, Action::Retire]));
        assert_eq!(remote.retired, scenario == Scenario::LostRetireAck);
        assert!(fixture.report().contains("collected"));
    }
}
#[tokio::test]
async fn retained_terminal_unit_can_collect_without_starting_again() {
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::UnitGc);
    remote.saved = Some(fixture.options(&[]).request(&"1".repeat(32)).unwrap());
    fixture
        .run(
            fixture.options(&["--collect", &"1".repeat(32)]),
            &mut remote,
        )
        .await
        .unwrap();
    assert_eq!(
        remote.actions,
        vec![
            Action::Lookup,
            Action::Status,
            Action::Collect,
            Action::Archive,
            Action::Retire
        ]
    );
    assert!(!fixture.output.join("start-requested.json").exists());
}
#[tokio::test]
async fn failed_build_never_starts_a_workload_and_equal_semantics_do_not_replace_original_bytes() {
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::BuildFailure);
    assert_eq!(
        fixture
            .run(fixture.options(&["--build-images"]), &mut remote)
            .await
            .unwrap_err()
            .code,
        1
    );
    assert_eq!(
        remote.actions,
        vec![Action::Preflight, Action::Build, Action::Status]
    );
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::Success);
    remote.altered_collection = true;
    assert_eq!(
        fixture
            .run(fixture.options(&[]), &mut remote)
            .await
            .unwrap_err()
            .code,
        1
    );
    assert!(!remote.actions.contains(&Action::Archive));
    assert!(!remote.retired);
}
#[tokio::test]
async fn replay_cannot_hide_changed_source_inventory_or_untracked_git_content() {
    for field in 0..3 {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, Scenario::Success);
        remote.source_after_replay = field == 0;
        remote.inventory_after_replay = field == 1;
        remote.dirty_after_replay = field == 2;
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            1
        );
        assert_eq!(remote.reports.len(), 1);
        assert!(remote.retired);
    }
}
#[tokio::test]
async fn subordinate_signal_like_exit_is_not_an_owner_cancellation() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, Scenario::Success);
        remote.subordinate_start = Some(code);
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            1
        );
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, Scenario::Success);
        remote.owner_start = Some(code);
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            code
        );
    }
}

#[tokio::test]
async fn every_later_dispatch_retains_opaque_acquisition_and_original_transfer_witnesses() {
    for name in ["acquired/simplestchat-ops", "transfer.tar"] {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, Scenario::Success);
        remote.mutate_before_status = Some(name);
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            1
        );
        assert_eq!(remote.actions, vec![Action::Preflight, Action::Start]);
        assert!(!remote.retired);
    }
}

#[tokio::test]
async fn lookup_and_reports_preserve_original_private_workload_bytes_and_numeric_tokens() {
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::Success);
    let mut saved = fixture.options(&[]).request(&"1".repeat(32)).unwrap();
    saved.pop();
    saved.extend_from_slice(
        br#", "private":Infinity,"large":9007199254740993,"signedZero":-0,"decimal":1.00}  "#,
    );
    let original = saved.clone();
    remote.saved = Some(saved);
    fixture
        .run(
            fixture.options(&["--collect", &"1".repeat(32)]),
            &mut remote,
        )
        .await
        .unwrap();
    assert_eq!(remote.status_requests, vec![original]);
    let report = fixture.report();
    for token in ["Infinity", "9007199254740993", "-0", "1.00"] {
        assert!(report.contains(token));
    }
}

#[tokio::test]
async fn malformed_or_overflowing_status_and_damaged_archive_cannot_authorize_retirement() {
    for fault in 1..=3 {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, Scenario::Success);
        remote.malformed_status = if fault < 3 { fault } else { 0 };
        remote.damaged_archive = fault == 3;
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            1
        );
        assert!(!remote.retired);
        assert!(!remote.actions.contains(&Action::Retire));
        if fault < 3 {
            let path = fixture
                .output
                .join(format!("process-{}.exit.json", sources::NAMES.len() + 6));
            let exit: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            assert_eq!(exit["status"], 0);
            assert_eq!(exit["settled"], true);
        }
    }
}

#[tokio::test]
async fn a_valid_response_and_sealed_output_do_not_replace_original_frame_consumption() {
    let fixture = Fixture::new();
    let mut remote = Remote::new(&fixture, Scenario::Success);
    remote.incomplete_input = true;
    assert_eq!(
        fixture
            .run(fixture.options(&[]), &mut remote)
            .await
            .unwrap_err()
            .code,
        1
    );
    assert_eq!(remote.actions, vec![Action::Preflight]);
    assert!(!remote.retired);
}

#[tokio::test]
async fn successful_return_and_json_never_replace_status_capture_or_peer_settlement() {
    for fault in 0..3 {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, Scenario::Success);
        remote.missing_exit = fault == 0;
        remote.missing_capture = fault == 1;
        remote.unsettled_exit = fault == 2;
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            1
        );
        assert_eq!(remote.actions, vec![Action::Preflight]);
        assert!(!remote.retired);
    }
}

#[tokio::test]
async fn late_replay_and_unsettled_executor_cleanup_still_retire_all_local_file_peers() {
    for fault in 0..2 {
        let fixture = Fixture::new();
        let mut remote = Remote::new(&fixture, Scenario::Success);
        remote.replay_code = (fault == 0).then_some(143);
        remote.cleanup_after_replay = fault == 1;
        assert_eq!(
            fixture
                .run(fixture.options(&[]), &mut remote)
                .await
                .unwrap_err()
                .code,
            if fault == 0 { 143 } else { 1 }
        );
        assert!(remote.retired);
        assert_eq!(remote.reports.len(), 1);
    }
}
