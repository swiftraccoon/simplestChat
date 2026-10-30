//! Actual local inert children over the shared Native owner; no network/host work.
use super::*;
use crate::{
    ci::{
        bounded_log::Log,
        command::SeparateLogs,
        process::{Executor, Native},
    },
    owned_process::Signals,
};
use std::{
    fs,
    fs::OpenOptions,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    sync::atomic::{AtomicU64, AtomicUsize},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct TestCustody {
    valid: AtomicBool,
    checks: AtomicUsize,
    refuse_at: Option<usize>,
    refuse_marker: Option<PathBuf>,
}
impl TestCustody {
    fn new() -> Self {
        Self {
            valid: AtomicBool::new(true),
            checks: AtomicUsize::new(0),
            refuse_at: None,
            refuse_marker: None,
        }
    }
}
impl Custody for TestCustody {
    fn check(&self) -> Result<()> {
        let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        if self.refuse_at == Some(check)
            || self
                .refuse_marker
                .as_ref()
                .is_some_and(|path| path.is_file())
        {
            self.valid.store(false, Ordering::SeqCst);
        }
        if self.valid.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(failure(1))
        }
    }
}
struct Fixture {
    root: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "deploy-tools-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::create_dir(root.join("real")).unwrap();
        let environment = Environment::from([
            ("PATH".into(), root.join("bin").into_os_string()),
            ("HOME".into(), root.clone().into_os_string()),
            ("GH_TOKEN".into(), "PRIVATE-GH".into()),
            ("GITHUB_TOKEN".into(), "PRIVATE-GITHUB".into()),
            (
                "GH_CONFIG_DIR".into(),
                root.join("private-auth").into_os_string(),
            ),
            ("HTTPS_PROXY".into(), "https://PRIVATE.invalid".into()),
            ("ANSIBLE_CONFIG".into(), "PRIVATE-WRONG-CONFIG".into()),
            ("ANSIBLE_HOST_KEY_CHECKING".into(), "False".into()),
            ("PYTHONPATH".into(), "PRIVATE-IMPORTS".into()),
        ]);
        let fixture = Self { root, environment };
        for name in [
            "git",
            "gh",
            "ansible-inventory",
            "ansible-playbook",
            "curl",
            "xtask",
        ] {
            fixture.write(
                name,
                concat!(
                    "[ -z \"${HTTPS_PROXY+x}\" ] || exit 81\n",
                    "[ -z \"${PYTHONPATH+x}\" ] || exit 82\n",
                    "[ \"${PYTHONDONTWRITEBYTECODE-}\" = 1 ] || exit 85\n",
                    "case \"${0##*/}\" in\n",
                    "gh|ansible-playbook) [ \"${GH_TOKEN-}\" = PRIVATE-GH ] || exit 83 ;;\n",
                    "git) [ \"${GIT_OPTIONAL_LOCKS-}\" = 0 ] && [ -z \"${GH_TOKEN+x}\" ] && [ -z \"${GITHUB_TOKEN+x}\" ] || exit 86 ;;\n",
                    "*) [ -z \"${GH_TOKEN+x}\" ] && [ -z \"${GITHUB_TOKEN+x}\" ] || exit 84 ;;\n",
                    "esac\nprintf owned-ok\n"
                ),
            );
        }
        fs::rename(
            fixture.root.join("bin/ansible-playbook"),
            fixture.root.join("real/ansible-playbook"),
        )
        .unwrap();
        symlink(
            fixture.root.join("real/ansible-playbook"),
            fixture.root.join("bin/ansible-playbook"),
        )
        .unwrap();
        fixture
    }
    fn write(&self, name: &str, body: &str) {
        let path = self.root.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn tools(&self) -> Tools {
        Tools::open(
            &self.root,
            &self.environment,
            &self.root.join("bin/ansible-playbook"),
            &self.root.join("bin/gh"),
            &self.root.join("bin/xtask"),
        )
        .unwrap()
    }
    fn spec(&self, program: Program) -> Spec {
        let now = Instant::now();
        Spec {
            program,
            arguments: vec!["--inert".into()],
            timeout: Duration::from_secs(2),
            grace: Duration::from_secs(2),
            maximum: 4096,
            end: now + Duration::from_secs(30),
            latest: now + Duration::from_secs(10),
            custody: Arc::new(TestCustody::new()),
        }
    }
    fn command(&self, proof: &Arc<Invocation>, ordinal: u64) -> Command {
        let output = |name: &str, maximum| {
            let path = self.root.join(format!("capture-{ordinal}.{name}"));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            Log::start(file, maximum).unwrap()
        };
        let mut command = Command::capture(proof.spec.program, &[]);
        command.arguments = proof.spec.arguments.clone();
        command.timeout = proof.spec.timeout;
        command.grace = proof.spec.grace;
        command.capture = None;
        command.separate_logs = Some(Arc::new(SeparateLogs {
            stdout: output("stdout", proof.spec.maximum),
            stderr: output("stderr", STDERR),
        }));
        command.frontend = Some(proof.clone());
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn credential_domains_config_and_selected_venv_paths_are_explicit() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    assert_eq!(
        tools.ansible_path().unwrap(),
        fixture.root.join("bin/ansible-playbook")
    );
    assert_eq!(
        tools.inventory_path().unwrap(),
        fixture.root.join("bin/ansible-inventory")
    );
    assert_eq!(tools.gh_path().unwrap(), fixture.root.join("bin/gh"));
    assert_eq!(tools.xtask_path().unwrap(), fixture.root.join("bin/xtask"));
    for program in [
        Program::FrontendGit,
        Program::FrontendGh,
        Program::FrontendInventory,
        Program::FrontendAnsible,
        Program::FrontendCurl,
    ] {
        let proof = tools.invocation(fixture.spec(program)).unwrap();
        let (_, env) = proof.context(program, &proof.spec.arguments).unwrap();
        let credentials = matches!(program, Program::FrontendGh | Program::FrontendAnsible);
        assert_eq!(env.contains_key(OsStr::new("GH_TOKEN")), credentials);
        assert_eq!(env.contains_key(OsStr::new("GITHUB_TOKEN")), credentials);
        assert!(!env.contains_key(OsStr::new("HTTPS_PROXY")));
        assert!(!env.contains_key(OsStr::new("PYTHONPATH")));
        assert_eq!(env[OsStr::new("PYTHONDONTWRITEBYTECODE")], "1");
        assert_eq!(
            env.contains_key(OsStr::new("GIT_OPTIONAL_LOCKS")),
            program == Program::FrontendGit
        );
        assert_eq!(
            std::env::split_paths(&env[OsStr::new("PATH")])
                .next()
                .unwrap(),
            fixture.root.join("bin")
        );
        if matches!(
            program,
            Program::FrontendAnsible | Program::FrontendInventory
        ) {
            assert_eq!(
                env[OsStr::new("ANSIBLE_CONFIG")],
                fixture
                    .root
                    .join("ops/ansible/ansible.cfg")
                    .into_os_string()
            );
            assert_eq!(env[OsStr::new("ANSIBLE_HOST_KEY_CHECKING")], "True");
        }
    }
    tools.finish().unwrap();
}

#[tokio::test]
async fn all_five_roles_use_actual_owned_children_and_checked_pair_completion() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let replay = Environment::new();
    let mut executor = Native::new(&fixture.root, &replay, Signals::new().unwrap());
    for (ordinal, program) in [
        Program::FrontendGit,
        Program::FrontendGh,
        Program::FrontendInventory,
        Program::FrontendAnsible,
        Program::FrontendCurl,
    ]
    .into_iter()
    .enumerate()
    {
        let proof = tools.invocation(fixture.spec(program)).unwrap();
        executor
            .run(fixture.command(&proof, ordinal as u64))
            .await
            .unwrap();
        let captures = proof.captures().unwrap();
        assert_eq!(captures.stdout.bytes, 8);
        assert_eq!(captures.stderr.bytes, 0);
        assert_eq!(proof.exit().unwrap().status, Some(0));
        assert!(proof.exit().unwrap().settled && executor.cleanup_passed());
        assert!(proof.exit().unwrap().body_completed);
        assert!(
            proof.captures().is_err(),
            "completed capture is consumed once"
        );
    }
    drop(executor);
    tools.finish().unwrap();
}

