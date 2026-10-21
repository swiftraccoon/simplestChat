//! Only an explicitly disposable Unix engine or the named owned Podman VM.

use super::{Context, evidence, options::Options, process::Runner};
use crate::tooling::{Failure, Result};
use serde_json::Value;
use std::ffi::OsStr;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

pub(super) const AMD64_IMAGE: &str = "docker.io/catthehacker/ubuntu@sha256:\
     4f2d5083a9d10d018c1c511eb8665cd480553c11975e78fd903a46daa830768b";
pub(super) const ARM64_IMAGE: &str = "docker.io/catthehacker/ubuntu@sha256:\
     84e94c96278dd26b8feb71226a521d259f8160cf6b1dd08e51fd42be43a82e56";
const MACHINE: &str = "simplestchat-ci";

/// The guest owns these Linux-only capability and binfmt operations. Keep the
/// maintained probe intact until a native guest helper replaces this boundary.
pub(super) const ARM64_GUEST: &str = r#"python3 - <<'PY_CAPABILITIES'
import ctypes

libc = ctypes.CDLL(None)
libc.getauxval.argtypes = [ctypes.c_ulong]
libc.getauxval.restype = ctypes.c_ulong
hwcap, hwcap2 = libc.getauxval(16), libc.getauxval(26)
if hwcap2 & (1 << 1) and not hwcap & (1 << 22):
    raise SystemExit(
        "The owned simplestchat-ci VM advertises SVE2 without SVE "
        f"(HWCAP={hwcap:#x}, HWCAP2={hwcap2:#x}); native tools can crash. "
        "After owned CI jobs have stopped, apply arm64.nosve with rpm-ostree "
        "and reboot only this VM. See docs/testing.md#recover-inconsistent-arm-cpu-features."
    )
PY_CAPABILITIES
test -x /mnt/rosetta
grep -qx 'interpreter /mnt/rosetta' /proc/sys/fs/binfmt_misc/rosetta
printf '1\n' > /proc/sys/fs/binfmt_misc/rosetta
grep -qx enabled /proc/sys/fs/binfmt_misc/rosetta
if test -e /proc/sys/fs/binfmt_misc/qemu-x86_64; then
  printf '0\n' > /proc/sys/fs/binfmt_misc/qemu-x86_64
  grep -qx disabled /proc/sys/fs/binfmt_misc/qemu-x86_64
fi
"#;

pub(super) fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes)
        .map(|value| value.trim_end_matches('\n').to_owned())
        .map_err(|_| Failure::new(1, "Local CI command returned invalid UTF-8"))
}

pub(super) fn interrupted(failure: &Failure) -> bool {
    matches!(failure.code, 130 | 143)
}

fn guard(failure: Failure, message: &str) -> Failure {
    if interrupted(&failure) {
        failure
    } else {
        Failure::new(2, message)
    }
}

pub(super) async fn select(
    runner: &mut Runner,
    context: &mut Context,
    options: &Options,
) -> Result<String> {
    let explicit = runner
        .environment
        .get(OsStr::new("DOCKER_HOST"))
        .cloned()
        .unwrap_or_default();
    if options.disposable {
        let host = explicit
            .to_str()
            .filter(|host| host.starts_with("unix:///"))
            .ok_or_else(|| {
                Failure::new(
                    2,
                    "--disposable-engine requires an explicit local unix:// DOCKER_HOST.",
                )
            })?;
        return Ok(host[7..].to_owned());
    }
    if text(runner.capture("uname", ["-s"]).await?)? != "Darwin" {
        return Err(Failure::new(
            2,
            "Linux: set DOCKER_HOST for an isolated engine and pass --disposable-engine.",
        ));
    }
    if !explicit.is_empty() {
        return Err(Failure::new(
            2,
            "Explicit DOCKER_HOST requires --disposable-engine; it is never selected implicitly.",
        ));
    }
    if text(runner.capture("uname", ["-m"]).await?)? == "arm64" {
        context.image = ARM64_IMAGE.into();
        context.platform = "linux/arm64".into();
    }
    super::require_tool(&runner.root, &runner.environment, "podman")
        .map_err(|_| Failure::new(2, "Install Podman to use the owned simplestchat-ci VM."))?;
    match runner
        .quiet("podman", ["machine", "inspect", MACHINE])
        .await
    {
        Ok(_) => {}
        Err(error) if interrupted(&error) => return Err(error),
        Err(_) => {
            runner
                .run(
                    "podman",
                    [
                        "machine",
                        "init",
                        "--rootful",
                        "--cpus",
                        "12",
                        "--memory",
                        "81920",
                        "--disk-size",
                        "80",
                        MACHINE,
                    ],
                )
                .await?
        }
    }
    let state = text(
        runner
            .capture(
                "podman",
                ["machine", "inspect", "--format", "{{.State}}", MACHINE],
            )
            .await?,
    )?;
    if state != "running" {
        let machines = text(
            runner
                .capture(
                    "podman",
                    ["machine", "list", "--format", "{{.Name}} {{.Running}}"],
                )
                .await?,
        )?;
        let other = machines
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let name = fields.next()?;
                (fields.next() == Some("true") && name != MACHINE).then_some(name)
            })
            .collect::<Vec<_>>();
        if !other.is_empty() {
            return Err(Failure::new(
                2,
                format!(
                    "Another Podman VM is running ({}); it was left untouched.",
                    other.join(" ")
                ),
            ));
        }
        runner.run("podman", ["machine", "start", MACHINE]).await?;
        context.started_machine = true;
    }
    let socket = text(
        runner
            .capture(
                "podman",
                [
                    "machine",
                    "inspect",
                    "--format",
                    "{{.ConnectionInfo.PodmanSocket.Path}}",
                    MACHINE,
                ],
            )
            .await?,
    )?;
    if !Path::new(&socket).is_absolute() {
        return Err(Failure::new(
            2,
            "The owned VM did not expose an absolute Unix socket.",
        ));
    }
    runner
        .environment
        .insert("DOCKER_HOST".into(), format!("unix://{socket}").into());
    Ok(socket)
}

