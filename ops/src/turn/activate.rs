//! Telling the application of the verified relay. An application that
//! already has the relay's three settings and runs what is on disk is only
//! checked; any other is to be restarted as the release it already runs,
//! through the bounded application replacement.

use super::store::Site;
use crate::host::fault::{
    Fault, Numbered, Result, SYNTAX, VALUE, decode, field, file, protected, require, set, string,
    text, utf8,
};
use crate::host::hexadecimal;
use crate::json::Json;
use crate::release::deploy::Relay;
use crate::release::manifest::Manifest;
use crate::text::python_lines;
use std::net::IpAddr;
use std::time::Duration;

const PROJECT: [&str; 2] = ["--project-name", "simplestchat-public"];
const CONTAINER_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},"#,
    r#""restarts":{{.RestartCount}},"#,
    r#""configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}"#,
);
/// A manifest that is not one.
const MANIFEST: Fault = Fault::Kind("ArtifactError");

/// The release an application is restarted as, and the relay it is told of.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Replacement {
    pub revision: String,
    pub relay: Relay,
}

/// A project command of the application's own project.
fn compose(site: &Site<'_>, runner: &mut Numbered<'_>, arguments: &[&str]) -> Result<Vec<u8>> {
    let directory = site.host.paths.config.to_str().ok_or(VALUE)?.to_owned();
    let environment = format!("{directory}/app.env");
    let project = format!("{directory}/compose.public.yml");
    let mut words = vec![
        "compose",
        PROJECT[0],
        PROJECT[1],
        "--project-directory",
        &directory,
        "--env-file",
        &environment,
        "-f",
        &project,
    ];
    words.extend(arguments);
    runner.docker(&words, 30.0)
}

/// The inspection of the one application container, which must be running
/// normally.
fn running(site: &Site<'_>, runner: &mut Numbered<'_>) -> Result<Json> {
    let listing = ["ps", "--status", "running", "--quiet", "simplestchat"];
    let listed = utf8(compose(site, runner, &listing)?)?;
    let identity = listed.trim();
    let one = hexadecimal(identity, 64);
    require(
        one,
        "Exactly one running simplestchat container is required",
    )?;
    let inspection = ["inspect", "--format", CONTAINER_FORMAT, identity];
    let inspected = decode(&runner.docker(&inspection, 30.0)?)?;
    let state = field(&inspected, "state")?;
    let normal = field(state, "Running")?.truthiness() && !field(state, "OOMKilled")?.truthiness();
    require(normal, "simplestchat is not running normally")?;
    Ok(inspected)
}

/// Require the application to answer as ready at its public origin within
/// three seconds, at `address` when one is given.
fn ready(
    site: &Site<'_>,
    runner: &mut Numbered<'_>,
    hostname: &str,
    address: Option<IpAddr>,
) -> Result<()> {
    let clock = site.host.clock;
    let deadline = clock.monotonic() + Duration::from_secs(3);
    let url = format!("https://{hostname}/ready");
    let destination = address.map(|address| match address {
        IpAddr::V6(address) => format!("{hostname}:443:[{address}]"),
        IpAddr::V4(address) => format!("{hostname}:443:{address}"),
    });
    let mut probe = vec![
        "/usr/bin/curl",
        "--disable",
        "--noproxy",
        "*",
        "--proto",
        "=https",
        "--fail",
        "--silent",
        "--show-error",
        "--max-time",
        "2",
    ];
    if let Some(destination) = &destination {
        probe.extend(["--resolve", destination]);
    }
    probe.push(&url);
    loop {
        let answered = runner.run(&probe, 5.0).and_then(|answer| {
            let value = decode(&answer)?;
            let status = value.get("status").and_then(Json::string);
            require(status == Some("ready"), "Application is not ready")
        });
        match answered {
            Ok(()) => return Ok(()),
            // A refusal or an answer that is not JSON is asked again; a
            // probe that outlives its own deadline is not.
            Err(fault) if matches!(fault, Fault::Refused(_)) || fault == SYNTAX => {
                let late = clock.monotonic() >= deadline;
                require(!late, "Application readiness deadline exceeded")?;
                clock.sleep(Duration::from_millis(500));
            }
            Err(fault) => return Err(fault),
        }
    }
}

/// The application's settings by name, a later line deciding. A commented
/// line is a setting of a name no relay setting has.
fn assignments(environment: &str) -> Vec<(&str, &str)> {
    let lines = python_lines(environment);
    let mut found: Vec<(&str, &str)> = Vec::new();
    for (name, value) in lines.filter_map(|line| line.split_once('=')) {
        found.retain(|(known, _)| *known != name);
        found.push((name, value));
    }
    found
}

/// Decide what telling the application of the relay takes: nothing, for an
/// application that has the settings, runs what is on disk and is ready;
/// otherwise a restart as the selected release, which must be retained
/// under its own identity.
pub(super) fn decide(
    site: &Site<'_>,
    runner: &mut Numbered<'_>,
    hostname: &str,
    address: Option<IpAddr>,
    report: &mut Json,
) -> Result<Option<Replacement>> {
    let host = site.host;
    let shared = host.paths.relay.join("secret");
    protected(host.owner, &shared, file(0o600, 64))?;
    let relay = Relay {
        domain: hostname.to_owned(),
        secret: utf8(std::fs::read(&shared)?)?,
        address,
    };
    let invalid = Fault::Refused("Invalid managed TURN configuration".to_owned());
    let desired = relay.settings().ok_or(invalid)?;
    let environment = utf8(std::fs::read(host.paths.config.join("app.env"))?)?;
    let current = assignments(&environment);
    let told = |(name, value): &(&str, String)| current.contains(&(*name, value.as_str()));
    if desired.iter().all(told) {
        let application = running(site, runner)?;
        let resolution = ["config", "--hash", "simplestchat"];
        let resolved = utf8(compose(site, runner, &resolution)?)?;
        let resolved: Vec<&str> = resolved.split_whitespace().collect();
        // What the project resolves to is asked for before the running
        // container's own digest is read.
        let digest = field(&application, "configHash")?.string();
        let same = digest.is_some_and(|digest| resolved == ["simplestchat", digest]);
        require(same, "Running configuration differs from disk")?;
        set(report, "alreadyConfigured", Json::Bool(true));
        ready(site, runner, hostname, address)?;
        return Ok(None);
    }
    let selected = decode(&std::fs::read(host.paths.config.join("images.json"))?)?;
    let revision = text(field(&selected, "revision")?)?.to_owned();
    let full = hexadecimal(&revision, 40);
    require(full, "Unexpected application revision")?;
    let retained = host.paths.root.join("releases").join(&revision);
    let retained = retained.join("release.json");
    protected(host.owner, &retained, file(0o600, 65536))?;
    let manifest = Manifest::decode(&std::fs::read(&retained)?).map_err(|_| MANIFEST)?;
    let same = manifest.fields().revision == revision;
    require(same, "Deployed release identity differs")?;
    set(report, "revision", string(&revision));
    Ok(Some(Replacement { revision, relay }))
}
