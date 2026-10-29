//! Fixed negative-fixture client; `argv[0]` alone never grants Docker authority.
//! Safe nix fexecve and rustix mount_bind retain the admitted original semantics.
//! Kernel-blocked I/O and kernel CLOEXEC completion have no checked wall bound.
use super::{Error, Result};
#[cfg(target_os = "linux")]
use super::{decode, hash, hex, require};
#[cfg(target_os = "linux")]
use serde_json::{Value, json};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::{ffi::OsString, io::Write, process::ExitCode};

#[cfg(target_os = "linux")]
mod context;
#[cfg(target_os = "linux")]
mod namespace;
#[cfg(all(test, target_os = "linux"))]
mod tests;
#[cfg(target_os = "linux")]
mod wire;

#[cfg(target_os = "linux")]
const ROOT: &str = "/root/simplestchat-vm-fixture";
#[cfg(target_os = "linux")]
const CONFIG: &str = "/root/simplestchat-vm-fixture/deploy-injection-config.json";
#[cfg(target_os = "linux")]
const SOCKET: &str = "/root/simplestchat-vm-fixture/deploy-injection.sock";
#[cfg(target_os = "linux")]
const ALIAS: &str = "/root/simplestchat-vm-fixture/deploy-injection-original-docker";
#[cfg(target_os = "linux")]
const RECEIPT: &str = "/root/simplestchat-vm-fixture/deploy-injection-receipt.json";
#[cfg(target_os = "linux")]
const INSTALLED: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
#[cfg(target_os = "linux")]
const DOCKER: &str = "/usr/bin/docker";
#[cfg(target_os = "linux")]
const MARKER: &str = "/etc/simplestchat-vm-fixture";
#[cfg(target_os = "linux")]
const FRAME: usize = 16384;
#[cfg(target_os = "linux")]
const STREAM: usize = 2 * 1024 * 1024;
#[cfg(target_os = "linux")]
const CHUNK: usize = 65536;
const FAILURE: &[u8] = b"Inspection barrier refused.\n";
#[cfg(target_os = "linux")]
const NATIVE_FORMAT: &str = concat!(
    "{\"id\":{{json .Id}},\"image\":{{json .Image}},\"state\":{{json .State}},",
    "\"restarts\":{{.RestartCount}},",
    "\"configHash\":{{json (index .Config.Labels \"com.docker.compose.config-hash\")}}}",
);
#[cfg(target_os = "linux")]
fn exact(value: &Value, names: &[&str]) -> Result<()> {
    super::super::protocol::exact(value, names)
}
#[cfg(target_os = "linux")]
fn identifier(args: &[OsString]) -> Option<&str> {
    let prefix = [
        "--host",
        "unix:///var/run/docker.sock",
        "inspect",
        "--format",
        NATIVE_FORMAT,
    ];
    if args.len() != 6 || !args.iter().zip(prefix).all(|(left, right)| left == right) {
        return None;
    }
    args[5].to_str().filter(|text| hex(text, 64))
}
#[cfg(target_os = "linux")]
fn digest(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|value| hex(value, 64))
        .ok_or(Error::Refused)
}
#[cfg(target_os = "linux")]
fn execute(args: &[OsString], enter: bool) -> Result<()> {
    // Numeric inheritance is registered before any bootstrap file can reuse it.
    let mut inherited = namespace::Inherited::parse(args, enter)?;
    let mut context = context::Context::new();
    let body = (|| {
        context.admit()?;
        if enter {
            namespace::enter(&mut context, &mut inherited, args)
        } else {
            let delivered = if let Some(id) = identifier(args) {
                wire::client(&mut context, id)?
            } else {
                false
            };
            if delivered {
                context.check()?;
                Ok(())
            } else {
                namespace::docker(&mut context, args)
            }
        }
    })();
    let inherited_close = inherited.close();
    let original_close = context.close();
    let final_fence = context.finish();
    body.and(inherited_close)
        .and(original_close)
        .and(final_fence)
}
fn run(args: &[OsString], enter: bool) -> ExitCode {
    #[cfg(target_os = "linux")]
    let result = execute(args, enter);
    #[cfg(not(target_os = "linux"))]
    let result: Result<()> = {
        let _ = (args, enter);
        Err(Error::Refused)
    };
    if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        let mut error = std::io::stderr().lock();
        let _ = error.write_all(FAILURE);
        let _ = error.flush();
        ExitCode::FAILURE
    }
}
pub(in super::super) fn client(args: &[OsString]) -> ExitCode {
    run(args, false)
}
pub(in super::super) fn enter(args: &[OsString]) -> ExitCode {
    run(args, true)
}
