use super::*;
use super::{
    policy::{Observation, interfaces},
    tools::{Paths, Role},
};
use crate::{
    ci::{command::Command, process::Executor},
    tooling::{Environment, Result},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

pub(super) struct TestClock(pub Arc<AtomicU64>);
impl Clock for TestClock {
    fn elapsed(&self) -> f64 {
        self.0.load(Ordering::SeqCst) as f64 / 1000.0
    }
    fn utc(&self) -> String {
        "2026-10-22T00:00:00.000Z".into()
    }
}
pub(super) struct Fixture {
    pub root: PathBuf,
    pub config: config::Config,
    pub paths: Paths,
    pub clock: TestClock,
}
impl Fixture {
    pub fn new(mode: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("mdns-fixture.{}", files::uuid().unwrap()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("build")).unwrap();
        fs::write(
            root.join("build/test-mdns.c"),
            b"/* inert source, never compiled */\n",
        )
        .unwrap();
        let tool = root.join("fake-tool");
        fs::write(&tool, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        let environment = Environment::from([
            ("MDNS_DIAGNOSTIC_E2E".into(), "1".into()),
            ("TEST_ANNOUNCE_IP".into(), "192.0.2.1".into()),
            (
                "E2E_ARTIFACTS".into(),
                root.join("artifacts").into_os_string(),
            ),
            ("TMPDIR".into(), root.clone().into_os_string()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        let config = config::Config::read(
            &root,
            &root,
            &[mode.into(), "--artifact-only".into()],
            &environment,
            "macos",
        )
        .unwrap();
        let paths = Paths(BTreeMap::from(
            [
                Role::Interfaces,
                Role::Plist,
                Role::Launch,
                Role::Logs,
                Role::Compiler,
            ]
            .map(|r| (r, tool.clone())),
        ));
        Self {
            root,
            config,
            paths,
            clock: TestClock(Arc::default()),
        }
    }
    pub fn executor(&self) -> Fake {
        Fake {
            clock: self.clock.0.clone(),
            ..Fake::default()
        }
    }
    pub fn report(&self) -> Value {
        serde_json::from_slice(
            &fs::read(
                self.config
                    .output
                    .join(format!("mdns-{}-results.json", self.config.phase)),
            )
            .unwrap(),
        )
        .unwrap()
    }
    pub fn no_temporary(&self) {
        assert!(!fs::read_dir(&self.root).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("simplestchat-mdns.")
        }));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

pub(super) struct Fake {
    pub cases: BTreeMap<Role, (Vec<u8>, u8, bool)>,
    pub calls: Vec<Role>,
    pub clock: Arc<AtomicU64>,
    pub advance: BTreeMap<Role, u64>,
    pub close_failed: Option<Role>,
    pub cancel: u8,
    pub replay: Vec<u8>,
    pub arguments: Vec<Vec<String>>,
    pub timeouts: Vec<u64>,
    pub ready_count: u32,
    pub ready_jump: Option<(u32, u64)>,
    pub cancel_at: Option<(u32, u8)>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            cases: BTreeMap::from([
                (
                    Role::Interfaces,
                    (
                        concat!(
                            "lo0: flags=49<UP,LOOPBACK,RUNNING> mtu 16384\n",
                            "\tinet 127.0.0.1 netmask 0xff000000\n",
                            "en0: flags=41<UP,RUNNING> mtu 1500\n",
                            "\tinet 192.0.2.1 netmask 0xffffff00\n"
                        )
                        .as_bytes()
                        .to_vec(),
                        0,
                        false,
                    ),
                ),
                (
                    Role::Plist,
                    (b"com.apple.mDNSResponder.reloaded\n".to_vec(), 0, false),
                ),
                (Role::Launch, (b"\tstate = running\n".to_vec(), 0, false)),
                (Role::Logs, (vec![], 0, false)),
                (Role::Compiler, (vec![], 0, false)),
                (
                    Role::Probe,
                    (
                        serde_json::to_vec(&json!({"stage":"callback","error":0,
        "callbackObserved":true,"timedOut":false,"deallocated":true}))
                        .unwrap(),
                        0,
                        false,
                    ),
                ),
            ]),
            calls: vec![],
            clock: Arc::default(),
            advance: BTreeMap::new(),
            close_failed: None,
            cancel: 0,
            replay: vec![],
            arguments: vec![],
            timeouts: vec![],
            ready_count: 0,
            ready_jump: None,
            cancel_at: None,
        }
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let role = [
            Role::Interfaces,
            Role::Plist,
            Role::Launch,
            Role::Logs,
            Role::Compiler,
            Role::Probe,
        ]
        .into_iter()
        .find(|r| r.program() == command.program)
        .unwrap();
        let proof = command.mdns.take().unwrap();
        proof.begin()?;
        proof.validate(command.program)?;
        self.calls.push(role);
        self.timeouts
            .push(command.timeout.as_millis().try_into().unwrap());
        self.arguments.push(
            command
                .arguments
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect(),
        );
        let (bytes, code, timeout) = self.cases[&role].clone();
        if role == Role::Compiler && code == 0 {
            let path = PathBuf::from(command.arguments.last().unwrap());
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
        let mut writer = logs.stdout.writer()?;
        writer.write_all(&bytes).unwrap();
        files::close(writer)?;
        let mut errors = logs.stderr.writer()?;
        errors.write_all(b"PRIVATE_HOST_PATH_ADDRESS\n").unwrap();
        files::close(errors)?;
        let (a, b) = tokio::join!(logs.stdout.finish(), logs.stderr.finish());
        let complete = a.is_ok() && b.is_ok() && self.close_failed != Some(role);
        proof.outcome(
            if timeout { None } else { Some(code.into()) },
            timeout,
            None,
            true,
        );
        proof.closed(complete);
        self.clock
            .fetch_add(*self.advance.get(&role).unwrap_or(&0), Ordering::SeqCst);
        if !complete {
            return Err(failure(1));
        }
        if timeout {
            return Err(failure(124));
        }
        if code != 0 {
            return Err(failure(code));
        }
        Ok(vec![])
    }
    async fn ready(&mut self) -> Result<()> {
        self.ready_count += 1;
        if let Some((count, code)) = self.cancel_at
            && self.ready_count == count
        {
            self.cancel = code;
        }
        if let Some((count, elapsed)) = self.ready_jump
            && self.ready_count == count
        {
            self.clock.store(elapsed, Ordering::SeqCst);
        }
        if self.cancel == 0 {
            Ok(())
        } else {
            Err(failure(self.cancel))
        }
    }
    async fn replay(&mut self, bytes: &[u8], _: bool, _: Instant) -> Result<()> {
        self.replay = bytes.to_vec();
        Ok(())
    }
}

#[tokio::test]
async fn complete_both_modes_publish_only_sanitized_observations_after_cleanup() {
    for mode in ["preflight", "snapshot"] {
        let f = Fixture::new(mode);
        let mut e = f.executor();
        execute::run(&f.config, &mut e, &f.clock, &f.paths)
            .await
            .unwrap();
        let report = f.report();
        assert_eq!(report["passed"], true);
        assert!(!report.to_string().contains("192.0.2.1"));
        assert!(!report.to_string().contains("PRIVATE"));
        assert_eq!(
            e.calls,
            if mode == "preflight" {
                vec![
                    Role::Interfaces,
                    Role::Plist,
                    Role::Launch,
                    Role::Logs,
                    Role::Compiler,
                    Role::Probe,
                ]
            } else {
                vec![Role::Interfaces, Role::Plist, Role::Launch, Role::Logs]
            }
        );
        f.no_temporary();
        assert_eq!(
            fs::metadata(f.config.output.join(format!("mdns-{mode}-results.json")))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }
}
#[test]
fn interface_counts_match_live_node_records_and_libuv_flags_not_ip_ranges() {
    let zero_then_owned = concat!(
        "stf0: flags=0<> mtu 1280\n",
        "en0: flags=41<UP,RUNNING> mtu 1500\n inet 192.0.2.1 netmask x\n"
    );
    assert_eq!(
        interfaces("192.0.2.1", zero_then_owned.as_bytes()).unwrap(),
        json!({
        "ipv4Addresses":1,"nonLoopbackIpv4Addresses":1,"selectedAddressAssigned":true})
    );
    for tail in ["", " metric 7 mtu 1500", " constrained"] {
        let input = format!("en0: flags=41<UP,RUNNING>{tail}\n inet 192.0.2.1 netmask x\n");
        assert_eq!(
            interfaces("192.0.2.1", input.as_bytes()).unwrap()["ipv4Addresses"],
            1
        );
    }
    let input = concat!(
        "lo0: flags=49<UP,LOOPBACK,RUNNING> mtu 16000\n",
        " inet 192.0.2.1 netmask x\nen0: flags=41<UP,RUNNING> mtu 1500\n",
        " inet 127.0.0.1 netmask x\n inet 127.0.0.1 netmask x\n",
        "en1: flags=1<UP> mtu 1500\n inet 192.0.2.2 netmask x\n",
        "en2: flags=40<RUNNING> mtu 1500\n inet 192.0.2.3 netmask x\n"
    )
    .as_bytes();
    assert_eq!(
        interfaces("127.0.0.1", input).unwrap(),
        json!({"ipv4Addresses":3,
        "nonLoopbackIpv4Addresses":2,"selectedAddressAssigned":true})
    );
    for address in ["192.0.2.1", "192.0.2.2", "192.0.2.3"] {
        assert!(interfaces(address, input).is_err());
    }
    for input in [
        b" inet 192.0.2.1\n".as_slice(),
        b"en0: flags=0<> mtu 1500\n inet 192.0.2.1\n",
    ] {
        assert!(interfaces("192.0.2.1", input).is_err());
    }
}
#[tokio::test]
async fn failed_reads_and_unknown_labels_skip_only_launch_and_preserve_probe_order() {
    for label in ["foreign PRIVATE", "com.apple.mDNSResponder"] {
        let f = Fixture::new("preflight");
        let mut e = f.executor();
        e.cases.insert(
            Role::Plist,
            (
                label.as_bytes().to_vec(),
                if label.starts_with("foreign") { 0 } else { 1 },
                false,
            ),
        );
        assert_eq!(
            execute::run(&f.config, &mut e, &f.clock, &f.paths)
                .await
                .err()
                .unwrap()
                .code,
            1
        );
        assert!(!e.calls.contains(&Role::Launch));
        assert!(e.calls.contains(&Role::Probe));
        assert_eq!(f.report()["system"]["responder"]["readable"], false);
        f.no_temporary();
    }
}
#[tokio::test]
async fn actual_exit124_is_not_a_timeout_and_cleanup_uncertainty_cannot_pass() {
    for (code, timed_out, closed, expected) in [
        (124, false, true, 1),
        (0, true, true, 124),
        (0, false, false, 1),
    ] {
        let f = Fixture::new("preflight");
        let mut e = f.executor();
        e.cases
            .insert(Role::Probe, (b"{}".to_vec(), code, timed_out));
        if !closed {
            e.close_failed = Some(Role::Probe);
        }
        assert_eq!(
            execute::run(&f.config, &mut e, &f.clock, &f.paths)
                .await
                .err()
                .unwrap()
                .code,
            expected
        );
        assert_eq!(f.report()["passed"], false);
        if !timed_out && closed {
            assert_eq!(f.report()["native"]["timedOut"], false);
        }
        f.no_temporary();
    }
}
#[tokio::test]
async fn remaining_body_admission_refuses_before_next_spawn_and_keeps_prior_failure() {
    for code in [0, 42] {
        let f = Fixture::new("preflight");
        let mut e = f.executor();
        e.cases.get_mut(&Role::Plist).unwrap().1 = code;
        e.advance.insert(Role::Logs, 25000);
        let error = execute::run(&f.config, &mut e, &f.clock, &f.paths)
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, if code == 0 { 124 } else { 1 });
        assert!(!e.calls.contains(&Role::Compiler));
        assert_eq!(f.report()["passed"], false);
        f.no_temporary();
    }
}
#[test]
fn native_projection_requires_explicit_typed_evidence_and_omits_unknowns() {
    for value in [
        json!({"stage":"callback","error":0.0,"callbackObserved":true,
        "timedOut":false,"deallocated":true,"PRIVATE":"SECRET"}),
        json!({"stage":"callback","error":0,"callbackObserved":true,"timedOut":false,
            "deallocated":true}),
    ] {
        let observation = Observation {
            stdout: serde_json::to_vec(&value).unwrap(),
            exit: Some(0),
            timed_out: false,
        };
        assert_eq!(policy::probe(&observation)["passed"], true);
        assert!(!policy::probe(&observation).to_string().contains("PRIVATE"));
    }
}

#[test]
fn unadmitted_temporary_leaf_is_retained_without_deleting_replacements() {
    for replace in [false, true] {
        let f = Fixture::new("snapshot");
        let mut name = None;
        let result = files::Temporary::new_after(&f.config, |path| {
            name = Some(path.to_owned());
            if replace {
                fs::rename(path, path.with_extension("retained")).unwrap();
                fs::create_dir(path).unwrap();
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o1700)).unwrap();
        });
        assert!(result.is_err());
        let path = name.unwrap();
        assert!(path.is_dir());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o7777,
            0o1700
        );
    }
}

#[test]
fn anchored_output_preserves_existing_reports_and_foreign_symlink_targets() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new("snapshot");
    let output = files::Output::new(&f.config.output).unwrap();
    fs::write(
        f.config.output.join("browser-results.json"),
        b"existing-browser-report",
    )
    .unwrap();
    let target = f.root.join("foreign.json");
    fs::write(&target, b"foreign-payload").unwrap();
    symlink(&target, f.config.output.join("mdns-snapshot-results.json")).unwrap();
    assert!(output.publish("snapshot", b"private new payload").is_err());
    output.finish().unwrap();
    assert_eq!(fs::read(target).unwrap(), b"foreign-payload");
    assert_eq!(
        fs::read(f.config.output.join("browser-results.json")).unwrap(),
        b"existing-browser-report"
    );
}

#[test]
fn retained_c_source_and_exact_temporary_membership_refuse_mutation() {
    let f = Fixture::new("preflight");
    let mut source = files::Source::new(&f.config.root).unwrap();
    fs::write(
        &source.path,
        b"/* changed private source, never compiled */",
    )
    .unwrap();
    assert!(source.verify().is_err());
    assert!(source.finish().is_err());
    let temporary = files::Temporary::new(&f.config).unwrap();
    let path = temporary.path.clone();
    fs::write(path.join("foreign-extra"), b"foreign payload").unwrap();
    assert!(temporary.finish().is_err());
    assert_eq!(
        fs::read(path.join("foreign-extra")).unwrap(),
        b"foreign payload"
    );
}

#[tokio::test]
async fn local_observer_timeout_keeps_later_diagnostics_when_budget_and_cleanup_permit() {
    let f = Fixture::new("preflight");
    let mut e = f.executor();
    e.cases.insert(Role::Plist, (vec![], 0, true));
    assert_eq!(
        execute::run(&f.config, &mut e, &f.clock, &f.paths)
            .await
            .err()
            .unwrap()
            .code,
        124
    );
    assert!(e.calls.contains(&Role::Logs));
    assert!(e.calls.contains(&Role::Probe));
    assert!(!e.calls.contains(&Role::Launch));
    assert_eq!(f.report()["system"]["responder"]["timedOut"], true);
    f.no_temporary();
}

#[tokio::test]
async fn generated_probe_replacement_before_tool_selection_never_dispatches() {
    let f = Fixture::new("preflight");
    let mut e = f.executor();
    let error = execute::run_after(&f.config, &mut e, &f.clock, &f.paths, |path| {
        fs::rename(path, path.with_extension("retained")).unwrap();
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    })
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, 1);
    assert!(!e.calls.contains(&Role::Probe));
    assert_eq!(f.report()["passed"], false);
}

