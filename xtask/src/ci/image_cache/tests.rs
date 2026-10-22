//! Exact reference framing, owned append files and inert process fixtures.

use super::{
    execute,
    plan::{Context, Operation},
};
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Environment, Failure, Result},
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[path = "native_tests.rs"]
mod native;

struct Fixture {
    root: PathBuf,
    context: Context,
    output: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "image cache {} {} $(literal)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let output = root.join("output $(touch unowned); literal");
        fs::write(&output, "prior=preserved\n").unwrap();
        let temporary = root.join("runner temp");
        fs::create_dir(&temporary).unwrap();
        let environment = [
            ("GITHUB_ACTIONS".into(), "true".into()),
            ("RUNNER_ENVIRONMENT".into(), "github-hosted".into()),
            ("RUNNER_TEMP".into(), temporary.into_os_string()),
            ("GITHUB_OUTPUT".into(), output.clone().into_os_string()),
            ("GITHUB_REF".into(), "refs/heads/main".into()),
            ("GITHUB_EVENT_NAME".into(), "push".into()),
        ]
        .into_iter()
        .collect::<Environment>();
        let context = Context::new(&root, environment, "Linux").unwrap();
        Self {
            root,
            context,
            output,
        }
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.output).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn prefix(trust: &str) -> String {
    format!("grype-db-v2-{trust}-{}-", "a".repeat(64))
}
fn key(trust: &str) -> String {
    format!("{}{}", prefix(trust), "b".repeat(64))
}

struct Fake {
    commands: Vec<Command>,
    responses: std::collections::VecDeque<Result<Vec<u8>>>,
    unproven_after: Option<usize>,
    cancelled_after: Option<usize>,
}
impl Fake {
    fn new(responses: Vec<Result<Vec<u8>>>) -> Self {
        Self {
            commands: vec![],
            responses: responses.into(),
            unproven_after: None,
            cancelled_after: None,
        }
    }
    fn success() -> Self {
        Self::new(vec![
            Ok(prefix("main").into_bytes()),
            Ok(key("main").into_bytes()),
        ])
    }
}
impl Executor for Fake {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        assert_eq!(command.program, Program::Python);
        assert_eq!(command.timeout, Duration::from_secs(30));
        assert_eq!(command.grace, Duration::from_secs(5));
        assert_eq!(command.capture, Some(4096));
        assert!(!command.cleanup);
        assert!(command.executable.is_none());
        self.commands.push(command);
        self.responses.pop_front().expect("unexpected command")
    }
    async fn ready(&mut self) -> Result<()> {
        if self
            .cancelled_after
            .is_some_and(|n| self.commands.len() >= n)
        {
            Err(Failure::new(143, "Fixed fixture cancellation."))
        } else {
            Ok(())
        }
    }
    fn cleanup_passed(&self) -> bool {
        !self
            .unproven_after
            .is_some_and(|n| self.commands.len() >= n)
    }
}

#[test]
fn reference_arguments_and_status_paths_are_fixed_beneath_runner_temp() {
    let fixture = Fixture::new();
    for (operation, expected) in [
        (
            None,
            vec!["build/security_image_cache.py".into(), "--prefix".into()],
        ),
        (
            Some(Operation::Hint),
            vec!["build/security_image_cache.py".into()],
        ),
        (
            Some(Operation::Preparation),
            vec![
                "build/security_image_cache.py".into(),
                "--status".into(),
                fixture
                    .root
                    .join("runner temp/image-database-preparation/database-status.json")
                    .into_os_string(),
            ],
        ),
        (
            Some(Operation::Production),
            vec![
                "build/security_image_cache.py".into(),
                "--status".into(),
                fixture
                    .root
                    .join("runner temp/production-security/image/database-status.json")
                    .into_os_string(),
            ],
        ),
    ] {
        let command = fixture.context.reference(operation);
        assert_eq!(command.arguments, expected);
        assert_eq!(command.program, Program::Python);
        assert_eq!(command.capture, Some(4096));
    }
}

