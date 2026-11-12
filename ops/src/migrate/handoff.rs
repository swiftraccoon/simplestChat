//! A two-hour bridge on the sealed source of a move that keeps its hostname.
//! While cached name answers still lead to the old address, one forwarding
//! proxy there presents the hostname's own certificate and passes every
//! request to the destination. The source's application, proxy and database
//! are never started: the bridge is a new container, created only after the
//! source is proven sealed and unchanged, and its expiry is scheduled before
//! it is started.

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;

use super::deployment::{Database, Move, bound, configuration, journal, machine, owned_container};
use super::request::{MAX_PRIVATE, Request, read_object, request_file, taken};
use super::source::MAX_DUMP;
use super::tls::{Files, operation, snapshot, validate};
use super::value::{says, stripped};
use crate::host::command::Local;
use crate::host::evidence::Evidence;
use crate::host::failure::Failure;
use crate::host::fault::{Fault, KEY, Result, VALUE, field, object, require, string};
use crate::host::{Host, Paths, System, emit, hexadecimal, image_id, state};
use crate::json::{InOrder, Json, python_dump};
use crate::release::assets::{self, Takeover, Workload};
use crate::turn::store::Site;
use std::ffi::OsString;
use std::io::Write;
use std::net::Ipv4Addr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, fchown};
use std::path::Path;
use std::process::ExitCode;

/// The one line a failed handoff leaves the caller.
const FAILED: &str = "Migration DNS handoff failed; inspect the retained private evidence.\n";
/// How long the bridge lives, as the caller must state it.
const LIFETIME: &str = "7200";
const SEALED: [&str; 2] = ["cutover-sealed", "retired"];
const SERVICES: [&str; 3] = ["simplestchat", "caddy", "postgres"];
/// The proxy images a bridge may run, each pinned by tag and digest.
const PROXIES: [&str; 2] = [
    "docker.io/library/caddy",
    "ghcr.io/swiftraccoon/simplestchat-caddy",
];
/// The files the bridge mounts, and where.
const MOUNTS: [(&str, &str); 3] = [
    ("certificate.pem", "/tls/certificate.pem"),
    ("key.pem", "/tls/key.pem"),
    ("Caddyfile", "/etc/caddy/Caddyfile"),
];
/// Address blocks that are not globally reachable: the shared address
/// space, then the special-purpose registry's private blocks as Python 3.13
/// reads it, and the two addresses that registry excepts.
const SPECIAL: [(Ipv4Addr, u32); 15] = [
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 0, 0, 170), 31),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
    (Ipv4Addr::new(255, 255, 255, 255), 32),
];
const EXCEPTED: [Ipv4Addr; 2] = [Ipv4Addr::new(192, 0, 0, 9), Ipv4Addr::new(192, 0, 0, 10)];

/// Whether an address is globally reachable.
fn global(address: Ipv4Addr) -> bool {
    let within = |(network, prefix): &(Ipv4Addr, u32)| {
        let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
        u32::from(address) & mask == u32::from(*network)
    };
    let shared = within(&SPECIAL[0]);
    let special = SPECIAL[1..].iter().any(within) && !EXCEPTED.contains(&address);
    !shared && !special
}

/// A public address. Only the one canonical spelling of an address is
/// read as one: four decimal numbers without leading zeros or blanks.
fn public(value: &str) -> Result<Ipv4Addr> {
    let address: Ipv4Addr = value
        .parse()
        .map_err(|_| Fault::Kind("AddressValueError"))?;
    require(global(address), "handoff_public_ipv4_required")?;
    Ok(address)
}

/// Whether a selector names one of the proxy images by tag and digest.
fn pinned(selector: &str) -> bool {
    let Some((named, digest)) = selector.rsplit_once("@sha256:") else {
        return false;
    };
    let Some((image, tag)) = named.rsplit_once(':') else {
        return false;
    };
    let plain =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte);
    PROXIES.contains(&image) && !tag.is_empty() && tag.bytes().all(plain) && hexadecimal(digest, 64)
}

/// The bridge's whole configuration: plain requests are sent to the secure
/// origin, and secure ones to the destination under the hostname's name.
fn configuration_for(domain: &str, target: &str) -> String {
    format!(
        "{{\n admin off\n auto_https off\n servers {{\n  protocols h1 h2\n }}\n}}\n\
         http://{domain} {{\n redir https://{domain}{{uri}} permanent\n}}\n\
         https://{domain} {{\n tls /tls/certificate.pem /tls/key.pem\n \
         reverse_proxy https://{target} {{\n  header_up Host {domain}\n  \
         transport http {{\n   tls_server_name {domain}\n  }}\n }}\n}}\n"
    )
}

