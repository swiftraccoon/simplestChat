//! Typed plans and fake owned subprocesses; no browser or service is started.

use super::*;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::process::Command;

struct Fixture {
    root: PathBuf,
    executable: PathBuf,
    environment: Environment,
}

impl Fixture {
    fn new(body: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "browser suite {} {} with spaces",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let artifacts = root.join("artifacts with literal $(text)");
        fs::create_dir(&artifacts).unwrap();
        let executable = root.join("owned fixture harness");
        let source = format!(
            r#"#!/usr/bin/env python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT'])
with (root/'events.jsonl').open('a') as output:
    output.write(json.dumps({{'args':sys.argv[1:], 'browser':os.environ.get('E2E_BROWSER'),
        'artifacts':os.environ.get('E2E_ARTIFACTS'), 'ip':os.environ.get('TEST_ANNOUNCE_IP'),
        'flag':os.environ.get('PRESENTATION_IMPAIRMENT')}})+'\n')
{body}
"#
        );
        fs::write(&executable, source).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut environment = tooling::inherited_env();
        environment.insert("RUNNER_TEMP".into(), artifacts.into_os_string());
        environment.insert("FIXTURE_ROOT".into(), root.clone().into_os_string());
        environment.insert("PRESENTATION_IMPAIRMENT".into(), "1".into());
        environment.insert("E2E_BROWSER".into(), "ambient-browser".into());
        environment.insert("E2E_ARTIFACTS".into(), "ambient-artifacts".into());
        Self {
            root,
            executable,
            environment,
        }
    }