#[test]
fn actual_python_oracle_preserves_scanner_pin_trust_and_validated_generation_framing() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let script = r#"
import sys
sys.path[:0]=[sys.argv[1]+'/build',sys.argv[1]+'/ops/ansible/files']
import security_image_cache as reference
print(reference.prefix())
print(reference.key_from_status({'from':'https://grype.anchore.io/databases/v6/fixture.tar.zst?checksum=sha256%3A'+'b'*64}))
"#;
    for (reference, event, trust) in [
        ("refs/heads/main", "push", "main"),
        ("refs/heads/main", "pull_request", "untrusted"),
        ("refs/heads/topic", "push", "untrusted"),
        ("refs/pull/1/merge", "pull_request", "untrusted"),
    ] {
        let fixture = Fixture::new();
        let mut environment = fixture.context.environment.clone();
        environment.insert("GITHUB_REF".into(), reference.into());
        environment.insert("GITHUB_EVENT_NAME".into(), event.into());
        let context = Context::new(&fixture.root, environment, "Linux").unwrap();
        let output = std::process::Command::new("python3")
            .args(["-B", "-c", script])
            .arg(repository)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("GITHUB_REF", reference)
            .env("GITHUB_EVENT_NAME", event)
            .output()
            .unwrap();
        assert!(output.status.success(), "Python pure oracle failed");
        let text = std::str::from_utf8(&output.stdout).unwrap();
        let mut lines = text.lines();
        let prefix = context.prefix(lines.next().unwrap().as_bytes()).unwrap();
        assert!(prefix.starts_with(&format!("grype-db-v2-{trust}-")));
        let key = lines.next().unwrap();
        assert_eq!(
            context
                .selection(&prefix, key.as_bytes(), true)
                .unwrap()
                .record(),
            format!("key={key}\nprefix={prefix}\n").as_bytes()
        );
        assert_eq!(
            context
                .selection(&prefix, key.as_bytes(), false)
                .unwrap()
                .record(),
            format!("key={key}\n").as_bytes()
        );
        assert!(lines.next().is_none());
    }
}

#[test]
fn malformed_replies_and_cross_trust_or_pin_keys_are_refused_before_output() {
    let fixture = Fixture::new();
    let good = prefix("main");
    for invalid in [
        prefix("untrusted"),
        good.replace("aaaa", "AAAA"),
        format!("{good}PRIVATE-SENTINEL\nother=bad"),
        format!("{good}\r"),
        format!(" {good}"),
        good[..good.len() - 1].into(),
        format!("{good}-"),
    ] {
        assert!(fixture.context.prefix(invalid.as_bytes()).is_err());
    }
    assert!(fixture.context.prefix(&[255]).is_err());
    for invalid in [
        key("untrusted"),
        key("main").replace("aaaa", "cccc"),
        key("main").replace("bbbb", "BBBB"),
        format!("{}\nprivate=PRIVATE-SENTINEL", key("main")),
        format!("{}\r", key("main")),
        format!("{} ", key("main")),
        format!("{}0", key("main")),
        good.clone(),
    ] {
        assert!(
            fixture
                .context
                .selection(&good, invalid.as_bytes(), false)
                .is_err()
        );
    }
    assert_eq!(
        fixture
            .context
            .selection(&good, format!("{good}\n\n").as_bytes(), true)
            .unwrap()
            .record(),
        format!("key={good}\nprefix={good}\n").as_bytes()
    );
    assert_eq!(fixture.bytes(), b"prior=preserved\n");
}

