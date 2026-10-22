//! Private files and inert process/wire interfaces only. No sockets or server execute.

use super::{
    config::{Config, Mode, Ports},
    controller::{self, PortSource, Services},
    wire::{self, Event, Socket, Wire},
};
use crate::bench::{
    clock::Clock,
    process::{Child, Command, Exit, Host, Retirement, Stream},
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
    elapsed_reads: AtomicU64,
    pub cancel: AtomicU8,
    pub stopping: AtomicBool,
    pub retired: AtomicBool,
    pub stop_count: AtomicU64,
    pub sockets_closed: AtomicU64,
    pub trace: Mutex<Vec<String>>,
    pub environment: Mutex<Value>,
    pub notify: tokio::sync::Notify,
}
impl State {
    fn flag(&self, key: &str) -> bool {
        self.options[key] == true
    }
    fn number(&self, key: &str, fallback: i64) -> i64 {
        self.options[key].as_i64().unwrap_or(fallback)
    }
    fn cancel(&self, code: u8) {
        self.cancel.store(code, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    fn event(&self, name: &str) {
        self.trace.lock().unwrap().push(name.into());
    }
}
pub(super) struct Fixture {
    pub root: PathBuf,
    pub config: Config,
    pub state: Arc<State>,
}
impl Fixture {
    pub fn new(options: Value, diagnostics: bool) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "smoke fixture literal $(x) {}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let binary = root.join("server");
        fs::write(&binary, b"#!/bin/sh\nexit 99\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        if options["missingBinary"] == true {
            fs::remove_file(&binary).unwrap();
        }
        let config = Config::parse(
            if diagnostics {
                Mode::Diagnostics
            } else {
                Mode::Shutdown
            },
            &[
                "--binary".into(),
                binary.into(),
                "--output".into(),
                root.join("artifacts").into(),
            ],
            root.clone(),
            super::config::Environment::from([
                ("PATH".into(), "/caller/fixed path".into()),
                ("DATABASE_URL".into(), "PRIVATE_DB".into()),
                ("JWT_SECRET".into(), "PRIVATE_SECRET".into()),
                ("TURN_SECRET".into(), "PRIVATE_TURN".into()),
            ]),
        )
        .unwrap();
        let state = Arc::new(State {
            options,
            clock: AtomicU64::new(0),
            elapsed_reads: AtomicU64::new(0),
            cancel: AtomicU8::new(0),
            stopping: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            stop_count: AtomicU64::new(0),
            sockets_closed: AtomicU64::new(0),
            trace: Mutex::new(vec![]),
            environment: Mutex::new(Value::Null),
            notify: tokio::sync::Notify::new(),
        });
        if state.flag("cancelBefore") {
            state.cancel(143)
        }
        Self {
            root,
            config,
            state,
        }
    }
    pub fn services(&self) -> Services<'_> {
        Services {
            host: self,
            clock: self,
            wire: self,
            ports: self,
        }
    }
    pub async fn run(&self) -> Result<controller::Outcome> {
        controller::execute(&self.config, &self.services()).await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[async_trait::async_trait]
impl Clock for Fixture {
    fn elapsed_ms(&self) -> f64 {
        let reads = self.state.elapsed_reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.state.options["clockJumpOnRead"].as_u64() == Some(reads) {
            self.state.clock.store(150000, Ordering::SeqCst);
        }
        self.state.clock.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        "2026-10-22T00:00:00Z".into()
    }
    async fn sleep(&self, value: Duration) {
        self.state
            .clock
            .fetch_add(value.as_millis() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}
#[async_trait::async_trait]
impl PortSource for Fixture {
    fn reserve(&self) -> Result<Ports> {
        self.state.event("reserve");
        Ok(Ports {
            http: 45101,
            media: 45102,
        })
    }
}
pub(super) fn records(options: &Value) -> Vec<u8> {
    if let Some(raw) = options["raw"].as_str() {
        return raw.as_bytes().to_vec();
    }
    let record = json!({"schemaVersion":1,"kind":"operation","operationId":1,"connectionId":1,
        "operation":"join_room","stage":null,"outcome":"ok","startedUs":1,"elapsedUs":100});
    let mut stage = record.clone();
    stage["kind"] = "stage".into();
    stage["stage"] = "room_lock_wait".into();
    stage["outcome"] = "completed".into();
    let mut values = options["records"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![record, stage]);
    if options["noSummary"] != true {
        let mut summary = json!({"schemaVersion":1,"kind":"summary","accepted":values.len(),
            "written":values.len(),"dropped":0,"expired":0,"unfinished":0,"writeFailed":false});
        if let Some(extra) = options["summary"].as_object() {
            for (k, v) in extra {
                summary[k] = v.clone();
            }
        }
        values.push(summary);
    }
    values
        .into_iter()
        .flat_map(|v| format!("{v}\n").into_bytes())
        .collect()
}
struct FakeChild {
    state: Arc<State>,
}
#[async_trait::async_trait]
impl Child for FakeChild {
    fn pid(&self) -> u32 {
        43210
    }
    fn retirement(&self) -> Option<Retirement> {
        if !self.state.retired.load(Ordering::SeqCst) || self.state.flag("unknownRetirement") {
            return None;
        }
        Some(Retirement {
            term_attempted: true,
            term_sent: !self.state.flag("killRefused"),
            kill_attempted: true,
            kill_sent: false,
            leader_terminal_before_cleanup: Some(false),
            leader_grace_expired: Some(self.state.flag("stalled")),
            wait_code: if self.state.options["exitSignal"].is_string() {
                None
            } else {
                Some(self.state.number("exitCode", 0) as i32)
            },
            wait_signal: match self.state.options["exitSignal"].as_str() {
                Some("SIGTERM") => Some(15),
                Some("SIGINT") => Some(2),
                Some(_) => Some(9),
                None => None,
            },
            group_cleanup_passed: !self.state.flag("cleanupFails"),
            streams_closed: !self.state.flag("streamFailure") && !self.state.flag("cleanupFails"),
        })
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        if self.state.flag("earlyExit") {
            return Ok(Some(Exit {
                code: Some(9),
                signal: None,
            }));
        }
        if self.state.flag("pollTimeout") {
            return Err(controller::Failure(124).into());
        }
        Ok(None)
    }
    async fn stop(&mut self, _: Duration) -> Result<Exit> {
        self.state.stop_count.fetch_add(1, Ordering::SeqCst);
        self.state.event("stop_begin");
        self.state.stopping.store(true, Ordering::SeqCst);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        self.state
            .clock
            .fetch_add(self.state.number("drainMs", 0) as u64, Ordering::SeqCst);
        self.state.retired.store(true, Ordering::SeqCst);
        self.state.event("stop_end");
        if let Some(code) = self.state.options["stopStatus"].as_u64() {
            return Err(controller::Failure(code as u8).into());
        }
        if self.state.flag("cleanupFails")
            || self.state.flag("killRefused")
            || self.state.flag("stalled")
        {
            anyhow::bail!("fake_cleanup_failed")
        }
        Ok(Exit {
            code: if self.state.options["exitSignal"].is_string() {
                None
            } else {
                Some(self.state.number("exitCode", 0) as i32)
            },
            signal: self.state.options["exitSignal"].as_str().map(str::to_owned),
        })
    }
    fn output(&self) -> &[u8] {
        b"private bounded stdout"
    }
    fn errors(&self) -> &[u8] {
        b"private bounded stderr"
    }
}
#[async_trait::async_trait]
impl Host for Fixture {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        self.state.event("spawn");
        ensure!(
            command.executable == self.config.binary && command.arguments.is_empty(),
            "fake_selected_server"
        );
        ensure!(
            !command.environment.keys().any(|v| matches!(
                v.to_str(),
                Some("DATABASE_URL" | "JWT_SECRET" | "TURN_SECRET")
            )),
            "fake_env_leak"
        );
        *self.state.environment.lock().unwrap() = Value::Object(
            command
                .environment
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        Value::String(v.to_string_lossy().into_owned()),
                    )
                })
                .collect(),
        );
        if self.state.flag("spawnError") {
            anyhow::bail!("fake_spawn_failure")
        }
        if let Some(path) = command
            .environment
            .get(&std::ffi::OsString::from("DIAGNOSTICS_PATH"))
            && !self.state.flag("missing")
        {
            fs::write(path, records(&self.state.options))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(Box::new(FakeChild {
            state: self.state.clone(),
        }))
    }
    async fn cancelled(&self) -> u8 {
        loop {
            if self.state.flag("cancelPublication")
                && self.config.output.join("diagnostics-smoke.json").exists()
            {
                self.state.cancel(143);
            }
            let notified = self.state.notify.notified();
            let code = self.state.cancel.load(Ordering::SeqCst);
            if code != 0 {
                return code;
            }
            notified.await;
        }
    }
}
struct FakeSocket {
    state: Arc<State>,
    peer: bool,
    stage: u8,
}
#[async_trait::async_trait]
impl Socket for FakeSocket {
    async fn join(&mut self, room: &str) -> Result<()> {
        ensure!(room.starts_with("shutdown-smoke-"), "fake_room");
        self.state.event("join");
        Ok(())
    }
    async fn peer_close(&mut self) -> Result<()> {
        self.state.event("peer_close");
        Ok(())
    }
    async fn next(&mut self) -> Result<Event> {
        if self.peer {
            if self.state.flag("noPeerClose") {
                return Err(controller::Failure(124).into());
            }
            return Ok(Event::Closed {
                code: self.state.number("peerCloseCode", 1000) as u16,
                clean: self.state.options["peerCloseClean"] != false,
            });
        }
        if self.stage == 0 {
            self.stage = 1;
            if self.state.flag("neverJoin") {
                return Err(controller::Failure(124).into());
            }
            if self.state.flag("admissionError") {
                anyhow::bail!("fake_admission")
            }
            return Ok(Event::Joined);
        }
        while !self.state.stopping.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        if self.stage == 1 {
            self.stage = 2;
            self.state.event("restart_observed");
            if self.state.flag("wrongReason") || self.state.flag("terminalRoom") {
                anyhow::bail!("fake_lifecycle")
            }
            return Ok(if self.state.flag("noRestartNotice") {
                Event::Other
            } else {
                Event::Restarting
            });
        }
        if self.state.flag("noClose") {
            return Err(controller::Failure(124).into());
        }
        self.state.event("wire_closed");
        Ok(Event::Closed {
            code: self.state.number("closeCode", 1001) as u16,
            clean: self.state.options["cleanClose"] != false,
        })
    }
    async fn finish(self: Box<Self>) -> Result<()> {
        self.state.sockets_closed.fetch_add(1, Ordering::SeqCst);
        self.state.event("socket_close");
        if self.state.flag("closeThrows") {
            anyhow::bail!("fake_close_failed")
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl Wire for Fixture {
    async fn ready(&self, _: Ports) -> Result<bool> {
        self.state.event("ready");
        self.state.clock.fetch_add(
            self.state.number("readyElapsed", 0) as u64,
            Ordering::SeqCst,
        );
        if self.state.flag("cancelPolling") {
            self.state.cancel(143)
        }
        if self.state.flag("fetchError") || self.state.number("status", 200) != 200 {
            self.state.clock.fetch_add(1000, Ordering::SeqCst);
            return Ok(false);
        }
        if self.state.flag("readyJsonError") {
            anyhow::bail!("fake_ready_json")
        }
        if !self.state.options["readyBody"].is_null() {
            return wire::ready_body(&serde_json::to_vec(&self.state.options["readyBody"])?);
        }
        Ok(true)
    }
    async fn connect(&self, _: Ports) -> Result<Box<dyn Socket>> {
        let mut trace = self.state.trace.lock().unwrap();
        let peer = !trace.iter().any(|v| v == "connect");
        trace.push("connect".into());
        drop(trace);
        if self.state.flag("neverOpen") {
            return Err(controller::Failure(124).into());
        }
        if self.state.flag("socketError") {
            anyhow::bail!("fake_websocket")
        }
        self.state.clock.fetch_add(
            self.state.number("connectElapsed", 0) as u64,
            Ordering::SeqCst,
        );
        if self.state.flag("cancelConnect") {
            self.state.cancel(143);
        }
        Ok(Box::new(FakeSocket {
            state: self.state.clone(),
            peer,
            stage: 0,
        }))
    }
}
