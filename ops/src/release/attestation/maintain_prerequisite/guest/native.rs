//! Exactly one retained installed-image run, with unchanged native1050/60 ownership.

use super::{Context, Error, Result, observation};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(super) struct Native {
    pub raw: Option<i32>,
    pub frame: Option<Value>,
    called: bool,
}
impl Native {
    pub fn new() -> Self {
        Self {
            raw: None,
            frame: None,
            called: false,
        }
    }
    pub async fn invoke(&mut self, ctx: &mut Context) -> Result<()> {
        if self.called {
            return Err(Error::Refused);
        }
        self.called = true;
        ctx.check()?;
        if ctx
            .control
            .body_end()
            .saturating_duration_since(Instant::now())
            < Duration::from_secs(1170)
        {
            return Err(Error::Deadline);
        }
        if ctx.request.is_host()
            && ctx
                .control
                .phase_end()
                .saturating_duration_since(Instant::now())
                < Duration::from_secs(1050)
        {
            return Err(Error::Deadline);
        }
        let previous = ctx.control.push_phase(Duration::from_secs(1050))?;
        let arguments = vec![
            "ops".into(),
            "release".into(),
            "maintain-prerequisite".into(),
            "run".into(),
            "--trial".into(),
            ctx.request.trial().into(),
            "--timeout-seconds".into(),
            "1050".into(),
            "--finalization-seconds".into(),
            "60".into(),
        ];
        let spawned = ctx.commands.spawn(
            &mut ctx.control,
            "native",
            &arguments,
            Duration::from_secs(1050),
        );
        let mut running = match spawned {
            Ok(child) => child,
            Err(error) => {
                let _ = ctx.control.pop_phase(previous);
                return Err(error);
            }
        };
        let body = async {
            running.set_limits(131072, 2 * 1024 * 1024)?;
            loop {
                ctx.control.check()?;
                ctx.wire.no_extra(&mut ctx.control)?;
                ctx.commands.monitor(&mut ctx.control)?;
                let chunk = running.pump(&mut ctx.control)?;
                if chunk.stdout_eof && chunk.stderr_eof && !running.running()? {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        }
        .await;
        if let Err(error) = body {
            ctx.control.record(error);
        }
        let finished = running.finish(&mut ctx.control).await;
        self.raw = running.status();
        let (stdout, _stderr) = running.snapshot();
        self.frame = observation::native_frame(stdout).ok();
        let result = (|| {
            body?;
            let output = finished?;
            let expected = match ctx.request.native_scenario() {
                "positive" => 0,
                "dirty-source" => 1,
                "cancel-owned-launcher" => 143,
                "timeout-stopped-launcher" => 124,
                _ => return Err(Error::Input),
            };
            if output.status != expected {
                return Err(Error::NativeStatus);
            }
            let frame = self.frame.as_ref().ok_or(Error::Refused)?;
            let mut hashes = BTreeMap::new();
            hashes.insert("opsSha256".into(), ctx.request.binary.sha256.clone());
            hashes.insert(
                "launcherSha256".into(),
                ctx.request.source_pins["ops/ansible/files/deploy-public.sh"].clone(),
            );
            hashes.insert(
                "unshareSha256".into(),
                ctx.commands.checksum("unshare")?.into(),
            );
            hashes.insert(
                "timeoutSha256".into(),
                ctx.commands.checksum("timeout")?.into(),
            );
            observation::require_native(frame, output.status, &ctx.request, &hashes)?;
            ctx.control.check()
        })();
        let phase = ctx.control.pop_phase(previous);
        result.and(phase)?;
        ctx.check()?;
        ctx.observed.set("systemToolHashesJoined", true)
    }
}