#[tokio::test]
async fn hint_and_both_status_operations_append_only_their_fixed_records() {
    for operation in [
        Operation::Hint,
        Operation::Preparation,
        Operation::Production,
    ] {
        let fixture = Fixture::new();
        let mut fake = Fake::success();
        execute::execute(&fixture.context, operation, &mut fake, &mut Vec::new())
            .await
            .unwrap();
        let expected = if operation == Operation::Hint {
            format!(
                "prior=preserved\nkey={}\nprefix={}\n",
                key("main"),
                prefix("main")
            )
        } else {
            format!("prior=preserved\nkey={}\n", key("main"))
        };
        assert_eq!(fixture.bytes(), expected.as_bytes());
        assert_eq!(fake.commands.len(), 2);
        assert_eq!(
            fake.commands[0].arguments,
            ["build/security_image_cache.py", "--prefix"]
        );
        assert_eq!(
            fake.commands[1].arguments,
            fixture.context.reference(Some(operation)).arguments
        );
        assert!(!fixture.root.join("unowned").exists());
    }
}

#[tokio::test]
async fn failed_hint_falls_back_only_after_successful_prefix_and_proven_process_cleanup() {
    for code in [1, 42, 124, 127] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(vec![
            Ok(prefix("main").into_bytes()),
            Err(Failure::new(code, "PRIVATE-SENTINEL")),
        ]);
        let mut warnings = Vec::new();
        execute::execute(&fixture.context, Operation::Hint, &mut fake, &mut warnings)
            .await
            .unwrap();
        assert_eq!(warnings, b"Database cache hint unavailable; the online updater will check current upstream data.\n");
        assert_eq!(
            fixture.bytes(),
            format!(
                "prior=preserved\nkey={}\nprefix={}\n",
                prefix("main"),
                prefix("main")
            )
            .as_bytes()
        );
    }
}

