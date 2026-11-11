//! The running relay: which container it is, the certificate it presents,
//! and its loopback metrics.

use super::settings::settings;
use super::store::Site;
use crate::host::command::Request;
use crate::host::fault::{Numbered, Result, VALUE, require, utf8};
use crate::host::hexadecimal;
use std::net::IpAddr;
use std::time::Duration;

const PROJECT: [&str; 2] = ["--project-name", "simplestchat-turn"];
const LABELS: &str = r#"{{.Image}} {{index .Config.Labels "com.docker.compose.config-hash"}}"#;
/// The three options that give the relay a loopback metrics listener.
pub(super) const METRICS_OPTIONS: &str =
    "prometheus\nprometheus-address=127.0.0.1\nprometheus-port=9641\n";

/// A project command of the separate relay project.
pub(super) fn compose(
    site: &Site<'_>,
    runner: &mut Numbered<'_>,
    arguments: &[&str],
) -> Result<Vec<u8>> {
    let directory = site.host.paths.relay.to_str().ok_or(VALUE)?.to_owned();
    let project = format!("{directory}/compose.yml");
    let mut words = vec![
        "compose",
        PROJECT[0],
        PROJECT[1],
        "--project-directory",
        &directory,
        "-f",
        &project,
    ];
    words.extend(arguments);
    runner.docker(&words, 30.0)
}

/// Exactly the one running container of the relay project, on the pinned
/// image and the configuration the project resolves to.
pub(super) fn identity(site: &Site<'_>, runner: &mut Numbered<'_>) -> Result<String> {
    let listing = [
        "ps",
        "--quiet",
        "--no-trunc",
        "--filter",
        "label=com.docker.compose.project=simplestchat-turn",
        "--filter",
        "label=com.docker.compose.service=turn",
    ];
    let listed = utf8(runner.docker(&listing, 30.0)?)?;
    let identity = listed.trim().to_owned();
    require(
        hexadecimal(&identity, 64),
        "Expected one running managed relay",
    )?;
    let selected = settings(site)?;
    let inspection = [
        "image",
        "inspect",
        "--format",
        "{{.Id}}",
        selected.image.as_str(),
    ];
    let image = utf8(runner.docker(&inspection, 30.0)?)?;
    let running = ["inspect", "--format", LABELS, identity.as_str()];
    let actual = utf8(runner.docker(&running, 30.0)?)?;
    let expected = utf8(compose(site, runner, &["config", "--hash", "turn"])?)?;
    let (actual, expected): (Vec<&str>, Vec<&str>) = (
        actual.split_whitespace().collect(),
        expected.split_whitespace().collect(),
    );
    let reviewed =
        expected.len() == 2 && expected[0] == "turn" && actual == [image.trim(), expected[1]];
    require(
        reviewed,
        "Running relay differs from its reviewed configuration",
    )?;
    Ok(identity)
}

/// Where a TLS check connects: the hostname, or an address given instead
/// while the name still points elsewhere.
pub(super) fn endpoint(hostname: &str, address: Option<IpAddr>) -> String {
    match address {
        None => format!("{hostname}:5349"),
        Some(IpAddr::V4(address)) => format!("{address}:5349"),
        Some(IpAddr::V6(address)) => format!("[{address}]:5349"),
    }
}

/// The SHA-256 fingerprint of the first certificate in `input`, or of the
/// file named in `argv`.
fn fingerprint(site: &Site<'_>, source: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut argv = vec![
        "/usr/bin/openssl",
        "x509",
        "-noout",
        "-fingerprint",
        "-sha256",
    ];
    argv.extend(source);
    let argv = argv.iter().map(|word| (*word).to_owned()).collect();
    let oversized = "Oversized certificate fingerprint";
    let request = Request {
        input: Some(input),
        ..Request::fixed(argv, 10.0, 4096, oversized)
    };
    Ok(site.host.commands.run(&request)?)
}

/// Require the relay to present, over TLS 1.2 or later with a chain the
/// system trusts for `hostname`, exactly the published leaf certificate,
/// within ten seconds.
pub(super) fn verify_tls(site: &Site<'_>, hostname: &str, address: Option<IpAddr>) -> Result<()> {
    let published = site.host.paths.relay.join("tls/current/certificate.pem");
    std::fs::read(&published)?;
    let published = published.to_str().ok_or(VALUE)?;
    let expected = fingerprint(site, &["-in", published], b"")?;
    let endpoint = endpoint(hostname, address);
    let handshake = [
        "/usr/bin/openssl",
        "s_client",
        "-connect",
        &endpoint,
        "-servername",
        hostname,
        "-verify_hostname",
        hostname,
        "-verify_return_error",
        "-no_tls1",
        "-no_tls1_1",
    ];
    let clock = site.host.clock;
    let deadline = clock.monotonic() + Duration::from_secs(10);
    loop {
        let argv = handshake.map(str::to_owned).to_vec();
        let oversized = "Oversized relay handshake";
        let request = Request {
            input: Some(b""),
            ..Request::fixed(argv, 3.0, 1024 * 1024, oversized)
        };
        let presented = site.host.commands.run(&request);
        let presented = presented
            .map_err(Into::into)
            .and_then(|session| fingerprint(site, &[], &session));
        if presented.is_ok_and(|presented| presented == expected) {
            return Ok(());
        }
        let late = clock.monotonic() >= deadline;
        require(!late, "Relay TLS verification failed")?;
        clock.sleep(Duration::from_millis(200));
    }
}

/// Require a metrics listener on loopback only that reports the relay's own
/// allocation counters.
pub(super) fn verify_metrics(runner: &mut Numbered<'_>) -> Result<()> {
    // The configuration selects loopback; the socket listing proves the
    // executable honored it. A wildcard listener is never accepted.
    let listening = utf8(runner.run(&["/usr/bin/ss", "-H", "-ltn", "sport = :9641"], 30.0)?)?;
    let listeners: Vec<&str> = listening.lines().collect();
    let local =
        listeners.len() == 1 && listeners[0].split_whitespace().nth(3) == Some("127.0.0.1:9641");
    require(local, "Relay metrics must listen only on loopback")?;
    let scrape = [
        "/usr/bin/curl",
        "--silent",
        "--show-error",
        "--fail",
        "--max-time",
        "3",
        "--noproxy",
        "*",
        "http://127.0.0.1:9641/metrics",
    ];
    let metrics = runner.run(&scrape, 30.0)?;
    let counter = b"turn_total_allocations";
    let counted = metrics
        .windows(counter.len())
        .any(|window| window == counter);
    require(counted, "Relay allocation metrics missing")
}
