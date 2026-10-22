//! Inert host and virtual clock for the whole owner. Selected files never run.
use super::{
    config::Config,
    controller::{self, PortSource, Readiness, Services},
};
use crate::bench::{
    clock::Clock,
    process::{Child, Command, Control, Exit, Host, Retirement, Stream},
    smoke::config::Ports,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

pub(super) struct State {
    pub options: Value,
    pub clock: AtomicU64,
    pub cancel: AtomicU8,
    pub servers: AtomicU8,
    pub sequence: AtomicU8,
    pub node_started: AtomicBool,
    pub control_closed: AtomicBool,
    pub trace: Mutex<Vec<String>>,
    pub commands: Mutex<Vec<Value>>,
    pub notify: tokio::sync::Notify,
    run: String,
    output: Vec<u8>,
    phases: u8,
    failed: bool,
    tail_start: AtomicU64,
}
impl State {
    pub fn flag(&self, name: &str) -> bool {
        self.options[name] == true
    }
    pub fn event(&self, event: impl Into<String>) {
        self.trace.lock().unwrap().push(event.into());
    }
    pub fn cancel(&self, code: u8) {
        self.cancel.store(code, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}
pub(super) struct Fixture {
    pub root: PathBuf,
    pub state: Arc<State>,
    pub config: Option<Config>,
}
impl Fixture {
    pub fn new(options: Value) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "restart fixture literal $(x) {}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["server", "node"] {
            fs::write(root.join(name), b"#!/bin/sh\nexit 99\n").unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        for name in super::files::NAMES {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"// inert selected source\n").unwrap();
        }
        let config = Config::parse(
            &[
                "--binary".into(),
                root.join("server").into(),
                "--output".into(),
                root.join("artifacts").into(),
                "--browser".into(),
                options["browser"].as_str().unwrap_or("chromium").into(),
                "--node".into(),
                root.join("node").into(),
            ],
            root.clone(),
            [
                ("PATH".into(), "/caller/fixed path".into()),
                ("HOME".into(), "/caller/home".into()),
                ("DATABASE_URL".into(), "PRIVATE_DB".into()),
                ("NODE_OPTIONS".into(), "PRIVATE_OPTIONS".into()),
            ]
            .into(),
        )
        .unwrap();
        let (observation, phases, failed, cleanup) = browser(&options);
        let record = json!({"browserName":config.browser,"launchMode":"browser-server",
            "completed":true,"passed":true,"gracefulCloseCompleted":true,
            "forceKillRequested":false,"exitStatusObserved":true,"exitCode":0,"signalCode":null});
        let receipt = json!({"schemaVersion":1,"runId":config.run_id,"completed":true,
            "phasePassed":!failed,"browserCleanupPassed":cleanup,"cancelled":null,
            "browserCount":if cleanup{1}else{0},
            "records":if cleanup{vec![record]}else{vec![]},"observation":observation});
        let output = if let Some(raw) = options["rawReceipt"].as_str() {
            raw.as_bytes().to_vec()
        } else if options["missingReceipt"] == true {
            vec![]
        } else {
            serde_json::to_vec(&receipt).unwrap()
        };
        let state = Arc::new(State {
            options,
            clock: AtomicU64::new(0),
            cancel: AtomicU8::new(0),
            servers: AtomicU8::new(0),
            sequence: AtomicU8::new(0),
            node_started: AtomicBool::new(false),
            control_closed: AtomicBool::new(false),
            trace: Mutex::new(vec![]),
            commands: Mutex::new(vec![]),
            notify: tokio::sync::Notify::new(),
            run: config.run_id.clone(),
            output,
            phases,
            failed,
            tail_start: AtomicU64::new(0),
        });
        Self {
            root,
            state,
            config: Some(config),
        }
    }
    pub async fn run(&mut self) -> Result<controller::Outcome> {
        let config = self.config.take().unwrap();
        controller::execute(
            config,
            &Services {
                host: self,
                clock: self,
                readiness: self,
                ports: self,
            },
        )
        .await
    }
    fn child(&self, command: Command, number: u8) -> Box<dyn Child> {
        self.state.event(format!("spawn_{number}"));
        let environment = command
            .environment
            .into_iter()
            .map(|(key, value)| (key.into_string().unwrap(), value.into_string().unwrap()))
            .collect::<std::collections::BTreeMap<_, _>>();
        self.state
            .commands
            .lock()
            .unwrap()
            .push(json!({"number":number,
            "executable":command.executable,"arguments":command.arguments,
            "directory":command.directory,"environment":environment,
            "timeout":command.timeout.as_secs(),"stdout":command.stdout_limit,
            "stderr":command.stderr_limit}));
        Box::new(FakeChild {
            state: self.state.clone(),
            number,
            stopped: false,
            output: if number == 0 {
                self.state.output.clone()
            } else {
                b"out\n".to_vec()
            },
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(config) = self.config.take() {
            let _ = config.finish();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[async_trait::async_trait]
impl Clock for Fixture {
    fn elapsed_ms(&self) -> f64 {
        if self.state.flag("latePublication") && self.root.join("artifacts/outcome.json").exists() {
            return 120000.0;
        }
        self.state.clock.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        if self.state.flag("cancelBeforePublication")
            && self.state.node_started.load(Ordering::SeqCst)
        {
            self.state.cancel(143);
        }
        "2026-10-22T00:00:00Z".into()
    }
    async fn sleep(&self, duration: Duration) {
        self.state
            .clock
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}
impl PortSource for Fixture {
    fn reserve(&self) -> Result<Ports> {
        Ok(Ports {
            http: 45101,
            media: 45102,
        })
    }
}
#[async_trait::async_trait]
impl Readiness for Fixture {
    async fn ready(&self, _ports: Ports) -> Result<bool> {
        if self.state.servers.load(Ordering::SeqCst) == 2 && self.state.flag("cancelStart") {
            self.state.cancel(143);
        }
        Ok(!self.state.flag("neverReady"))
    }
}
#[async_trait::async_trait]
impl Host for Fixture {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        let number = self.state.servers.fetch_add(1, Ordering::SeqCst) + 1;
        ensure!(
            !(number == 2 && self.state.flag("startFailure")),
            "inert_spawn_failed"
        );
        Ok(self.child(command, number))
    }
    fn spawn_controlled(&self, command: Command) -> Result<(Box<dyn Child>, Box<dyn Control>)> {
        Ok((
            self.child(command, 0),
            Box::new(FakeControl {
                state: self.state.clone(),
                sent: 0,
            }),
        ))
    }
    async fn cancelled(&self) -> u8 {
        loop {
            let changed = self.state.notify.notified();
            let value = self.state.cancel.load(Ordering::SeqCst);
            if value != 0 {
                return value;
            }
            changed.await;
        }
    }
}
struct FakeControl {
    state: Arc<State>,
    sent: u8,
}
#[async_trait::async_trait]
impl Control for FakeControl {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let sequence = self.state.sequence.load(Ordering::SeqCst);
        if sequence >= self.state.phases {
            return Ok(0);
        }
        if self.sent > sequence {
            return std::future::pending().await;
        }
        let mut bytes = serde_json::to_vec(&json!({"schemaVersion":1,"runId":self.state.run,
            "sequence":sequence+1,"phase":super::protocol::PHASES[sequence as usize]}))
        .unwrap();
        // json!'s sorted key order is not the wire's canonical struct order.
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        bytes = format!(
            "{{\"schemaVersion\":1,\"runId\":\"{}\",\"sequence\":{},\"phase\":\"{}\"}}\n",
            value["runId"].as_str().unwrap(),
            sequence + 1,
            value["phase"].as_str().unwrap()
        )
        .into_bytes();
        assert!(bytes.len() <= buffer.len());
        buffer[..bytes.len()].copy_from_slice(&bytes);
        self.sent = sequence + 1;
        Ok(bytes.len())
    }
    async fn write(&mut self, _bytes: &[u8]) -> Result<()> {
        anyhow::bail!("unbounded_write_unused")
    }
    async fn write_bounded(&mut self, bytes: &[u8], limit: Duration) -> Result<()> {
        assert_eq!(limit, Duration::from_secs(2));
        self.state.event("ack_begin");
        if self.state.flag("heldAck") {
            self.state.cancel(143);
            while !self.state.node_started.load(Ordering::SeqCst) {
                let changed = self.state.notify.notified();
                if self.state.node_started.load(Ordering::SeqCst) {
                    break;
                }
                changed.await;
            }
        }
        if self.state.flag("ackFails") {
            self.state.cancel(143);
            anyhow::bail!("inert_ack_failed");
        }
        let value: Value = serde_json::from_slice(bytes)?;
        self.state
            .sequence
            .store(value["sequence"].as_u64().unwrap() as u8, Ordering::SeqCst);
        self.state.event("ack_end");
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<()> {
        self.state.control_closed.store(true, Ordering::SeqCst);
        self.state.event("control_closed");
        ensure!(
            !self.state.flag("controlCloseFails"),
            "inert_control_close_failed"
        );
        Ok(())
    }
}
struct FakeChild {
    state: Arc<State>,
    number: u8,
    stopped: bool,
    output: Vec<u8>,
}
#[async_trait::async_trait]
impl Child for FakeChild {
    fn pid(&self) -> u32 {
        100 + u32::from(self.number)
    }
    fn retirement(&self) -> Option<Retirement> {
        if !self.stopped || self.state.flag("unknownRetirement") {
            return None;
        }
        Some(Retirement {
            term_attempted: true,
            term_sent: true,
            kill_attempted: true,
            kill_sent: true,
            leader_terminal_before_cleanup: Some(self.number == 0),
            leader_grace_expired: Some(false),
            wait_code: Some(0),
            wait_signal: None,
            group_cleanup_passed: !self.state.flag("cleanupFails"),
            streams_closed: self.number != 0 && !self.state.flag("streamFails"),
        })
    }
    fn owned_streams_closed(&self) -> bool {
        self.stopped && !self.state.flag("streamFails")
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        if self.number != 0 {
            return Ok(None);
        }
        if self.state.sequence.load(Ordering::SeqCst) >= self.state.phases
            || self.state.flag("heldNodeExit")
        {
            if self.state.flag("cancelTerminal") {
                self.state.cancel(143);
            }
            Ok(Some(Exit {
                code: if self.state.options["nodeSignal"].is_string() {
                    None
                } else {
                    Some(
                        self.state.options["nodeExit"]
                            .as_i64()
                            .unwrap_or(i64::from(self.state.failed)) as i32,
                    )
                },
                signal: self.state.options["nodeSignal"].as_str().map(str::to_owned),
            }))
        } else {
            Ok(None)
        }
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        assert_eq!(
            grace,
            Duration::from_secs(if self.number == 0 { 35 } else { 20 })
        );
        self.state.event(format!("stop_begin_{}", self.number));
        if self.number == 0 {
            self.state.node_started.store(true, Ordering::SeqCst);
            self.state.notify.notify_waiters();
        } else if self.state.flag("heldStop") || self.state.flag("heldNodeExit") {
            self.state
                .tail_start
                .store(self.state.clock.load(Ordering::SeqCst), Ordering::SeqCst);
            if self.state.flag("heldStop") {
                self.state.cancel(143);
            }
            while !self.state.node_started.load(Ordering::SeqCst) {
                let changed = self.state.notify.notified();
                if self.state.node_started.load(Ordering::SeqCst) {
                    break;
                }
                changed.await;
            }
        }
        if self.state.flag("tailBounds") {
            let start = self.state.tail_start.load(Ordering::SeqCst);
            self.state.clock.fetch_max(
                start + if self.number == 0 { 76000 } else { 66000 },
                Ordering::SeqCst,
            );
        }
        if self.number == 0
            && let Some(ms) = self.state.options["nodeStopAt"].as_u64()
        {
            self.state.clock.store(ms, Ordering::SeqCst);
        }
        self.stopped = true;
        self.state.event(format!("stop_end_{}", self.number));
        ensure!(
            !self.state.flag("cleanupFails")
                && !self.state.flag("stopFails")
                && !(self.number == 0 && self.state.flag("nodeStopFails")),
            "inert_stop_failed"
        );
        Ok(Exit {
            code: Some(0),
            signal: None,
        })
    }
    fn output(&self) -> &[u8] {
        &self.output
    }
    fn errors(&self) -> &[u8] {
        b"err\n"
    }
}

/// Browser observations are inputs supplied by the inert Node role. The actual
/// unchanged JavaScript oracle independently checks this entire selected row.
fn browser(options: &Value) -> (Value, u8, bool, bool) {
    let flag = |key: &str| options[key] == true;
    let mut observation = json!({"browser":options["browser"].as_str().unwrap_or("chromium"),
        "phase":"complete","pages":[],"captureCalls":[],"pageErrors":0,
        "blockedExternalRequests":0,"browserVersion":"inert-browser-1",
        "rejoinMilliseconds":0,"draftPreserved":true,"messageDelivery":{"before":true,"after":true},
        "navigationCountsBeforeRestart":[1,1]});
    let early = flag("launchFailure") || flag("connectFailure");
    let setup = early
        || flag("noInitialJoin")
        || flag("pageError")
        || flag("external")
        || flag("foreignSocket");
    let before = flag("dropBefore");
    let notice = flag("noRestartNotice");
    let restart = notice || flag("noRejoin") || flag("navigateOnRestart") || flag("lostDraft");
    let after = flag("dropAfter")
        || flag("terminalRoom")
        || flag("capture")
        || flag("malformed")
        || flag("invalidCapture");
    let failed = setup
        || before
        || restart
        || after
        || flag("lateCleanupExternal")
        || flag("lateCleanupPageError");
    let phases = if setup || before {
        0
    } else if notice {
        1
    } else if restart || after {
        2
    } else {
        3
    };
    observation["phase"] = if setup {
        "setup"
    } else if before {
        "before_restart"
    } else if restart {
        "restart"
    } else if after {
        "after_restart"
    } else {
        "complete"
    }
    .into();
    let count = if early {
        0
    } else if setup {
        1
    } else {
        2
    };
    let rejoined = !(setup || before || notice || flag("noRejoin"));
    let mut pages = vec![];
    for _ in 0..count {
        let mut received = json!({});
        let mut sent = json!({});
        if !flag("foreignSocket") {
            if !flag("noInitialJoin") {
                received["roomJoined"] = if rejoined { 2 } else { 1 }.into();
            }
            if !notice {
                received["serverRestarting"] = 1.into();
            }
            sent["joinRoom"] = 1.into();
        }
        if flag("terminalRoom") {
            received["roomClosed"] = 1.into();
        }
        pages.push(
            json!({"sockets":if flag("foreignSocket"){0}else if rejoined{2}else{1},
            "navigations":if flag("navigateOnRestart"){2}else{1},"received":received,"sent":sent,
            "malformedFrames":u8::from(flag("malformed"))}),
        );
    }
    observation["pages"] = pages.into();
    observation["pageErrors"] = u8::from(flag("pageError") || flag("lateCleanupPageError")).into();
    observation["blockedExternalRequests"] =
        u8::from(flag("external") || flag("foreignSocket") || flag("lateCleanupExternal")).into();
    if early {
        observation["browserVersion"] = Value::Null;
    }
    if !rejoined {
        observation["rejoinMilliseconds"] = Value::Null;
    }
    if setup || before || restart {
        observation["draftPreserved"] = Value::Null;
    }
    if setup || before || restart || after {
        observation["messageDelivery"] = Value::Null;
    }
    if setup || before {
        observation["navigationCountsBeforeRestart"] = Value::Null;
    }
    if !(setup || before || restart || flag("dropAfter")) {
        observation["captureCalls"] = json!([{"user":u8::from(flag("capture")),"display":0},
            {"user":u8::from(flag("capture")),"display":0}]);
    }
    (observation, phases, failed, !flag("launchFailure"))
}