#[tokio::test]
async fn prefix_failure_status_failure_cancel_and_unproven_cleanup_never_publish() {
    for (operation, prefix_failed, code) in [
        (Operation::Hint, true, 42),
        (Operation::Hint, false, 130),
        (Operation::Hint, false, 143),
        (Operation::Preparation, false, 42),
        (Operation::Production, false, 124),
    ] {
        let fixture = Fixture::new();
        let mut responses = vec![];
        if !prefix_failed {
            responses.push(Ok(prefix("main").into_bytes()));
        }
        responses.push(Err(Failure::new(code, "PRIVATE-SENTINEL")));
        let mut fake = Fake::new(responses);
        let error = execute::execute(&fixture.context, operation, &mut fake, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(fake.commands.len(), if prefix_failed { 1 } else { 2 });
        assert_eq!(fixture.bytes(), b"prior=preserved\n");
    }
    for count in [0, 1, 2] {
        for cancel in [false, true] {
            let fixture = Fixture::new();
            let mut fake = Fake::success();
            if cancel {
                fake.cancelled_after = Some(count);
            } else {
                fake.unproven_after = Some(count);
            }
            assert!(
                execute::execute(
                    &fixture.context,
                    Operation::Hint,
                    &mut fake,
                    &mut Vec::new()
                )
                .await
                .is_err()
            );
            assert_eq!(fixture.bytes(), b"prior=preserved\n");
        }
    }
    let fixture = Fixture::new();
    let mut fake = Fake::new(vec![
        Ok(prefix("main").into_bytes()),
        Err(Failure::new(42, "PRIVATE-SENTINEL")),
    ]);
    fake.unproven_after = Some(2);
    assert!(
        execute::execute(
            &fixture.context,
            Operation::Hint,
            &mut fake,
            &mut Vec::new()
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.bytes(), b"prior=preserved\n");
}

#[tokio::test]
async fn malformed_successful_reply_is_fatal_and_missing_output_starts_no_python() {
    for responses in [
        vec![Ok(b"PRIVATE-SENTINEL".to_vec())],
        vec![
            Ok(prefix("main").into_bytes()),
            Ok(b"PRIVATE-SENTINEL\nother=bad".to_vec()),
        ],
    ] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(responses);
        assert!(
            execute::execute(
                &fixture.context,
                Operation::Hint,
                &mut fake,
                &mut Vec::new()
            )
            .await
            .is_err()
        );
        assert_eq!(fixture.bytes(), b"prior=preserved\n");
    }
    let mut fixture = Fixture::new();
    let alias = fixture.root.join("alias");
    symlink(&fixture.output, &alias).unwrap();
    for path in [
        PathBuf::new(),
        PathBuf::from("relative"),
        fixture.root.join("missing"),
        fixture.root.clone(),
        alias,
    ] {
        fixture
            .context
            .environment
            .insert("GITHUB_OUTPUT".into(), path.into_os_string());
        let mut fake = Fake::success();
        assert!(
            execute::execute(
                &fixture.context,
                Operation::Hint,
                &mut fake,
                &mut Vec::new()
            )
            .await
            .is_err()
        );
        assert!(fake.commands.is_empty());
        assert_eq!(fixture.bytes(), b"prior=preserved\n");
    }
}

#[test]
fn only_fixed_operations_and_explicit_linux_context_are_admitted() {
    let fixture = Fixture::new();
    for (args, operation) in [
        (vec!["hint"], Operation::Hint),
        (vec!["status", "preparation"], Operation::Preparation),
        (vec!["status", "production"], Operation::Production),
    ] {
        assert_eq!(
            Operation::parse(&args.iter().map(|s| (*s).into()).collect::<Vec<_>>()).unwrap(),
            operation
        );
    }
    for args in [
        vec![],
        vec!["status"],
        vec!["hint", "private"],
        vec!["status", "/PRIVATE-SENTINEL"],
        vec!["--status", "preparation"],
    ] {
        assert_eq!(
            Operation::parse(&args.iter().map(|s| (*s).into()).collect::<Vec<_>>())
                .unwrap_err()
                .code,
            2
        );
    }
    assert!(Context::new(&fixture.root, fixture.context.environment.clone(), "Darwin").is_err());
    let mut environment = fixture.context.environment.clone();
    environment.remove(std::ffi::OsStr::new("RUNNER_ENVIRONMENT"));
    assert!(Context::new(&fixture.root, environment.clone(), "Linux").is_err());
    environment.insert("ACT".into(), "true".into());
    assert!(Context::new(&fixture.root, environment.clone(), "Linux").is_err());
    environment.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
    Context::new(&fixture.root, environment, "Linux").unwrap();
    assert_eq!(
        super::run(&fixture.root, &["--help".into()]).unwrap(),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        super::run(&fixture.root, &["status".into(), "private".into()]).unwrap(),
        std::process::ExitCode::from(2)
    );
    assert_eq!(fixture.bytes(), b"prior=preserved\n");
}

#[tokio::test]
async fn warning_is_emitted_only_for_clean_hint_failure_and_write_failure_prevents_publication() {
    for (prefix_failed, cancelled) in [(true, false), (false, false), (false, true)] {
        let fixture = Fixture::new();
        let mut fake = if prefix_failed {
            Fake::new(vec![Err(Failure::new(42, "PRIVATE-SENTINEL"))])
        } else {
            Fake::success()
        };
        if cancelled {
            fake.cancelled_after = Some(2);
        }
        let mut warnings = Vec::new();
        let result =
            execute::execute(&fixture.context, Operation::Hint, &mut fake, &mut warnings).await;
        assert_eq!(result.is_ok(), !prefix_failed && !cancelled);
        assert!(warnings.is_empty());
    }
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("PRIVATE-SENTINEL"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let mut fake = Fake::new(vec![
        Ok(prefix("main").into_bytes()),
        Err(Failure::new(42, "PRIVATE-SENTINEL")),
    ]);
    let error = execute::execute(&fixture.context, Operation::Hint, &mut fake, &mut Broken)
        .await
        .unwrap_err();
    assert!(!error.message.contains("PRIVATE-SENTINEL"));
    assert_eq!(fixture.bytes(), b"prior=preserved\n");
}
