//! Separate, fixed-purpose proof of the complete prepared-host transaction.
//!
//! The cold launcher remains a setup prerequisite. Only the subsequent actual
//! installed `release maintain` owns the new event epoch and host observations.
pub(super) mod events;
mod files;
pub(super) mod migration;
#[cfg(test)]
mod tests;
pub(super) mod upgrade;

use super::{
    Context, Error, Result, effects::Fixture, first, native::Native, observation, protocol, sources,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) struct Baseline {
    pub pg: String,
    pub app: String,
    pub proxy: String,
    pub image: String,
    pub caddy_image: String,
    pub network: String,
    pub none_network: String,
    pub health: String,
    pub health_pending: BTreeMap<String, u8>,
    pub shell_pg_commands: BTreeMap<String, usize>,
    pub shell_attempt: Option<String>,
    pub app_row: Value,
    pub proxy_row: Value,
    pub attempts: std::collections::BTreeSet<String>,
}

const NAMES: [&str; 28] = [
    "baselinePassed",
    "distributionPublished",
    "reviewedSettingObserved",
    "candidateConsumed",
    "selectionRetained",
    "backupBeforeStopObserved",
    "backupHeaderAndDigestMatched",
    "backupListPassed",
    "databaseContainerUnchanged",
    "exactLedgerObserved",
    "systemToolHashesJoined",
    "naturalRootTrusted",
    "freshActorInventoryJoined",
    "monitorEpochJoined",
    "finiteDaemonHistoryObserved",
    "workloadLockReleased",
    "externalGuardRetained",
    "noObservedExternalPackets",
    "knownFixtureActorsRetired",
    "knownFixtureNetworkRetired",
    "applicationReadinessObserved",
    "sourceCleanObserved",
    "applicationStoppedObserved",
    "sourceDirtyObserved",
    "daemonEffectsUncertain",
    "authenticatedOpsUpgradeObserved",
    "roleNumberingUpgradeObserved",
    "browserProxyUpgradeObserved",
];
pub(super) struct Observed(BTreeMap<String, bool>);
impl Observed {
    fn new() -> Self {
        Self(NAMES.iter().map(|name| ((*name).into(), false)).collect())
    }
    pub(super) fn set(&mut self, name: &str, value: bool) -> Result<()> {
        *self.0.get_mut(name).ok_or(Error::Input)? = value;
        Ok(())
    }
    fn validate(&self, scenario: &str) -> Result<()> {
        require(
            [
                "host-success",
                "host-launcher-refusal",
                "host-migration-upgrade",
                "host-proxy-upgrade",
            ]
            .contains(&scenario),
        )?;
        for (name, value) in &self.0 {
            let expected = match name.as_str() {
                "applicationReadinessObserved" | "sourceCleanObserved" => {
                    scenario != "host-launcher-refusal"
                }
                "applicationStoppedObserved" | "sourceDirtyObserved" => {
                    scenario == "host-launcher-refusal"
                }
                "daemonEffectsUncertain" => false,
                "reviewedSettingObserved" | "candidateConsumed" => scenario != "host-proxy-upgrade",
                "roleNumberingUpgradeObserved" => scenario == "host-migration-upgrade",
                "browserProxyUpgradeObserved" => scenario == "host-proxy-upgrade",
                _ => true,
            };
            require(*value == expected)?;
        }
        Ok(())
    }
}
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
pub(super) async fn prepare_trust(ctx: &mut Context, root: &[u8]) -> Result<()> {
    require(ctx.request.is_host() || ctx.request.is_deploy())?;
    files::trust_install(ctx, root).await
}
struct Run {
    bootstrap: Native,
    raw: Option<i32>,
    report: Option<Value>,
    attempt: Option<PathBuf>,
    observed: Observed,
    upgrade_evidence: Value,
}
impl Run {
    fn new() -> Self {
        Self {
            bootstrap: Native::new(),
            raw: None,
            report: None,
            attempt: None,
            observed: Observed::new(),
            upgrade_evidence: json!({"migrationBefore":null,"migrationAfter":null,"proxy":null}),
        }
    }
    async fn invoke(&mut self, ctx: &mut Context) -> Result<()> {
        ctx.check()?;
        require(
            ctx.control
                .body_end()
                .saturating_duration_since(Instant::now())
                >= Duration::from_secs(2040),
        )?;
        let phase = ctx.control.push_phase(Duration::from_secs(1800))?;
        let mut args = vec![
            "ops".into(),
            "release".into(),
            "maintain".into(),
            "--revision".into(),
            ctx.request.revision.clone(),
        ];
        if ctx.request.proxy_upgrade() {
            args.push("--browser-assets-upgrade".into());
        } else {
            args.extend([
                "--candidate-env".into(),
                format!("{}/app.env.candidate", super::CONFIG),
            ]);
        }
        args.extend([
            "--timeout-seconds".into(),
            "1800".into(),
            "--finalization-seconds".into(),
            "60".into(),
        ]);
        let mut running = match ctx.commands.spawn(
            &mut ctx.control,
            "host-native",
            &args,
            Duration::from_secs(1800),
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = ctx.control.pop_phase(phase);
                return Err(e);
            }
        };
        let pumped = async {
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
        if let Err(e) = pumped {
            ctx.control.record(e);
        }
        let finished = running.finish(&mut ctx.control).await;
        self.raw = running.status();
        let (stdout, _) = running.snapshot();
        let decoded = protocol::decode(stdout);
        let result = (|| {
            pumped?;
            let output = finished?;
            let success = ctx.request.host_success();
            require(output.status == if success { 0 } else { 1 })?;
            let report = decoded?;
            let projection = projection(
                &report,
                success,
                &ctx.request.revision,
                ctx.request.proxy_upgrade(),
            )?;
            let hashes = BTreeMap::from([
                ("opsSha256".into(), ctx.request.binary.sha256.clone()),
                (
                    "launcherSha256".into(),
                    ctx.request.source_pins["ops/ansible/files/deploy-public.sh"].clone(),
                ),
                (
                    "unshareSha256".into(),
                    ctx.commands.checksum("unshare")?.into(),
                ),
                (
                    "timeoutSha256".into(),
                    ctx.commands.checksum("timeout")?.into(),
                ),
            ]);
            observation::require_facts(
                &projection["nativeLauncher"]["facts"],
                if success { 0 } else { 1 },
                if success { "positive" } else { "dirty-source" },
                &hashes,
            )?;
            require(
                projection["nativeLauncher"]["facts"]["launcherRawStatus"].as_i64()
                    == Some(if success { 0 } else { 1 }),
            )?;
            self.attempt = Some(files::attempt(&report)?);
            self.report = Some(projection);
            self.observed.set("systemToolHashesJoined", true)?;
            self.observed.set("backupBeforeStopObserved", true)?;
            Ok(())
        })();
        let fence = ctx.control.pop_phase(phase);
        result.and(fence)?;
        ctx.check()
    }
}