#[tokio::test]
async fn a_late_ready_fence_cannot_accept_a_successful_last_read() {
    let f = Fixture::new("snapshot");
    let mut e = f.executor();
    e.ready_jump = Some((9, 25000));
    assert_eq!(
        execute::run(&f.config, &mut e, &f.clock, &f.paths)
            .await
            .err()
            .unwrap()
            .code,
        124
    );
    assert_eq!(e.calls.last(), Some(&Role::Logs));
    assert_eq!(f.report()["passed"], false);
    f.no_temporary();
}

#[tokio::test]
async fn local_json_replay_follows_cleanup_and_preserves_failed_report_status() {
    for code in [0, 42] {
        let mut f = Fixture::new("preflight");
        f.config.artifact_only = false;
        let mut e = f.executor();
        e.cases.get_mut(&Role::Probe).unwrap().1 = code;
        let status = execute::run(&f.config, &mut e, &f.clock, &f.paths)
            .await
            .err()
            .map_or(0, |e| e.code);
        assert_eq!(status, if code == 0 { 0 } else { 1 });
        f.no_temporary();
        assert_eq!(
            serde_json::from_slice::<Value>(&e.replay).unwrap(),
            f.report()
        );
    }
}

#[test]
fn raw_capture_parent_cannot_equal_or_descend_from_the_physical_upload_tree() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new("snapshot");
    fs::create_dir(&f.config.output).unwrap();
    let child = f.config.output.join("nested");
    fs::create_dir(&child).unwrap();
    let alias = f.root.join("output-alias");
    symlink(&f.config.output, &alias).unwrap();
    for (output, temporary) in [
        (&f.config.output, &f.config.output),
        (&f.config.output, &child),
        (&alias, &child),
        (&f.config.output, &alias),
    ] {
        let env = Environment::from([
            ("MDNS_DIAGNOSTIC_E2E".into(), "1".into()),
            ("E2E_ARTIFACTS".into(), output.as_os_str().to_owned()),
            ("TMPDIR".into(), temporary.as_os_str().to_owned()),
        ]);
        assert_eq!(
            config::Config::read(&f.root, &f.root, &["snapshot".into()], &env, "macos")
                .err()
                .unwrap()
                .code,
            2
        );
    }
    let env = Environment::from([
        ("MDNS_DIAGNOSTIC_E2E".into(), "1".into()),
        (
            "E2E_ARTIFACTS".into(),
            f.config.output.clone().into_os_string(),
        ),
        ("TMPDIR".into(), f.root.clone().into_os_string()),
    ]);
    assert!(config::Config::read(&f.root, &f.root, &["snapshot".into()], &env, "macos").is_ok());
}

