//! Complete fixed disposable maintenance guest. No Python code is invoked here.

pub(super) mod control;
pub(super) mod effects;
pub(super) mod files;
pub(super) mod host;
pub(super) mod materials;
pub(super) mod native;
pub(super) mod observation;
pub(super) mod process;
pub(super) mod protocol;
pub(super) mod render;
pub(super) mod sources;
pub(super) mod tls;

use control::Control;
use files::Original;
use observation::Observations;
use process::Commands;
use protocol::{Request, Wire};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

pub(super) const ROOT: &str = "/root/simplestchat-vm-fixture";
pub(super) const PRIVATE: &str = "/root/simplestchat-vm-fixture/maintain-prerequisite";
pub(super) const CONFIG: &str = "/etc/simplestchat-public";
pub(super) const PUBLIC: &str = "/srv/simplestchat-public";
pub(super) const LIBRARY: &str = "/usr/local/libexec/simplestchat-public";
pub(super) const INSTALLED: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
pub(super) const BUNDLE: &str = "/root/simplestchat-vm-fixture/maintain-source.bundle";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Error {
    Input,
    Refused,
    Changed,
    Io,
    Close,
    Deadline,
    Interrupt,
    Terminate,
    Command,
    Bounds,
    NativeStatus,
}
pub(super) type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn category(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Changed => "changed",
            Self::Command => "command",
            Self::Bounds => "bounds",
            Self::Refused => "refused",
            Self::Io | Self::Close => "io",
            Self::Deadline => "deadline",
            Self::Interrupt | Self::Terminate => "cancelled",
            Self::NativeStatus => "native-status",
        }
    }
}

struct Context {
    control: Control,
    commands: Commands,
    request: Request,
    source_root: PathBuf,
    sources: BTreeMap<String, Vec<u8>>,
    originals: Vec<Original>,
    observed: Observations,
    wire: Wire,
    socket: Option<(PathBuf, std::fs::Metadata)>,
    host_retired: BTreeSet<usize>,
    host_diagnostic: Option<Value>,
}
impl Context {
    pub fn admit_socket(&mut self) -> Result<()> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        self.control.check()?;
        if self.socket.is_none() {
            let path = std::fs::canonicalize("/var/run/docker.sock").map_err(|_| Error::Io)?;
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| Error::Io)?;
            if !metadata.file_type().is_socket() || metadata.uid() != 0 {
                return Err(Error::Refused);
            }
            self.socket = Some((path, metadata));
        }
        sources::socket_check(self)
    }
    pub fn check(&mut self) -> Result<()> {
        self.control.check()?;
        self.wire.no_extra(&mut self.control)?;
        for (index, original) in self.originals.iter().enumerate() {
            if self.host_retired.contains(&index) {
                if !self.request.is_host() || !original.closed() {
                    return Err(Error::Refused);
                }
                continue;
            }
            original.check(&mut self.control)?;
        }
        sources::socket_check(self)?;
        self.control.check()
    }
    pub fn retain(
        &mut self,
        path: &std::path::Path,
        maximum: u64,
        uid: u32,
        mode: u32,
    ) -> Result<usize> {
        let original = Original::open(path, maximum, uid, mode, &mut self.control)?;
        self.originals.push(original);
        Ok(self.originals.len() - 1)
    }
}

fn first(control: &mut Control, result: Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error) => {
            control.record(error);
            false
        }
    }
}

fn create_directory(path: &Path, uid: u32, mode: u32, control: &mut Control) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    control.check()?;
    std::fs::create_dir(path).map_err(|_| Error::Io)?;
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(|_| Error::Io)?;
    let result = (|| {
        rustix::fs::fchown(
            &file,
            Some(rustix::process::Uid::from_raw(uid)),
            Some(rustix::process::Gid::from_raw(uid)),
        )
        .map_err(|_| Error::Io)?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|_| Error::Io)?;
        control.check()
    })();
    if let Err(error) = result {
        control.record(error);
    }
    let closed = files::close(file, control);
    result.and(closed)
}

fn retain_matching(
    original: Original,
    expected: &[u8],
    originals: &mut Vec<Original>,
    control: &mut Control,
) -> Result<()> {
    originals.push(original);
    let original = originals.last().ok_or(Error::Close)?;
    if original.data(1024 * 1024, control)? != expected {
        return Err(Error::Changed);
    }
    Ok(())
}

