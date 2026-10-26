//! Complete fixed controller body. Every admitted owner remains registered until finalization.

use super::{
    Error, Result, auth, catalog,
    control::{Control, DEBT, SETUP},
    decode,
    files::{self, Original},
    options::Options,
    process::Runner,
    projection, source,
    vm::{self, Session, Vm},
};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn checkpoint(report: &mut Value, name: &str) {
    report["checkpoint"] = name.into();
    report["failure"] = name.into();
}

#[derive(Clone, Copy)]
enum InstallCheckpoint {
    Inventory,
    StageCommand,
    StageRecap,
    StageIntegrity,
    GuestCommand,
    GuestBounds,
    GuestDecode,
    GuestProjection,
    GuestResult,
    Integrity,
}
impl InstallCheckpoint {
    fn name(self) -> &'static str {
        match self {
            Self::Inventory => "inventory",
            Self::StageCommand => "stage-command",
            Self::StageRecap => "stage-recap",
            Self::StageIntegrity => "stage-integrity",
            Self::GuestCommand => "guest-command",
            Self::GuestBounds => "guest-bounds",
            Self::GuestDecode => "guest-decode",
            Self::GuestProjection => "guest-projection",
            Self::GuestResult => "guest-result",
            Self::Integrity => "integrity",
        }
    }
}
fn install_checkpoint(report: &mut Value, checkpoint: InstallCheckpoint, raw: Option<i32>) {
    report["installDiagnostic"] = json!({"checkpoint":checkpoint.name(),"rawExit":raw});
}
pub(super) fn install_command_error(report: &mut Value, error: Error) -> Error {
    // Only the actual observed child status may replace this diagnostic's unknown value.
    if let Error::Child(status) = error {
        report["installDiagnostic"]["rawExit"] = status.into();
    }
    error
}
fn clear_install_diagnostic(report: &mut Value) {
    if let Some(object) = report.as_object_mut() {
        object.remove("installDiagnostic");
    }
}
pub(super) fn observe_ops_receipt(
    report: &mut Value,
    bytes: &[u8],
    run_id: &str,
    raw: i32,
) -> Result<()> {
    install_checkpoint(report, InstallCheckpoint::GuestBounds, Some(raw));
    if bytes.len() > 65536 {
        return Err(Error::Refused);
    }
    install_checkpoint(report, InstallCheckpoint::GuestDecode, Some(raw));
    let value = decode(bytes)?;
    install_checkpoint(report, InstallCheckpoint::GuestProjection, Some(raw));
    let receipt = projection::admit_ops(value, run_id, raw)?;
    report["opsCases"] = receipt.cases().clone();
    report["install"] = receipt.install().clone();
    install_checkpoint(report, InstallCheckpoint::GuestResult, Some(raw));
    if let Some(failure) = receipt.failure() {
        report["opsGuestFailure"] = failure.into();
    }
    receipt.require_success()?;
    clear_install_diagnostic(report);
    Ok(())
}