#[tokio::test]
async fn known_policy_failure_precedes_late_cleanup_cancellation() {
    for invalid_probe in [false, true] {
        let f = Fixture::new(if invalid_probe {
            "preflight"
        } else {
            "snapshot"
        });
        let mut e = f.executor();
        if invalid_probe {
            e.cases.get_mut(&Role::Probe).unwrap().0 = b"{}".to_vec();
            e.cancel_at = Some((14, 143));
        } else {
            e.cases.get_mut(&Role::Plist).unwrap().0 = b"unknown PRIVATE".to_vec();
            e.cancel_at = Some((8, 143));
        }
        assert_eq!(
            execute::run(&f.config, &mut e, &f.clock, &f.paths)
                .await
                .err()
                .unwrap()
                .code,
            1
        );
        assert_eq!(e.cancel, 143);
        assert_eq!(f.report()["passed"], false);
        f.no_temporary();
    }
}

#[test]
fn readable_fields_follow_actual_exit_zero_without_reinterpreting_timeout_facts() {
    let plist = Observation {
        stdout: b"com.apple.mDNSResponder".to_vec(),
        exit: Some(0),
        timed_out: true,
    };
    let launch = Observation {
        stdout: b"state = waiting\n".to_vec(),
        exit: Some(0),
        timed_out: true,
    };
    let logs = Observation {
        stdout: vec![],
        exit: Some(0),
        timed_out: true,
    };
    let system = policy::system(&plist, &launch, &logs).unwrap();
    assert_eq!(system["responder"]["readable"], true);
    assert_eq!(system["responder"]["timedOut"], true);
    assert_eq!(system["responder"]["state"], "waiting");
    assert_eq!(system["logs"]["readable"], true);
    assert_eq!(system["logs"]["timedOut"], true);
}