#[tokio::test]
async fn changed_role_arguments_and_capture_contracts_refuse_before_child_effects() {
    for case in 0..8 {
        let fixture = Fixture::new();
        let tools = fixture.tools();
        let proof = tools
            .invocation(fixture.spec(Program::FrontendGit))
            .unwrap();
        let mut command = fixture.command(&proof, 0);
        match case {
            0 => command.program = Program::FrontendGh,
            1 => command.arguments.push("--foreign".into()),
            2 => command.timeout += Duration::from_secs(1),
            3 => command.grace += Duration::from_secs(1),
            4 => command.cleanup = true,
            5 => command.capture = Some(1),
            6 => command.frontend = None,
            _ => {
                let logs = Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
                let (a, b) = tokio::join!(logs.stdout.finish(), logs.stderr.finish());
                a.unwrap();
                b.unwrap();
            }
        }
        let env = Environment::new();
        let mut executor = Native::new(&fixture.root, &env, Signals::new().unwrap());
        assert!(executor.run(command).await.is_err(), "mutation {case}");
        assert!(proof.exit().is_err());
        assert!(executor.cleanup_passed());
        drop(proof);
        drop(executor);
        tools.finish().unwrap();
    }
}

#[tokio::test]
async fn executable_replacement_and_single_use_admission_cannot_be_replayed() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let proof = tools
        .invocation(fixture.spec(Program::FrontendGit))
        .unwrap();
    fs::rename(
        fixture.root.join("bin/git"),
        fixture.root.join("original-git"),
    )
    .unwrap();
    fixture.write("git", "printf foreign\n");
    let env = Environment::new();
    let mut executor = Native::new(&fixture.root, &env, Signals::new().unwrap());
    assert!(executor.run(fixture.command(&proof, 0)).await.is_err());
    assert!(proof.exit().is_err());
    assert!(executor.run(fixture.command(&proof, 1)).await.is_err());
    assert!(executor.cleanup_passed());
    drop(proof);
    drop(executor);
    assert!(
        tools.finish().is_err(),
        "original executable replacement stays refused"
    );
}