    fn plan(&self, suite: Suite) -> Vec<Invocation> {
        plan(
            suite,
            &self.environment,
            &self.executable,
            Some(Ipv4Addr::new(192, 0, 2, 1)),
        )
        .unwrap()
    }

    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn idle_signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn every_suite_fixes_both_browser_arguments_artifacts_and_only_needed_overrides() {
    let fixture = Fixture::new("sys.exit(0)");
    for (name, script, prefix, media) in [
        ("auth-lifecycle", "auth-lifecycle", "auth-lifecycle", false),
        ("auth-cross-tab", "auth-cross-tab", "account-tabs", false),
        (
            "room-control-confirmation",
            "room-control-confirmation",
            "room-controls",
            false,
        ),
        (
            "media-continuity",
            "media-continuity",
            "media-continuity",
            true,
        ),
        ("presentation", "presentation", "presentation", true),
    ] {
        let suite = Suite::parse(name).unwrap();
        assert_eq!(suite.needs_media_ip(), media);
        let invocations = fixture.plan(suite);
        assert_eq!(invocations.len(), 2);
        for (index, browser) in ["chromium", "firefox"].into_iter().enumerate() {
            let invocation = &invocations[index];
            assert_eq!(invocation.executable, fixture.executable);
            let arguments: Vec<_> = invocation
                .arguments
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect();
            assert_eq!(
                arguments,
                ["test-server", "node", &format!("web/e2e/{script}.cjs")]
            );
            let mut expected = fixture.environment.clone();
            expected.insert("E2E_BROWSER".into(), browser.into());
            expected.insert(
                "E2E_ARTIFACTS".into(),
                Path::new(&expected[OsStr::new("RUNNER_TEMP")])
                    .join(format!("{prefix}-{browser}"))
                    .into_os_string(),
            );
            if media {
                expected.insert("TEST_ANNOUNCE_IP".into(), "192.0.2.1".into());
            }
            assert_eq!(invocation.environment, expected);
        }
    }
    for unknown in [
        "",
        "community",
        "passkey",
        "webkit",
        "presentation; PRIVATE",
        "--PRIVATE",
    ] {
        assert!(Suite::parse(unknown).is_none());
    }
}

#[test]
fn invalid_invocation_and_artifact_roots_refuse_before_a_child_starts() {
    let fixture = Fixture::new("sys.exit(0)");
    for args in [
        vec![],
        vec!["unknown".into()],
        vec!["presentation".into(), "extra".into()],
    ] {
        assert_eq!(run(&fixture.root, &args).unwrap(), ExitCode::from(2));
    }
    for value in [
        None,
        Some(OsString::new()),
        Some("relative".into()),
        Some("/not/a/real/root".into()),
    ] {
        let mut environment = fixture.environment.clone();
        environment.remove(OsStr::new("RUNNER_TEMP"));
        if let Some(value) = value {
            environment.insert("RUNNER_TEMP".into(), value);
        }
        assert_eq!(
            plan(
                Suite::AuthLifecycle,
                &environment,
                &fixture.executable,
                None
            )
            .err()
            .unwrap()
            .code,
            2
        );
    }
    assert_eq!(
        plan(
            Suite::Presentation,
            &fixture.environment,
            &fixture.executable,
            None
        )
        .err()
        .unwrap()
        .code,
        1
    );
    assert!(fixture.events().is_empty());
    assert!(CLEANUP_GRACE >= Duration::from_secs(30));
}

struct FutureDetector<F>(Option<F>);
impl<F: std::future::Future<Output = anyhow::Result<Ipv4Addr>>> crate::media_ip::Detector
    for FutureDetector<F>
{
    async fn detect(&mut self, signals: &mut Signals) -> tooling::Result<Ipv4Addr> {
        tokio::select! {
            biased;
            code = signals.next() => Err(Failure::new(code, "Inert detector interrupted.")),
            result = self.0.take().unwrap() => result.map_err(|_| Failure::new(1,
                "Inert detector failed.")),
        }
    }
}

#[tokio::test]
async fn native_detector_retires_its_query_before_cancelled_browser_preparation_returns() {
    let mut fixture = Fixture::new("sys.exit(0)");
    let source = concat!(
        "#!/usr/bin/python3\n",
        "import os,signal,sys,time\nfrom pathlib import Path\n",
        "root=Path(os.environ['FIXTURE_ROOT'])\n",
        "def finish(*_):\n (root/'query-retired').write_text('observed');sys.exit(0)\n",
        "signal.signal(signal.SIGTERM,finish)\n",
        "(root/'query-ready').touch()\n",
        "while True:time.sleep(.01)\n"
    );
    for name in ["route", "ip"] {
        let path = fixture.root.join(name);
        fs::write(&path, source).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture
        .environment
        .insert("PATH".into(), fixture.root.clone().into_os_string());
    let mut detector = crate::media_ip::NativeDetector {
        root: &fixture.root,
        environment: &fixture.environment,
    };
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    let cancel = async {
        ready(&fixture.root.join("query-ready")).await;
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(
        prepare(
            Suite::Presentation,
            &fixture.environment,
            &fixture.executable,
            &mut signals,
            &mut detector
        ),
        cancel
    );
    assert_eq!(result.err().unwrap().code, 143);
    assert_eq!(
        fs::read_to_string(fixture.root.join("query-retired")).unwrap(),
        "observed"
    );
    assert!(fixture.events().is_empty());
}

#[tokio::test]
async fn actual_fake_harness_receives_literal_arguments_and_runs_chromium_then_firefox() {
    let fixture = Fixture::new("sys.exit(0)");
    execute(
        &fixture.root,
        &fixture.plan(Suite::Presentation),
        &mut idle_signals(),
        CLEANUP_GRACE,
    )
    .await
    .unwrap();
    let events = fixture.events();
    assert_eq!(events.len(), 2);
    for (index, browser) in ["chromium", "firefox"].into_iter().enumerate() {
        assert_eq!(events[index]["browser"], browser);
        assert_eq!(
            events[index]["args"],
            serde_json::json!(["test-server", "node", "web/e2e/presentation.cjs"])
        );
        assert!(
            events[index]["artifacts"]
                .as_str()
                .unwrap()
                .contains("literal $(text)")
        );
        assert_eq!(events[index]["ip"], "192.0.2.1");
        assert_eq!(events[index]["flag"], "1");
    }
}

#[tokio::test]
async fn first_browser_failure_signal_or_launch_error_preserves_status_and_stops_the_plan() {
    for (body, code) in [
        ("sys.exit(37)", 37),
        ("os.kill(os.getpid(),signal.SIGTERM)", 143),
    ] {
        let fixture = Fixture::new(body);
        let error = execute(
            &fixture.root,
            &fixture.plan(Suite::AuthCrossTab),
            &mut idle_signals(),
            CLEANUP_GRACE,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(fixture.events().len(), 1);
    }
    for missing in [false, true] {
        let fixture = Fixture::new("sys.exit(0)");
        if missing {
            fs::remove_file(&fixture.executable).unwrap();
        } else {
            fs::set_permissions(&fixture.executable, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = execute(
            &fixture.root,
            &fixture.plan(Suite::RoomControl),
            &mut idle_signals(),
            CLEANUP_GRACE,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, if missing { 127 } else { 126 });
        assert!(fixture.events().is_empty());
    }
}

#[tokio::test]
async fn cancellation_allows_harness_cleanup_and_never_signals_an_unrelated_child() {
    for code in [130, 143] {
        let delay = if code == 143 { 2.2 } else { 0.0 };
        let fixture = Fixture::new(&format!(
            r#"
def cleanup(signum,frame):
    time.sleep({delay})
    (root/'cleanup').write_text(str(signum))
    sys.exit(0)
signal.signal(signal.SIGINT,cleanup)
signal.signal(signal.SIGTERM,cleanup)
(root/'ready').write_text('ready')
while True: time.sleep(10)
"#
        ));
        let mut unrelated = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let invocations = fixture.plan(Suite::MediaContinuity);
        let started = Instant::now();
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            sender.send(code).unwrap();
        };
        let (result, ()) = tokio::join!(
            execute(&fixture.root, &invocations, &mut signals, CLEANUP_GRACE),
            cancel
        );
        assert_eq!(result.unwrap_err().code, code);
        assert!(fixture.root.join("cleanup").exists());
        assert_eq!(fixture.events().len(), 1);
        if code == 143 {
            assert!(started.elapsed() >= Duration::from_secs(2));
        }
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.start_kill().unwrap();
        unrelated.wait().await.unwrap();
    }
}

#[tokio::test]
async fn forced_cleanup_and_normal_leader_exit_cannot_leave_owned_group_descendants() {
    for hang in [false, true] {
        let fixture = Fixture::new(&format!(
            r#"
signal.signal(signal.SIGTERM,signal.SIG_IGN)
pid=os.fork()
if pid==0:
    time.sleep(0.5)
    (root/'survived').write_text('stray descendant')
    os._exit(0)
(root/'ready').write_text('ready')
if {hang}:
    while True: time.sleep(10)
sys.exit(0)
"#,
            hang = if hang { "True" } else { "False" }
        ));
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let mut invocations = fixture.plan(Suite::AuthLifecycle);
        invocations.truncate(1);
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            if hang {
                sender.send(143).unwrap();
            }
        };
        let (result, ()) = tokio::join!(
            execute(
                &fixture.root,
                &invocations,
                &mut signals,
                Duration::from_millis(50)
            ),
            cancel
        );
        if hang {
            assert_eq!(result.unwrap_err().code, 143);
        } else {
            result.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!fixture.root.join("survived").exists());
    }
}

#[tokio::test]
async fn address_resolution_is_once_for_media_never_for_account_suites_and_failure_stops_setup() {
    for suite in [
        Suite::AuthLifecycle,
        Suite::MediaContinuity,
        Suite::Presentation,
    ] {
        let fixture = Fixture::new("sys.exit(0)");
        let mut detections = 0;
        let detector = async {
            detections += 1;
            Ok(Ipv4Addr::new(192, 0, 2, 1))
        };
        let invocations = prepare(
            suite,
            &fixture.environment,
            &fixture.executable,
            &mut idle_signals(),
            &mut FutureDetector(Some(detector)),
        )
        .await
        .unwrap();
        assert_eq!(detections, usize::from(suite.needs_media_ip()));
        assert_eq!(invocations.len(), 2);
        assert!(fixture.events().is_empty());
    }
    let fixture = Fixture::new("sys.exit(0)");
    let error = prepare(
        Suite::MediaContinuity,
        &fixture.environment,
        &fixture.executable,
        &mut idle_signals(),
        &mut FutureDetector(Some(async { anyhow::bail!("fixture route unavailable") })),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, 1);
    assert!(fixture.events().is_empty());
}

#[tokio::test]
async fn detector_receives_the_same_signal_and_pending_signals_cannot_launch_a_browser() {
    struct Probe(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let fixture = Fixture::new("sys.exit(0)");
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe = Probe(released.clone());
    let (started, observed) = tokio::sync::oneshot::channel();
    let detector = async {
        let _probe = probe;
        started.send(()).unwrap();
        std::future::pending::<Result<Ipv4Addr>>().await
    };
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    let cancel = async {
        observed.await.unwrap();
        sender.send(143).unwrap();
    };
    let mut detector = FutureDetector(Some(detector));
    let (result, ()) = tokio::join!(
        prepare(
            Suite::Presentation,
            &fixture.environment,
            &fixture.executable,
            &mut signals,
            &mut detector
        ),
        cancel
    );
    assert_eq!(result.err().unwrap().code, 143);
    assert!(released.load(Ordering::Relaxed));
    assert!(fixture.events().is_empty());

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(130).unwrap();
    let error = execute(
        &fixture.root,
        &fixture.plan(Suite::AuthLifecycle),
        &mut Signals::injected(receiver),
        CLEANUP_GRACE,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, 130);
    assert!(fixture.events().is_empty());
}