fn assets_initialized(bytes: &[u8]) -> Result<()> {
    let value = protocol::decode(bytes)?;
    protocol::exact(&value, &["passed", "changed"])?;
    if value["passed"].as_bool() != Some(true) || value["changed"].as_bool().is_none() {
        return Err(Error::Refused);
    }
    Ok(())
}

async fn rendered_files(ctx: &mut Context, image: &str) -> Result<()> {
    ctx.control.checkpoint = "render";
    let mut secrets = BTreeMap::new();
    for name in render::SECRET_NAMES {
        ctx.control.check()?;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Io)?;
        secrets.insert(name.to_owned(), files::hex(&bytes));
        ctx.control.check()?;
    }
    let bridge = format!("sc{}", &ctx.request.run_id[..12]);
    let outputs = render::render(
        &ctx.sources,
        render::Identity {
            image,
            revision: &ctx.request.revision,
            run_id: &ctx.request.run_id,
            postgres: &ctx.request.postgres_image,
            caddy: &ctx.request.caddy_image,
            bridge: &bridge,
        },
        &secrets,
    )?;
    if outputs.len() != 13 {
        return Err(Error::Refused);
    }
    // Deliberate fixture correction: retire only the validated original inert
    // Ops preflight input before any exclusive real configuration publication.
    ctx.control.checkpoint = "render-inert-handoff";
    files::inert_images_handoff(&mut ctx.control)?;
    ctx.control.checkpoint = "render-files";
    for (name, bytes) in outputs {
        let uid = if [
            "postgres-admin-password",
            "pg_hba.conf",
            "init-database.sql",
        ]
        .contains(&name.as_str())
        {
            999
        } else {
            0
        };
        let mode = if name == "Caddyfile" { 0o644 } else { 0o600 };
        let original = files::write_new(
            &Path::new(CONFIG).join(&name),
            &bytes,
            uid,
            mode,
            &mut ctx.control,
        )?;
        ctx.originals.push(original);
        ctx.originals
            .last()
            .ok_or(Error::Close)?
            .require_parent(0, 0o700, &mut ctx.control)?;
    }
    ctx.control.checkpoint = "render-helpers";
    for (target, source, mode) in [
        (
            "/usr/local/bin/simplestchat-public-deploy",
            "ops/ansible/files/deploy-public.sh",
            0o755,
        ),
        (
            "/usr/local/bin/simplestchat-public",
            "ops/ansible/files/public-compose.sh",
            0o755,
        ),
        (
            "/usr/local/libexec/simplestchat-bench/seed-public-room.py",
            "ops/ansible/files/seed-public-room.py",
            0o755,
        ),
        (
            "/usr/local/libexec/simplestchat-bench/seed_public_room.py",
            "ops/ansible/files/seed_public_room.py",
            0o644,
        ),
        (
            "/usr/local/libexec/simplestchat-bench/release_json.py",
            "ops/ansible/files/release_json.py",
            0o644,
        ),
        (
            "/usr/local/libexec/simplestchat-public/runtime_profile.py",
            "ops/ansible/files/runtime_profile.py",
            0o644,
        ),
        (
            "/usr/local/libexec/simplestchat-public/release_json.py",
            "ops/ansible/files/release_json.py",
            0o644,
        ),
    ] {
        let bytes = ctx.sources.get(source).ok_or(Error::Input)?;
        if Path::new(target).starts_with(LIBRARY) && std::fs::symlink_metadata(target).is_ok() {
            let original =
                Original::open(Path::new(target), 1024 * 1024, 0, mode, &mut ctx.control)?;
            retain_matching(original, bytes, &mut ctx.originals, &mut ctx.control)?;
        } else {
            let original = files::write_new(Path::new(target), bytes, 0, mode, &mut ctx.control)?;
            ctx.originals.push(original);
        }
    }
    ctx.control.checkpoint = "render-directories";
    for (name, uid, mode) in [
        ("postgres", 999, 0o700),
        ("postgres-socket", 999, 0o755),
        ("caddy-data", 10001, 0o700),
        ("caddy-config", 10001, 0o700),
    ] {
        create_directory(&Path::new(PUBLIC).join(name), uid, mode, &mut ctx.control)?;
    }
    ctx.control.checkpoint = "render-assets";
    ctx.check()?;
    let cap = ctx.control.remaining(Duration::from_secs(1800))?;
    let output = ctx
        .commands
        .execute(
            &mut ctx.control,
            "browser-assets-initialize",
            &[
                "ops".into(),
                "release".into(),
                "assets".into(),
                "initialize".into(),
            ],
            cap,
            &[0],
        )
        .await?;
    ctx.control.checkpoint = "render-assets-receipt";
    assets_initialized(&output.stdout)?;
    ctx.check()?;
    ctx.control.checkpoint = "render-tools";
    for (role, path) in [
        ("public", "/usr/local/bin/simplestchat-public"),
        ("unshare", "/usr/bin/unshare"),
        ("timeout", "/usr/bin/timeout"),
    ] {
        ctx.commands
            .admit(role, Path::new(path), &mut ctx.control)?;
    }
    ctx.check()
}

