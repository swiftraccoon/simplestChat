//! The two live, bounded observers owned by this one disposable guest.

use super::super::{Context, Control, Error, Result, process::Running};
use super::{policy, policy::require};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) type Shared = Arc<Mutex<Monitors>>;

pub(super) fn append_events(
    rows: &mut Vec<Value>,
    buffer: &mut Vec<u8>,
    consumed: &mut usize,
    bytes: &[u8],
) -> Result<()> {
    *consumed = consumed.checked_add(bytes.len()).ok_or(Error::Bounds)?;
    require(*consumed <= 2 * 1024 * 1024)?;
    buffer.extend_from_slice(bytes);
    while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
        require(end > 0 && end <= 131072 && rows.len() < 2048)?;
        let row = policy::decode(&buffer[..end])?;
        policy::object(&row)?;
        rows.push(row);
        buffer.drain(..=end);
    }
    require(buffer.len() <= 131072)
}

#[derive(Default)]
pub(super) struct Monitors {
    pub collector: Option<Collector>,
    pub watch: Option<Watch>,
}
impl Monitors {
    pub fn check(&mut self, control: &mut Control) -> Result<()> {
        if let Some(collector) = &mut self.collector
            && !collector.closed
        {
            collector.check(control)?;
        }
        if let Some(watch) = &mut self.watch
            && !watch.closed
        {
            watch.check(control)?;
        }
        Ok(())
    }
}
pub(super) fn install(ctx: &mut Context, monitors: &Shared) {
    let shared = monitors.clone();
    ctx.commands.set_monitor(Some(Box::new(move |control| {
        shared.lock().map_err(|_| Error::Io)?.check(control)
    })));
}

pub(super) struct Collector {
    actor: Running,
    pub rows: Vec<Value>,
    buffer: Vec<u8>,
    consumed: usize,
    pub marker: Option<String>,
    pub native_start: usize,
    pub closed: bool,
    failure: Option<Error>,
}
impl Collector {
    pub async fn start(ctx: &mut Context) -> Result<Self> {
        ctx.check()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Io)?;
        let since = format!("{}.{:09}", now.as_secs(), now.subsec_nanos());
        let args = [
            "docker".into(),
            "--host".into(),
            "unix:///var/run/docker.sock".into(),
            "events".into(),
            "--since".into(),
            since,
            "--format".into(),
            "{{json .}}".into(),
        ];
        let actor = ctx.commands.spawn(
            &mut ctx.control,
            "daemon-events",
            &args,
            Duration::from_secs(1800),
        )?;
        let mut value = Self {
            actor,
            rows: Vec::new(),
            buffer: Vec::new(),
            consumed: 0,
            marker: None,
            native_start: 0,
            closed: false,
            failure: None,
        };
        let configured = (|| {
            value.actor.set_retirement_grace(Duration::from_secs(1))?;
            value.actor.set_limits(2 * 1024 * 1024, 2 * 1024 * 1024)?;
            ctx.control.check()
        })();
        if let Err(error) = configured {
            ctx.control.record(error);
            if let Err(later) = value.actor.finish(&mut ctx.control).await {
                ctx.control.record(later);
            }
            return Err(error);
        }
        Ok(value)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        append_events(&mut self.rows, &mut self.buffer, &mut self.consumed, bytes)
    }
    pub fn check(&mut self, control: &mut Control) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            control.check()?;
            require(!self.closed)?;
            let chunk = self.actor.pump(control)?;
            self.append(&chunk.stdout)?;
            require(!chunk.stdout_eof && !chunk.stderr_eof && self.actor.running()?)?;
            control.check()
        })();
        if let Err(error) = result {
            self.failure.get_or_insert(error);
        }
        result
    }
    pub fn boundary(&mut self, control: &mut Control) -> Result<()> {
        require(self.marker.is_some())?;
        self.check(control)?;
        self.native_start = self.rows.len();
        Ok(())
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<()> {
        if self.closed {
            return self.failure.map_or(Ok(()), Err);
        }
        self.closed = true;
        let mut first = self.failure;
        let result = async {
            control.check()?;
            let end = Instant::now()
                .checked_add(control.remaining(Duration::from_secs(15))?)
                .ok_or(Error::Deadline)?;
            if self.actor.running()? {
                self.actor.signal(control, rustix::process::Signal::INT)?;
            }
            let allowance = end
                .saturating_duration_since(Instant::now())
                .checked_sub(Duration::from_secs(6))
                .ok_or(Error::Deadline)?;
            let observed = self.actor.wait(control, allowance).await?;
            require(observed.stdout.len() >= self.consumed)?;
            let consumed = self.consumed;
            self.append(&observed.stdout[consumed..])?;
            require(self.buffer.is_empty() && [0, -2, 130].contains(&observed.status))?;
            control.check()
        }
        .await;
        if let Err(error) = result {
            first.get_or_insert(error);
            control.record(error);
        }
        if let Err(error) = self.actor.finish(control).await {
            first.get_or_insert(error);
            control.record(error);
        }
        self.failure = first;
        first.map_or(Ok(()), Err)
    }
}

