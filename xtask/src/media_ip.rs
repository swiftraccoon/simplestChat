//! `media-ip`: the IPv4 address an owned non-loopback media announcement may
//! use on this machine, read from the default route's interface and nothing
//! else. Browser suites announce it (`TEST_ANNOUNCE_IP`) so Firefox's native
//! ICE sees a real candidate while the HTTP origin and database stay on
//! loopback. Only routing and interface state is read: no DNS, no probes,
//! no configuration changes.

use anyhow::{Context, Result, anyhow, bail};
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const OUTPUT_LIMIT: usize = 64 * 1024;

/// The platforms whose routing tools this command reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Linux,
    Darwin,
}

impl Platform {
    fn current() -> Result<Self> {
        match std::env::consts::OS {
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Darwin),
            other => bail!("Unsupported platform: {other}; expected linux or darwin"),
        }
    }

    /// The read-only default-route query.
    fn route_command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Darwin => ("route", &["-n", "get", "default"]),
            Self::Linux => ("ip", &["-j", "-4", "route", "show", "default"]),
        }
    }

    /// The read-only address listing for one interface.
    fn address_command(self, interface: &str) -> (&'static str, Vec<String>) {
        match self {
            Self::Darwin => ("ifconfig", vec![interface.to_owned()]),
            Self::Linux => (
                "ip",
                ["-j", "-4", "addr", "show", "dev", interface]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            ),
        }
    }
}

pub(crate) fn run(_root: &Path, arguments: &[String]) -> Result<()> {
    if !arguments.is_empty() {
        bail!("Usage: cargo run -q -p xtask -- media-ip");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(detect()) {
        Ok(address) => {
            println!("{address}");
            Ok(())
        }
        Err(error) => Err(anyhow!("Cannot select test media IP: {error:#}")),
    }
}

pub(crate) async fn detect() -> Result<Ipv4Addr> {
    let platform = Platform::current()?;
    let (command, arguments) = platform.route_command();
    let output = read_tool(
        command,
        arguments.iter().map(|arg| (*arg).to_owned()).collect(),
    )
    .await?;
    let interface = default_interface(platform, &output)?;
    let (command, arguments) = platform.address_command(&interface);
    let listing = read_tool(command, arguments).await?;
    select_address(platform, &interface, &listing)
}

/// A tool's standard output with a clean locale, a deadline and a size bound;
/// its own output never reaches an error message.
async fn read_tool(command: &str, arguments: Vec<String>) -> Result<String> {
    let child = Command::new(command)
        .args(&arguments)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("Cannot read default route with {command}: command not installed")
        }
        Err(_) => bail!("Cannot read default route with {command}: command failed or timed out"),
    };
    let output = tokio::time::timeout(COMMAND_TIMEOUT, child.wait_with_output())
        .await
        .ok()
        .and_then(|result| result.ok())
        .filter(|output| output.status.success())
        .ok_or_else(|| {
            anyhow!("Cannot read default route with {command}: command failed or timed out")
        })?;
    if output.stdout.len() > OUTPUT_LIMIT {
        bail!("Cannot read default route with {command}: command failed or timed out");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The one interface the default route uses.
pub(crate) fn default_interface(platform: Platform, output: &str) -> Result<String> {
    if output.trim().is_empty() {
        bail!("Default-route command returned no output");
    }
    match platform {
        Platform::Darwin => {
            let interfaces: Vec<&str> = output
                .lines()
                .filter_map(|line| line.trim().strip_prefix("interface:"))
                .map(str::trim)
                .collect();
            match interfaces[..] {
                [name] if !name.is_empty() && !name.contains(char::is_whitespace) => {
                    Ok(name.to_owned())
                }
                _ => bail!("Expected exactly one default-route interface from route"),
            }
        }
        Platform::Linux => {
            let routes: serde_json::Value =
                serde_json::from_str(output).context("Invalid JSON from default-route command")?;
            let Some(routes) = routes.as_array().filter(|routes| !routes.is_empty()) else {
                bail!("No IPv4 default route found");
            };
            let mut entries = Vec::new();
            for route in routes {
                let Some(route) = route.as_object() else {
                    bail!("Invalid IPv4 default-route entry");
                };
                let destination = route.get("dst").and_then(serde_json::Value::as_str);
                let kind = route.get("type");
                if !matches!(destination, Some("default" | "0.0.0.0/0"))
                    || kind.is_some_and(|kind| kind.as_str() != Some("unicast"))
                {
                    bail!("Invalid IPv4 default-route entry");
                }
                let device = route
                    .get("dev")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|device| !device.is_empty() && !device.contains(char::is_whitespace));
                let Some(device) = device else {
                    bail!(
                        "Default route has no single interface; multipath routes are unsupported"
                    );
                };
                let metric = match route.get("metric") {
                    None => 0,
                    Some(value) => value
                        .as_u64()
                        .filter(|_| value.is_u64())
                        .ok_or_else(|| anyhow!("Invalid default-route metric"))?,
                };
                entries.push((metric, device.to_owned()));
            }
            let best = entries.iter().map(|(metric, _)| *metric).min().unwrap_or(0);
            let mut chosen: Vec<&str> = entries
                .iter()
                .filter(|(metric, _)| *metric == best)
                .map(|(_, device)| device.as_str())
                .collect();
            chosen.sort_unstable();
            chosen.dedup();
            match chosen[..] {
                [device] => Ok(device.to_owned()),
                _ => bail!("Ambiguous IPv4 default route: equal metrics on different interfaces"),
            }
        }
    }
}