fn timing(report: &mut Value, name: &str, control: &mut Control) -> Result<()> {
    control.check()?;
    let elapsed = Instant::now()
        .checked_duration_since(control.start)
        .ok_or(Error::Deadline)?;
    if elapsed >= super::control::WHOLE {
        return Err(Error::Deadline);
    }
    report["elapsedSeconds"][name] = json!((elapsed.as_secs_f64() * 1000.0).round() / 1000.0);
    control.check()
}
fn identity(control: &mut Control) -> Result<String> {
    control.check()?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open("/dev/urandom")
        .map_err(|_| Error::Io)?;
    let mut random = [0u8; 16];
    let read = (|| {
        let mut at = 0;
        while at < random.len() {
            control.check()?;
            match file.read(&mut random[at..]) {
                Ok(0) => return Err(Error::Io),
                Ok(count) => at += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Error::Io),
            }
            control.check()?;
        }
        Ok(())
    })();
    if let Err(error) = read {
        control.record(error);
    }
    let close = files::close(file, control);
    read.and(close)?;
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    control.check()?;
    Ok(files::hex(&random))
}
fn fixture_config(runner: &Runner, control: &mut Control) -> Result<(String, String, String)> {
    let data = runner.source("ops/ansible/group_vars/benchmark_hosts.yml", control)?;
    let value: Value =
        serde_saphyr::from_str(std::str::from_utf8(&data).map_err(|_| Error::Input)?)
            .map_err(|_| Error::Input)?;
    let image = value["scpub_postgres_image"].as_str().ok_or(Error::Input)?;
    let engine = value["scbench_docker_engine_version"]
        .as_str()
        .ok_or(Error::Input)?;
    let version = engine
        .split_once(':')
        .and_then(|(_, tail)| tail.split_once('-'))
        .map(|(version, _)| version)
        .ok_or(Error::Input)?;
    let pattern = regex::Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$").map_err(|_| Error::Input)?;
    if !image.contains("@sha256:") || !pattern.is_match(version) {
        return Err(Error::Input);
    }
    // Fixed concatenated Python string literals are data only; no Python evaluator runs.
    let caddy = runner.source("build/caddy_probe_guest.py", control)?;
    let text = std::str::from_utf8(&caddy).map_err(|_| Error::Input)?;
    let expression =
        regex::Regex::new(r#"(?s)\nIMAGE = \(\s*"([^"]+)"\s*"([^"]+)"\s*"([^"]+)"\s*\)"#)
            .map_err(|_| Error::Input)?;
    let found = expression.captures(text).ok_or(Error::Input)?;
    let caddy = format!("{}{}{}", &found[1], &found[2], &found[3]);
    if !caddy.contains("@sha256:") {
        return Err(Error::Input);
    }
    control.check()?;
    Ok((image.into(), version.into(), caddy))
}
fn quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    {
        text.into()
    } else {
        format!("'{}'", text.replace('\'', "'\"'\"'"))
    }
}
fn ssh_options(work: &Path) -> Vec<String> {
    let mut values = vec!["-F".into(), "/dev/null".into()];
    for value in [
        "BatchMode=yes".into(),
        "IdentitiesOnly=yes".into(),
        "IdentityAgent=none".into(),
        "ForwardAgent=no".into(),
        "PasswordAuthentication=no".into(),
        "KbdInteractiveAuthentication=no".into(),
        "StrictHostKeyChecking=yes".into(),
        "GlobalKnownHostsFile=/dev/null".into(),
        format!("UserKnownHostsFile={}", work.join("known_hosts").display()),
        "ProxyCommand=none".into(),
        "ProxyJump=none".into(),
        "ControlMaster=no".into(),
        "ControlPath=none".into(),
        "ControlPersist=no".into(),
        "ConnectTimeout=5".into(),
        "ConnectionAttempts=1".into(),
        "ServerAliveInterval=5".into(),
        "ServerAliveCountMax=2".into(),
    ] {
        values.extend(["-o".into(), value]);
    }
    values
}
fn inventory(
    runner: &mut Runner,
    control: &mut Control,
    vm: &Vm,
    options: &Options,
    run: &str,
) -> Result<PathBuf> {
    let ssh = ssh_options(&runner.work)
        .iter()
        .map(|part| quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    let value = json!({"benchmark_hosts":{"hosts":{"vmfixture":{
        "ansible_host":"127.0.0.1","ansible_port":vm.port(),"ansible_user":"fixture",
        "ansible_python_interpreter":"/usr/bin/python3","ansible_ssh_private_key_file":
            runner.work.join("client"),"ansible_ssh_args":ssh,"ansible_become":true,
        "vm_fixture_run_id":run,"vm_fixture_revision":options.revision}}}});
    let path = runner.work.join("inventory.json");
    files::write(&path, &auth::encoded(&value)?, control)?;
    let original = Original::open(&path, 16384, control)?;
    runner
        .sources
        .insert("generated/inventory.json".into(), original);
    Ok(path)
}
fn recap(data: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(data).map_err(|_| Error::Refused)?;
    let re = regex::Regex::new(concat!(
        r"(?m)^vmfixture\s+:\s+ok=(\d+)\s+changed=(\d+)\s+unreachable=(\d+)",
        r"\s+failed=(\d+)\s+skipped=(\d+)\s+rescued=(\d+)\s+ignored=(\d+)\s*$"
    ))
    .map_err(|_| Error::Input)?;
    let matches = re.captures_iter(text).collect::<Vec<_>>();
    let hosts = regex::Regex::new(r"(?m)^(\S+)\s+:\s+ok=").map_err(|_| Error::Input)?;
    let hosts = hosts
        .captures_iter(text)
        .map(|row| row[1].to_string())
        .collect::<Vec<_>>();
    if matches.len() != 1 || hosts != ["vmfixture"] {
        return Err(Error::Refused);
    }
    let counts = (1..=7)
        .map(|index| matches[0][index].parse::<u64>().map_err(|_| Error::Refused))
        .collect::<Result<Vec<_>>>()?;
    if counts[0] == 0 || [2, 3, 5, 6].iter().any(|index| counts[*index] != 0) {
        return Err(Error::Refused);
    }
    Ok(())
}

struct Owners {
    runner: Option<Runner>,
    ops: Option<auth::AuthenticatedOps>,
    server: Option<auth::AuthenticatedServer>,
    bundle: Option<source::Bundle>,
    transfer: Option<source::Transfer>,
    vm: Option<Vm>,
    session: Option<Session>,
}
impl Owners {
    fn new() -> Self {
        Self {
            runner: None,
            ops: None,
            server: None,
            bundle: None,
            transfer: None,
            vm: None,
            session: None,
        }
    }
    async fn body(
        &mut self,
        options: &Options,
        control: &mut Control,
        report: &mut Value,
    ) -> Result<()> {
        options.guard()?;
        control.check()?;
        let parent = options.output.parent().ok_or(Error::Input)?;
        vm::preflight(parent, control)?;
        fs::create_dir(&options.output).map_err(|_| Error::Io)?;
        fs::set_permissions(&options.output, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::Io)?;
        let root = crate::repository_root().map_err(|_| Error::Input)?;
        self.runner = Some(Runner::new(root, options.output.clone(), control)?);
        let runner = self.runner.as_mut().ok_or(Error::Refused)?;
        for name in catalog::sources() {
            let original = Original::open(&runner.root.join(&name), 1024 * 1024, control)?;
            runner.sources.insert(name, original);
        }
        report["sourceSha256"] = json!(
            runner
                .sources
                .iter()
                .map(|(name, original)| (name.clone(), original.sha256.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        );
        checkpoint(report, "authentication");
        self.ops = Some(auth::ops(runner, control, options, report).await?);
        let ops = self.ops.as_ref().ok_or(Error::Refused)?;
        report["opsVerifierCalls"] = 6.into();
        report["authenticatedOpsDistribution"] = true.into();
        report["opsArtifactId"] = options.artifact.into();
        report["opsSelection"] = ops.selection.clone();
        timing(report, "authentication", control)?;
        checkpoint(report, "server-authentication");
        self.server = Some(auth::server(runner, control, options, report).await?);
        let server = self.server.as_ref().ok_or(Error::Refused)?;
        report["serverVerifierCalls"] = 8.into();
        report["serverCurrentMetadataApiCalls"] = 2.into();
        report["authenticatedServerDistribution"] = true.into();
        timing(report, "server-authentication", control)?;
        checkpoint(report, "source-bundle");
        self.bundle = Some(source::bundle(runner, control, options).await?);
        timing(report, "source-bundle", control)?;
        let run = identity(control)?;
        report["runId"] = run.clone().into();
        self.transfer = Some(source::transfer(runner, control, ops, options, &run).await?);
        let transfer = self.transfer.as_ref().ok_or(Error::Refused)?;
        checkpoint(report, "boot");
        let work = runner.work.clone();
        self.vm = Some(Vm::create(runner, control, &work, &run).await?);
        let vm = self.vm.as_mut().ok_or(Error::Refused)?;
        runner.set_guest_health(vm.health(), vm.failure());
        timing(report, "boot", control)?;
        checkpoint(report, "install");
        install_checkpoint(report, InstallCheckpoint::Inventory, None);
        let inventory = inventory(runner, control, vm, options, &run)?;
        let argv = vec![
            "-i".into(),
            inventory.as_os_str().into(),
            runner
                .root
                .join("security/vm/ops-stage.yml")
                .into_os_string(),
        ];
        install_checkpoint(report, InstallCheckpoint::StageCommand, None);
        let staged = runner
            .run(
                control,
                "ops-stage",
                "ansible-playbook",
                argv,
                Duration::from_secs(180),
                &[0],
            )
            .await
            .map_err(|error| install_command_error(report, error))?;
        install_checkpoint(report, InstallCheckpoint::StageRecap, Some(staged.status));
        recap(&staged.stdout)?;
        install_checkpoint(
            report,
            InstallCheckpoint::StageIntegrity,
            Some(staged.status),
        );
        vm.check(control)?;
        install_checkpoint(report, InstallCheckpoint::GuestCommand, None);
        let mut command = vec![
            "/usr/bin/sudo".to_string(),
            "-n".into(),
            "/usr/bin/python3".into(),
            "-I".into(),
            "-B".into(),
            "-X".into(),
            "pycache_prefix=/root/simplestchat-vm-fixture/python-cache".into(),
            "/root/simplestchat-vm-fixture/security_vm_ops_guest.py".into(),
        ];
        for value in &transfer.argv {
            command.push(value.to_str().ok_or(Error::Input)?.into());
        }
        let arguments = vm.ssh(runner, control, &command)?;
        transfer.original.check(control)?;
        let observed = runner
            .run_input(
                control,
                "ops-cases",
                arguments,
                Duration::from_secs(1800),
                transfer.original.file()?,
            )
            .await
            .map_err(|error| install_command_error(report, error))?;
        observe_ops_receipt(report, &observed.stdout, &run, observed.status)?;
        install_checkpoint(report, InstallCheckpoint::Integrity, Some(observed.status));
        ops.check(control)?;
        transfer.original.check(control)?;
        vm.check(control)?;
        timing(report, "install", control)?;
        clear_install_diagnostic(report);
        let bundle = self.bundle.as_ref().ok_or(Error::Refused)?;
        let values = runner.output.join("fixture-input-vars.json");
        files::write(
            &values,
            &auth::encoded(&json!({"vm_maintain_bundle_path":bundle.original.path,
            "vm_maintain_bundle_sha256":bundle.original.sha256,
            "vm_current_artifact_dir":server.path()}))?,
            control,
        )?;
        let original = Original::open(&values, 131072, control)?;
        runner
            .sources
            .insert("generated/fixture-input-vars.json".into(), original);
        checkpoint(report, "fixture-setup");
        let argv = vec![
            "-i".into(),
            inventory.into_os_string(),
            runner
                .root
                .join("ops/ansible/security-vm-maintain-prerequisite.yml")
                .into_os_string(),
            "--extra-vars".into(),
            format!("@{}", values.display()).into(),
        ];
        let prepared = runner
            .run(
                control,
                "maintain-fixture-setup",
                "ansible-playbook",
                argv,
                Duration::from_secs(300),
                &[0],
            )
            .await?;
        recap(&prepared.stdout)?;
        bundle.original.check(control)?;
        server.check(control)?;
        ops.check(control)?;
        timing(report, "fixture-setup", control)?;
        checkpoint(report, "guest-setup");
        self.session = Some(Session::start(vm, runner, control).await?);
        let session = self.session.as_mut().ok_or(Error::Refused)?;
        let (postgres, engine, caddy) = fixture_config(runner, control)?;
        control.check()?;
        let sample = Instant::now();
        let sources = catalog::guest_sources()
            .into_iter()
            .map(|name| {
                let checksum = runner
                    .sources
                    .get(&name)
                    .ok_or(Error::Input)?
                    .sha256
                    .clone();
                Ok((name, checksum))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
        let helpers = catalog::HELPERS
            .into_iter()
            .map(|name| {
                let source = if name == "security_vm_ops_guest.py" {
                    format!("build/{name}")
                } else {
                    format!("ops/ansible/files/{name}")
                };
                Ok((
                    name,
                    runner
                        .sources
                        .get(&source)
                        .ok_or(Error::Input)?
                        .sha256
                        .clone(),
                ))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
        let pin = |original: &Original| json!({"sha256":original.sha256,"bytes":original.bytes});
        let expected = json!({"schemaVersion":1,"scope":"MaintainLauncherPrerequisiteRequest",
            "postgresImage":postgres,"engineVersion":engine,"caddyImage":caddy,"runId":run,
            "revision":options.revision,"scenario":options.scenario,
            "setupSeconds":(control.start+SETUP).saturating_duration_since(sample).as_secs(),
            "bodySeconds":control.whole_body_end().saturating_duration_since(sample).as_secs(),
            "endSeconds":control.end().saturating_duration_since(sample).as_secs(),
            "sourcePins":sources,"helperPins":helpers,"binary":pin(ops.pin("simplestchat-ops")?),
            "manifest":pin(server.pin("release.json")?),"archive":pin(server.pin("image.tar")?),
            "sourceBundle":pin(&bundle.original),"sourceTree":bundle.tree});
        session.send(&expected, control).await?;
        let end = control
            .body_end()
            .min(Instant::now() + Duration::from_secs(300));
        let first = session.line(control, end).await?;
        if first["scope"] == "MaintainLauncherPrerequisiteGuest" {
            report["maintainProof"] = projection::guest(first, &expected)?;
            return Err(Error::Refused);
        }
        projection::exact(&first, &["schemaVersion", "scope", "runId", "ready"])?;
        if first
            != json!({"schemaVersion":1,"scope":"MaintainLauncherPrerequisiteReady",
            "runId":run,"ready":true})
        {
            return Err(Error::Refused);
        }
        timing(report, "guest-setup", control)?;
        ops.check(control)?;
        server.check(control)?;
        bundle.original.check(control)?;
        runner.integrity(control)?;
        vm.check(control)?;
        control.advance()?;
        timing(report, "ready", control)?;
        if control
            .whole_body_end()
            .saturating_duration_since(Instant::now())
            < DEBT
        {
            return Err(Error::Deadline);
        }
        control.check()?;
        session
            .send(&json!({"schemaVersion":1,"runId":run,"go":true}), control)
            .await?;
        checkpoint(report, "native");
        let end = control.body_end().min(Instant::now() + DEBT);
        let final_frame = projection::guest(session.line(control, end).await?, &expected)?;
        let passed = final_frame["passed"] == true;
        report["maintainProof"] = final_frame;
        timing(report, "native", control)?;
        if !passed {
            return Err(Error::Refused);
        }
        server.check(control)?;
        bundle.original.check(control)?;
        ops.check(control)?;
        runner.integrity(control)?;
        vm.check(control)?;
        control.check()?;
        report["failure"] = Value::Null;
        report["checkpoint"] = "complete".into();
        Ok(())
    }
    async fn finish(&mut self, control: &mut Control, report: &mut Value) -> Result<()> {
        let mut first = None;
        fn remember(first: &mut Option<Error>, result: Result<()>, control: &mut Control) {
            if let Err(error) = result {
                first.get_or_insert(error);
                control.record(error);
            }
        }
        if let Some(session) = &mut self.session {
            let result = session.finish(control).await;
            if let Ok(status) = &result {
                report["guestRawExit"] = (*status).into();
            }
            remember(
                &mut first,
                result.and_then(|status| {
                    if status == 0 {
                        Ok(())
                    } else {
                        Err(Error::Child(status))
                    }
                }),
                control,
            );
            remember(&mut first, session.close(control).await, control);
            report["guestSession"] = session.diagnostics();
        }
        if let Some(transfer) = &self.transfer {
            remember(&mut first, transfer.original.check(control), control);
        }
        if let Some(server) = &self.server {
            remember(&mut first, server.check(control), control);
        }
        if let Some(ops) = &self.ops {
            remember(&mut first, ops.check(control), control);
        }
        if let Some(bundle) = &self.bundle {
            remember(&mut first, bundle.original.check(control), control);
        }
        if let Some(runner) = &self.runner {
            remember(&mut first, runner.integrity(control), control);
        }
        if let Some(vm) = &mut self.vm {
            let result = vm.finish(control).await;
            report["qemu"] = vm.diagnostics();
            remember(&mut first, result.map(|_| ()), control);
        }
        // Keep the same actual payload originals through every VM finalizer.
        if let Some(transfer) = &mut self.transfer {
            remember(&mut first, transfer.original.finish(control), control);
        }
        if let Some(server) = &mut self.server {
            remember(&mut first, server.finish(control), control);
        }
        if let Some(ops) = &mut self.ops {
            remember(&mut first, ops.finish(control), control);
        }
        if let Some(bundle) = &mut self.bundle {
            remember(&mut first, bundle.original.finish(control), control);
        }
        if let Some(runner) = &mut self.runner {
            remember(&mut first, runner.finish(control), control);
        }
        remember(&mut first, control.check(), control);
        first.map_or(Ok(()), Err)
    }
}
pub(super) async fn run(options: &Options, control: &mut Control) -> Value {
    let mut report = json!({"schemaVersion":1,
        "scope":"SignedMuslMaintenanceLauncherPrerequisiteKvm",
        "scenario":options.scenario,"passed":false,"failure":"guard","checkpoint":"guard",
        "cleanupPassed":false,"failureKind":null,"elapsedSeconds":{},"opsVerifierCalls":0,
        "opsCases":[],"maintainProof":null,"serverVerifierCalls":0,
        "serverCurrentMetadataApiCalls":0,
        "authenticatedServerDistribution":false,"authenticatedOpsDistribution":false,
        "nativeTarget":"x86_64-unknown-linux-musl",
        "controllerImplementation":"rust-outer-controller",
        "controllerBinaryUse":"source-built-gnu-acquisition-and-passive-ELF-inspection-only",
        "guestClientWholeGroupIndependentlyObserved":false});
    let mut owners = Owners::new();
    if let Err(error) = owners.body(options, control, &mut report).await {
        control.record(error);
    }
    if let Err(error) = timing(&mut report, "body-end", control) {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn successful_recap_requires_complete_counts_and_exact_sole_host() {
        let good =
            "vmfixture : ok=9 changed=3 unreachable=0 failed=0 skipped=2 rescued=0 ignored=0\n";
        assert!(recap(good.as_bytes()).is_ok());
        for bad in [
            good.replace("ok=9", "ok=0"),
            good.replace("rescued=0", "rescued=1"),
            good.replace("ignored=0", "ignored=1"),
            good.replace("failed=0", "failed=1"),
            good.replace(" skipped=2 rescued=0 ignored=0", ""),
            format!(
                "{good}foreign : ok=1 changed=0 unreachable=0 failed=0 \
                skipped=0 rescued=0 ignored=0\n"
            ),
            format!("{good}{good}"),
        ] {
            assert_eq!(recap(bad.as_bytes()), Err(Error::Refused));
        }
    }
}