#[tokio::test]
async fn nonzero_timeout_and_interrupt_keep_raw_status_separate_from_failure() {
    for case in 0..3 {
        let fixture = Fixture::new();
        fixture.write(
            "git",
            if case == 0 {
                "exit 23\n"
            } else {
                "printf ready > child-ready\n/bin/sleep 2\n"
            },
        );
        let tools = fixture.tools();
        let mut spec = fixture.spec(Program::FrontendGit);
        if case == 1 {
            spec.timeout = Duration::from_millis(35);
        }
        let proof = tools.invocation(spec).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let ready = fixture.root.join("child-ready");
        let signal = tokio::spawn(async move {
            if case == 2 {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !ready.is_file() && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                assert!(ready.is_file(), "owned child reached its signal barrier");
                sender.send(143).unwrap();
            }
        });
        let env = Environment::new();
        let mut executor = Native::new(&fixture.root, &env, Signals::injected(receiver));
        let error = executor.run(fixture.command(&proof, 0)).await.unwrap_err();
        signal.await.unwrap();
        assert_eq!(error.code, [23, 124, 143][case]);
        let observed = proof.exit().unwrap();
        assert!(observed.settled && executor.cleanup_passed());
        assert_ne!(observed.status, Some(0));
        assert_eq!(observed.body_completed, case == 0);
        proof.captures().unwrap();
        drop(proof);
        drop(executor);
        tools.finish().unwrap();
    }
}

#[tokio::test]
async fn timeout_that_triggers_a_zero_exit_is_not_normal_body_completion() {
    let fixture = Fixture::new();
    fixture.write(
        "ansible-playbook",
        concat!(
            "trap 'printf handled > term-handled; exit 0' TERM\n",
            "printf ready > child-ready\n",
            "while :; do :; done\n"
        ),
    );
    let tools = fixture.tools();
    let mut spec = fixture.spec(Program::FrontendAnsible);
    spec.timeout = Duration::from_secs(1);
    let proof = tools.invocation(spec).unwrap();
    let env = Environment::new();
    let mut executor = Native::new(&fixture.root, &env, Signals::new().unwrap());
    let error = executor.run(fixture.command(&proof, 0)).await.unwrap_err();
    assert_eq!(
        error.code, 124,
        "the original timeout remains authoritative"
    );
    assert!(fixture.root.join("child-ready").is_file());
    assert!(fixture.root.join("term-handled").is_file());
    let observed = proof.exit().unwrap();
    assert_eq!(observed.status, Some(0));
    assert!(observed.settled && executor.cleanup_passed());
    assert!(
        !observed.body_completed,
        "a zero exit caused by termination does not prove normal completion"
    );
    proof.captures().unwrap();
    drop(proof);
    drop(executor);
    tools.finish().unwrap();
}

