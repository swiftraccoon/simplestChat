//! Inert command observations through the production transaction. Actual private
//! paired logs receive EOF and checked closes; no tool, network or host runs.
//! These fixtures establish orchestration, not authentication or deployment.

use super::super::lifecycle::Completed;
use super::*;
use crate::ci::ops_artifact::verify::tools::{Captures, Seal};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const PRIVATE: &str = "PRIVATE_DEPLOY_INVENTORY_SENTINEL";
const SOURCE_PATHS: [&str; 6] = [
    "build/verify-release.py",
    "build/release_attestation.py",
    "build/release_fetch_controller.py",
    "ops/ansible/ansible.cfg",
    "ops/ansible/release.yml",
    "ops/ansible/tasks/signed-ops.yml",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Positive,
    HeadDrift,
    MainDrift,
    AttemptDrift,
    ApplicationDrift,
    OpsDrift,
    InventoryDrift,
    BuildKeeperDrift,
    GitReferenceDrift,
    AnsibleFailure,
    AnsibleTerminatedZero,
    AnsibleCaptureRefusal,
    AnsibleReadbackRefusal,
    AnsibleFinalProofRefusal,
    PostAnsibleAttemptDrift,
    PublicWrongRevision,
    PublicHttpFailure,
}

fn json_bytes(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}
fn repository() -> Value {
    json!({"id":1155881649u64,"full_name":policy::REPOSITORY,"fork":false,
        "owner":{"id":47705353u64,"login":"swiftraccoon"}})
}
fn run_record(attempt: u64) -> Value {
    json!({"id":123,"run_attempt":attempt,"head_sha":"a".repeat(40),"head_branch":"main",
        "name":"CI","path":".github/workflows/ci.yml","event":"push",
        "status":"completed","conclusion":"success","repository":repository(),"head_repository":repository()})
}
fn inventory_record(changed: bool) -> Value {
    json!({"benchmark_hosts":{"hosts":["public_vps"]},"_meta":{"hostvars":{"public_vps":{
        "scpub_domain":"chat.example.test","ansible_host":if changed {"192.0.2.20"} else {"192.0.2.10"},
        "ansible_user":"debian","ansible_ssh_private_key_file":"/private/fixture-key",
        "private_inventory_value":PRIVATE
    }}}})
}
fn artifact_record(ops: bool, changed: bool) -> Value {
    let id = if ops { 457 } else { 456 };
    let name = if ops {
        format!(
            "simplestchat-ops-{}-x86_64-unknown-linux-musl",
            "a".repeat(40)
        )
    } else {
        format!("simplestchat-production-{}", "a".repeat(40))
    };
    let url = format!(
        "https://api.github.com/repos/{}/actions/artifacts/{id}",
        policy::REPOSITORY
    );
    json!({"id":id,"name":name,"url":url,"archive_download_url":format!("{url}/zip"),
        "expired":false,"size_in_bytes":12345,"digest":format!("sha256:{}",if changed {"c".repeat(64)} else {"b".repeat(64)}),
        "workflow_run":{"id":123,"head_sha":"a".repeat(40),"repository_id":1155881649u64,"head_repository_id":1155881649u64}})
}

