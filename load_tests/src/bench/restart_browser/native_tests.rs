//! Real owned Node/guest processes and FD0, with no application or browser.

use super::{
    config::Config,
    controller::{self, PortSource, Readiness, Services},
};
use crate::bench::{
    clock::RealClock,
    process::{Child, Command, Control, Exit, Host, NativeHost, Retirement, Stream},
    smoke::config::Ports,
};
use anyhow::Result;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("restart-native-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone)]
struct Observed {
    number: u8,
    exit: Option<Exit>,
    retirement: Option<Retirement>,
    owned_streams: bool,
}
struct Recorded {
    child: Box<dyn Child>,
    number: u8,
    observations: Arc<Mutex<Vec<Observed>>>,
}
#[async_trait::async_trait]
impl Child for Recorded {
    fn pid(&self) -> u32 {
        self.child.pid()
    }
    fn retirement(&self) -> Option<Retirement> {
        self.child.retirement()
    }
    fn owned_streams_closed(&self) -> bool {
        self.child.owned_streams_closed()
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        self.child.take_stdout()
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        self.child.poll().await
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        let result = self.child.stop(grace).await;
        self.observations.lock().unwrap().push(Observed {
            number: self.number,
            exit: result.as_ref().ok().cloned(),
            retirement: self.child.retirement(),
            owned_streams: self.child.owned_streams_closed(),
        });
        result
    }
    fn output(&self) -> &[u8] {
        self.child.output()
    }
    fn errors(&self) -> &[u8] {
        self.child.errors()
    }
}
struct TestHost {
    native: NativeHost,
    fixture: PathBuf,
    bootstrap: PathBuf,
    sequence: Arc<AtomicU8>,
    observations: Arc<Mutex<Vec<Observed>>>,
    mode: &'static str,
}
impl TestHost {
    fn recorded(&self, child: Box<dyn Child>, number: u8) -> Box<dyn Child> {
        Box::new(Recorded {
            child,
            number,
            observations: self.observations.clone(),
        })
    }
}
#[async_trait::async_trait]
impl Host for TestHost {
    fn spawn(&self, mut command: Command) -> Result<Box<dyn Child>> {
        let number = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(number <= 2);
        assert!(command.arguments.is_empty());
        command.environment.insert(
            "FIXTURE_READY".into(),
            self.fixture.join(format!("ready-{number}")).into(),
        );
        command.environment.insert(
            "FIXTURE_RETIRED".into(),
            self.fixture.join(format!("retired-{number}")).into(),
        );
        Ok(self.recorded(self.native.spawn(command)?, number))
    }
    fn spawn_controlled(&self, mut command: Command) -> Result<(Box<dyn Child>, Box<dyn Control>)> {
        assert_eq!(command.arguments.len(), 1);
        let entry = command.arguments.pop().unwrap();
        assert!(entry.ends_with("/web/e2e/restart-browser-phase.cjs"));
        command.arguments = vec![
            self.bootstrap.to_str().unwrap().into(),
            entry,
            self.mode.into(),
        ];
        let (child, control) = self.native.spawn_controlled(command)?;
        Ok((self.recorded(child, 0), control))
    }
    async fn cancelled(&self) -> u8 {
        self.native.cancelled().await
    }
}
struct Ready {
    fixture: PathBuf,
    sequence: Arc<AtomicU8>,
}
#[async_trait::async_trait]
impl Readiness for Ready {
    async fn ready(&self, ports: Ports) -> Result<bool> {
        assert_eq!((ports.http, ports.media), (45101, 45102));
        let number = self.sequence.load(Ordering::SeqCst);
        Ok(self.fixture.join(format!("ready-{number}")).is_file())
    }
}
struct Allocated;
impl PortSource for Allocated {
    fn reserve(&self) -> Result<Ports> {
        Ok(Ports {
            http: 45101,
            media: 45102,
        })
    }
}
fn node() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|path| path.join("node"))
        .find(|path| {
            path.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .expect("Installed Node required by actual inert bridge fixture")
        .canonicalize()
        .unwrap()
}
async fn case(mode: &'static str) -> (u8, Value, Vec<Observed>) {
    let fixture = Fixture::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let node = node();
    let guest = fixture.0.join("guest.cjs");
    let code = concat!(
        "const fs=require('node:fs');let alive=setInterval(()=>{},1000);",
        "process.once('SIGTERM',()=>{fs.writeFileSync(process.env.FIXTURE_RETIRED,'retired');",
        "clearInterval(alive);process.exitCode=0;});",
        "fs.writeFileSync(process.env.FIXTURE_READY,'ready');\n"
    );
    fs::write(&guest, format!("#!{}\n{code}", node.display())).unwrap();
    fs::set_permissions(&guest, fs::Permissions::from_mode(0o700)).unwrap();
    let output = fixture.0.join("output");
    let environment: BTreeMap<OsString, OsString> =
        [("PATH".into(), std::env::var_os("PATH").unwrap_or_default())].into();
    let config = Config::parse(
        &[
            "--binary".into(),
            guest.into(),
            "--browser".into(),
            "chromium".into(),
            "--output".into(),
            output.as_os_str().to_owned(),
            "--node".into(),
            node.into(),
        ],
        root.clone(),
        environment,
    )
    .unwrap();
    let sequence = Arc::new(AtomicU8::new(0));
    let observations = Arc::new(Mutex::new(vec![]));
    let host = TestHost {
        native: NativeHost::default(),
        fixture: fixture.0.clone(),
        bootstrap: root.join("load_tests/src/bench/restart_browser/native_fixture.cjs"),
        sequence: sequence.clone(),
        observations: observations.clone(),
        mode,
    };
    let readiness = Ready {
        fixture: fixture.0.clone(),
        sequence,
    };
    let clock = RealClock::default();
    let services = Services {
        host: &host,
        clock: &clock,
        readiness: &readiness,
        ports: &Allocated,
    };
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        controller::execute(config, &services),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(host.sequence.load(Ordering::SeqCst), 2);
    for number in [1, 2] {
        assert_eq!(
            fs::read(fixture.0.join(format!("retired-{number}"))).unwrap(),
            b"retired"
        );
    }
    let saved: Value =
        serde_json::from_slice(&fs::read(output.join("outcome.json")).unwrap()).unwrap();
    assert_eq!(saved["passed"], result.code == 0);
    let facts = observations.lock().unwrap().clone();
    (result.code, saved, facts)
}

#[tokio::test]
async fn actual_node_phase_and_fd0_drive_both_owned_guest_lifetimes_to_zero() {
    let (code, report, facts) = case("success").await;
    assert_eq!(code, 0);
    assert_eq!(report["passed"], true);
    assert_eq!(report["phase"], "complete");
    assert_eq!(report["draftPreserved"], true);
    assert_eq!(
        report["navigationCountsBeforeRestart"],
        serde_json::json!([1, 1])
    );
    for index in 0..2 {
        assert_eq!(report["recoveryPages"][index]["navigations"], 1);
        assert_eq!(report["pages"][index]["navigations"], 2);
        assert_eq!(
            report["recoveryPages"][index]["received"]["serverRestarting"],
            1
        );
        assert_eq!(report["pages"][index]["received"]["serverRestarting"], 2);
    }
    assert_eq!(
        report["messageDelivery"],
        serde_json::json!({"before":true,"after":true})
    );
    assert_eq!(facts.len(), 3);
    for child in &facts {
        assert!(child.exit.as_ref().unwrap().success());
        assert!(child.owned_streams);
        let retirement = child.retirement.as_ref().unwrap();
        assert!(retirement.group_cleanup_passed);
        assert_eq!(retirement.wait_code, Some(0));
        assert_eq!(retirement.wait_signal, None);
        // FD0 is independently owned by Session, never invented as Child proof.
        assert_eq!(retirement.streams_closed, child.number != 0);
    }
}

#[tokio::test]
async fn queued_trailing_control_bytes_after_three_real_acks_cannot_pass() {
    let (code, report, facts) = case("extra").await;
    assert_ne!(code, 0);
    assert_eq!(report["passed"], false);
    assert_eq!(facts.len(), 3);
    for child in facts {
        assert!(child.owned_streams);
        assert!(child.retirement.as_ref().unwrap().group_cleanup_passed);
        if child.number != 0 {
            assert!(child.exit.as_ref().unwrap().success());
        }
    }
}