pub(super) async fn snapshot(
    runner: &mut Runner,
    socket: &str,
    endpoint: &str,
    path: &Path,
) -> Result<Value> {
    let bytes = runner
        .capture(
            "curl",
            [
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                "10",
                "--unix-socket",
                socket,
                endpoint,
            ],
        )
        .await?;
    evidence::write(path, &bytes)?;
    serde_json::from_slice(&bytes).map_err(|error| Failure::new(1, error.to_string()))
}

pub(super) async fn ready(runner: &mut Runner, socket: &str, output: &Path) -> Result<()> {
    runner
        .capture(
            "curl",
            [
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                "10",
                "--unix-socket",
                socket,
                "http://localhost/_ping",
            ],
        )
        .await
        .map_err(|failure| guard(failure, "The owned container engine is not responding."))?;
    let containers = snapshot(
        runner,
        socket,
        "http://localhost/containers/json?all=1",
        &output.join("engine-before.json"),
    )
    .await?;
    empty(
        &containers,
        "The disposable engine contains existing containers; they were left untouched.",
    )
}

pub(super) fn empty(value: &Value, message: &str) -> Result<()> {
    if value.as_array().is_some_and(Vec::is_empty) {
        Ok(())
    } else {
        Err(Failure::new(1, message))
    }
}

pub(super) fn gateway(networks: &Value) -> Result<Ipv4Addr> {
    let error = || {
        Failure::new(
            1,
            "The isolated engine must expose one private IPv4 bridge gateway",
        )
    };
    let mut addresses = Vec::new();
    for network in networks.as_array().ok_or_else(error)? {
        if network["Name"] != "bridge" {
            continue;
        }
        for entry in network["IPAM"]["Config"].as_array().ok_or_else(error)? {
            let Some(address) = entry.get("Gateway") else {
                continue;
            };
            let address = address
                .as_str()
                .ok_or_else(error)?
                .parse::<IpAddr>()
                .map_err(|_| error())?;
            if let IpAddr::V4(address) = address {
                addresses.push(address);
            }
        }
    }
    if addresses.len() == 1 && reference_private(addresses[0]) {
        Ok(addresses[0])
    } else {
        Err(error())
    }
}

/// Preserve `ipaddress.ip_address(...).is_private` from the Python reference:
/// this is the non-global IANA range policy, rather than only RFC 1918.
fn reference_private(address: Ipv4Addr) -> bool {
    match address.octets() {
        [0 | 10 | 127, ..] => true,
        [169, 254, ..] | [172, 16..=31, ..] => true,
        [192, 0, 0, last] => !matches!(last, 9 | 10),
        [192, 0, 2, _]
        | [192, 168, ..]
        | [198, 18..=19, ..]
        | [198, 51, 100, _]
        | [203, 0, 113, _] => true,
        [first, ..] => first >= 240,
    }
}

pub(super) async fn stop(runner: &mut Runner) -> Result<()> {
    runner.run("podman", ["machine", "stop", MACHINE]).await
}

pub(super) async fn arm64(runner: &mut Runner) -> Result<()> {
    runner
        .input(
            "podman",
            ["machine", "ssh", MACHINE, "sudo sh -se"],
            ARM64_GUEST.as_bytes(),
        )
        .await
}