/// The interface's first owned unicast IPv4 address: never unspecified,
/// loopback, link-local, multicast or broadcast.
pub(crate) fn select_address(
    platform: Platform,
    interface: &str,
    listing: &str,
) -> Result<Ipv4Addr> {
    let candidates: Vec<Ipv4Addr> = match platform {
        Platform::Darwin => listing
            .lines()
            .filter_map(|line| line.trim().strip_prefix("inet "))
            .filter_map(|rest| rest.split_whitespace().next())
            .filter_map(|address| address.parse().ok())
            .collect(),
        Platform::Linux => {
            let entries: serde_json::Value = serde_json::from_str(listing)
                .context("Invalid JSON from the interface address command")?;
            entries
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get("addr_info").and_then(serde_json::Value::as_array))
                .flatten()
                .filter(|info| {
                    info.get("family").and_then(serde_json::Value::as_str) == Some("inet")
                })
                .filter_map(|info| info.get("local").and_then(serde_json::Value::as_str))
                .filter_map(|address| address.parse().ok())
                .collect()
        }
    };
    candidates
        .into_iter()
        .find(|address| usable(*address))
        .ok_or_else(|| {
            anyhow!(
                "Default-route interface {interface} has no usable owned non-loopback IPv4 address"
            )
        })
}