#[tokio::test]
async fn captured_caller_directory_keeps_relative_paths_and_native_path_search() {
    use crate::ci::{command::Program, process::Native};
    use crate::owned_process::Signals;
    let f = Fixture::new("snapshot");
    let caller = f.root.join("caller");
    for path in [
        &caller,
        &caller.join("temporary"),
        &caller.join("relative-tools"),
    ] {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let env = Environment::from([
        ("MDNS_DIAGNOSTIC_E2E".into(), "1".into()),
        ("E2E_ARTIFACTS".into(), "artifacts".into()),
        ("TMPDIR".into(), "temporary".into()),
        ("PATH".into(), "relative-tools".into()),
    ]);
    let config =
        config::Config::read(&f.root, &caller, &["snapshot".into()], &env, "macos").unwrap();
    assert_eq!(config.root, f.root);
    assert_eq!(config.caller, caller);
    assert_eq!(config.output, caller.join("artifacts"));
    assert_eq!(config.temporary, caller.join("temporary"));
    let peer = caller.join("relative-tools/owned-peer");
    fs::write(&peer, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(peer, fs::Permissions::from_mode(0o700)).unwrap();
    let tool = f.root.join("owned-cwd-tool");
    fs::write(
        &tool,
        concat!(
            "#!/usr/bin/python3\nimport os,subprocess\n",
            "assert os.environ['PATH']=='relative-tools'\n",
            "assert os.getcwd().endswith('/caller')\n",
            "subprocess.run(['owned-peer'],check=True)\n",
            "os.write(1,b'caller-and-relative-path')\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
    let selected = tools::FixtureTool::new(Role::Interfaces, &tool).unwrap();
    let mut temporary = files::Temporary::new(&config).unwrap();
    let mut command = Command::capture(Program::MdnsInterfaces, &[]);
    command.capture = None;
    command.mdns = Some(selected.invocation.clone());
    command.separate_logs = Some(Arc::new(temporary.logs(0).await.unwrap()));
    let mut native = Native::new(&config.caller, &config.environment, Signals::new().unwrap());
    native.run(command).await.unwrap();
    assert_eq!(temporary.read(0).unwrap(), b"caller-and-relative-path");
    let proof = selected.invocation.observed().unwrap();
    assert!(proof.group_closed && proof.io_closed);
    selected.finish().unwrap();
    temporary.finish().unwrap();
    let mut absolute = env;
    absolute.insert(
        "E2E_ARTIFACTS".into(),
        f.config.output.clone().into_os_string(),
    );
    absolute.insert("TMPDIR".into(), f.root.clone().into_os_string());
    let config =
        config::Config::read(&f.root, &caller, &["snapshot".into()], &absolute, "macos").unwrap();
    assert_eq!(config.output, f.config.output);
    assert_eq!(config.temporary, f.root);
    let source = files::Source::new(&config.root).unwrap();
    assert_eq!(source.path, f.root.join("build/test-mdns.c"));
    source.finish().unwrap();
}