struct Fixture {
    root: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "deploy-workflow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::create_dir(root.join("results")).unwrap();
        fs::set_permissions(root.join("results"), fs::Permissions::from_mode(0o700)).unwrap();
        for name in [
            "git",
            "gh",
            "ansible-playbook",
            "ansible-inventory",
            "curl",
            "xtask",
        ] {
            let path = root.join("bin").join(name);
            fs::write(&path, b"#!/bin/sh\nexit 99\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        for name in SOURCE_PATHS {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"inert private tracked source\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        for (name, bytes) in [
            (".git/HEAD", "ref: refs/heads/main\n".to_owned()),
            (".git/index", "inert private index\n".to_owned()),
            (
                ".git/config",
                "[remote \"origin\"]\nurl = git@github.com:swiftraccoon/simplestChat.git\n"
                    .to_owned(),
            ),
            (".git/refs/heads/main", format!("{}\n", "a".repeat(40))),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let path = root.join("inventory.json");
        fs::write(&path, json_bytes(inventory_record(false))).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        let environment = Environment::from([
            ("PATH".into(), root.join("bin").into_os_string()),
            ("HOME".into(), root.clone().into_os_string()),
            ("GH_TOKEN".into(), "PRIVATE_GH_SENTINEL".into()),
            ("HTTPS_PROXY".into(), "https://PRIVATE.invalid".into()),
            ("PYTHONPATH".into(), "PRIVATE_IMPORTS".into()),
            ("ANSIBLE_HOST_KEY_CHECKING".into(), "False".into()),
        ]);
        Self { root, environment }
    }
    fn options(&self) -> Arc<Options> {
        let text = |path: PathBuf| path.to_str().unwrap().to_owned();
        let arguments = vec![
            "--inventory".into(),
            text(self.root.join("inventory.json")),
            "--repository".into(),
            policy::REPOSITORY.into(),
            "--origin".into(),
            "https://chat.example.test".into(),
            "--limit".into(),
            "public_vps".into(),
            "--wait-seconds".into(),
            "0".into(),
            "--ansible-playbook".into(),
            text(self.root.join("bin/ansible-playbook")),
            "--gh".into(),
            text(self.root.join("bin/gh")),
        ];
        Arc::new(super::super::options::parse(&arguments).unwrap().unwrap())
    }
    fn evidence(&self) -> PathBuf {
        let paths = fs::read_dir(self.root.join("results"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 1);
        paths[0].clone()
    }
    async fn run(&self, case: Case) -> (Result<Completed>, Fake, Value) {
        let options = self.options();
        let plan = Plan::native(Instant::now(), options.wait_seconds);
        let mut fake = Fake::new(case, self.root.clone());
        let result = transaction(
            self.root.clone(),
            options,
            self.environment.clone(),
            self.root.join("bin/xtask"),
            &plan,
            &mut fake,
        )
        .await;
        let report =
            serde_json::from_slice(&fs::read(self.evidence().join("outcome.json")).unwrap())
                .unwrap();
        (result, fake, report)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct Fake {
    case: Case,
    root: PathBuf,
    calls: Vec<(Program, Vec<OsString>)>,
    heads: usize,
    mains: usize,
    inventories: usize,
    statuses: usize,
    applications: usize,
    operations: usize,
    frozen: Option<Value>,
    vars: Option<Value>,
    private_paths: Vec<PathBuf>,
    late_mutation: bool,
}
impl Fake {
    fn new(case: Case, root: PathBuf) -> Self {
        Self {
            case,
            root,
            calls: Vec::new(),
            heads: 0,
            mains: 0,
            inventories: 0,
            statuses: 0,
            applications: 0,
            operations: 0,
            frozen: None,
            vars: None,
            private_paths: Vec::new(),
            late_mutation: false,
        }
    }
    fn count(&self, program: Program) -> usize {
        self.calls
            .iter()
            .filter(|(observed, _)| *observed == program)
            .count()
    }
    fn response(&mut self, program: Program, args: &[OsString]) -> (Vec<u8>, u8) {
        let text = args
            .iter()
            .map(|part| part.to_str().unwrap())
            .collect::<Vec<_>>();
        let bytes = match program {
            Program::FrontendGit => match text.as_slice() {
                ["rev-parse", "HEAD"] => {
                    self.heads += 1;
                    format!(
                        "{}\n",
                        if self.case == Case::HeadDrift && self.heads == 2 {
                            "b".repeat(40)
                        } else {
                            "a".repeat(40)
                        }
                    )
                    .into_bytes()
                }
                ["branch", "--show-current"] => b"main\n".to_vec(),
                ["status", "--porcelain", "--untracked-files=all"] => Vec::new(),
                ["remote", "get-url", "origin"] => {
                    b"git@github.com:swiftraccoon/simplestChat.git\n".to_vec()
                }
                ["check-ignore", "--", "results/deploy.evidence"] => {
                    b"results/deploy.evidence\n".to_vec()
                }
                ["ls-files", "-z", "--", "ops/ansible", "build"] => {
                    format!("{}\0", SOURCE_PATHS.join("\0")).into_bytes()
                }
                ["ls-remote", "origin", "refs/heads/main"] => {
                    format!("{}\trefs/heads/main\n", "a".repeat(40)).into_bytes()
                }
                _ => panic!("unexpected inert Git command"),
            },
            Program::FrontendGh if text == ["--version"] => {
                b"gh version 2.102.0 (inert fixture)\n".to_vec()
            }
            Program::FrontendGh => {
                assert_eq!(text[0], "api");
                for expected in [
                    ["--method", "GET"],
                    ["--hostname", "github.com"],
                    ["-H", "Accept: application/vnd.github+json"],
                    ["-H", "X-GitHub-Api-Version: 2022-11-28"],
                ] {
                    assert!(text.windows(2).any(|pair| pair == expected));
                }
                let endpoint = *text.last().unwrap();
                if endpoint.ends_with("/git/ref/heads/main") {
                    self.mains += 1;
                    json_bytes(json!({"ref":"refs/heads/main","object":{"type":"commit",
                        "sha":if self.case==Case::MainDrift && self.mains==2 {"b".repeat(40)} else {"a".repeat(40)}}}))
                } else if endpoint.contains("/actions/workflows/ci.yml/runs?") {
                    assert!(endpoint.contains(&format!("head_sha={}", "a".repeat(40))));
                    assert!(endpoint.ends_with("&per_page=100"));
                    json_bytes(json!({"total_count":1,"workflow_runs":[run_record(2)]}))
                } else if endpoint.contains("/artifacts?name=") {
                    let ops = endpoint.contains("name=simplestchat-ops-");
                    let ordinal = if ops {
                        self.operations += 1;
                        self.operations
                    } else {
                        self.applications += 1;
                        self.applications
                    };
                    let drift = ordinal == 2
                        && (self.case == Case::OpsDrift && ops
                            || self.case == Case::ApplicationDrift && !ops);
                    if ops
                        && ordinal == 2
                        && matches!(self.case, Case::BuildKeeperDrift | Case::GitReferenceDrift)
                    {
                        self.late_mutation = true;
                    }
                    json_bytes(json!({"total_count":1,"artifacts":[artifact_record(ops,drift)]}))
                } else {
                    assert_eq!(endpoint, "repos/swiftraccoon/simplestChat/actions/runs/123");
                    self.statuses += 1;
                    json_bytes(run_record(
                        if self.case == Case::AttemptDrift && self.statuses == 3
                            || self.case == Case::PostAnsibleAttemptDrift && self.statuses == 4
                        {
                            3
                        } else {
                            2
                        },
                    ))
                }
            }
            Program::FrontendInventory => {
                assert_eq!(text.len(), 3);
                assert_eq!(text[0], "-i");
                assert_eq!(text[2], "--list");
                self.inventories += 1;
                json_bytes(inventory_record(
                    self.case == Case::InventoryDrift && self.inventories == 2,
                ))
            }
            Program::FrontendAnsible => {
                assert_eq!(text.len(), 5);
                assert_eq!(text[0], "-i");
                assert_eq!(text[3], "--extra-vars");
                let inventory = PathBuf::from(text[1]);
                let vars = PathBuf::from(text[4].strip_prefix('@').unwrap());
                self.frozen = Some(serde_json::from_slice(&fs::read(&inventory).unwrap()).unwrap());
                self.vars = Some(serde_json::from_slice(&fs::read(&vars).unwrap()).unwrap());
                self.private_paths.extend([inventory, vars]);
                b"inert one-call result\n".to_vec()
            }
            Program::FrontendCurl => {
                let url = *text.last().unwrap();
                let body = if url.ends_with("/health") {
                    json_bytes(json!({"status":"ok"}))
                } else if url.ends_with("/ready") {
                    json_bytes(json!({"status":"ready"}))
                } else if url.ends_with("/app-version.json") {
                    json_bytes(json!({"revision":
                        if self.case==Case::PublicWrongRevision {"b".repeat(40)} else {"a".repeat(40)}}))
                } else {
                    assert_eq!(url, "https://chat.example.test/");
                    b"<!doctype html><html></html>".to_vec()
                };
                let status = if self.case == Case::PublicHttpFailure && url.ends_with("/ready") {
                    "503"
                } else {
                    "200"
                };
                [body, format!("\n{status}").into_bytes()].concat()
            }
            _ => panic!("unexpected inert workflow role"),
        };
        let status = if self.case == Case::AnsibleFailure && program == Program::FrontendAnsible {
            23
        } else if self.case == Case::PublicHttpFailure
            && program == Program::FrontendCurl
            && text.last().unwrap().ends_with("/ready")
        {
            22
        } else {
            0
        };
        (bytes, status)
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let proof = command.frontend.clone().expect("production invocation");
        proof.begin(&command)?;
        let (_, environment) = proof.context(command.program, &command.arguments)?;
        let credentials = matches!(
            command.program,
            Program::FrontendGh | Program::FrontendAnsible
        );
        assert_eq!(
            environment.contains_key(OsStr::new("GH_TOKEN")),
            credentials
        );
        assert!(!environment.contains_key(OsStr::new("HTTPS_PROXY")));
        assert!(!environment.contains_key(OsStr::new("PYTHONPATH")));
        let (bytes, status) = self.response(command.program, &command.arguments);
        self.calls
            .push((command.program, command.arguments.clone()));
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap())
            .ok()
            .unwrap();
        let mut output = logs.stdout.writer()?;
        output.write_all(&bytes).map_err(|_| failure(1))?;
        nix::unistd::close(output).map_err(|_| failure(1))?;
        let error = logs.stderr.writer()?;
        nix::unistd::close(error).map_err(|_| failure(1))?;
        let (out, err) = tokio::join!(logs.stdout.finish(), logs.stderr.finish());
        let seal = |done: crate::ci::bounded_log::Finished| {
            assert!(done.complete);
            Seal {
                sha256: done.sha256,
                bytes: done.bytes,
                metadata: done.metadata,
            }
        };
        let captures = Captures {
            stdout: seal(out?),
            stderr: seal(err?),
        };
        proof.observed_io(
            if self.case == Case::AnsibleCaptureRefusal
                && command.program == Program::FrontendAnsible
            {
                None
            } else {
                Some(captures)
            },
        );
        let body_completed = !(self.case == Case::AnsibleTerminatedZero
            && command.program == Program::FrontendAnsible);
        proof.observed_exit(Some(i32::from(status)), true, body_completed);
        if !body_completed {
            // A timed-out command can trap TERM and exit zero. Its original
            // timeout remains primary even though both streams and the raw
            // child have now settled; no successful body fence is fabricated.
            return Err(failure(124));
        }
        if self.late_mutation {
            self.late_mutation = false;
            let path = if self.case == Case::BuildKeeperDrift {
                self.root.join("build/release_attestation.py")
            } else {
                self.root.join(".git/refs/heads/main")
            };
            // The final metadata result has arrived, but no Ansible invocation
            // has begun. Only original filesystem custody can detect this gap.
            fs::write(path, b"changed after the last metadata observation\n").unwrap();
        }
        if command.program == Program::FrontendAnsible {
            if self.case == Case::AnsibleReadbackRefusal {
                let evidence = self.private_paths[0].parent().unwrap();
                fs::write(
                    evidence.join(format!("command-{:04}.stdout", self.calls.len() - 1)),
                    b"changed after actual stream close\n",
                )
                .unwrap();
            } else if self.case == Case::AnsibleFinalProofRefusal {
                fs::write(
                    self.root.join("build/release_attestation.py"),
                    b"changed after the actual Ansible exit observation\n",
                )
                .unwrap();
            }
        }
        proof.completed()?;
        if status == 0 {
            Ok(Vec::new())
        } else {
            Err(crate::tooling::Failure::child_exit(status))
        }
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn complete_routine_transaction_freezes_private_identity_calls_once_and_checks_four_anonymous_gets()
 {
    let fixture = Fixture::new();
    let (result, fake, report) = fixture.run(Case::Positive).await;
    let completed = result.unwrap();
    assert_eq!(completed.status, 0);
    assert_eq!(completed.summary, report);
    assert_eq!(report["candidatePassed"], true);
    assert!(report.get("passed").is_none());
    assert_eq!(report["deployed"], true);
    assert_eq!(report["phase"], "complete");
    assert_eq!(report["remoteOutcome"], "release_succeeded");
    assert_eq!(
        report["ansibleExit"],
        json!({"status":0,"settled":true,"bodyCompleted":true})
    );
    assert_eq!(fake.count(Program::FrontendAnsible), 1);
    assert_eq!(fake.count(Program::FrontendCurl), 4);
    let ansible = fake
        .calls
        .iter()
        .position(|(program, _)| *program == Program::FrontendAnsible)
        .unwrap();
    let first_public = fake
        .calls
        .iter()
        .position(|(program, _)| *program == Program::FrontendCurl)
        .unwrap();
    assert!(ansible < first_public);
    assert_eq!(
        fake.calls[ansible].1[2],
        fixture
            .root
            .join("ops/ansible/release.yml")
            .into_os_string()
    );
    assert!(
        fake.calls
            .iter()
            .flat_map(|(_, args)| args)
            .all(|arg| !arg.to_string_lossy().contains(PRIVATE))
    );
    assert_eq!(
        fake.frozen.unwrap(),
        json!({"benchmark_hosts":{"hosts":{
        "public_vps":inventory_record(false)["_meta"]["hostvars"]["public_vps"].clone()}}})
    );
    let vars = fake.vars.unwrap();
    assert_eq!(vars["scpub_release_artifact_id"], 456);
    assert_eq!(vars["scpub_ops_artifact_id"], 457);
    assert_eq!(vars["scpub_release_expected_revision"], "a".repeat(40));
    assert_eq!(vars["scpub_release_ci_run"], 123);
    assert_eq!(vars["scpub_ops_selection"]["runAttempt"], 2);
    assert_eq!(
        vars["scpub_ops_selection"],
        json!({"repository":policy::REPOSITORY,
        "revision":"a".repeat(40),"packageVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":1,
        "target":"x86_64-unknown-linux-musl","buildRunId":123,"runAttempt":2})
    );
    assert_eq!(vars["scpub_release_deploy"], true);
    assert_eq!(vars["scpub_release_native_verification"], true);
    assert_eq!(vars["scpub_release_prepared"], true);
    assert_eq!(vars["scpub_release_quiet_seconds"], 600);
    assert_eq!(
        vars["scpub_ops_xtask"],
        fixture.root.join("bin/xtask").to_str().unwrap()
    );
    assert_eq!(
        vars["scpub_ops_gh_executable"],
        fixture.root.join("bin/gh").to_str().unwrap()
    );
    assert!(vars.get("scpub_release_verified_directory").is_none());
    assert!(vars.get("scpub_ops_artifact_zip").is_none());
    assert!(!serde_json::to_string(&report).unwrap().contains(PRIVATE));
    for path in fake.private_paths {
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
    }
    let requests = fake
        .calls
        .iter()
        .filter(|(program, _)| *program == Program::FrontendCurl)
        .map(|(_, args)| {
            assert_eq!(args.first().unwrap(), "--disable");
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--noproxy" && pair[1] == "*")
            );
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--request" && pair[1] == "GET")
            );
            assert!(
                !args
                    .iter()
                    .any(|arg| arg == "--location" || arg == "-L" || arg == "--user")
            );
            args.last().unwrap().to_str().unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests,
        [
            "https://chat.example.test/health",
            "https://chat.example.test/ready",
            "https://chat.example.test/",
            "https://chat.example.test/app-version.json"
        ]
    );
}

#[tokio::test]
async fn original_head_attempt_artifact_and_private_inventory_drift_never_dispatch_ansible() {
    for case in [
        Case::HeadDrift,
        Case::MainDrift,
        Case::AttemptDrift,
        Case::ApplicationDrift,
        Case::OpsDrift,
        Case::InventoryDrift,
        Case::BuildKeeperDrift,
        Case::GitReferenceDrift,
    ] {
        let fixture = Fixture::new();
        let (result, fake, report) = fixture.run(case).await;
        assert_ne!(result.unwrap().status, 0, "{case:?}");
        assert_eq!(fake.count(Program::FrontendAnsible), 0, "{case:?}");
        assert_eq!(fake.count(Program::FrontendCurl), 0, "{case:?}");
        assert_eq!(report["deployed"], false);
        assert_eq!(report["remoteOutcome"], "not_started");
        assert_eq!(report["candidatePassed"], false);
        assert!(!serde_json::to_string(&report).unwrap().contains(PRIVATE));
    }
}

#[tokio::test]
async fn failed_ansible_keeps_actual_status_and_remote_uncertainty_without_retry_or_public_calls() {
    let fixture = Fixture::new();
    let (result, fake, report) = fixture.run(Case::AnsibleFailure).await;
    assert_eq!(result.unwrap().status, 23);
    assert_eq!(fake.count(Program::FrontendAnsible), 1);
    assert_eq!(fake.count(Program::FrontendCurl), 0);
    assert_eq!(report["candidatePassed"], false);
    assert_eq!(report["deployed"], false);
    assert_eq!(report["phase"], "deploy");
    assert_eq!(report["remoteOutcome"], "inspect_if_interrupted");
    assert_eq!(
        report["ansibleExit"],
        json!({"status":23,"settled":true,"bodyCompleted":true})
    );
}

#[tokio::test]
async fn timed_out_ansible_that_exits_zero_cannot_be_promoted_to_a_successful_release() {
    let fixture = Fixture::new();
    let (result, fake, report) = fixture.run(Case::AnsibleTerminatedZero).await;
    assert_eq!(result.unwrap().status, 124);
    assert_eq!(fake.count(Program::FrontendAnsible), 1);
    assert_eq!(fake.count(Program::FrontendCurl), 0);
    assert_eq!(report["candidatePassed"], false);
    assert_eq!(report["deployed"], false);
    assert_eq!(report["remoteOutcome"], "inspect_if_interrupted");
    assert_eq!(
        report["ansibleExit"],
        json!({"status":0,"settled":true,"bodyCompleted":false})
    );
}

#[tokio::test]
async fn actual_ansible_zero_survives_later_capture_readback_and_final_custody_refusals() {
    for case in [
        Case::AnsibleCaptureRefusal,
        Case::AnsibleReadbackRefusal,
        Case::AnsibleFinalProofRefusal,
    ] {
        let fixture = Fixture::new();
        let (result, fake, report) = fixture.run(case).await;
        assert_eq!(result.unwrap().status, 1, "{case:?}");
        assert_eq!(fake.count(Program::FrontendAnsible), 1, "{case:?}");
        assert_eq!(fake.count(Program::FrontendCurl), 0, "{case:?}");
        assert_eq!(report["candidatePassed"], false, "{case:?}");
        assert_eq!(report["deployed"], true, "{case:?}");
        assert_eq!(report["remoteOutcome"], "release_succeeded", "{case:?}");
        assert_eq!(
            report["ansibleExit"],
            json!({"status":0,"settled":true,"bodyCompleted":true}),
            "{case:?}"
        );
    }
}

#[tokio::test]
async fn post_deploy_attempt_drift_refuses_verification_without_repeating_the_successful_release() {
    let fixture = Fixture::new();
    let (result, fake, report) = fixture.run(Case::PostAnsibleAttemptDrift).await;
    assert_eq!(result.unwrap().status, 1);
    assert_eq!(fake.count(Program::FrontendAnsible), 1);
    assert_eq!(fake.count(Program::FrontendCurl), 0);
    assert_eq!(report["candidatePassed"], false);
    assert_eq!(report["deployed"], true);
    assert_eq!(report["remoteOutcome"], "release_succeeded");
    assert_eq!(
        report["ansibleExit"],
        json!({"status":0,"settled":true,"bodyCompleted":true})
    );
}

#[tokio::test]
async fn failed_public_checks_retain_successful_deployment_without_retry_or_rollback() {
    for (case, status, count) in [
        (Case::PublicWrongRevision, 1, 4),
        (Case::PublicHttpFailure, 22, 2),
    ] {
        let fixture = Fixture::new();
        let (result, fake, report) = fixture.run(case).await;
        assert_eq!(result.unwrap().status, status, "{case:?}");
        assert_eq!(fake.count(Program::FrontendAnsible), 1);
        assert_eq!(fake.count(Program::FrontendCurl), count);
        assert_eq!(report["candidatePassed"], false);
        assert_eq!(report["deployed"], true);
        assert_eq!(report["remoteOutcome"], "release_succeeded");
        assert_eq!(
            report["ansibleExit"],
            json!({"status":0,"settled":true,"bodyCompleted":true})
        );
    }
}

#[test]
fn public_reply_requires_exact_status_typed_unique_fields_revision_and_html() {
    let revision = policy::admit_revision(
        &"a".repeat(40),
        "main",
        "",
        "git@github.com:swiftraccoon/simplestChat.git",
    )
    .unwrap();
    for (path, body) in [
        ("/health", "{\"status\":\"ok\"}"),
        ("/ready", "{\"status\":\"ready\"}"),
        ("/", "<!doctype html><html></html>"),
    ] {
        public_reply(format!("{body}\n200").as_bytes(), path, &revision).unwrap();
        for suffix in ["301", "302", "503", "200\n", "200 extra"] {
            assert!(public_reply(format!("{body}\n{suffix}").as_bytes(), path, &revision).is_err());
        }
    }
    public_reply(
        format!("{{\"revision\":\"{}\"}}\n200", "a".repeat(40)).as_bytes(),
        "/app-version.json",
        &revision,
    )
    .unwrap();
    let unicode_html = "<!doctype html>".to_owned() + &"x".repeat(240) + "é\n200";
    public_reply(unicode_html.as_bytes(), "/", &revision).unwrap();
    for (path, reply) in [
        (
            "/health",
            "{\"status\":\"ok\",\"status\":\"ok\"}\n200".to_owned(),
        ),
        ("/ready", "{\"status\":true}\n200".to_owned()),
        ("/ready", "{\"status\":\"unready\"}\n200".to_owned()),
        (
            "/app-version.json",
            format!("{{\"revision\":\"{}\"}}\n200", "b".repeat(40)),
        ),
        (
            "/app-version.json",
            format!(
                "{{\"revision\":\"{0}\",\"revision\":\"{0}\"}}\n200",
                "a".repeat(40)
            ),
        ),
        ("/", "<html>missing doctype</html>\n200".to_owned()),
        ("/", "x".repeat(257) + "<!doctype html>\n200"),
    ] {
        assert!(
            public_reply(reply.as_bytes(), path, &revision).is_err(),
            "{path}"
        );
    }
    assert!(public_reply(&vec![b'x'; 1024 * 1024 + 5], "/", &revision).is_err());
    assert!(public_reply(b"\xff\n200", "/", &revision).is_err());
}