fn evidence(value: &Value, success: bool) -> Result<()> {
    const KEYS: [&str; 4] = [
        "backupPublishedElapsedNs",
        "firstStopDispatchedElapsedNs",
        "firstUnlockElapsedNs",
        "secondLockAcquiredElapsedNs",
    ];
    protocol::exact(value, &KEYS)?;
    let mut last = 0;
    for (index, key) in KEYS.iter().enumerate() {
        if index == 3 && !success {
            require(value[*key].is_null())?;
            continue;
        }
        let elapsed = value[*key].as_u64().ok_or(Error::Refused)?;
        require(elapsed >= last && elapsed <= 1_800_000_000_000)?;
        last = elapsed;
    }
    Ok(())
}
fn projection(report: &Value, success: bool, revision: &str, proxy: bool) -> Result<Value> {
    let changes = if proxy {
        success
            && report.get("environmentChanges").is_none()
            && report["browserAssetProxyUpgraded"] == true
    } else {
        report["environmentChanges"] == json!(["MAX_ROOMS"])
    };
    require(
        report.is_object()
            && report["action"] == "maintain"
            && report["revision"] == revision
            && report["passed"] == success
            && report["launcherPassed"] == success
            && report["phase"] == if success { "complete" } else { "launcher" }
            && changes,
    )?;
    protocol::exact(&report["nativeLauncher"], &["facts"])?;
    evidence(&report["maintenanceEvidence"], success)?;
    Ok(json!({
        "action":"maintain",
        "revision":revision,
        "passed":success,
        "phase":report["phase"],
        "launcherPassed":success,
        "environmentChanges":report["environmentChanges"],
        "nativeLauncher":report["nativeLauncher"],
        "maintenanceEvidence":report["maintenanceEvidence"]
    }))
}
fn upgraded(ctx: &mut Context) -> Result<()> {
    let receipt = protocol::decode(&files::read(
        ctx,
        &Path::new(super::PRIVATE).join("host-upgrade.json"),
        4096,
    )?)?;
    protocol::exact(
        &receipt,
        &[
            "schemaVersion",
            "scope",
            "runId",
            "passed",
            "changed",
            "repeatChanged",
            "currentBinarySha256",
            "previousBinaryExecuted",
        ],
    )?;
    require(
        receipt["schemaVersion"] == 1
            && receipt["scope"] == "PreparedHostOpsUpgrade"
            && receipt["runId"] == ctx.request.run_id
            && receipt["passed"] == true
            && receipt["changed"] == true
            && receipt["repeatChanged"] == false
            && receipt["currentBinarySha256"] == ctx.request.binary.sha256
            && receipt["previousBinaryExecuted"] == false,
    )
}
async fn body(ctx: &mut Context, fixture: &mut Fixture, run: &mut Run) -> Result<()> {
    upgraded(ctx)?;
    run.observed.set("authenticatedOpsUpgradeObserved", true)?;
    let general = ctx.control.push_phase(Duration::from_secs(510))?;
    let mut proxy_candidate = None;
    let prepared = async {
        sources::prepare(ctx).await?;
        fixture.prepare_payload(ctx).await?;
        super::rendered_files(ctx, &fixture.image).await?;
        fixture.setup_network(ctx).await?;
        fixture.setup_database(ctx).await?;
        fixture.prepare_caddy(ctx).await?;
        if ctx.request.proxy_upgrade() {
            ctx.control.checkpoint = "host-proxy-prepare";
            proxy_candidate = Some(fixture.proxy_prepare(ctx)?);
        }
        fixture.setup_complete(ctx).await?;
        ctx.control.checkpoint = "host-distribution";
        files::distribution(ctx)?;
        run.observed.set("distributionPublished", true)
    }
    .await;
    let fence = ctx.control.pop_phase(general);
    prepared.and(fence)?;
    ctx.control.checkpoint = "host-baseline";
    fixture.native_boundary(ctx)?;
    run.bootstrap.invoke(ctx).await?;
    ctx.control.checkpoint = "host-baseline-observe";
    let mut baseline = fixture.host_baseline(ctx).await?;
    if ctx.request.proxy_upgrade() {
        ctx.control.checkpoint = "host-proxy-baseline";
        fixture.proxy_baseline(ctx, &baseline)?;
    }
    run.observed.set("baselinePassed", true)?;
    ctx.control.checkpoint = "host-trust";
    files::trust_ready(ctx).await?;
    run.observed.set("naturalRootTrusted", true)?;
    ctx.control.checkpoint = "host-candidate";
    if ctx.request.migration_upgrade() {
        run.upgrade_evidence["migrationBefore"] =
            fixture.host_migration_prepare(ctx, &baseline).await?;
    }
    let selection = if let Some(candidate) = &proxy_candidate {
        files::Selection {
            environment: candidate.environment.clone(),
            compose: candidate.compose.clone(),
            images: candidate.images.clone(),
        }
    } else {
        files::candidate(ctx)?
    };
    if ctx.request.scenario == "host-launcher-refusal" {
        sources::dirty(ctx).await?;
    }
    if let Some(candidate) = &proxy_candidate {
        fixture.proxy_selected_retire(ctx, candidate)?;
    } else {
        files::retire_selection(ctx)?;
    }
    ctx.control.checkpoint = "host-ready";
    ctx.check()?;
    let ready = json!({
        "schemaVersion":1,"scope":"PreparedHostMaintenanceReady",
        "runId":ctx.request.run_id,"ready":true
    });
    ctx.wire.emit(&ready, &mut ctx.control)?;
    let go = protocol::decode(&ctx.wire.line(&mut ctx.control)?)?;
    protocol::exact(&go, &["schemaVersion", "runId", "go"])?;
    require(go["schemaVersion"] == 1 && go["runId"] == ctx.request.run_id && go["go"] == true)?;
    ctx.wire.no_extra(&mut ctx.control)?;
    ctx.control.advance_host()?;
    fixture.host_boundary(ctx, &mut baseline)?;
    run.observed.set("monitorEpochJoined", true)?;
    ctx.control.checkpoint = "host-native";
    let invoked = run.invoke(ctx).await;
    let post = ctx.control.host_post();
    invoked.and(post)?;
    ctx.control.checkpoint = "host-result";
    // Sixty seconds remain for known daemon retirement (15+15+10), capture
    // retirement (5), retained-file checks and explicit closes. The persistent
    // 120-second ceiling also clamps recovery and settling after this pop.
    let observe = ctx.control.push_phase(Duration::from_secs(60))?;
    let result = async {
        files::selected(ctx, &selection)?;
        if !ctx.request.proxy_upgrade() {
            run.observed.set("reviewedSettingObserved", true)?;
            run.observed.set("candidateConsumed", true)?;
        }
        run.observed.set("selectionRetained", true)?;
        let proxy_row = fixture
            .host_observe(
                ctx,
                &baseline,
                run.attempt.as_deref().ok_or(Error::Refused)?,
                &mut run.observed,
            )
            .await?;
        if ctx.request.migration_upgrade() {
            run.upgrade_evidence["migrationAfter"] =
                fixture.host_migration_observe(ctx, &baseline).await?;
            run.observed.set("roleNumberingUpgradeObserved", true)?;
        }
        if let Some(candidate) = &proxy_candidate {
            ctx.control.checkpoint = "host-proxy-verify";
            fixture.proxy_verify(
                ctx,
                candidate,
                &baseline,
                &proxy_row,
                run.attempt.as_deref().ok_or(Error::Refused)?,
            )?;
            run.upgrade_evidence["proxy"] = json!({
                "schemaVersion":1,"scope":"PreparedHostBrowserProxyUpgrade",
                "legacyCaddyfileSha256":concat!(
                    "ea24dcb24b1285b4529e3aa70083550d",
                    "25431c56cc3a762b8891a53f1306f3ab"
                ),
                "currentCaddyfileSha256":ctx.request.source_pins["Caddyfile"],
                "legacyProxyId":baseline.proxy,"replacementProxyId":proxy_row["Id"],
            });
            run.observed.set("browserProxyUpgradeObserved", true)?;
        }
        ctx.check()
    }
    .await;
    let fence = ctx.control.pop_phase(observe);
    result.and(fence)?;
    ctx.check()
}
fn receipt(
    ctx: &Context,
    run: &Run,
    checkpoint: &str,
    cleanup: bool,
    baseline_pg_evidence: Option<Value>,
) -> Value {
    let passed = ctx.control.first.is_none() && ctx.control.status == 0 && cleanup;
    let final_status = if ctx.control.status != 0 {
        ctx.control.status
    } else if passed {
        0
    } else {
        1
    };
    json!({
        "schemaVersion":1,
        "scope":"PreparedHostMaintenanceGuest",
        "runId":ctx.request.run_id,
        "revision":ctx.request.revision,
        "scenario":ctx.request.scenario,
        "passed":passed,
        "failure":ctx.control.first.map(Error::category),
        "checkpoint":if passed{"complete"}else{checkpoint},
        "finalStatus":final_status,
        "cleanupPassed":cleanup,
        "bootstrapRawExit":run.bootstrap.raw,
        "bootstrapFrame":run.bootstrap.frame,
        "hostRawExit":run.raw,
        "hostReport":run.report,
        "observations":run.observed.0,
        "hostDiagnostic":ctx.host_diagnostic,
        "baselinePgEvidence":baseline_pg_evidence,
        "upgradeEvidence":run.upgrade_evidence,
        "sourcePins":ctx.request.source_pins
    })
}
pub(super) fn failed_receipt(ctx: &Context) -> Value {
    receipt(ctx, &Run::new(), "input", false, None)
}
pub(super) async fn transaction(ctx: &mut Context) -> Value {
    let mut fixture = Fixture::new(&ctx.request);
    let mut run = Run::new();
    if let Err(e) = body(ctx, &mut fixture, &mut run).await {
        ctx.control.record(e);
    }
    if let Err(e) = ctx.control.host_post() {
        ctx.control.record(e);
    }
    let mut checkpoint = ctx.control.checkpoint;
    ctx.control.begin_recovery();
    let result = fixture.finish_body(ctx).await;
    let mut cleanup = first(&mut ctx.control, result);
    ctx.control.begin_settling();
    let result = fixture.finish_local(ctx).await;
    checkpoint = super::local_checkpoint(checkpoint, fixture.primary_capture_checkpoint());
    cleanup &= first(&mut ctx.control, result);
    for (index, original) in ctx.originals.iter_mut().enumerate() {
        if ctx.host_retired.contains(&index) {
            cleanup &= original.closed();
            continue;
        }
        let result = original.finish(&mut ctx.control);
        cleanup &= first(&mut ctx.control, result);
    }
    let commands = ctx.commands.finish(&mut ctx.control).await;
    cleanup &= first(&mut ctx.control, commands);
    for name in [
        "workloadLockReleased",
        "externalGuardRetained",
        "noObservedExternalPackets",
        "knownFixtureActorsRetired",
        "knownFixtureNetworkRetired",
        "daemonEffectsUncertain",
    ] {
        match ctx
            .observed
            .get(name)
            .and_then(|v| run.observed.set(name, v))
        {
            Ok(()) => {}
            Err(e) => ctx.control.record(e),
        }
    }
    cleanup &=
        !ctx.control.cleanup_failed && !ctx.observed.get("daemonEffectsUncertain").unwrap_or(true);
    if !cleanup && ctx.control.first.is_none() {
        ctx.control.record(Error::Close);
    }
    if ctx.control.first.is_none()
        && let Err(e) = run.observed.validate(&ctx.request.scenario)
    {
        ctx.control.record(e);
    }
    receipt(
        ctx,
        &run,
        checkpoint,
        cleanup,
        fixture.baseline_pg_evidence(),
    )
}