#[tokio::test]
async fn final_deadline_and_dependency_fences_cannot_erase_passive_exit_evidence() {
    for late in [false, true] {
        let fixture = Fixture::new();
        let tools = fixture.tools();
        let mut proof = tools
            .invocation(fixture.spec(Program::FrontendAnsible))
            .unwrap();
        let env = Environment::new();
        let mut executor = Native::new(&fixture.root, &env, Signals::new().unwrap());
        executor.run(fixture.command(&proof, 0)).await.unwrap();
        if late {
            Arc::get_mut(&mut proof).unwrap().spec.end = Instant::now() - Duration::from_nanos(1);
        } else {
            fs::rename(
                fixture.root.join("bin/xtask"),
                fixture.root.join("original-xtask"),
            )
            .unwrap();
            fixture.write("xtask", "printf foreign\n");
        }
        assert!(proof.captures().is_err());
        assert_eq!(proof.exit().unwrap().status, Some(0));
        assert!(proof.exit().unwrap().settled);
        assert!(proof.exit().unwrap().body_completed);
        drop(proof);
        drop(executor);
        assert_eq!(tools.finish().is_ok(), late);
    }
}

#[test]
fn invalid_deadlines_caps_and_outstanding_borrowers_are_not_positive_completion() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    for case in 0..4 {
        let mut spec = fixture.spec(Program::FrontendGit);
        match case {
            0 => spec.latest = Instant::now() - Duration::from_secs(1),
            1 => spec.end = spec.latest,
            2 => spec.maximum = 4 * 1024 * 1024 + 1,
            _ => spec.timeout = Duration::ZERO,
        }
        assert!(tools.invocation(spec).is_err());
    }
    let proof = tools
        .invocation(fixture.spec(Program::FrontendGit))
        .unwrap();
    assert!(tools.finish().is_err());
    assert!(
        proof
            .validate(proof.spec.program, &proof.spec.arguments)
            .is_ok()
    );
}

#[tokio::test]
async fn custody_lost_after_earlier_admissions_refuses_the_first_child_effect() {
    let fixture = Fixture::new();
    fixture.write("git", "printf effect > first-effect\nprintf owned-ok\n");
    let tools = fixture.tools();
    let custody = Arc::new(TestCustody {
        // Initial admission, environment selection and executable selection
        // each passed their paired checks. Refuse the deferred launch fence.
        refuse_at: Some(7),
        ..TestCustody::new()
    });
    let mut spec = fixture.spec(Program::FrontendGit);
    spec.custody = custody.clone();
    let proof = tools.invocation(spec).unwrap();
    assert_eq!(
        custody.checks.load(Ordering::SeqCst),
        0,
        "construction does not lock the caller's custody state"
    );
    let env = Environment::new();
    let mut executor = Native::new(&fixture.root, &env, Signals::new().unwrap());
    assert_eq!(
        executor
            .run(fixture.command(&proof, 0))
            .await
            .unwrap_err()
            .code,
        1
    );
    assert!(custody.checks.load(Ordering::SeqCst) >= 7);
    assert!(!fixture.root.join("first-effect").exists());
    assert!(proof.exit().is_err(), "no child was admitted");
    assert!(executor.cleanup_passed());
    custody.valid.store(true, Ordering::SeqCst);
    assert!(
        proof
            .validate(proof.spec.program, &proof.spec.arguments)
            .is_err(),
        "a known custody refusal cannot revive the same invocation"
    );
    drop(proof);
    drop(executor);
    tools.finish().unwrap();
}

#[tokio::test]
async fn post_child_custody_refusal_keeps_actual_zero_and_completed_stream_evidence() {
    let fixture = Fixture::new();
    fixture.write("git", "printf effect > first-effect\nprintf owned-ok\n");
    let tools = fixture.tools();
    let custody = Arc::new(TestCustody {
        refuse_marker: Some(fixture.root.join("first-effect")),
        ..TestCustody::new()
    });
    let mut spec = fixture.spec(Program::FrontendGit);
    spec.custody = custody;
    let proof = tools.invocation(spec).unwrap();
    let env = Environment::new();
    let mut executor = Native::new(&fixture.root, &env, Signals::new().unwrap());
    assert_eq!(
        executor
            .run(fixture.command(&proof, 0))
            .await
            .unwrap_err()
            .code,
        1
    );
    assert!(fixture.root.join("first-effect").is_file());
    assert_eq!(proof.exit().unwrap().status, Some(0));
    assert!(proof.exit().unwrap().settled && executor.cleanup_passed());
    assert!(proof.exit().unwrap().body_completed);
    {
        let received = proof.captures.lock().unwrap();
        let Received::Complete(captures) = &*received else {
            panic!("both original streams must complete before the late custody fence");
        };
        assert_eq!(captures.stdout.bytes, 8);
        assert_eq!(captures.stderr.bytes, 0);
    }
    assert!(
        proof.captures().is_err(),
        "completed output does not outvote changed custody"
    );
    drop(proof);
    drop(executor);
    tools.finish().unwrap();
}
