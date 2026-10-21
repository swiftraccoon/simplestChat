//! Local UI launch with loopback HTTP and explicit, child-owned build settings.
//!
//! Preparation validates addresses and probes only owned sockets before any
//! installation. It returns the final server command; production replaces the
//! launcher with that command so exit statuses and signals remain unchanged.

#[cfg(test)]
mod tests;

use crate::tooling::{self, Environment, Failure};
use anyhow::Result;
use std::ffi::OsStr;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "\
Usage: cargo run --locked -q -p xtask -- run [--skip-web]
Build the web UI and start the debug server at http://localhost:3000.
  --skip-web  Reuse an existing web/dist build; skip npm install/build.
Environment: ANNOUNCE_IP, PORT, MEDIA_WORKERS, WEBRTC_SERVER_PORT_BASE.
Database/auth settings are inherited, but DATABASE_URL must be local.
HTTP stays on 127.0.0.1. No database is created or migrations enabled.
";

#[derive(Clone, Copy)]
pub(crate) enum Platform {
    Darwin,
    Other,
}

impl Platform {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else {
            Self::Other
        }
    }
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    match prepare(
        root,
        arguments,
        &tooling::inherited_env(),
        Platform::current(),
    ) {
        Ok(None) => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Ok(Some(mut command)) => {
            let error = command.exec();
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => 127,
                std::io::ErrorKind::PermissionDenied => 126,
                _ => 1,
            };
            eprintln!("Cannot execute the pinned Rust launcher: {error}");
            Ok(ExitCode::from(code))
        }
        Err(failure) => {
            eprintln!("{}", failure.message);
            Ok(ExitCode::from(failure.code))
        }
    }
}

/// Prepare a local launch without mutating this process's environment or
/// replacing the caller. Help returns `None` before any filesystem or tool work.
pub(crate) fn prepare(
    root: &Path,
    arguments: &[String],
    inherited: &Environment,
    platform: Platform,
) -> tooling::Result<Option<Command>> {
    let mut skip_web = false;
    for argument in arguments {
        match argument.as_str() {
            "--skip-web" => skip_web = true,
            "--help" | "-h" => return Ok(None),
            _ => return Err(Failure::new(2, format!("Unknown argument.\n{USAGE}"))),
        }
    }
    let mut environment = inherited.clone();
    for dependency in ["rustup", "node"] {
        if executable(&environment, dependency).is_none() {
            return Err(Failure::new(
                2,
                format!("Missing {dependency}; see docs/development.md for prerequisites."),
            ));
        }
    }
    if skip_web && !root.join("web/dist/index.html").is_file() {
        return Err(Failure::new(
            2,
            "--skip-web requires web/dist/index.html; run without --skip-web first.",
        ));
    }
    let channel = tooling::pinned_toolchain(root)?;
    let inputs = tooling::native_env(root, &mut environment, false);
    if !inputs.pip_constraint.is_file() {
        return Err(Failure::new(
            2,
            "Missing build/pip-constraints.txt; restore the pinned native prerequisites.",
        ));
    }
    set_announcement(root, &mut environment, platform)?;
    local_defaults(&mut environment);
    preflight(&environment)?;
    tooling::pin_compilers(root, &mut environment, &channel)?;
    for name in ["RUSTC", "RUSTDOC"] {
        if !environment
            .get(OsStr::new(name))
            .is_some_and(|path| is_executable(Path::new(path)))
        {
            return Err(Failure::new(
                2,
                format!(
                    concat!(
                        "Rust {channel} is unavailable; install it with: ",
                        "rustup toolchain install {channel}"
                    ),
                    channel = channel
                ),
            ));
        }
    }
    if !inputs.openssl_dir.join("lib/libssl.a").is_file()
        || !inputs.openssl_dir.join("lib/libcrypto.a").is_file()
    {
        println!(concat!(
            "Installing the checksum-pinned static OpenSSL build ",
            "(first launch may take several minutes)."
        ));
        tooling::run(
            root,
            &environment,
            root.join("build/install-openssl.sh"),
            [&inputs.openssl_dir],
        )?;
    }
    if !inputs.aws_lc_dir.join("lib/libcrypto-awslc.a").is_file()
        || !inputs
            .aws_lc_dir
            .join("share/rust/aws_lc_bindings.rs")
            .is_file()
    {
        println!(concat!(
            "Installing checksum-pinned static AWS-LC and matching Rust bindings ",
            "(first launch may take several minutes)."
        ));
        tooling::run(
            root,
            &environment,
            root.join("build/install-aws-lc.sh"),
            [&inputs.aws_lc_dir],
        )?;
    }
    tooling::require_native_archives(&inputs)?;
    if !skip_web {
        let prefix = root.join("target/npm-12.2.0");
        tooling::run(
            root,
            &environment,
            "node",
            [OsStr::new("build/install-npm.mjs"), prefix.as_os_str()],
        )?;
        prepend_path(&mut environment, prefix.join("bin"))?;
        tooling::run(
            root,
            &environment,
            "npm",
            ["--prefix", "web", "ci", "--ignore-scripts"],
        )?;
        tooling::run(
            root,
            &environment,
            "npm",
            ["--prefix", "web", "run", "build"],
        )?;
    }
    let port = environment[OsStr::new("PORT")].to_string_lossy();
    let address = environment[OsStr::new("ANNOUNCE_IP")].to_string_lossy();
    let workers = environment[OsStr::new("MEDIA_WORKERS")].to_string_lossy();
    println!(
        "Starting http://localhost:{port} (HTTP loopback only; media advertised as {address})."
    );
    println!("Using Rust {channel} and {workers} media worker(s). Press Ctrl-C to stop.");
    Ok(Some(tooling::command(
        root,
        &environment,
        "rustup",
        [
            "run",
            &channel,
            "cargo",
            "run",
            "--locked",
            "--bin",
            "simplestChat",
        ],
    )))
}

