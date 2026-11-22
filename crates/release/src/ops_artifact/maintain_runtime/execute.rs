//! Complete fixed controller body. Every admitted owner remains registered until finalization.

use super::{
    Error, Result, auth, catalog,
    control::Control,
    decode,
    deploy_options::DeployOptions,
    deploy_projection,
    files::{self, Original},
    host_projection,
    options::{Options, Purpose, REPOSITORY},
    process::Runner,
    projection, source,
    vm::{self, Session, Vm},
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
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

/// The last step of the signed Ops delivery that began; a failed delivery leaves it in
/// `installDiagnostic` with the playbook's raw exit once one was observed.
#[derive(Clone, Copy)]
enum InstallCheckpoint {
    Inventory,
    Command,
    Recap,
    Integrity,
}
impl InstallCheckpoint {
    fn name(self) -> &'static str {
        match self {
            Self::Inventory => "inventory",
            Self::Command => "install-command",
            Self::Recap => "install-recap",
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
/// What the guest delivery is given: the variables a routine `release deploy` gives
/// `signed-ops.yml`, with the controller and `gh` it retains, plus the guest's own
/// inputs: the authenticated server directory whose predicate and manifest the delivery
/// joins, the installed executable it must find afterwards, and the flow, whose
/// prepared-host upgrade alone needs the delivered candidate members kept.
pub(super) struct Delivery<'a> {
    pub revision: &'a str,
    pub run: u64,
    pub artifact: u64,
    pub selection: &'a Value,
    pub controller: &'a Path,
    pub gh: &'a Path,
    pub server: &'a Path,
    pub binary: &'a Original,
    pub purpose: Purpose,
}
impl Delivery<'_> {
    pub fn variables(&self) -> Value {
        json!({"scpub_release_repository":REPOSITORY,"scpub_release_ci_run":self.run,
            "scpub_release_expected_revision":self.revision,
            "scpub_release_revision":self.revision,
            "scpub_ops_release_controller":self.controller,"scpub_ops_gh_executable":self.gh,
            "scpub_ops_artifact_id":self.artifact,"scpub_ops_selection":self.selection,
            "vm_current_artifact_dir":self.server,
            "vm_ops_binary":{"sha256":self.binary.sha256,"bytes":self.binary.bytes},
            "vm_ops_upgrade_input":self.purpose == Purpose::Host})
    }
}
/// The one `ansible-playbook` command line of the guest delivery.
pub(super) fn delivery_command(root: &Path, inventory: &Path, values: &Path) -> Vec<OsString> {
    vec![
        "-i".into(),
        inventory.as_os_str().into(),
        root.join(catalog::DELIVERY_PLAYBOOK).into_os_string(),
        "--extra-vars".into(),
        format!("@{}", values.display()).into(),
    ]
}

fn timing(report: &mut Value, name: &str, control: &mut Control) -> Result<()> {
    control.check()?;
    let elapsed = Instant::now()
        .checked_duration_since(control.start)
        .ok_or(Error::Deadline)?;
    if elapsed >= control.whole() {
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
/// The Caddy image the installed guest admits; it refuses a request that names any other.
pub(super) const CADDY_IMAGE: &str = concat!(
    "ghcr.io/swiftraccoon/simplestchat-caddy:",
    "2.11.7-73c81fdbfe625d3df3989ba807064c81fbe2c071",
    "@sha256:23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0"
);
/// The images and the Docker engine version the fixture takes from the deployment's group
/// variables.
#[derive(Debug, PartialEq, Eq)]
struct FixtureConfig {
    postgres: String,
    engine: String,
    caddy: String,
}
impl FixtureConfig {
    /// Reads the three values from the bytes of `benchmark_hosts.yml`.
    ///
    /// Refuses with [`Error::Input`] unless the PostgreSQL image is pinned by digest, the
    /// engine package version carries a three-part upstream version, and `scpub_caddy_image`
    /// is exactly [`CADDY_IMAGE`].
    fn parse(data: &[u8]) -> Result<Self> {
        let value: Value =
            serde_saphyr::from_str(std::str::from_utf8(data).map_err(|_| Error::Input)?)
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
        let caddy = value["scpub_caddy_image"].as_str().ok_or(Error::Input)?;
        if caddy != CADDY_IMAGE {
            return Err(Error::Input);
        }
        Ok(Self {
            postgres: image.into(),
            engine: version.into(),
            caddy: caddy.into(),
        })
    }
}
fn fixture_config(runner: &Runner, control: &mut Control) -> Result<FixtureConfig> {
    let data = runner.source("ops/ansible/group_vars/benchmark_hosts.yml", control)?;
    let config = FixtureConfig::parse(&data)?;
    control.check()?;
    Ok(config)
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

pub(super) struct Owners {
    pub runner: Option<Runner>,
    pub ops: Option<auth::AuthenticatedOps>,
    pub server: Option<auth::AuthenticatedServer>,
    pub previous: Option<auth::AuthenticatedServer>,
    pub bundle: Option<source::Bundle>,
    pub vm: Option<Vm>,
    pub session: Option<Session>,
}
pub(super) struct Prepared {
    pub run: String,
    pub postgres: String,
    pub engine: String,
    pub caddy: String,
}
impl Owners {
    pub fn new() -> Self {
        Self {
            runner: None,
            ops: None,
            server: None,
            previous: None,
            bundle: None,
            vm: None,
            session: None,
        }
    }
    pub async fn prepare(
        &mut self,
        options: &Options,
        deploy: Option<&DeployOptions>,
        control: &mut Control,
        report: &mut Value,
    ) -> Result<Prepared> {
        if options.purpose != control.purpose()
            || deploy.is_some() != (options.purpose == Purpose::Deploy)
        {
            return Err(Error::Input);
        }
        options.guard()?;
        control.check()?;
        let parent = options.output.parent().ok_or(Error::Input)?;
        vm::preflight(parent, control)?;
        fs::create_dir(&options.output).map_err(|_| Error::Io)?;
        fs::set_permissions(&options.output, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::Io)?;
        let root = crate::root().map_err(|_| Error::Input)?;
        self.runner = Some(Runner::new(root, options.output.clone(), control)?);
        let runner = self.runner.as_mut().ok_or(Error::Refused)?;
        for name in catalog::sources_for(options.purpose) {
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
        self.server = Some(match deploy {
            Some(selected) => {
                auth::server_explicit(
                    runner,
                    control,
                    options,
                    selected.current_server,
                    false,
                    report,
                )
                .await?
            }
            None => auth::server(runner, control, options, report).await?,
        });
        let server = self.server.as_ref().ok_or(Error::Refused)?;
        if deploy.is_some() {
            report["currentServerVerifierCalls"] = 8.into();
            report["currentServerMetadataApiCalls"] = 2.into();
        } else {
            report["serverVerifierCalls"] = 8.into();
            report["serverCurrentMetadataApiCalls"] = 2.into();
        }
        report["authenticatedServerDistribution"] = true.into();
        timing(report, "server-authentication", control)?;
        if let Some(selected) = deploy {
            checkpoint(report, "previous-server-authentication");
            self.previous = Some(
                auth::server_explicit(
                    runner,
                    control,
                    &selected.previous_options(),
                    selected.previous.artifact,
                    true,
                    report,
                )
                .await?,
            );
            let previous = self.previous.as_ref().ok_or(Error::Refused)?;
            report["previousServerVerifierCalls"] = 8.into();
            report["previousServerMetadataApiCalls"] = 2.into();
            report["authenticatedPreviousServerDistribution"] = true.into();
            report["previousRevision"] = selected.previous.revision.clone().into();
            report["previousServerArtifactId"] = selected.previous.artifact.into();
            report["previousServerCiRunId"] = selected.previous.run.into();
            report["previousServerRunAttempt"] = selected.previous.attempt.into();
            report["serverArtifactId"] = selected.current_server.into();
            report["serverCiRunId"] = options.run.into();
            report["revision"] = options.revision.clone().into();
            deploy_projection::same_migrations(
                server.pin("release.json")?,
                previous.pin("release.json")?,
                control,
            )?;
            timing(report, "previous-server-authentication", control)?;
        }
        checkpoint(report, "source-bundle");
        self.bundle = Some(source::bundle(runner, control, options).await?);
        timing(report, "source-bundle", control)?;
        let run = identity(control)?;
        report["runId"] = run.clone().into();
        checkpoint(report, "boot");
        let work = runner.work.clone();
        self.vm = Some(Vm::create(runner, control, &work, &run).await?);
        let vm = self.vm.as_mut().ok_or(Error::Refused)?;
        runner.set_guest_health(vm.health(), vm.failure());
        timing(report, "boot", control)?;
        checkpoint(report, "install");
        install_checkpoint(report, InstallCheckpoint::Inventory, None);
        let inventory = inventory(runner, control, vm, options, &run)?;
        let controller = PathBuf::from(runner.tool("controller", control)?);
        let gh = PathBuf::from(runner.tool("gh", control)?);
        let delivery = Delivery {
            revision: &options.revision,
            run: options.run,
            artifact: options.artifact,
            selection: &ops.selection,
            controller: &controller,
            gh: &gh,
            server: server.path(),
            binary: ops.pin("simplestchat-ops")?,
            purpose: options.purpose,
        };
        let values = runner.output.join("ops-install-vars.json");
        files::write(&values, &auth::encoded(&delivery.variables())?, control)?;
        let original = Original::open(&values, 16384, control)?;
        runner
            .sources
            .insert("generated/ops-install-vars.json".into(), original);
        let argv = delivery_command(&runner.root, &inventory, &values);
        install_checkpoint(report, InstallCheckpoint::Command, None);
        // The delivery's export authenticates through `gh`, as a release's does.
        let installed = runner
            .credential(
                control,
                "ops-install",
                "ansible-playbook",
                argv,
                Duration::from_secs(600),
            )
            .await
            .map_err(|error| install_command_error(report, error))?;
        install_checkpoint(report, InstallCheckpoint::Recap, Some(installed.status));
        recap(&installed.stdout)?;
        install_checkpoint(report, InstallCheckpoint::Integrity, Some(installed.status));
        ops.check(control)?;
        server.check(control)?;
        vm.check(control)?;
        runner.integrity(control)?;
        timing(report, "install", control)?;
        clear_install_diagnostic(report);
        let bundle = self.bundle.as_ref().ok_or(Error::Refused)?;
        let values = runner.output.join("fixture-input-vars.json");
        let mut variables = json!({"vm_maintain_bundle_path":bundle.original.path,
            "vm_maintain_bundle_sha256":bundle.original.sha256,
            "vm_current_artifact_dir":server.path()});
        if let Some(selected) = deploy {
            variables["vm_previous_artifact_dir"] =
                json!(self.previous.as_ref().ok_or(Error::Refused)?.path());
            variables["vm_previous_revision"] = selected.previous.revision.clone().into();
        }
        files::write(&values, &auth::encoded(&variables)?, control)?;
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
                .join(if deploy.is_some() {
                    "ops/ansible/security-vm-native-deploy.yml"
                } else {
                    "ops/ansible/security-vm-maintain-prerequisite.yml"
                })
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
                Duration::from_secs(if deploy.is_some() { 600 } else { 300 }),
                &[0],
            )
            .await?;
        recap(&prepared.stdout)?;
        bundle.original.check(control)?;
        server.check(control)?;
        ops.check(control)?;
        let FixtureConfig {
            postgres,
            engine,
            caddy,
        } = fixture_config(runner, control)?;
        // Acquire network-dependent images before destroying the preparation
        // network stack. The fresh restricted guest admits only local digests.
        for (role, selected) in [("postgres", &postgres), ("caddy", &caddy)] {
            let arguments = vm.ssh(
                runner,
                control,
                &[
                    "/usr/bin/sudo".into(),
                    "-n".into(),
                    "/usr/bin/docker".into(),
                    "--host".into(),
                    "unix:///var/run/docker.sock".into(),
                    "pull".into(),
                    selected.clone(),
                ],
            )?;
            runner
                .run(
                    control,
                    &format!("prepare-{role}-image"),
                    "ssh",
                    arguments,
                    Duration::from_secs(90),
                    &[0],
                )
                .await?;
            vm.check(control)?;
        }
        let restarted = vm.restart_restricted(runner, control, &run).await;
        if let Some(preparation) = vm.preparation_diagnostics() {
            report["preparationQemu"] = preparation;
        }
        restarted.map(|_| ())?;
        vm.check(control)?;
        runner.integrity(control)?;
        timing(report, "fixture-setup", control)?;
        if let Some(previous) = &self.previous {
            previous.check(control)?;
        }
        Ok(Prepared {
            run,
            postgres,
            engine,
            caddy,
        })
    }
    async fn body(
        &mut self,
        options: &Options,
        control: &mut Control,
        report: &mut Value,
    ) -> Result<()> {
        let Prepared {
            run,
            postgres,
            engine,
            caddy,
        } = self.prepare(options, None, control, report).await?;
        let runner = self.runner.as_mut().ok_or(Error::Refused)?;
        let ops = self.ops.as_ref().ok_or(Error::Refused)?;
        let server = self.server.as_ref().ok_or(Error::Refused)?;
        let bundle = self.bundle.as_ref().ok_or(Error::Refused)?;
        let vm = self.vm.as_mut().ok_or(Error::Refused)?;
        if options.purpose == Purpose::Host {
            checkpoint(report, "host-upgrade");
            let arguments = vm.ssh(
                runner,
                control,
                &[
                    "/usr/bin/sudo".into(),
                    "-n".into(),
                    "/root/simplestchat-vm-fixture/ops-input/simplestchat-ops".into(),
                    "release".into(),
                    "maintain-prerequisite".into(),
                    "host-upgrade".into(),
                ],
            )?;
            let upgraded = runner
                .run(
                    control,
                    "host-upgrade",
                    "ssh",
                    arguments,
                    Duration::from_secs(120),
                    &[0],
                )
                .await?;
            if upgraded.stdout.len() > 16384 {
                return Err(Error::Refused);
            }
            report["hostUpgrade"] = host_projection::upgrade(
                decode(&upgraded.stdout)?,
                &run,
                &ops.pin("simplestchat-ops")?.sha256,
                upgraded.status,
            )?;
            ops.check(control)?;
            vm.check(control)?;
            timing(report, "host-upgrade", control)?;
        }
        checkpoint(report, "guest-setup");
        self.session = Some(Session::start(vm, runner, control).await?);
        let session = self.session.as_mut().ok_or(Error::Refused)?;
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
        let pin = |original: &Original| json!({"sha256":original.sha256,"bytes":original.bytes});
        let request_scope = match options.purpose {
            Purpose::Prerequisite => "MaintainLauncherPrerequisiteRequest",
            Purpose::Host => "PreparedHostMaintenanceRequest",
            Purpose::Deploy => return Err(Error::Input),
        };
        let setup_seconds = (control.start + control.setup_limit())
            .saturating_duration_since(sample)
            .as_secs();
        let expected = json!({"schemaVersion":1,"scope":request_scope,
            "postgresImage":postgres,"engineVersion":engine,"caddyImage":caddy,"runId":run,
            "revision":options.revision,"scenario":options.scenario,
            "setupSeconds":setup_seconds,
            "bodySeconds":control.whole_body_end().saturating_duration_since(sample).as_secs(),
            "endSeconds":control.end().saturating_duration_since(sample).as_secs(),
            "sourcePins":sources,"binary":pin(ops.pin("simplestchat-ops")?),
            "manifest":pin(server.pin("release.json")?),"archive":pin(server.pin("image.tar")?),
            "sourceBundle":pin(&bundle.original),"sourceTree":bundle.tree});
        session.send(&expected, control).await?;
        let end = match options.purpose {
            Purpose::Prerequisite => control
                .body_end()
                .min(Instant::now() + Duration::from_secs(300)),
            Purpose::Host => control.body_end(),
            Purpose::Deploy => return Err(Error::Input),
        };
        let first = session.line(control, end).await?;
        let (guest_scope, ready_scope) = match options.purpose {
            Purpose::Prerequisite => (
                "MaintainLauncherPrerequisiteGuest",
                "MaintainLauncherPrerequisiteReady",
            ),
            Purpose::Host => (
                "PreparedHostMaintenanceGuest",
                "PreparedHostMaintenanceReady",
            ),
            Purpose::Deploy => return Err(Error::Input),
        };
        if first["scope"] == guest_scope {
            report[options.purpose.proof_key()] = match options.purpose {
                Purpose::Prerequisite => projection::guest(first, &expected)?,
                Purpose::Host => host_projection::guest(first, &expected)?,
                Purpose::Deploy => return Err(Error::Input),
            };
            return Err(Error::Refused);
        }
        projection::exact(&first, &["schemaVersion", "scope", "runId", "ready"])?;
        if first
            != json!({"schemaVersion":1,"scope":ready_scope,
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
            < control.debt()
        {
            return Err(Error::Deadline);
        }
        control.check()?;
        session
            .send(&json!({"schemaVersion":1,"runId":run,"go":true}), control)
            .await?;
        checkpoint(report, "native");
        let end = control
            .body_end()
            .min(Instant::now() + control.response_debt());
        let frame = session.line(control, end).await?;
        let final_frame = match options.purpose {
            Purpose::Prerequisite => projection::guest(frame, &expected)?,
            Purpose::Host => host_projection::guest(frame, &expected)?,
            Purpose::Deploy => return Err(Error::Input),
        };
        let passed = final_frame["passed"] == true;
        report[options.purpose.proof_key()] = final_frame;
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
    pub async fn finish(&mut self, control: &mut Control, report: &mut Value) -> Result<()> {
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
        if let Some(server) = &self.server {
            remember(&mut first, server.check(control), control);
        }
        if let Some(previous) = &self.previous {
            remember(&mut first, previous.check(control), control);
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
            if let Some(preparation) = vm.preparation_diagnostics() {
                report["preparationQemu"] = preparation;
            }
            report["qemu"] = vm.diagnostics();
            if result.is_err()
                && let Some(diagnostic) = vm.startup_diagnostic()
            {
                report["vmStartupDiagnostic"] = diagnostic;
            }
            remember(&mut first, result.map(|_| ()), control);
        }
        // Keep the same actual payload originals through every VM finalizer.
        if let Some(server) = &mut self.server {
            remember(&mut first, server.finish(control), control);
        }
        if let Some(previous) = &mut self.previous {
            remember(&mut first, previous.finish(control), control);
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
    let mut report = json!({"schemaVersion":2,
        "scope":options.purpose.summary_scope(),
        "scenario":options.scenario,"passed":false,"failure":"guard","checkpoint":"guard",
        "cleanupPassed":false,"failureKind":null,"elapsedSeconds":{},"opsVerifierCalls":0,
        "maintainProof":null,"serverVerifierCalls":0,
        "serverCurrentMetadataApiCalls":0,
        "authenticatedServerDistribution":false,"authenticatedOpsDistribution":false,
        "nativeTarget":"x86_64-unknown-linux-musl",
        "controllerImplementation":"rust-outer-controller",
        "controllerBinaryUse":"source-built-gnu-acquisition-and-passive-ELF-inspection-only",
        "guestClientWholeGroupIndependentlyObserved":false,"preparationQemu":null});
    if options.purpose == Purpose::Host {
        report
            .as_object_mut()
            .expect("fixed report object")
            .remove("maintainProof");
        report["hostProof"] = Value::Null;
        report["hostUpgrade"] = Value::Null;
    }
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
    fn a_failed_delivery_names_the_step_it_reached() {
        // A red hosted run is read by these labels, so each step keeps its own.
        for (step, name) in [
            (InstallCheckpoint::Inventory, "inventory"),
            (InstallCheckpoint::Command, "install-command"),
            (InstallCheckpoint::Recap, "install-recap"),
            (InstallCheckpoint::Integrity, "integrity"),
        ] {
            let mut report = json!({"checkpoint":"install"});
            install_checkpoint(&mut report, step, Some(2));
            assert_eq!(
                report,
                json!({"checkpoint":"install",
                    "installDiagnostic":{"checkpoint":name,"rawExit":2}})
            );
            clear_install_diagnostic(&mut report);
            assert_eq!(report, json!({"checkpoint":"install"}));
        }
    }
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
    const GROUP_VARS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../ops/ansible/group_vars/benchmark_hosts.yml"
    ));
    /// The checkout's group variables with the value of `scpub_caddy_image` replaced.
    fn group_vars_with(caddy: &str) -> String {
        let key = "scpub_caddy_image: >-\n";
        let (head, tail) = GROUP_VARS.split_once(key).expect("the key in block form");
        let (_, rest) = tail.split_once('\n').expect("the value's own line");
        assert!(!rest.contains("scpub_caddy_image:"));
        format!("{head}{caddy}{rest}")
    }
    #[test]
    fn the_group_variables_name_the_caddy_image_the_guest_admits() {
        let config = FixtureConfig::parse(GROUP_VARS.as_bytes()).expect("the checkout's file");
        assert_eq!(config.caddy, CADDY_IMAGE);
        // The guest is built with a feature this crate leaves off, so its literal is read
        // from its source: the string pieces of its `CADDY_IMAGE`, joined as `concat!` does.
        let guest = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ops/src/release/attestation/maintain_prerequisite/guest/protocol.rs"
        ));
        let (_, tail) = guest
            .split_once("pub(super) const CADDY_IMAGE: &str = concat!(")
            .expect("the guest's constant");
        let (pieces, _) = tail.split_once(");").expect("the end of the constant");
        let admitted = pieces
            .split('"')
            .skip(1)
            .step_by(2)
            .collect::<Vec<_>>()
            .concat();
        assert!(guest.contains("request.caddy_image != CADDY_IMAGE"));
        assert_eq!(config.caddy, admitted);
        // Replacing the value with itself is the identity, so the refusals below come from
        // the replaced value alone.
        let same = group_vars_with(&format!("scpub_caddy_image: {CADDY_IMAGE}\n"));
        assert_eq!(FixtureConfig::parse(same.as_bytes()), Ok(config));
    }
    #[test]
    fn group_variables_without_the_admitted_caddy_image_are_refused() {
        let digest = CADDY_IMAGE.replace("@sha256:2", "@sha256:3");
        let tag = CADDY_IMAGE.replace(":2.11.7-", ":2.11.8-");
        assert!(digest != CADDY_IMAGE && tag != CADDY_IMAGE);
        for (case, replacement) in [
            ("missing", String::new()),
            ("null", "scpub_caddy_image:\n".into()),
            ("empty", "scpub_caddy_image: \"\"\n".into()),
            ("number", "scpub_caddy_image: 7\n".into()),
            ("boolean", "scpub_caddy_image: true\n".into()),
            ("list", format!("scpub_caddy_image: [\"{CADDY_IMAGE}\"]\n")),
            (
                "mapping",
                format!("scpub_caddy_image: {{image: \"{CADDY_IMAGE}\"}}\n"),
            ),
            ("another digest", format!("scpub_caddy_image: {digest}\n")),
            ("another tag", format!("scpub_caddy_image: {tag}\n")),
            (
                "a prefix",
                format!("scpub_caddy_image: {}\n", &CADDY_IMAGE[..80]),
            ),
            ("a suffix", format!("scpub_caddy_image: {CADDY_IMAGE}0\n")),
            (
                "another key",
                format!("scpub_caddy_images: {CADDY_IMAGE}\n"),
            ),
        ] {
            assert_eq!(
                FixtureConfig::parse(group_vars_with(&replacement).as_bytes()),
                Err(Error::Input),
                "{case}"
            );
        }
    }
}