/// A failed command is named by its number in the kept evidence.
fn numbered(session: &Move<'_>, failure: Failure) -> Fault {
    match failure {
        Failure::Command => {
            let number = session.runner.number;
            Fault::Refused(format!(
                "Command {number:03} failed; inspect private output"
            ))
        }
        other => Fault::from(other),
    }
}

fn utf8(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| VALUE)
}

/// Require the sealed source this journal describes: the same machine, the
/// same site at the stated address, and its three containers stopped for
/// good and unchanged. The selected images are returned. That the request
/// keeps one origin, the hostname's, was proven when the operation was
/// admitted.
fn sealed_source(session: &mut Move<'_>, prior: &Json, source: &str) -> Result<Json> {
    let host = session.host;
    let request = session.request;
    let release = host.paths.root.join("release-state.json");
    std::fs::symlink_metadata(&release)?;
    let shared = read_object(host, &release, MAX_PRIVATE)?;
    let phase = shared.get("phase").and_then(Json::string);
    let sealed = says(&shared, "operation", "server_migration")
        && bound(&shared, request)
        && phase.is_some_and(|phase| SEALED.contains(&phase));
    require(sealed, "handoff_global_seal_required")?;
    let before = field(prior, "before")?;
    let same = field(before, "machineId")?.string() == Some(machine(host)?.as_str());
    require(same, "handoff_source_identity")?;
    let (images, environment) = configuration(host)?;
    let placed = environment.get("ALLOWED_ORIGINS").ok_or(KEY)? == &request.source
        && environment.get("ANNOUNCE_IP").ok_or(KEY)? == source;
    require(placed, "handoff_origin_or_address_mismatch")?;
    for service in SERVICES {
        let value = owned_container(session, service, false);
        let value = value.map_err(|failure| numbered(session, failure))?;
        let value = value.ok_or(VALUE)?;
        let disabled = matches!(field(&value, "running")?, Json::Bool(false))
            && field(&value, "restartPolicy")?.string() == Some("no");
        require(disabled, "handoff_source_writer_not_disabled")?;
        let recorded = field(field(field(before, "containers")?, service)?, "id")?;
        let unchanged = field(&value, "id")?.semantic_eq(recorded);
        require(unchanged, "handoff_source_container_changed")?;
    }
    Ok(images)
}

/// One new private file that the proxy's own account may read.
fn held(site: &Site<'_>, path: &Path, data: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    let mut output = options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    fchown(&output, Some(site.proxy), Some(site.proxy_group))?;
    output.write_all(data)?;
    output.sync_all()?;
    Ok(())
}

/// The command line that creates the bridge without starting it.
fn creation(operation: &str, source: &str, attempt: &Path, image: &str) -> Result<Vec<String>> {
    let name = format!("simplestchat-dns-handoff-{operation}");
    let attempt = attempt.to_str().ok_or(VALUE)?;
    let mut command: Vec<String> = [
        "create",
        "--name",
        &name,
        "--pull=never",
        "--restart=no",
        "--read-only",
        "--user",
        "10001:10001",
        "--cap-drop=ALL",
        "--cap-add=NET_BIND_SERVICE",
        "--security-opt=no-new-privileges",
        "--memory=256m",
        "--cpus=0.5",
        "--pids-limit=128",
        "--label",
        &format!("simplestchat.migration.handoff={operation}"),
        "--label",
        &format!("com.docker.compose.project={name}"),
        "--log-driver=local",
        "--log-opt=max-size=10m",
        "--log-opt=max-file=2",
        "--publish",
        &format!("{source}:80:80/tcp"),
        "--publish",
        &format!("{source}:443:443/tcp"),
        "--tmpfs",
        "/data:rw,noexec,nosuid,uid=10001,gid=10001,size=8m",
        "--tmpfs",
        "/config:rw,noexec,nosuid,uid=10001,gid=10001,size=8m",
    ]
    .map(str::to_owned)
    .to_vec();
    for (file, destination) in MOUNTS {
        command.push("--mount".to_owned());
        command.push(format!(
            "type=bind,source={attempt}/{file},target={destination},readonly"
        ));
    }
    let run = [
        image,
        "caddy",
        "run",
        "--config",
        "/etc/caddy/Caddyfile",
        "--adapter",
        "caddyfile",
    ];
    command.extend(run.map(str::to_owned));
    Ok(command)
}