fn local_defaults(environment: &mut Environment) {
    environment.insert("BIND_ADDR".into(), "127.0.0.1".into());
    for (name, value) in [
        ("PORT", "3000"),
        ("MEDIA_WORKERS", "1"),
        ("WEBRTC_SERVER_PORT_BASE", "40000"),
        ("ALLOW_AD_HOC_ROOMS", "true"),
    ] {
        if environment
            .get(OsStr::new(name))
            .is_none_or(|value| value.is_empty())
        {
            environment.insert(name.into(), value.into());
        }
    }
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn executable(environment: &Environment, name: &str) -> Option<PathBuf> {
    std::env::split_paths(environment.get(OsStr::new("PATH"))?)
        .map(|directory| directory.join(name))
        .find(|path| is_executable(path))
}

fn prepend_path(environment: &mut Environment, prefix: PathBuf) -> tooling::Result<()> {
    let mut paths = vec![prefix];
    if let Some(path) = environment.get(OsStr::new("PATH")) {
        paths.extend(std::env::split_paths(path));
    }
    let path = std::env::join_paths(paths)
        .map_err(|_| Failure::new(2, "Cannot prepare the pinned npm PATH."))?;
    environment.insert("PATH".into(), path);
    Ok(())
}

fn quiet_output(
    root: &Path,
    environment: &Environment,
    program: &str,
    args: &[&str],
) -> Option<String> {
    let output = tooling::command(root, environment, program, args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn set_announcement(
    root: &Path,
    environment: &mut Environment,
    platform: Platform,
) -> tooling::Result<()> {
    if environment
        .get(OsStr::new("ANNOUNCE_IP"))
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(());
    }
    if matches!(platform, Platform::Darwin) {
        let route = quiet_output(root, environment, "route", &["-n", "get", "default"]);
        let interface = route.as_deref().and_then(|route| {
            route.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("interface:")?
                    .split_whitespace()
                    .next()
            })
        });
        let address = interface
            .and_then(|interface| {
                quiet_output(root, environment, "ipconfig", &["getifaddr", interface])
            })
            .filter(|address| !address.trim().is_empty())
            .or_else(|| quiet_output(root, environment, "ipconfig", &["getifaddr", "en0"]))
            .filter(|address| !address.trim().is_empty());
        if let Some(address) = address {
            environment.insert("ANNOUNCE_IP".into(), address.trim().into());
            return Ok(());
        }
    }
    Err(Failure::new(
        2,
        "Cannot detect a LAN address. Set ANNOUNCE_IP to your active interface address.",
    ))
}

fn integer(environment: &Environment, name: &str, maximum: u32) -> tooling::Result<u32> {
    let value = environment
        .get(OsStr::new(name))
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if value.starts_with('0') || !value.bytes().all(|value| value.is_ascii_digit()) {
        return Err(Failure::new(
            2,
            format!("{name} must be an integer from 1 through {maximum}."),
        ));
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=maximum).contains(value))
        .ok_or_else(|| {
            Failure::new(
                2,
                format!("{name} must be an integer from 1 through {maximum}."),
            )
        })
}

/// Refuse nonlocal configuration, then bind and immediately release only our
/// own HTTP/media probes. Never connect to the database or stop another owner.
fn preflight(environment: &Environment) -> tooling::Result<()> {
    let port = integer(environment, "PORT", 65535)?;
    let workers = integer(environment, "MEDIA_WORKERS", 64)?;
    let media = integer(environment, "WEBRTC_SERVER_PORT_BASE", 65535)?;
    if media + workers - 1 > 65535 {
        return Err(Failure::new(
            2,
            "The media worker port range exceeds 65535.",
        ));
    }
    if environment
        .get(OsStr::new("ANNOUNCE_IP"))
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<Ipv4Addr>().ok())
        .is_none()
    {
        return Err(Failure::new(
            2,
            "ANNOUNCE_IP must be a literal IPv4 address for the IPv4 media sockets.",
        ));
    }
    if let Some(database) = environment.get(OsStr::new("DATABASE_URL")) {
        if database.is_empty() {
            return Err(Failure::new(
                2,
                "DATABASE_URL is empty; unset it for guest-only mode.",
            ));
        }
        let database = database
            .to_str()
            .and_then(|value| url::Url::parse(value).ok())
            .ok_or_else(|| Failure::new(2, "Invalid local DATABASE_URL."))?;
        if !matches!(database.scheme(), "postgres" | "postgresql")
            || !matches!(
                database.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            )
            || database.fragment().is_some()
            || database.path().len() < 2
            || database.query_pairs().any(|(key, _)| key != "sslmode")
        {
            return Err(Failure::new(
                2,
                concat!(
                    "DATABASE_URL must name a local PostgreSQL database; ",
                    "only the sslmode query option is accepted."
                ),
            ));
        }
    }
    let port = u16::try_from(port).expect("validated HTTP port");
    let media = u16::try_from(media).expect("validated media port");
    drop(
        TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
            .map_err(|error| socket_failure("HTTP loopback", port, error))?,
    );
    for index in 0..workers {
        let port = media + u16::try_from(index).expect("validated worker count");
        drop(
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
                .map_err(|error| socket_failure("media UDP", port, error))?,
        );
    }
    Ok(())
}

fn socket_failure(kind: &str, port: u16, error: std::io::Error) -> Failure {
    let detail = if error.kind() == std::io::ErrorKind::AddrInUse {
        "EADDRINUSE"
    } else {
        "bind failed"
    };
    Failure::new(
        2,
        format!("Cannot launch locally: {kind} port {port}: {detail}."),
    )
}
