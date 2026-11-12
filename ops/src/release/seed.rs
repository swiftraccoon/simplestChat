//! `simplestchat-ops release seed-lobby`: keep the public lobby in place.
//! The owner's address and password come from the deployment's private
//! configuration and are retained once, unchangeably, in `owner.json`; the
//! seeding transaction then signs that owner in through the private
//! backend and creates only what is missing. One line reports what
//! happened, and it never carries a credential.

#[cfg(test)]
#[path = "seed_tests.rs"]
mod tests;

use crate::host::emit;
use rustix::fs::{
    AtFlags, FlockOperation, Mode, OFlags, flock, fsync, linkat, open, openat, unlinkat,
};
use rustix::io::Errno;
use serde::Serialize;
use simplestchat_lobby_seed::{FAILED, Json, Limits, Owner, Report, parse, seed};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

const CONFIG: &str = "/etc/simplestchat-public";
/// The backend's private listener.
const BACKEND: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 3000);
/// The most bytes a private input may hold.
const MAX_PRIVATE: u64 = 64 * 1024;
/// How long everything but ending the session may take.
const WHOLE: Duration = Duration::from_secs(55);
const ORIGIN: &str = "invalid_origin_configuration";
const SECRET: &str = "invalid_owner_secret";
const UNSAFE: &str = "unsafe_private_file";

type Outcome<T> = Result<T, &'static str>;

/// The lines of an environment file, ended wherever the earlier helper's
/// language ends a line: not only at a line feed.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split([
        '\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
    ])
}

/// Whether `label` is one name of a host: letters and digits, with hyphens
/// only inside, at most 63 characters.
fn label(label: &str) -> bool {
    let plain = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = label.as_bytes();
    let ends = bytes.first().is_some_and(|byte| plain(*byte))
        && bytes.last().is_some_and(|byte| plain(*byte));
    ends && bytes.len() <= 63 && bytes.iter().all(|byte| plain(*byte) || *byte == b'-')
}

/// The one literal HTTPS origin of an environment file, without a trailing
/// slash, and the owner it implies with the configured password. Nothing is
/// executed or exported; any other shape is refused.
fn configuration(environment: &str, secrets: &Json) -> Outcome<(String, Owner)> {
    let named = lines(environment).filter_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == "ALLOWED_ORIGINS").then_some(value)
    });
    let candidates: Vec<&str> = named.collect();
    let [origin] = candidates[..] else {
        return Err(ORIGIN);
    };
    let bare = origin.strip_suffix('/').unwrap_or(origin);
    let authority = bare.strip_prefix("https://").ok_or(ORIGIN)?;
    let (domain, port) = match authority.split_once(':') {
        Some((domain, port)) => (domain, Some(port)),
        None => (authority, None),
    };
    let numbered = port.is_none_or(|port| {
        let digits = port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit());
        digits
            && port
                .parse::<u32>()
                .is_ok_and(|port| (1..=65535).contains(&port))
    });
    let named = domain.len() <= 249 && domain.contains('.') && domain.split('.').all(label);
    if !numbered || !named {
        return Err(ORIGIN);
    }
    let password = secrets.get("owner_password").and_then(Json::text);
    let password = password.ok_or(SECRET)?;
    let printable = password.bytes().all(|byte| (33..=126).contains(&byte));
    if !(32..=128).contains(&password.len()) || !printable {
        return Err(SECRET);
    }
    let owner = Owner {
        email: format!("owner@{domain}"),
        password: password.to_owned(),
    };
    Ok((bare.to_owned(), owner))
}

/// Require a bounded regular file of this owner that no one else can read
/// and that has no second name.
fn private(file: &File, owner: u32) -> Outcome<()> {
    let information = file.metadata().map_err(|_| FAILED)?;
    let safe = information.is_file()
        && information.uid() == owner
        && information.mode() & 0o7777 == 0o600
        && information.nlink() == 1
        && information.len() <= MAX_PRIVATE;
    match safe {
        true => Ok(()),
        false => Err(UNSAFE),
    }
}

/// The text of one private file of the configuration directory, or nothing
/// when there is no such file. A link is never followed.
fn read(directory: BorrowedFd<'_>, name: &str, owner: u32) -> Outcome<Option<String>> {
    // Opening never waits, whatever kind of file bears the name.
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let mut file = match openat(directory, name, flags, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(FAILED),
    };
    private(&file, owner)?;
    let mut raw = Vec::new();
    let mut bounded = (&mut file).take(MAX_PRIVATE + 1);
    bounded.read_to_end(&mut raw).map_err(|_| FAILED)?;
    if u64::try_from(raw.len()).is_ok_and(|length| length > MAX_PRIVATE) {
        return Err("private_file_too_large");
    }
    String::from_utf8(raw).map(Some).map_err(|_| FAILED)
}

/// Whether two texts are equal, without telling where they differ.
fn alike(one: &str, other: &str) -> bool {
    let (one, other) = (one.as_bytes(), other.as_bytes());
    let mut difference = u8::from(one.len() != other.len());
    for (index, byte) in one.iter().enumerate() {
        difference |= byte ^ other.get(index).copied().unwrap_or_default();
    }
    difference == 0
}