async fn body(
    ctx: &mut Context,
    fixture: &mut effects::Fixture,
    native: &mut native::Native,
) -> Result<()> {
    sources::prepare(ctx).await?;
    fixture.prepare_payload(ctx).await?;
    rendered_files(ctx, &fixture.image).await?;
    fixture.setup_network(ctx).await?;
    fixture.setup_database(ctx).await?;
    fixture.prepare_caddy(ctx).await?;
    fixture.setup_complete(ctx).await?;
    if ctx.request.scenario == "dirty-source" {
        sources::dirty(ctx).await?;
    }
    ctx.check()?;
    ctx.control.checkpoint = "ready";
    ctx.wire.emit(
        &json!({
            "schemaVersion":1,
            "scope":"MaintainLauncherPrerequisiteReady",
            "runId":ctx.request.run_id,
            "ready":true,
        }),
        &mut ctx.control,
    )?;
    let go = protocol::decode(&ctx.wire.line(&mut ctx.control)?)?;
    protocol::exact(&go, &["schemaVersion", "runId", "go"])?;
    if go["schemaVersion"].as_u64() != Some(1)
        || go["runId"] != ctx.request.run_id
        || go["go"] != true
    {
        return Err(Error::Refused);
    }
    ctx.wire.no_extra(&mut ctx.control)?;
    ctx.control.advance()?;
    ctx.control.checkpoint = "native";
    fixture.native_boundary(ctx)?;
    native.invoke(ctx).await?;
    ctx.control.checkpoint = "native-result";
    fixture.observe(ctx).await?;
    ctx.check()
}

fn receipt(ctx: &Context, native: &native::Native, checkpoint: &str, cleanup: bool) -> Value {
    let failed = ctx.control.first;
    let passed = failed.is_none() && ctx.control.status == 0 && cleanup;
    let final_status = if ctx.control.status != 0 {
        ctx.control.status
    } else if passed {
        0
    } else {
        1
    };
    json!({
        "schemaVersion":1,
        "scope":"MaintainLauncherPrerequisiteGuest",
        "runId":ctx.request.run_id,
        "revision":ctx.request.revision,
        "scenario":ctx.request.scenario,
        "passed":passed,
        "failure":failed.map(Error::category),
        "checkpoint":if passed{"complete"}else{checkpoint},
        "finalStatus":final_status,
        "cleanupPassed":cleanup,
        "nativeRawExit":native.raw,
        "nativeFrame":native.frame,
        "observations":ctx.observed.as_value(),
        "sourcePins":ctx.request.source_pins,
    })
}

fn failed_status(status: u8) -> u8 {
    match status {
        124 | 130 | 143 => status,
        _ => 1,
    }
}

fn local_checkpoint(previous: &'static str, primary_capture: Option<&'static str>) -> &'static str {
    primary_capture.unwrap_or(previous)
}

