//! New-profile setup only: an authenticated current candidate upgrades an inert,
//! unexecuted old canonical record before the actual host guest process starts.
use super::super::{
    Error, INSTALLED, LIBRARY, PRIVATE, ROOT, Result,
    control::Control,
    files::{self, Original},
    process::Commands,
    protocol,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString, fs, os::unix::fs::MetadataExt, path::Path, process::ExitCode, time::Instant,
};

const INERT: &[u8] = b"#!/bin/sh\nexit 97\n";
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
async fn execute(control: &mut Control, commands: &mut Commands) -> Result<Value> {
    require(
        rustix::process::geteuid().is_root()
            && rustix::process::getegid().as_raw() == 0
            && cfg!(target_os = "linux"),
    )?;
    private_directory(control)?;
    let mut originals = Vec::new();
    let operation = perform(control, commands, &mut originals).await;
    if let Err(error) = operation {
        control.record(error);
    }
    for original in &mut originals {
        if let Err(error) = original.finish(control) {
            control.record(error);
        }
    }
    operation.and_then(|value| control.first.map_or(Ok(value), Err))
}
async fn perform(
    control: &mut Control,
    commands: &mut Commands,
    originals: &mut Vec<Original>,
) -> Result<Value> {
    originals.push(Original::open(
        Path::new("/etc/simplestchat-vm-fixture"),
        128,
        0,
        0o600,
        control,
    )?);
    let marker = originals[0].data(128, control)?;
    let run = std::str::from_utf8(&marker)
        .map_err(|_| Error::Input)?
        .strip_suffix('\n')
        .ok_or(Error::Refused)?
        .to_owned();
    require(protocol::digest(&run, 32))?;
    let mut pins = serde_json::Map::new();
    for (key, name, maximum, mode) in [
        ("manifest", "ops-release.json", 65536, 0o600),
        ("selection", "ops-selection.json", 16384, 0o600),
        ("binary", "simplestchat-ops", 256 * 1024 * 1024, 0o755),
    ] {
        let path = Path::new(ROOT).join("ops-input").join(name);
        originals.push(Original::open(&path, maximum, 0, mode, control)?);
        let source = originals.last().ok_or(Error::Close)?;
        pins.insert(
            key.into(),
            json!({"path":path,"sha256":source.sha256,"bytes":source.bytes}),
        );
    }
    let binary = &originals[3];
    let manifest = protocol::decode(&originals[1].data(65536, control)?)?;
    let selection = protocol::decode(&originals[2].data(16384, control)?)?;
    require(
        manifest["binary"]["sha256"] == binary.sha256
            && manifest["binary"]["bytes"].as_u64() == Some(binary.bytes)
            && manifest["revision"] == selection["revision"]
            && manifest["buildRunId"] == selection["buildRunId"]
            && manifest["runAttempt"] == selection["runAttempt"]
            && manifest["repository"] == "swiftraccoon/simplestChat",
    )?;
    let executable = rustix::fs::open(
        "/proc/self/exe",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(|_| Error::Io)?;
    let same = (|| {
        let running = executable.metadata().map_err(|_| Error::Io)?;
        let selected = binary.file()?.metadata().map_err(|_| Error::Io)?;
        require(running.dev() == selected.dev() && running.ino() == selected.ino())
    })();
    if let Err(e) = same {
        control.record(e);
    }
    let closed = files::close(executable, control);
    same.and(closed)?;
    let mut installed = Original::open(Path::new(INSTALLED), 256 * 1024 * 1024, 0, 0o755, control)?;
    require(installed.sha256 == binary.sha256 && installed.bytes == binary.bytes)?;
    installed.finish(control)?;
    // This lock is released before either upgrade invokes its own real lock.
    let workload = crate::release::assets::Workload::acquire().map_err(|_| Error::Refused)?;
    let inert = Path::new(LIBRARY).join(".host-inert-old");
    let result = (|| {
        let mut old = files::write_new(&inert, INERT, 0, 0o755, control)?;
        old.finish(control)?;
        fs::rename(&inert, INSTALLED).map_err(|_| Error::Io)?;
        control.check()
    })();
    if let Err(e) = result {
        control.record(e);
    }
    let finished = workload.finish().map_err(|_| Error::Close);
    result.and(finished)?;
    pins.insert("schemaVersion".into(), 2.into());
    pins.insert(
        "installedBinary".into(),
        json!({"sha256":files::hex(&Sha256::digest(INERT)),"bytes":INERT.len()}),
    );
    let request = serde_json::to_vec(&Value::Object(pins)).map_err(|_| Error::Input)?;
    let path = Path::new(PRIVATE).join("host-upgrade-request.json");
    originals.push(files::write_new(&path, &request, 0, 0o600, control)?);
    let sha = files::hex(&Sha256::digest(&request));
    commands.admit(
        "ops",
        &Path::new(ROOT).join("ops-input/simplestchat-ops"),
        control,
    )?;
    let args = vec![
        "ops".into(),
        "distribution".into(),
        "upgrade".into(),
        "--request".into(),
        path.to_str().ok_or(Error::Input)?.into(),
        "--request-sha256".into(),
        sha,
    ];
    for (changed, role) in [(true, "host-upgrade-first"), (false, "host-upgrade-repeat")] {
        // Each real CLI has its own process and signal mask; both borrow
        // this helper's original ceiling through the bounded child owner.
        let output = commands
            .execute(
                control,
                role,
                &args,
                std::time::Duration::from_secs(105),
                &[0],
            )
            .await?;
        let receipt = protocol::decode(&output.stdout)?;
        protocol::exact(
            &receipt,
            &[
                "schemaVersion",
                "passed",
                "changed",
                "manifestSha256",
                "manifestBytes",
                "selectionSha256",
                "selectionBytes",
                "binarySha256",
                "binaryBytes",
            ],
        )?;
        require(
            receipt["schemaVersion"] == 1
                && receipt["passed"] == true
                && receipt["changed"] == changed
                && receipt["manifestSha256"] == originals[1].sha256
                && receipt["manifestBytes"].as_u64() == Some(originals[1].bytes)
                && receipt["selectionSha256"] == originals[2].sha256
                && receipt["selectionBytes"].as_u64() == Some(originals[2].bytes)
                && receipt["binarySha256"] == originals[3].sha256
                && receipt["binaryBytes"].as_u64() == Some(originals[3].bytes),
        )?;
        control.check()?;
    }
    let binary_sha = originals[3].sha256.clone();
    let binary_bytes = originals[3].bytes;
    originals.push(Original::open(
        Path::new(INSTALLED),
        256 * 1024 * 1024,
        0,
        0o755,
        control,
    )?);
    require(
        originals.last().ok_or(Error::Close)?.sha256 == binary_sha
            && originals.last().ok_or(Error::Close)?.bytes == binary_bytes,
    )?;
    Ok(json!({
        "schemaVersion":1,"scope":"PreparedHostOpsUpgrade","runId":run,"passed":true,
        "changed":true,"repeatChanged":false,"currentBinarySha256":binary_sha,
        "previousBinaryExecuted":false
    }))
}
fn private_directory(control: &mut Control) -> Result<()> {
    control.check()?;
    let parent = rustix::fs::open(
        ROOT,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(|_| Error::Io)?;
    let result = (|| {
        let held = parent.metadata().map_err(|_| Error::Io)?;
        require(
            held.is_dir()
                && held.uid() == 0
                && held.gid() == 0
                && held.mode() & 0o7777 == 0o700
                && files::same(&held, &fs::symlink_metadata(ROOT).map_err(|_| Error::Io)?),
        )?;
        match fs::symlink_metadata(PRIVATE) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                super::super::create_directory(Path::new(PRIVATE), 0, 0o700, control)?
            }
            Ok(meta) => require(
                meta.is_dir()
                    && meta.uid() == 0
                    && meta.gid() == 0
                    && meta.mode() & 0o7777 == 0o700,
            )?,
            Err(_) => return Err(Error::Io),
        }
        let visible = fs::symlink_metadata(ROOT).map_err(|_| Error::Io)?;
        // Creating the known direct child changes parent times, not authority.
        require(
            visible.dev() == held.dev()
                && visible.ino() == held.ino()
                && visible.mode() == held.mode()
                && visible.uid() == held.uid()
                && visible.gid() == held.gid(),
        )?;
        parent.sync_all().map_err(|_| Error::Io)?;
        control.check()
    })();
    if let Err(e) = result {
        control.record(e);
    }
    result.and(files::close(parent, control))
}
pub(in crate::release::attestation) fn run(args: &[OsString]) -> ExitCode {
    use std::io::Write;
    let started = Instant::now();
    if !args.is_empty() {
        return ExitCode::from(2);
    }
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let mut control = match Control::new_upgrade(started) {
        Ok(v) => v,
        Err(_) => {
            nix::sys::stat::umask(previous);
            return ExitCode::FAILURE;
        }
    };
    let mut commands = Commands::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let operation = match &runtime {
        Ok(runtime) => runtime.block_on(execute(&mut control, &mut commands)),
        Err(_) => Err(Error::Io),
    };
    control.begin_settling();
    let closed = match &runtime {
        Ok(runtime) => runtime.block_on(commands.finish(&mut control)),
        Err(_) => Err(Error::Close),
    };
    drop(runtime);
    let result = operation
        .and_then(|value| closed.map(|()| value))
        .and_then(|value| {
            control.check()?;
            let mut bytes = serde_json::to_vec(&value).map_err(|_| Error::Input)?;
            bytes.push(b'\n');
            let mut original = files::write_new(
                &Path::new(PRIVATE).join("host-upgrade.json"),
                &bytes,
                0,
                0o600,
                &mut control,
            )?;
            original.finish(&mut control)?;
            std::io::stdout()
                .lock()
                .write_all(&bytes)
                .map_err(|_| Error::Io)?;
            control.check()
        });
    if let Err(error) = result {
        control.record(error);
    }
    nix::sys::stat::umask(previous);
    if control.finish().is_ok() && result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(super::super::failed_status(control.status))
    }
}