/// Whether a retained record is exactly this owner and nothing more.
fn same_owner(retained: &Json, owner: &Owner) -> bool {
    let Json::Object(members) = retained else {
        return false;
    };
    let password = retained.get("password").and_then(Json::text);
    members.len() == 2
        && retained.get("email").and_then(Json::text) == Some(&owner.email)
        && password.is_some_and(|password| alike(password, &owner.password))
}

/// Retain the owner's credentials once. An existing record is never
/// rewritten: it must be this owner's. A new one appears complete or not at
/// all, under its final name only after its bytes are durable.
fn retain(directory: BorrowedFd<'_>, uid: u32, owner: &Owner) -> Outcome<bool> {
    if let Some(retained) = read(directory, "owner.json", uid)? {
        return match same_owner(&parse(retained.as_bytes())?, owner) {
            true => Ok(false),
            false => Err("owner_credentials_conflict"),
        };
    }
    let mut random = [0_u8; 12];
    getrandom::fill(&mut random).map_err(|_| FAILED)?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let pending = format!("owner.pending.{suffix}");
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let created = openat(directory, &pending, flags, Mode::from_raw_mode(0o600));
    let mut file = File::from(created.map_err(|_| FAILED)?);
    let email = serde_json::to_string(&owner.email).map_err(|_| FAILED)?;
    let password = serde_json::to_string(&owner.password).map_err(|_| FAILED)?;
    let record = format!("{{\"email\":{email},\"password\":{password}}}\n");
    let written = file
        .write_all(record.as_bytes())
        .and_then(|()| file.sync_all());
    let published = written.is_ok()
        && linkat(
            directory,
            &pending,
            directory,
            "owner.json",
            AtFlags::empty(),
        )
        .is_ok();
    let removed = unlinkat(directory, &pending, AtFlags::empty()).is_ok();
    let durable = fsync(directory).is_ok();
    match published && removed && durable {
        true => Ok(true),
        false => Err(FAILED),
    }
}

/// Admit the caller and the configuration directory, hold the seeding lock,
/// retain the owner and run the transaction. Its report comes back with
/// whether the owner's record was created.
fn perform(
    config: &Path,
    backend: SocketAddr,
    uid: u32,
    privileged: bool,
    limits: &Limits,
) -> Outcome<(Report, bool)> {
    if !privileged {
        return Err("root_required");
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let directory = File::from(open(config, flags, Mode::empty()).map_err(|_| FAILED)?);
    let information = directory.metadata().map_err(|_| FAILED)?;
    if information.uid() != uid || information.mode() & 0o022 != 0 {
        return Err("unsafe_configuration_directory");
    }
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let lock = openat(&directory, "seed.lock", flags, Mode::from_raw_mode(0o600));
    let lock = File::from(lock.map_err(|_| FAILED)?);
    private(&lock, uid)?;
    match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(Errno::WOULDBLOCK) => return Err("seed_already_running"),
        Err(_) => return Err(FAILED),
    }
    let directory = directory.as_fd();
    let environment = read(directory, "app.env", uid)?.ok_or(FAILED)?;
    let secrets = read(directory, "secrets.json", uid)?.ok_or(FAILED)?;
    let (origin, owner) = configuration(&environment, &parse(secrets.as_bytes())?)?;
    let created = retain(directory, uid, &owner)?;
    Ok((seed(backend, &origin, &owner, limits), created))
}

/// The fixed, credential-free result of one run.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Line<'a> {
    schema_version: u8,
    passed: bool,
    changed: bool,
    owner_created: bool,
    room_created: bool,
    owner_file_created: bool,
    existing_settings_preserved: bool,
    refresh_session_revoked: Option<bool>,
    failure: Option<&'a str>,
    owner_credential_file: String,
}

/// Run one seeding and return its report line and whether it passed.
fn execute(
    config: &Path,
    backend: SocketAddr,
    uid: u32,
    privileged: bool,
    limits: &Limits,
) -> (String, bool) {
    // A run refused before the transaction did nothing but fail.
    let refused = |code| {
        let report = Report {
            failure: Some(code),
            ..Report::default()
        };
        (report, false)
    };
    let performed = perform(config, backend, uid, privileged, limits);
    let (report, created) = performed.unwrap_or_else(refused);
    let line = Line {
        schema_version: 1,
        passed: report.passed(),
        changed: report.owner_created || report.room_created || created,
        owner_created: report.owner_created,
        room_created: report.room_created,
        owner_file_created: created,
        existing_settings_preserved: report.existing_settings_preserved,
        refresh_session_revoked: report.refresh_session_revoked,
        failure: report.failure,
        owner_credential_file: format!("{}/owner.json", config.display()),
    };
    let text = serde_json::to_string(&line).unwrap_or_default();
    (format!("{text}\n"), line.passed)
}

/// Run `release seed-lobby`. It takes no argument; anything else is a
/// syntax error, returned as 2 before any effect. The report line is
/// printed whatever happened, and only a passed run returns success.
pub fn run(arguments: &[OsString]) -> ExitCode {
    if !arguments.is_empty() {
        return ExitCode::from(2);
    }
    rustix::process::umask(Mode::from_raw_mode(0o077));
    let privileged = rustix::process::geteuid().is_root();
    let limits = Limits::within(WHOLE);
    let (line, passed) = execute(Path::new(CONFIG), BACKEND, 0, privileged, &limits);
    match emit(std::io::stdout(), &line) && passed {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    }
}