async fn transaction(ctx: &mut Context) -> Value {
    let mut fixture = effects::Fixture::new(&ctx.request);
    let mut native = native::Native::new();
    if let Err(error) = body(ctx, &mut fixture, &mut native).await {
        ctx.control.record(error);
    }
    let mut checkpoint = ctx.control.checkpoint;
    ctx.control.begin_recovery();
    let body_cleanup = fixture.finish_body(ctx).await;
    let mut cleanup = first(&mut ctx.control, body_cleanup);
    ctx.control.begin_settling();
    let local = fixture.finish_local(ctx).await;
    checkpoint = local_checkpoint(checkpoint, fixture.primary_capture_checkpoint());
    cleanup &= first(&mut ctx.control, local);
    for original in &mut ctx.originals {
        let result = original.finish(&mut ctx.control);
        cleanup &= first(&mut ctx.control, result);
    }
    let commands = ctx.commands.finish(&mut ctx.control).await;
    cleanup &= first(&mut ctx.control, commands);
    cleanup &=
        !ctx.control.cleanup_failed && !ctx.observed.get("daemonEffectsUncertain").unwrap_or(true);
    if !cleanup && ctx.control.first.is_none() {
        ctx.control.record(Error::Close);
    }
    if ctx.control.first.is_none()
        && let Err(error) = ctx.observed.validate_success(&ctx.request.scenario)
    {
        ctx.control.record(error);
    }
    receipt(ctx, &native, checkpoint, cleanup)
}

/// Entry captures its one monotonic origin before arguments, flags, I/O or effects.
pub(in crate::release::attestation) fn run(arguments: &[OsString]) -> ExitCode {
    entry(arguments, false)
}
pub(in crate::release::attestation) fn run_host(arguments: &[OsString]) -> ExitCode {
    entry(arguments, true)
}
pub(in crate::release::attestation) fn run_upgrade(arguments: &[OsString]) -> ExitCode {
    host::upgrade::run(arguments)
}
fn entry(arguments: &[OsString], host: bool) -> ExitCode {
    let started = Instant::now();
    if !arguments.is_empty() {
        eprintln!("Usage: simplestchat-ops release maintain-prerequisite guest");
        return ExitCode::from(2);
    }
    if !cfg!(target_os = "linux") {
        return ExitCode::FAILURE;
    }
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let admitted_control = if host {
        Control::new_host(started)
    } else {
        Control::new(started)
    };
    let mut control = match admitted_control {
        Ok(value) => value,
        Err(_) => {
            nix::sys::stat::umask(previous);
            return ExitCode::FAILURE;
        }
    };
    let mut wire = match Wire::new(&mut control) {
        Ok(value) => value,
        Err(error) => {
            control.record(error);
            control.begin_settling();
            nix::sys::stat::umask(previous);
            let _ = control.finish();
            return ExitCode::from(failed_status(control.status));
        }
    };
    let admitted = (|| {
        let bytes = wire.line(&mut control)?;
        let request = if host {
            Request::decode_host(&bytes)?
        } else {
            Request::decode(&bytes)?
        };
        control.configure(&request)?;
        wire.no_extra(&mut control)?;
        Ok(request)
    })();
    let request = match admitted {
        Ok(value) => value,
        Err(error) => {
            control.record(error);
            control.begin_settling();
            let _ = wire.close(&mut control);
            nix::sys::stat::umask(previous);
            let _ = control.finish();
            eprintln!("Maintenance launcher prerequisite is unproven.");
            return ExitCode::from(failed_status(control.status));
        }
    };
    let mut ctx = Context {
        source_root: Path::new("/srv/simplestchat-bench/sources").join(&request.revision),
        control,
        commands: if host {
            Commands::new_host()
        } else {
            Commands::new()
        },
        request,
        sources: BTreeMap::new(),
        originals: Vec::new(),
        observed: Observations::new(),
        wire,
        socket: None,
        host_retired: BTreeSet::new(),
        host_diagnostic: None,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build();
    let report = match runtime {
        Ok(runtime) => {
            let report = if host {
                runtime.block_on(host::transaction(&mut ctx))
            } else {
                runtime.block_on(transaction(&mut ctx))
            };
            drop(runtime);
            report
        }
        Err(_) => {
            ctx.control.record(Error::Io);
            ctx.control.begin_settling();
            if host {
                host::failed_receipt(&ctx)
            } else {
                receipt(&ctx, &native::Native::new(), "input", true)
            }
        }
    };
    if let Err(error) = ctx.wire.emit(&report, &mut ctx.control) {
        ctx.control.record(error);
    }
    let closed = ctx.wire.close(&mut ctx.control);
    first(&mut ctx.control, closed);
    nix::sys::stat::umask(previous);
    let finished = ctx.control.finish();
    if finished.is_ok() && report["passed"] == true {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(failed_status(ctx.control.status))
    }
}

#[cfg(test)]
#[path = "guest_tests.rs"]
mod tests;