/// What a handoff is asked for, and its source as the address it spells.
struct Asked<'a> {
    operation: &'a str,
    source: &'a str,
    address: Ipv4Addr,
    target: &'a str,
    domain: &'a str,
}

/// Build and start the bridge under the held workload lock, and return the
/// line that reports it.
fn bridge(
    site: &Site<'_>,
    asked: &Asked<'_>,
    request: &Request,
    directory: &Path,
    probe: &dyn Fn(Ipv4Addr) -> std::io::Result<()>,
) -> Result<String> {
    let host = site.host;
    let attempt = directory.join("dns-handoff");
    require(!taken(&attempt), "handoff_already_prepared")?;
    let mut session = Move {
        host,
        request,
        directory: directory.to_owned(),
        runner: Evidence::new(host, attempt.clone()),
        rearm: &|| {},
        database: Database {
            owner: host.owner,
            group: host.owner,
            archive: MAX_DUMP,
        },
    };
    // The sealed journal is read before even the private evidence directory
    // is made.
    let prior = journal(&session)?;
    let sealed = prior.as_ref().is_some_and(|prior| {
        let phase = prior.get("phase").and_then(Json::string);
        says(prior, "role", "source") && phase.is_some_and(|phase| SEALED.contains(&phase))
    });
    require(sealed, "handoff_source_not_sealed")?;
    let prior = prior.ok_or(VALUE)?;
    std::fs::DirBuilder::new().mode(0o700).create(&attempt)?;
    let images = sealed_source(&mut session, &prior, asked.source)?;
    probe(asked.address)?;
    let selector = match field(&images, "caddyImage")? {
        Json::String(selector) => selector.as_str(),
        _ => return Err(Fault::Kind("TypeError")),
    };
    require(pinned(selector), "handoff_pinned_caddy_required")?;
    let inspection = ["image", "inspect", "--format", "{{.Id}}", selector];
    let image = session.docker(&inspection, 15.0);
    let image = utf8(image.map_err(|failure| numbered(&session, failure))?)?;
    let image = stripped(&image);
    require(image_id(image), "handoff_image_identity")?;
    let (issuer, files): (String, Files) = snapshot(site, asked.domain)?;
    validate(site, &attempt, asked.domain, &files)?;
    let part = |suffix: &str| files.iter().find(|(name, _)| name == suffix);
    let (certificate, key) = (part("crt").ok_or(KEY)?, part("key").ok_or(KEY)?);
    held(site, &attempt.join("certificate.pem"), &certificate.1)?;
    held(site, &attempt.join("key.pem"), &key.1)?;
    let configured = configuration_for(asked.domain, asked.target);
    held(site, &attempt.join("Caddyfile"), configured.as_bytes())?;
    let command = creation(asked.operation, asked.source, &attempt, image)?;
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    let created = session.docker(&words, 30.0);
    let created = utf8(created.map_err(|failure| numbered(&session, failure))?)?;
    let container = stripped(&created);
    require(hexadecimal(container, 64), "handoff_container_identity")?;
    let ownership = object(vec![
        ("containerId", string(container)),
        ("operationId", string(asked.operation)),
        ("source", string(asked.source)),
        ("target", string(asked.target)),
        ("domain", string(asked.domain)),
        ("issuer", string(&issuer)),
        ("lifetimeSeconds", Json::Integer(LIFETIME.to_owned())),
    ]);
    state::atomic_json(&attempt.join("ownership.json"), &InOrder(&ownership))?;
    // The stop of exactly this container is scheduled before it can accept
    // a connection.
    let unit = format!("--unit=simplestchat-dns-handoff-{}-expiry", asked.operation);
    let expiry = [
        "/usr/bin/systemd-run",
        &unit,
        "--on-active=7200",
        "--timer-property=AccuracySec=1s",
        "--property=TimeoutStartSec=45",
        "--collect",
        "/usr/bin/docker",
        "--host",
        "unix:///var/run/docker.sock",
        "stop",
        "--time",
        "10",
        container,
    ];
    let scheduled = session.run(&expiry, 30.0);
    scheduled.map_err(|failure| numbered(&session, failure))?;
    let started = session.docker(&["start", container], 30.0);
    started.map_err(|failure| numbered(&session, failure))?;
    let state = ["inspect", "--format", "{{.State.Running}}", container];
    let running = session.docker(&state, 15.0);
    let running = utf8(running.map_err(|failure| numbered(&session, failure))?)?;
    require(stripped(&running) == "true", "handoff_not_running")?;
    let resolved = format!("{}:443:{}", asked.domain, asked.source);
    let health = format!("https://{}/health", asked.domain);
    let answered = [
        "/usr/bin/curl",
        "--fail",
        "--silent",
        "--show-error",
        "--noproxy",
        "*",
        "--resolve",
        &resolved,
        "--connect-timeout",
        "5",
        "--max-time",
        "10",
        "--retry",
        "3",
        "--retry-delay",
        "1",
        "--retry-connrefused",
        "--retry-max-time",
        "20",
        &health,
    ];
    let reached = session.run(&answered, 35.0);
    reached.map_err(|failure| numbered(&session, failure))?;
    let report = object(vec![
        ("passed", Json::Bool(true)),
        ("containerId", string(container)),
        ("expiresAfterSeconds", Json::Integer(LIFETIME.to_owned())),
    ]);
    Ok(format!("{}\n", python_dump(&report, false)))
}

