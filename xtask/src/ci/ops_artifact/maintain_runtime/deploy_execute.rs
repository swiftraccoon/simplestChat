//! Complete deploy controller over the shared retained owners and one original clock.
use super::{
    Error, Result, catalog,
    control::Control,
    deploy_options::DeployOptions,
    deploy_projection,
    execute::{Owners, Prepared},
    files::Original,
    vm::Session,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

fn checkpoint(report: &mut Value, name: &'static str) {
    report["checkpoint"] = name.into();
    report["failure"] = name.into();
}
fn pin(original: &Original) -> Value {
    json!({"sha256":original.sha256,"bytes":original.bytes})
}
fn integrity(owners: &mut Owners, control: &mut Control) -> Result<()> {
    owners.ops.as_ref().ok_or(Error::Refused)?.check(control)?;
    owners
        .server
        .as_ref()
        .ok_or(Error::Refused)?
        .check(control)?;
    owners
        .previous
        .as_ref()
        .ok_or(Error::Refused)?
        .check(control)?;
    owners
        .bundle
        .as_ref()
        .ok_or(Error::Refused)?
        .original
        .check(control)?;
    owners
        .runner
        .as_ref()
        .ok_or(Error::Refused)?
        .integrity(control)?;
    owners.vm.as_mut().ok_or(Error::Refused)?.check(control)?;
    control.check()
}
fn request(
    owners: &mut Owners,
    options: &DeployOptions,
    prepared: &Prepared,
    control: &mut Control,
) -> Result<Value> {
    integrity(owners, control)?;
    let runner = owners.runner.as_ref().ok_or(Error::Refused)?;
    let ops = owners.ops.as_ref().ok_or(Error::Refused)?;
    let current = owners.server.as_ref().ok_or(Error::Refused)?;
    let previous = owners.previous.as_ref().ok_or(Error::Refused)?;
    let bundle = owners.bundle.as_ref().ok_or(Error::Refused)?;
    let source_pins = catalog::guest_sources()
        .into_iter()
        .map(|name| {
            let sha = runner
                .sources
                .get(&name)
                .ok_or(Error::Input)?
                .sha256
                .clone();
            Ok((name, sha))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let helper_pins = catalog::HELPERS
        .into_iter()
        .map(|name| {
            let path = if name == "security_vm_ops_guest.py" {
                format!("build/{name}")
            } else {
                format!("ops/ansible/files/{name}")
            };
            Ok((
                name,
                runner
                    .sources
                    .get(&path)
                    .ok_or(Error::Input)?
                    .sha256
                    .clone(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let now = Instant::now();
    let setup = (control.start + control.setup_limit())
        .saturating_duration_since(now)
        .as_secs()
        .saturating_sub(10);
    let body = control
        .whole_body_end()
        .saturating_duration_since(now)
        .as_secs()
        .saturating_sub(20);
    if setup < 30 || body < 3720 {
        return Err(Error::Deadline);
    }
    let value = json!({
        "schemaVersion": 1,
        "scope": "NativeHostDeployRequest",
        "runId": prepared.run,
        "revision": options.common.revision,
        "previousRevision": options.previous.revision,
        "setupSeconds": setup,
        "bodySeconds": body,
        "sourcePins": source_pins,
        "helperPins": helper_pins,
        "binary": pin(ops.pin("simplestchat-ops")?),
        "current": {
            "release.json": pin(current.pin("release.json")?),
            "image.tar": pin(current.pin("image.tar")?),
        },
        "previous": {
            "release.json": pin(previous.pin("release.json")?),
            "image.tar": pin(previous.pin("image.tar")?),
        },
        "sourceBundle": pin(&bundle.original),
        "sourceTree": bundle.tree,
        "postgresImage": prepared.postgres,
        "engineVersion": prepared.engine,
        "caddyImage": prepared.caddy,
    });
    integrity(owners, control)?;
    Ok(value)
}
fn observe(
    report: &mut Value,
    frame: Value,
    expected: &Value,
    ready: Option<&Value>,
) -> Result<bool> {
    // Retain a separately admitted failure observation before any whole-result gate.
    if let Some(capture) = deploy_projection::capture_failure(&frame, expected)? {
        report["failureCapture"] = capture;
    }
    let admitted = deploy_projection::guest(frame, expected, ready)?;
    let passed = admitted["passed"] == true;
    report["deployProof"] = admitted;
    Ok(passed)
}
async fn body(
    owners: &mut Owners,
    options: &DeployOptions,
    control: &mut Control,
    report: &mut Value,
) -> Result<()> {
    let prepared = owners
        .prepare(&options.common, Some(options), control, report)
        .await?;
    checkpoint(report, "deploy-guest-start");
    integrity(owners, control)?;
    let expected = request(owners, options, &prepared, control)?;
    let runner = owners.runner.as_mut().ok_or(Error::Refused)?;
    let vm = owners.vm.as_ref().ok_or(Error::Refused)?;
    owners.session = Some(Session::start(vm, runner, control).await?);
    let session = owners.session.as_mut().ok_or(Error::Refused)?;
    session.send(&expected, control).await?;
    checkpoint(report, "deploy-guest-prepare");
    let setup_end = control.body_end();
    let first = session.line(control, setup_end).await?;
    if first["kind"] == "result" {
        let _ = observe(report, first, &expected, None)?;
        return Err(Error::Refused);
    }
    let ready = deploy_projection::prepared(first, &expected)?;
    report["deployPrepared"] = ready.clone();
    integrity(owners, control)?;
    control.advance()?;
    if control
        .whole_body_end()
        .saturating_duration_since(Instant::now())
        < Duration::from_secs(3720)
    {
        return Err(Error::Deadline);
    }
    checkpoint(report, "deploy-positive-and-owned-rollback");
    let session = owners.session.as_mut().ok_or(Error::Refused)?;
    session
        .send(
            &json!({"schemaVersion":1,"kind":"continue","runId":prepared.run}),
            control,
        )
        .await?;
    let end = control
        .body_end()
        .checked_sub(Duration::from_secs(20))
        .ok_or(Error::Deadline)?;
    let frame = session.line(control, end).await?;
    if !observe(report, frame, &expected, Some(&ready))? {
        return Err(Error::Refused);
    }
    integrity(owners, control)?;
    report["checkpoint"] = "complete".into();
    report["failure"] = Value::Null;
    Ok(())
}
pub(super) async fn run(options: &DeployOptions, control: &mut Control) -> Value {
    let mut report = json!({
        "schemaVersion": 2,
        "scope": "SignedMuslHostDeployKvm",
        "scenario": options.common.scenario,
        "passed": false,
        "cleanupPassed": false,
        "failure": "guard",
        "checkpoint": "guard",
        "failureKind": null,
        "elapsedSeconds": {},
        "opsVerifierCalls": 0,
        "opsCases": [],
        "currentServerVerifierCalls": 0,
        "previousServerVerifierCalls": 0,
        "currentServerMetadataApiCalls": 0,
        "previousServerMetadataApiCalls": 0,
        "authenticatedOpsDistribution": false,
        "authenticatedServerDistribution": false,
        "authenticatedPreviousServerDistribution": false,
        "nativeTarget": "x86_64-unknown-linux-musl",
        "controllerImplementation": "rust-outer-controller",
        "controllerBinaryUse":
            "source-built-gnu-acquisition-and-passive-ELF-inspection-only",
        "guestClientWholeGroupIndependentlyObserved": false,
        "preparationQemu": null,
        "deployPrepared": null,
        "deployProof": null,
        "failureCapture": null,
    });
    let mut owners = Owners::new();
    if let Err(error) = body(&mut owners, options, control, &mut report).await {
        control.record(error);
    }
    if let Err(error) = control.check() {
        control.record(error);
    }
    control.settle();
    let cleanup = owners.finish(control, &mut report).await;
    report["cleanupPassed"] = (cleanup.is_ok() && !control.cleanup_failed).into();
    if let Err(error) = cleanup {
        control.record(error);
    }
    if let Some(error) = control.first {
        report["failureKind"] = error.kind().into();
        if report["failure"].is_null() {
            report["failure"] = "finalization".into();
        }
    }
    report["passed"] = (control.first.is_none() && control.status == 0).into();
    report
}
