//! Closed Docker observations and mutations for one explicitly selected service.

use super::plan::{self, Context};
use crate::{owned_process::Signals, test_env, tooling};
use serde::Deserialize;
use std::{collections::BTreeMap, process::Stdio, time::Duration};

const INSPECT: &str = concat!(
    "{\"id\":{{json .Id}},\"image\":{{json .Image}},",
    "\"networks\":{{json .NetworkSettings.Networks}},",
    "\"ports\":{{json .HostConfig.PortBindings}},",
    "\"status\":{{json .State.Status}},\"running\":{{json .State.Running}},",
    "\"paused\":{{json .State.Paused}},\"restarting\":{{json .State.Restarting}},",
    "\"dead\":{{json .State.Dead}},\"exit\":{{json .State.ExitCode}},",
    "\"oom\":{{json .State.OOMKilled}},\"error\":{{json .State.Error}},",
    "\"health\":{{if .State.Health}}{{json .State.Health.Status}}{{else}}null{{end}}}"
);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Daemon,
    Image,
    Inspect,
    Stop,
    Start,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    pub operation: Operation,
    pub timeout: Duration,
    pub cleanup: bool,
}
pub(super) trait Engine {
    async fn call(&mut self, request: Request, signals: &mut Signals) -> tooling::Result<Vec<u8>>;
    fn settled(&self) -> bool;
}
pub(super) struct Native<'a> {
    pub context: &'a Context,
    pub settled: bool,
}
impl Native<'_> {
    pub(super) fn arguments(&self, operation: Operation) -> Vec<&str> {
        let mut arguments = vec!["--host", plan::ENDPOINT];
        arguments.extend(match operation {
            Operation::Daemon => vec!["info", "--format", "{{json .ID}}"],
            Operation::Image => vec!["image", "inspect", "--format", "{{json .Id}}", plan::IMAGE],
            Operation::Inspect => vec!["inspect", "--format", INSPECT, &self.context.id],
            Operation::Stop => vec!["stop", &self.context.id],
            Operation::Start => vec!["start", &self.context.id],
        });
        arguments
    }
}
impl Engine for Native<'_> {
    async fn call(&mut self, request: Request, signals: &mut Signals) -> tooling::Result<Vec<u8>> {
        let mut command = tooling::command(
            &self.context.root,
            &self.context.environment,
            "docker",
            self.arguments(request.operation),
        );
        let capture = matches!(
            request.operation,
            Operation::Daemon | Operation::Image | Operation::Inspect
        );
        command.stderr(Stdio::null());
        if !capture {
            command.stdout(Stdio::null());
        }
        let result = test_env::helper_with_signals(
            command,
            request.timeout,
            capture.then_some(16 * 1024),
            if request.cleanup { None } else { Some(signals) },
        )
        .await;
        match result {
            Ok((0, output)) => Ok(output),
            Ok((status, _)) => Err(plan::failed(u8::try_from(status).unwrap_or(1))),
            Err(error) => {
                // The helper owns retirement, but its error has no positive
                // settlement projection. Do not manufacture one in the caller.
                self.settled = false;
                let code = error.downcast_ref::<test_env::Stop>().map_or(1, |v| v.code);
                Err(plan::failed(code))
            }
        }
    }
    fn settled(&self) -> bool {
        self.settled
    }
}

#[derive(Deserialize)]
struct Network {
    #[serde(rename = "NetworkID")]
    id: String,
}
#[derive(Deserialize)]
struct Port {
    #[serde(rename = "HostPort")]
    port: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Inspection {
    id: String,
    image: String,
    networks: BTreeMap<String, Network>,
    ports: BTreeMap<String, Vec<Port>>,
    status: String,
    running: bool,
    paused: bool,
    restarting: bool,
    dead: bool,
    exit: i32,
    oom: bool,
    error: String,
    health: Option<String>,
}
impl Inspection {
    pub fn decode(bytes: &[u8], context: &Context, image: &str) -> tooling::Result<Self> {
        if bytes.len() > 16 * 1024 {
            return Err(plan::failed(1));
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| plan::failed(1))?;
        if value.id != context.id
            || plan::image_id(&value.image) != Some(image)
            || !value
                .networks
                .iter()
                .any(|(name, v)| name == &context.network || v.id == context.network)
            || value.ports.len() != 1
            || !value.ports.get("5432/tcp").is_some_and(|ports| {
                !ports.is_empty()
                    && ports.len() <= 2
                    && ports.iter().all(|p| p.port == context.port)
            })
        {
            return Err(plan::failed(1));
        }
        Ok(value)
    }
    pub fn healthy(&self) -> bool {
        self.status == "running"
            && self.running
            && !self.paused
            && !self.restarting
            && !self.dead
            && !self.oom
            && self.error.is_empty()
            && self.health.as_deref() == Some("healthy")
    }
    pub fn stopped(&self) -> bool {
        self.status == "exited"
            && !self.running
            && !self.paused
            && !self.restarting
            && !self.dead
            && !self.oom
            && self.exit == 0
            && self.error.is_empty()
    }
    pub fn may_start(&self) -> bool {
        matches!(self.status.as_str(), "exited" | "created")
            && !self.running
            && !self.paused
            && !self.restarting
            && !self.dead
    }
    pub fn may_settle(&self) -> bool {
        matches!(self.status.as_str(), "running" | "restarting")
            && !self.paused
            && !self.dead
            && !self.oom
    }
}