/// Admit the caller and the request, take the workload lock through `lock`
/// for this move only, and release it whatever happens under it.
fn execute(
    site: &Site<'_>,
    words: [&str; 5],
    privileged: bool,
    probe: &dyn Fn(Ipv4Addr) -> std::io::Result<()>,
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Result<String> {
    let [operation_id, source, target, domain, lifetime] = words;
    require(
        privileged && lifetime == LIFETIME,
        "handoff_identity_or_lifetime",
    )?;
    let address = public(source)?;
    public(target)?;
    require(source != target, "handoff_distinct_addresses_required")?;
    let (directory, hostname) = operation(site, operation_id)?;
    require(hostname == domain, "handoff_domain_mismatch")?;
    let request = request_file(site.host, &directory.join("request.json"))?;
    let takeover = Takeover::Migration {
        operation: &request.operation,
        source: &request.source,
        destination: &request.destination,
        revision: &request.revision,
    };
    let workload = match lock(takeover) {
        Ok(workload) => workload,
        Err(failure) if failure.busy() => return Err(Fault::Kind("BlockingIOError")),
        Err(_) => return Err(Fault::Refused("unfinished_host_work".to_owned())),
    };
    let asked = Asked {
        operation: operation_id,
        source,
        address,
        target,
        domain,
    };
    let outcome = bridge(site, &asked, &request, &directory, probe);
    let released = workload.finish().map_err(|_| Fault::Kind("OSError"));
    let line = outcome?;
    released.map(|()| line)
}

/// Whether this machine holds `address`: a socket can be bound to it.
fn bindable(address: Ipv4Addr) -> std::io::Result<()> {
    use rustix::net::{AddressFamily, SocketAddrV4, SocketType, bind, socket};
    let probe = socket(AddressFamily::INET, SocketType::STREAM, None)?;
    bind(&probe, &SocketAddrV4::new(address, 0))?;
    Ok(())
}

/// `handoff OPERATION SOURCE TARGET DOMAIN 7200`, or nothing for any other
/// command line.
fn parse(arguments: &[OsString]) -> Option<[&str; 5]> {
    let [family, operation, source, target, domain, lifetime] = arguments else {
        return None;
    };
    (family == "handoff").then_some(())?;
    let words = [operation, source, target, domain, lifetime];
    let mut text = [""; 5];
    for (word, text) in words.into_iter().zip(&mut text) {
        *text = word.to_str()?;
    }
    Some(text)
}

/// Run `migrate handoff …` on the sealed source of a move. A syntax error
/// returns 2 before any effect; any failure is one fixed line.
pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    let Some(words) = parse(arguments) else {
        return ExitCode::from(2);
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let clock = System::new();
    let host = Host {
        commands: &Local,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let site = Site {
        host: &host,
        proxy: 10001,
        proxy_group: 10001,
        relay_group: 10002,
        rearm: &|| {},
    };
    let privileged = rustix::process::geteuid().is_root();
    let lock = Workload::acquire_for;
    match execute(&site, words, privileged, &bindable, lock) {
        Ok(line) if emit(std::io::stdout(), &line) => ExitCode::SUCCESS,
        _ => {
            let _ = emit(std::io::stderr(), FAILED);
            ExitCode::FAILURE
        }
    }
}