pub(super) struct Watch {
    actor: Running,
    pub closed: bool,
    failure: Option<Error>,
}
impl Watch {
    pub async fn start(ctx: &mut Context, interface: &str) -> Result<Self> {
        let args = [
            "tcpdump".into(),
            "-p".into(),
            "-n".into(),
            "-U".into(),
            "-i".into(),
            interface.into(),
            "-s".into(),
            "128".into(),
            "-c".into(),
            "4096".into(),
            "-w".into(),
            "-".into(),
            "not (ip and host 10.0.2.2 and tcp port 22) and not (arp and host 10.0.2.2)".into(),
        ];
        let actor = ctx.commands.spawn(
            &mut ctx.control,
            "wire-capture",
            &args,
            Duration::from_secs(1800),
        )?;
        let mut value = Self {
            actor,
            closed: false,
            failure: None,
        };
        let configured = (|| {
            value.actor.set_retirement_grace(Duration::from_secs(1))?;
            value.actor.set_limits(1024 * 1024, 8192)?;
            ctx.control.check()
        })();
        if let Err(error) = configured {
            ctx.control.record(error);
            if let Err(later) = value.actor.finish(&mut ctx.control).await {
                ctx.control.record(later);
            }
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&mut self, control: &mut Control) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = (|| {
            control.check()?;
            require(!self.closed)?;
            let chunk = self.actor.pump(control)?;
            let (out, err) = self.actor.snapshot();
            require(
                out.len() <= 1024 * 1024
                    && err.len() <= 8192
                    && !chunk.stdout_eof
                    && !chunk.stderr_eof
                    && self.actor.running()?,
            )?;
            control.check()
        })();
        if let Err(error) = result {
            self.failure.get_or_insert(error);
        }
        result
    }
    pub fn ready(&self) -> bool {
        self.actor
            .snapshot()
            .1
            .windows(b"listening on".len())
            .any(|v| v == b"listening on")
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<()> {
        if self.closed {
            return self.failure.map_or(Ok(()), Err);
        }
        let mut first = self.failure;
        if let Err(error) = self.check(control) {
            first.get_or_insert(error);
            control.record(error);
        }
        self.closed = true;
        let result = async {
            if self.actor.running()? {
                self.actor.signal(control, rustix::process::Signal::INT)?;
            }
            let observed = self.actor.wait(control, Duration::from_secs(5)).await?;
            require(
                observed.status == 0
                    && observed.stdout.len() == 24
                    && observed.stderr.len() <= 8192,
            )?;
            control.check()
        }
        .await;
        if let Err(error) = result {
            first.get_or_insert(error);
            control.record(error);
        }
        if let Err(error) = self.actor.finish(control).await {
            first.get_or_insert(error);
            control.record(error);
        }
        self.failure = first;
        first.map_or(Ok(()), Err)
    }
}