fn usable(address: Ipv4Addr) -> bool {
    let [first, ..] = address.octets();
    first != 0
        && !address.is_loopback()
        && !address.is_link_local()
        && !address.is_multicast()
        && !address.is_broadcast()
        && first < 224
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC_ROUTE: &str = "   route to: default\ndestination: default\n    gateway: 198.51.100.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC>\n";

    fn linux_routes(routes: &[(&str, Option<u64>)]) -> String {
        let entries: Vec<serde_json::Value> = routes
            .iter()
            .map(|(device, metric)| match metric {
                Some(metric) => {
                    serde_json::json!({"dst": "default", "dev": device, "metric": metric})
                }
                None => serde_json::json!({"dst": "default", "dev": device}),
            })
            .collect();
        serde_json::Value::Array(entries).to_string()
    }

    #[test]
    fn linux_and_macos_name_the_default_route_interface() {
        assert_eq!(
            default_interface(Platform::Linux, &linux_routes(&[("eth0", None)])).unwrap(),
            "eth0"
        );
        assert_eq!(
            default_interface(Platform::Darwin, MAC_ROUTE).unwrap(),
            "en0"
        );
    }

    #[test]
    fn multiple_linux_defaults_prefer_the_lowest_metric_and_must_agree() {
        for routes in [
            linux_routes(&[("en0", Some(600)), ("eth0", Some(100))]),
            linux_routes(&[("eth0", Some(100)), ("en0", Some(600))]),
            linux_routes(&[("en0", Some(1)), ("eth0", None)]),
            linux_routes(&[("eth0", None), ("eth0", None)]),
        ] {
            assert_eq!(
                default_interface(Platform::Linux, &routes).unwrap(),
                "eth0",
                "{routes}"
            );
        }
        let ambiguous = default_interface(
            Platform::Linux,
            &linux_routes(&[("eth0", None), ("en0", None)]),
        );
        assert!(ambiguous.unwrap_err().to_string().contains("Ambiguous"));
    }

    #[test]
    fn malformed_routes_fail_clearly() {
        for output in [
            "",
            " ",
            "not JSON",
            "{}",
            "[]",
            "[null]",
            "[[]]",
            r#"[{"dst": "192.0.2.0/24", "dev": "eth0"}]"#,
            r#"[{"dst": "default", "dev": ""}]"#,
            r#"[{"dst": "default", "dev": "eth0 extra"}]"#,
            r#"[{"dst": "default", "dev": "eth0", "metric": -1}]"#,
            r#"[{"dst": "default", "dev": "eth0", "metric": "100"}]"#,
            r#"[{"dst": "default", "dev": "eth0", "metric": 1.5}]"#,
            r#"[{"dst": "default", "dev": "eth0", "type": "blackhole"}]"#,
            r#"[{"dst": "default", "nexthops": [{"dev": "eth0"}, {"dev": "en0"}]}]"#,
        ] {
            let error = default_interface(Platform::Linux, output)
                .unwrap_err()
                .to_string();
            assert!(
                error.to_ascii_lowercase().contains("route") || error.contains("JSON"),
                "{output:?}: {error}"
            );
        }
        for output in [
            "",
            "route: not found",
            "interface:",
            "interface:\nen0",
            "interface: en0 extra",
            &format!("{MAC_ROUTE}\ninterface: eth0\n"),
        ] {
            let error = default_interface(Platform::Darwin, output)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("route") || error.contains("interface"),
                "{output:?}: {error}"
            );
        }
    }

    #[test]
    fn the_first_usable_owned_address_wins_and_unusable_ones_are_skipped() {
        let listing = "\tinet6 fe80::1234%eth0 prefixlen 64\n\tinet 0.0.0.0 netmask 0xff000000\n\
\tinet 127.2.3.4 netmask 0xff000000\n\tinet 169.254.1.1 netmask 0xffff0000\n\
\tinet 224.0.0.1 netmask 0xf0000000\n\tinet 255.255.255.255 netmask 0xffffffff\n\
\tinet 192.0.2.20 netmask 0xffffff00 broadcast 192.0.2.255\n\tinet 192.0.2.21 netmask 0xffffff00\n";
        assert_eq!(
            select_address(Platform::Darwin, "eth0", listing).unwrap(),
            Ipv4Addr::new(192, 0, 2, 20)
        );
        let json = r#"[{"ifname": "eth0", "addr_info": [
            {"family": "inet6", "local": "fe80::1234"},
            {"family": "inet", "local": "127.0.0.1"},
            {"family": "inet", "local": "192.0.2.44"}]}]"#;
        assert_eq!(
            select_address(Platform::Linux, "eth0", json).unwrap(),
            Ipv4Addr::new(192, 0, 2, 44)
        );
        for listing in [
            "",
            "\tinet 127.0.0.1 netmask 0xff000000\n",
            "status: active\n",
        ] {
            let error = select_address(Platform::Darwin, "missing", listing)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("missing has no usable owned"),
                "{listing:?}: {error}"
            );
        }
    }
}
