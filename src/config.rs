//! The server's complete runtime configuration.
//!
//! Every operator setting is read here, once, from a [`Source`] (the process
//! environment in production, a slice of pairs in tests) and validated before
//! any listener, native worker or database connection exists. Modules receive
//! typed sections and never consult the environment themselves; a test pins
//! that. [`Config::settings`] renders the effective non-secret values by
//! variable name for `--check-config`, and a test keeps `docs/configuration.md`
//! and `docker-compose.yml` in step with the defaults declared here.
//!
//! One grammar covers every setting:
//!
//! - an unset variable selects the documented default;
//! - an integer is a whole number within its documented bounds; an explicit
//!   empty, malformed or out-of-range value is an error, so a typo can never
//!   silently select a larger limit;
//! - an optional setting (a ceiling, address, secret, URL or path) treats an
//!   empty value as unset, because Compose forwards an unset variable as `""`;
//! - a flag is `true`, `false`, `1` or `0`, case-insensitively; empty selects
//!   the default;
//! - a secret holds at least [`MIN_SECRET_BYTES`] bytes and is never echoed by
//!   an error, a log line or the settings summary.

use crate::media::MediaConfig;
use crate::room::moderation::RetentionConfig;
use crate::saturation::SaturationConfig;
use crate::secret::SecretString;
use crate::turn::TurnConfig;
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Display;
use std::net::{IpAddr, Ipv6Addr};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

/// Shortest secret the server accepts for `JWT_SECRET`, `METRICS_TOKEN`,
/// `TRUSTED_PROXY_SECRET` and `TURN_SECRET`.
pub const MIN_SECRET_BYTES: usize = 32;

/// Where settings come from.
pub trait Source {
    /// The raw value of `name`, `None` when unset. A value that is not UTF-8
    /// is an error naming the setting.
    fn get(&self, name: &str) -> Result<Option<String>>;
}

/// The process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct Environment;

impl Source for Environment {
    fn get(&self, name: &str) -> Result<Option<String>> {
        match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => bail!("{name} must be valid UTF-8"),
        }
    }
}

fn lookup(pairs: &[(&str, &str)], name: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| (*value).to_owned())
}

/// Fixed pairs, for tests and tools.
impl Source for Vec<(&str, &str)> {
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(lookup(self, name))
    }
}

impl<const N: usize> Source for [(&str, &str); N] {
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(lookup(self, name))
    }
}

/// A trimmed optional text; empty means unset.
fn text(source: &dyn Source, name: &str) -> Result<Option<String>> {
    Ok(source
        .get(name)?
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty()))
}

fn parse_bounded<T>(name: &str, value: &str, bounds: &RangeInclusive<T>) -> Result<T>
where
    T: FromStr + PartialOrd + Display,
{
    match value.trim().parse::<T>() {
        Ok(parsed) if bounds.contains(&parsed) => Ok(parsed),
        _ => bail!(
            "{name} must be a whole number from {} to {}",
            bounds.start(),
            bounds.end()
        ),
    }
}

/// A required integer: unset selects `default`, anything else must parse
/// within `bounds`.
fn bounded<T>(source: &dyn Source, name: &str, default: T, bounds: RangeInclusive<T>) -> Result<T>
where
    T: FromStr + PartialOrd + Display,
{
    match source.get(name)? {
        None => Ok(default),
        Some(value) => parse_bounded(name, &value, &bounds),
    }
}

/// An optional integer: unset or empty is `None`.
fn optional_bounded<T>(
    source: &dyn Source,
    name: &str,
    bounds: RangeInclusive<T>,
) -> Result<Option<T>>
where
    T: FromStr + PartialOrd + Display,
{
    text(source, name)?
        .map(|value| parse_bounded(name, &value, &bounds))
        .transpose()
}

fn flag(source: &dyn Source, name: &str, default: bool) -> Result<bool> {
    match text(source, name)? {
        None => Ok(default),
        Some(value) if value.eq_ignore_ascii_case("true") || value == "1" => Ok(true),
        Some(value) if value.eq_ignore_ascii_case("false") || value == "0" => Ok(false),
        Some(_) => bail!("{name} must be true, false, 1 or 0"),
    }
}

/// An optional secret of at least [`MIN_SECRET_BYTES`]; empty means unset.
/// The value is taken verbatim: secrets are not trimmed.
fn secret(source: &dyn Source, name: &str) -> Result<Option<SecretString>> {
    match source.get(name)? {
        None => Ok(None),
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => {
            ensure!(
                value.len() >= MIN_SECRET_BYTES,
                "{name} must contain at least {MIN_SECRET_BYTES} bytes"
            );
            Ok(Some(SecretString::from(value)))
        }
    }
}

fn address<T: FromStr>(source: &dyn Source, name: &str, kind: &str) -> Result<Option<T>> {
    text(source, name)?
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|_| anyhow::anyhow!("{name} must be {kind}"))
        })
        .transpose()
}

fn secret_state(value: Option<&SecretString>) -> Value {
    value.map_or(Value::Null, |_| json!("configured"))
}

/// Listener and announced addresses, origins and proxy trust.
#[derive(Debug, Clone)]
pub struct Network {
    /// `BIND_ADDR`: the HTTP/WebSocket listener address.
    pub bind_addr: IpAddr,
    /// `PORT`: the HTTP/WebSocket listener port.
    pub port: u16,
    /// `ALLOWED_ORIGINS`: exact origins accepted on the WebSocket handshake;
    /// empty means the request's own host, which only a loopback bind allows.
    pub allowed_origins: Vec<String>,
    /// `ANNOUNCE_IP`: the address clients reach, IPv4 or IPv6.
    pub announce_ip: IpAddr,
    /// `ANNOUNCE_IPV6`: on a dual-stack host, the IPv6 address announced
    /// beside an IPv4 `ANNOUNCE_IP`.
    pub announce_ipv6: Option<Ipv6Addr>,
    /// `TRUSTED_PROXY_SECRET`: shared proxy-authentication secret behind which
    /// forwarded client addresses are believed.
    pub trusted_proxy_secret: Option<SecretString>,
}

impl Network {
    fn read(source: &dyn Source) -> Result<Self> {
        let bind_addr = address::<IpAddr>(source, "BIND_ADDR", "an IP address")?
            .unwrap_or(IpAddr::from([127, 0, 0, 1]));
        let allowed_origins = text(source, "ALLOWED_ORIGINS")?
            .as_deref()
            .map(parse_allowed_origins)
            .transpose()?
            .unwrap_or_default();
        ensure!(
            bind_addr.is_loopback() || !allowed_origins.is_empty(),
            "ALLOWED_ORIGINS is required when BIND_ADDR is not loopback"
        );
        let announce_ip = address::<IpAddr>(source, "ANNOUNCE_IP", "an IP address")?
            .unwrap_or(IpAddr::from([127, 0, 0, 1]));
        let announce_ipv6 = address::<Ipv6Addr>(source, "ANNOUNCE_IPV6", "an IPv6 address")?;
        ensure!(
            announce_ipv6.is_none() || announce_ip.is_ipv4(),
            "ANNOUNCE_IPV6 goes beside an IPv4 ANNOUNCE_IP; an IPv6 ANNOUNCE_IP already announces that family"
        );
        Ok(Self {
            bind_addr,
            port: bounded(source, "PORT", 3000, 1..=65_535)?,
            allowed_origins,
            announce_ip,
            announce_ipv6,
            trusted_proxy_secret: secret(source, "TRUSTED_PROXY_SECRET")?,
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert("BIND_ADDR", json!(self.bind_addr.to_string()));
        out.insert("PORT", json!(self.port));
        out.insert("ALLOWED_ORIGINS", json!(self.allowed_origins));
        out.insert("ANNOUNCE_IP", json!(self.announce_ip.to_string()));
        out.insert(
            "ANNOUNCE_IPV6",
            self.announce_ipv6
                .map_or(Value::Null, |address| json!(address.to_string())),
        );
        out.insert(
            "TRUSTED_PROXY_SECRET",
            secret_state(self.trusted_proxy_secret.as_ref()),
        );
    }
}

/// `ALLOWED_ORIGINS`: comma-separated exact `http(s)` origins without
/// credentials, paths, queries or fragments, serialized in ASCII form.
pub fn parse_allowed_origins(value: &str) -> Result<Vec<String>> {
    value
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            let parsed = url::Url::parse(origin)
                .map_err(|_| anyhow::anyhow!("ALLOWED_ORIGINS contains an invalid origin"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || parsed.path() != "/"
            {
                bail!("ALLOWED_ORIGINS entries must be exact http(s) origins without paths");
            }
            Ok(parsed.origin().ascii_serialization())
        })
        .collect()
}

/// Capacity ceilings of one server process.
#[derive(Debug, Clone)]
pub struct Limits {
    /// `MAX_CONNECTIONS`: concurrent WebSocket permits, retained reconnect
    /// sessions and account update subscribers, each bounded separately.
    pub max_connections: usize,
    /// `MAX_CONNECTIONS_PER_IP`: concurrent WebSocket connections per client address.
    pub max_connections_per_ip: usize,
    /// `MAX_ROOMS`: rooms held live by this process.
    pub max_rooms: usize,
    /// `MAX_PERSISTED_ROOMS`: database-backed room cap, enforced transactionally.
    pub max_persisted_rooms: i64,
    /// `MAX_USERS`: database-backed account cap, enforced transactionally.
    pub max_users: i64,
    /// `MAX_PARTICIPANTS_PER_ROOM`: server-wide participant ceiling for every room.
    pub max_participants_per_room: Option<usize>,
    /// `MAX_BROADCASTERS_PER_ROOM`: server-wide ceiling on participants publishing media.
    pub max_broadcasters_per_room: Option<usize>,
    /// `MAX_PASSWORD_WORKERS`: concurrent Argon2 jobs for both password lanes;
    /// unset keeps each lane's own derived default.
    pub max_password_workers: Option<usize>,
}

impl Limits {
    fn read(source: &dyn Source) -> Result<Self> {
        Ok(Self {
            max_connections: bounded(source, "MAX_CONNECTIONS", 10_000, 1..=1_000_000)?,
            max_connections_per_ip: bounded(source, "MAX_CONNECTIONS_PER_IP", 50, 1..=1_000_000)?,
            max_rooms: bounded(source, "MAX_ROOMS", 1_000, 1..=1_000_000)?,
            max_persisted_rooms: bounded(source, "MAX_PERSISTED_ROOMS", 10_000, 1..=1_000_000)?,
            max_users: bounded(source, "MAX_USERS", 100_000, 1..=10_000_000)?,
            max_participants_per_room: optional_bounded(
                source,
                "MAX_PARTICIPANTS_PER_ROOM",
                2..=10_000,
            )?,
            max_broadcasters_per_room: optional_bounded(
                source,
                "MAX_BROADCASTERS_PER_ROOM",
                1..=1_000,
            )?,
            max_password_workers: optional_bounded(source, "MAX_PASSWORD_WORKERS", 1..=32)?,
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert("MAX_CONNECTIONS", json!(self.max_connections));
        out.insert("MAX_CONNECTIONS_PER_IP", json!(self.max_connections_per_ip));
        out.insert("MAX_ROOMS", json!(self.max_rooms));
        out.insert("MAX_PERSISTED_ROOMS", json!(self.max_persisted_rooms));
        out.insert("MAX_USERS", json!(self.max_users));
        out.insert(
            "MAX_PARTICIPANTS_PER_ROOM",
            json!(self.max_participants_per_room),
        );
        out.insert(
            "MAX_BROADCASTERS_PER_ROOM",
            json!(self.max_broadcasters_per_room),
        );
        out.insert("MAX_PASSWORD_WORKERS", json!(self.max_password_workers));
    }
}

/// Request rates and concurrency caps.
#[derive(Debug, Clone)]
pub struct Rates {
    /// `WS_HANDSHAKES_PER_MINUTE`: WebSocket upgrades per client address cohort.
    pub ws_handshakes_per_minute: u32,
    /// `AUTH_REQUESTS_PER_MINUTE`: authentication requests per client address.
    pub auth_requests_per_minute: u32,
    /// `AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE`: signed-in password checks per account.
    pub auth_requests_per_account_per_minute: u32,
    /// `REGISTRATIONS_PER_IP_PER_HOUR`: registrations, and taken-email answers,
    /// one client address may receive per hour. Without email verification the
    /// register endpoint says whether an address has an account, so the answer
    /// is what is metered.
    pub registrations_per_ip_per_hour: u32,
    /// `ACCOUNT_REQUESTS_PER_MINUTE`: authenticated application data requests per address.
    pub account_requests_per_minute: u32,
    /// `PROFILE_REQUESTS_PER_MINUTE`: public profile reads (avatars for every
    /// participant a viewer sees) get their own address budget so a busy room
    /// cannot spend the sign-in and refresh one.
    pub profile_requests_per_minute: u32,
    /// `ROOM_API_REQUESTS_PER_MINUTE`: room-management HTTP requests per address.
    pub room_api_requests_per_minute: u32,
    /// `ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE`: persisted-room creations per account.
    pub room_creations_per_account_per_minute: u32,
    /// `JOIN_ATTEMPTS_PER_IP_PER_MINUTE`: room joins one address may start across rooms.
    pub join_attempts_per_ip_per_minute: u32,
    /// `JOIN_ATTEMPTS_PER_ROOM_IP_PER_MINUTE`: room joins one address may start into one room.
    pub join_attempts_per_room_ip_per_minute: u32,
    /// `AUTH_MAX_CONCURRENCY`: concurrent authentication operations.
    pub auth_concurrency: usize,
    /// `ACCOUNT_MAX_CONCURRENCY`: concurrent authenticated application data operations.
    pub account_concurrency: usize,
    /// `ROOM_API_MAX_CONCURRENCY`: concurrent room-management HTTP requests.
    pub room_api_concurrency: usize,
}

impl Rates {
    fn read(source: &dyn Source) -> Result<Self> {
        let rate = |name, default| bounded::<u32>(source, name, default, 1..=1_000_000);
        let concurrency = |name, default| bounded::<usize>(source, name, default, 1..=1_000_000);
        Ok(Self {
            ws_handshakes_per_minute: rate("WS_HANDSHAKES_PER_MINUTE", 120)?,
            auth_requests_per_minute: rate("AUTH_REQUESTS_PER_MINUTE", 60)?,
            auth_requests_per_account_per_minute: rate("AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE", 20)?,
            registrations_per_ip_per_hour: rate("REGISTRATIONS_PER_IP_PER_HOUR", 5)?,
            account_requests_per_minute: rate("ACCOUNT_REQUESTS_PER_MINUTE", 300)?,
            profile_requests_per_minute: rate("PROFILE_REQUESTS_PER_MINUTE", 600)?,
            room_api_requests_per_minute: rate("ROOM_API_REQUESTS_PER_MINUTE", 120)?,
            room_creations_per_account_per_minute: rate(
                "ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE",
                10,
            )?,
            join_attempts_per_ip_per_minute: bounded(
                source,
                "JOIN_ATTEMPTS_PER_IP_PER_MINUTE",
                30,
                1..=100_000,
            )?,
            join_attempts_per_room_ip_per_minute: bounded(
                source,
                "JOIN_ATTEMPTS_PER_ROOM_IP_PER_MINUTE",
                10,
                1..=100_000,
            )?,
            auth_concurrency: concurrency("AUTH_MAX_CONCURRENCY", 16)?,
            account_concurrency: concurrency("ACCOUNT_MAX_CONCURRENCY", 32)?,
            room_api_concurrency: concurrency("ROOM_API_MAX_CONCURRENCY", 32)?,
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert(
            "WS_HANDSHAKES_PER_MINUTE",
            json!(self.ws_handshakes_per_minute),
        );
        out.insert(
            "AUTH_REQUESTS_PER_MINUTE",
            json!(self.auth_requests_per_minute),
        );
        out.insert(
            "AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE",
            json!(self.auth_requests_per_account_per_minute),
        );
        out.insert(
            "REGISTRATIONS_PER_IP_PER_HOUR",
            json!(self.registrations_per_ip_per_hour),
        );
        out.insert(
            "ACCOUNT_REQUESTS_PER_MINUTE",
            json!(self.account_requests_per_minute),
        );
        out.insert(
            "PROFILE_REQUESTS_PER_MINUTE",
            json!(self.profile_requests_per_minute),
        );
        out.insert(
            "ROOM_API_REQUESTS_PER_MINUTE",
            json!(self.room_api_requests_per_minute),
        );
        out.insert(
            "ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE",
            json!(self.room_creations_per_account_per_minute),
        );
        out.insert(
            "JOIN_ATTEMPTS_PER_IP_PER_MINUTE",
            json!(self.join_attempts_per_ip_per_minute),
        );
        out.insert(
            "JOIN_ATTEMPTS_PER_ROOM_IP_PER_MINUTE",
            json!(self.join_attempts_per_room_ip_per_minute),
        );
        out.insert("AUTH_MAX_CONCURRENCY", json!(self.auth_concurrency));
        out.insert("ACCOUNT_MAX_CONCURRENCY", json!(self.account_concurrency));
        out.insert("ROOM_API_MAX_CONCURRENCY", json!(self.room_api_concurrency));
    }
}

/// Passkey relying party settings; both variables configure it together.
#[derive(Debug, Clone)]
pub struct WebAuthn {
    /// `WEBAUTHN_RP_ID`: the relying-party domain.
    pub rp_id: String,
    /// `WEBAUTHN_ORIGIN`: the exact HTTPS origin of passkey ceremonies
    /// (HTTP loopback is allowed for development).
    pub origin: url::Url,
}

impl WebAuthn {
    fn read(source: &dyn Source) -> Result<Option<Self>> {
        let rp_id = text(source, "WEBAUTHN_RP_ID")?;
        let origin = text(source, "WEBAUTHN_ORIGIN")?;
        let (rp_id, origin) = match (rp_id, origin) {
            (None, None) => return Ok(None),
            (Some(rp_id), Some(origin)) => (rp_id, origin),
            _ => bail!("WEBAUTHN_RP_ID and WEBAUTHN_ORIGIN must be configured together"),
        };
        let origin = url::Url::parse(&origin)
            .map_err(|_| anyhow::anyhow!("WEBAUTHN_ORIGIN must be a valid URL"))?;
        let secure_origin = origin.scheme() == "https"
            || (origin.scheme() == "http"
                && origin.host_str().is_some_and(|host| {
                    host.eq_ignore_ascii_case("localhost")
                        || host
                            .parse::<IpAddr>()
                            .is_ok_and(|address| address.is_loopback())
                }));
        ensure!(
            secure_origin
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "WEBAUTHN_ORIGIN must be an HTTPS origin without a path (HTTP localhost is allowed for development)"
        );
        Ok(Some(Self { rp_id, origin }))
    }
}

/// Account authentication.
#[derive(Debug, Clone)]
pub struct Auth {
    /// `JWT_SECRET`: HS256 secret; without it the auth endpoints answer 503.
    pub jwt_secret: Option<SecretString>,
    /// `REGISTRATION_ENABLED`: whether new password and passkey accounts may be created.
    pub registration_enabled: bool,
    /// Passkeys, when both `WEBAUTHN_*` variables are set.
    pub webauthn: Option<WebAuthn>,
}

impl Auth {
    fn read(source: &dyn Source) -> Result<Self> {
        Ok(Self {
            jwt_secret: secret(source, "JWT_SECRET")?,
            registration_enabled: flag(source, "REGISTRATION_ENABLED", false)?,
            webauthn: WebAuthn::read(source)?,
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert("JWT_SECRET", secret_state(self.jwt_secret.as_ref()));
        out.insert("REGISTRATION_ENABLED", json!(self.registration_enabled));
        out.insert(
            "WEBAUTHN_RP_ID",
            json!(self.webauthn.as_ref().map(|webauthn| &webauthn.rp_id)),
        );
        out.insert(
            "WEBAUTHN_ORIGIN",
            json!(
                self.webauthn
                    .as_ref()
                    .map(|webauthn| webauthn.origin.as_str())
            ),
        );
    }
}

/// Room behaviour that is not a capacity ceiling.
#[derive(Debug, Clone)]
pub struct Rooms {
    /// `ALLOW_AD_HOC_ROOMS`: whether a join may create an ephemeral room.
    pub allow_ad_hoc: bool,
    /// `MODERATION_ADDRESS_RETENTION_DAYS` and `MODERATION_HISTORY_RETENTION_DAYS`.
    pub retention: RetentionConfig,
}

impl Rooms {
    fn read(source: &dyn Source) -> Result<Self> {
        Ok(Self {
            allow_ad_hoc: flag(source, "ALLOW_AD_HOC_ROOMS", false)?,
            retention: RetentionConfig::parse(
                text(source, "MODERATION_ADDRESS_RETENTION_DAYS")?.as_deref(),
                text(source, "MODERATION_HISTORY_RETENTION_DAYS")?.as_deref(),
            )?,
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert("ALLOW_AD_HOC_ROOMS", json!(self.allow_ad_hoc));
        out.insert(
            "MODERATION_ADDRESS_RETENTION_DAYS",
            json!(self.retention.address_days),
        );
        out.insert(
            "MODERATION_HISTORY_RETENTION_DAYS",
            json!(self.retention.history_days),
        );
    }
}

/// PostgreSQL connection and pool settings. The pool bounds are validated
/// even without a URL so a misconfigured deployment fails on its first start.
#[derive(Debug, Clone)]
pub struct Database {
    /// `DATABASE_URL`: a PostgreSQL URL (it may carry credentials); unset
    /// runs the server in guest-only mode.
    pub url: Option<SecretString>,
    /// `DATABASE_MAX_CONNECTIONS`: the application pool's size.
    pub max_connections: u32,
    /// `DATABASE_MIN_CONNECTIONS`: the warm connection floor.
    pub min_connections: u32,
    /// `DATABASE_ACQUIRE_TIMEOUT_SECS`: the longest wait for a pool connection.
    pub acquire_timeout: Duration,
    /// `RUN_MIGRATIONS`: apply `./migrations` at startup (development only).
    pub run_migrations: bool,
}

impl Database {
    fn read(source: &dyn Source) -> Result<Self> {
        let max_connections = bounded(source, "DATABASE_MAX_CONNECTIONS", 20, 1..=1_000)?;
        let min_connections = bounded(source, "DATABASE_MIN_CONNECTIONS", 2, 0..=1_000)?;
        ensure!(
            min_connections <= max_connections,
            "DATABASE_MIN_CONNECTIONS must not exceed DATABASE_MAX_CONNECTIONS"
        );
        Ok(Self {
            url: text(source, "DATABASE_URL")?.map(SecretString::from),
            max_connections,
            min_connections,
            acquire_timeout: Duration::from_secs(bounded(
                source,
                "DATABASE_ACQUIRE_TIMEOUT_SECS",
                3,
                1..=60,
            )?),
            run_migrations: flag(source, "RUN_MIGRATIONS", false)?,
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert("DATABASE_URL", secret_state(self.url.as_ref()));
        out.insert("DATABASE_MAX_CONNECTIONS", json!(self.max_connections));
        out.insert("DATABASE_MIN_CONNECTIONS", json!(self.min_connections));
        out.insert(
            "DATABASE_ACQUIRE_TIMEOUT_SECS",
            json!(self.acquire_timeout.as_secs()),
        );
        out.insert("RUN_MIGRATIONS", json!(self.run_migrations));
    }
}

/// Private local operation records (`src/diagnostics.rs`).
#[derive(Debug, Clone)]
pub struct Diagnostics {
    /// `DIAGNOSTICS_PATH`: an absolute path for a new private JSONL file; unset disables recording.
    pub path: Option<PathBuf>,
    /// `DIAGNOSTICS_MAX_RECORDS`: records one file keeps before recording stops.
    pub max_records: u64,
    /// `DIAGNOSTICS_DURATION_SECS`: how long a recording runs.
    pub duration: Duration,
}

impl Diagnostics {
    fn read(source: &dyn Source) -> Result<Self> {
        Ok(Self {
            path: text(source, "DIAGNOSTICS_PATH")?.map(PathBuf::from),
            max_records: bounded(
                source,
                "DIAGNOSTICS_MAX_RECORDS",
                crate::diagnostics::DEFAULT_MAX_RECORDS,
                1..=crate::diagnostics::MAX_RECORDS_LIMIT,
            )?,
            duration: Duration::from_secs(bounded(
                source,
                "DIAGNOSTICS_DURATION_SECS",
                crate::diagnostics::DEFAULT_DURATION_SECS,
                1..=crate::diagnostics::MAX_DURATION_SECS,
            )?),
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert(
            "DIAGNOSTICS_PATH",
            json!(self.path.as_ref().map(|path| path.display().to_string())),
        );
        out.insert("DIAGNOSTICS_MAX_RECORDS", json!(self.max_records));
        out.insert("DIAGNOSTICS_DURATION_SECS", json!(self.duration.as_secs()));
    }
}

/// `LOG_FORMAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line, for aggregation.
    Json,
    /// Human-readable lines.
    Text,
}

impl LogFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

/// Logging, metrics and sampling.
#[derive(Debug, Clone)]
pub struct Observability {
    /// `LOG_FORMAT`.
    pub log_format: LogFormat,
    /// `METRICS_TOKEN`: bearer token for `/metrics`; unset answers 404.
    pub metrics_token: Option<SecretString>,
    /// `MEDIA_DIAGNOSTICS_ENABLED`: authenticated on-demand native media snapshots.
    pub media_diagnostics_enabled: bool,
    /// `SOURCE_REVISION`: the 40-hexadecimal commit of the running build.
    pub source_revision: Option<String>,
    /// `QUALITY_SAMPLE_INTERVAL_SECS`: time between server-side media quality samples.
    pub quality_sample_interval: Duration,
    /// `QUALITY_SAMPLE_MAX_TRANSPORT_STATS`: receive transports asked for statistics per sample.
    pub quality_sample_max_transport_stats: usize,
}

impl Observability {
    fn read(source: &dyn Source) -> Result<Self> {
        let log_format = match text(source, "LOG_FORMAT")?.as_deref() {
            None | Some("json") => LogFormat::Json,
            Some("text") => LogFormat::Text,
            Some(_) => bail!("LOG_FORMAT must be json or text"),
        };
        let source_revision = text(source, "SOURCE_REVISION")?;
        if let Some(revision) = &source_revision {
            ensure!(
                revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "SOURCE_REVISION must be a 40-character hexadecimal commit"
            );
        }
        Ok(Self {
            log_format,
            metrics_token: secret(source, "METRICS_TOKEN")?,
            media_diagnostics_enabled: flag(source, "MEDIA_DIAGNOSTICS_ENABLED", false)?,
            source_revision,
            quality_sample_interval: Duration::from_secs(bounded(
                source,
                "QUALITY_SAMPLE_INTERVAL_SECS",
                15,
                5..=300,
            )?),
            quality_sample_max_transport_stats: bounded(
                source,
                "QUALITY_SAMPLE_MAX_TRANSPORT_STATS",
                100,
                0..=10_000,
            )?,
        })
    }

    fn settings(&self, out: &mut BTreeMap<&'static str, Value>) {
        out.insert("LOG_FORMAT", json!(self.log_format.name()));
        out.insert("METRICS_TOKEN", secret_state(self.metrics_token.as_ref()));
        out.insert(
            "MEDIA_DIAGNOSTICS_ENABLED",
            json!(self.media_diagnostics_enabled),
        );
        out.insert("SOURCE_REVISION", json!(self.source_revision));
        out.insert(
            "QUALITY_SAMPLE_INTERVAL_SECS",
            json!(self.quality_sample_interval.as_secs()),
        );
        out.insert(
            "QUALITY_SAMPLE_MAX_TRANSPORT_STATS",
            json!(self.quality_sample_max_transport_stats),
        );
    }
}

/// Everything the server reads from its operator.
#[derive(Debug, Clone)]
pub struct Config {
    pub network: Network,
    pub limits: Limits,
    pub rates: Rates,
    pub auth: Auth,
    pub rooms: Rooms,
    pub database: Database,
    pub media: MediaConfig,
    /// `None` when `CPU_SATURATION_DISABLED` is set.
    pub saturation: Option<SaturationConfig>,
    /// `None` without `TURN_URLS`.
    pub turn: Option<TurnConfig>,
    pub diagnostics: Diagnostics,
    pub observability: Observability,
}

impl Config {
    /// Reads and validates every setting from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::from_source(&Environment)
    }

    /// Reads and validates every setting from `source`. Nothing is opened,
    /// bound or connected: this is the complete offline preflight.
    pub fn from_source(source: &dyn Source) -> Result<Self> {
        Ok(Self {
            network: Network::read(source)?,
            limits: Limits::read(source)?,
            rates: Rates::read(source)?,
            auth: Auth::read(source)?,
            rooms: Rooms::read(source)?,
            database: Database::read(source)?,
            media: MediaConfig::from_source(source)?,
            saturation: SaturationConfig::from_values(
                text(source, "CPU_SATURATION_DISABLED")?.as_deref(),
                text(source, "CPU_SATURATION_THROTTLED_FRACTION")?.as_deref(),
                text(source, "CPU_SATURATION_PRESSURE_AVG10")?.as_deref(),
                text(source, "CPU_SATURATION_WORKER_UTILIZATION")?.as_deref(),
                text(source, "MEMORY_SATURATION_FRACTION")?.as_deref(),
            )?,
            turn: TurnConfig::parse(
                text(source, "TURN_URLS")?.as_deref(),
                secret(source, "TURN_SECRET")?,
                text(source, "TURN_TTL")?.as_deref(),
            )?,
            diagnostics: Diagnostics::read(source)?,
            observability: Observability::read(source)?,
        })
    }

    /// The defaults of an empty environment, for fixtures that build servers
    /// without touching the process environment.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::from_source(&[]).expect("an empty environment is valid")
    }

    /// The effective value of every setting by variable name. Secrets appear
    /// as `"configured"` or `null`; `RUST_LOG` belongs to the tracing filter
    /// and is not listed.
    pub fn settings(&self) -> BTreeMap<&'static str, Value> {
        let mut out = BTreeMap::new();
        self.network.settings(&mut out);
        self.limits.settings(&mut out);
        self.rates.settings(&mut out);
        self.auth.settings(&mut out);
        self.rooms.settings(&mut out);
        self.database.settings(&mut out);
        self.media.settings(&mut out);
        saturation_settings(self.saturation.as_ref(), &mut out);
        turn_settings(self.turn.as_ref(), &mut out);
        self.diagnostics.settings(&mut out);
        self.observability.settings(&mut out);
        out
    }
}

fn saturation_settings(
    saturation: Option<&SaturationConfig>,
    out: &mut BTreeMap<&'static str, Value>,
) {
    let defaults = SaturationConfig::default();
    let config = saturation.unwrap_or(&defaults);
    out.insert("CPU_SATURATION_DISABLED", json!(saturation.is_none()));
    out.insert(
        "CPU_SATURATION_THROTTLED_FRACTION",
        json!(config.throttled_fraction),
    );
    out.insert(
        "CPU_SATURATION_PRESSURE_AVG10",
        json!(config.pressure_avg10),
    );
    out.insert(
        "CPU_SATURATION_WORKER_UTILIZATION",
        json!(config.worker_utilization),
    );
    out.insert("MEMORY_SATURATION_FRACTION", json!(config.memory_fraction));
}

fn turn_settings(turn: Option<&TurnConfig>, out: &mut BTreeMap<&'static str, Value>) {
    out.insert("TURN_URLS", json!(turn.map(|turn| &turn.urls)));
    out.insert("TURN_SECRET", secret_state(turn.map(|turn| &turn.secret)));
    out.insert(
        "TURN_TTL",
        json!(turn.map_or(crate::turn::DEFAULT_TURN_TTL_SECS, |turn| turn.ttl_secs)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(pairs: &[(&str, &str)]) -> Result<Config> {
        Config::from_source(&pairs.to_vec())
    }

    fn error(pairs: &[(&str, &str)]) -> String {
        parse(pairs).unwrap_err().to_string()
    }

    #[test]
    fn an_empty_source_yields_the_documented_defaults() {
        let config = parse(&[]).unwrap();
        assert_eq!(config.network.port, 3000);
        assert!(config.network.bind_addr.is_loopback());
        assert!(config.network.allowed_origins.is_empty());
        assert_eq!(config.limits.max_connections, 10_000);
        assert_eq!(config.limits.max_password_workers, None);
        assert_eq!(config.rates.join_attempts_per_ip_per_minute, 30);
        assert!(config.auth.jwt_secret.is_none());
        assert!(!config.auth.registration_enabled);
        assert!(!config.rooms.allow_ad_hoc);
        assert!(config.database.url.is_none());
        assert!(!config.database.run_migrations);
        assert!(config.saturation.is_some());
        assert!(config.turn.is_none());
        assert_eq!(config.observability.log_format, LogFormat::Json);
        assert_eq!(config.diagnostics.max_records, 10_000);
    }

    #[test]
    fn integers_are_strict_and_name_only_the_setting() {
        for value in ["", " ", "0", "-1", "12x", "1.5", "1000001"] {
            assert_eq!(
                error(&[("MAX_CONNECTIONS", value)]),
                "MAX_CONNECTIONS must be a whole number from 1 to 1000000",
                "value {value:?}"
            );
        }
        assert_eq!(
            parse(&[("MAX_CONNECTIONS", " 12 ")])
                .unwrap()
                .limits
                .max_connections,
            12
        );
        assert_eq!(
            error(&[("JOIN_ATTEMPTS_PER_IP_PER_MINUTE", "")]),
            "JOIN_ATTEMPTS_PER_IP_PER_MINUTE must be a whole number from 1 to 100000"
        );
    }

    #[test]
    fn optional_ceilings_treat_empty_as_unset() {
        for value in ["", "  "] {
            let config = parse(&[("MAX_PARTICIPANTS_PER_ROOM", value)]).unwrap();
            assert_eq!(config.limits.max_participants_per_room, None);
        }
        assert_eq!(
            parse(&[("MAX_PARTICIPANTS_PER_ROOM", " 80 ")])
                .unwrap()
                .limits
                .max_participants_per_room,
            Some(80)
        );
        assert_eq!(
            error(&[("MAX_PARTICIPANTS_PER_ROOM", "1")]),
            "MAX_PARTICIPANTS_PER_ROOM must be a whole number from 2 to 10000"
        );
        assert_eq!(
            error(&[("MAX_BROADCASTERS_PER_ROOM", "1001")]),
            "MAX_BROADCASTERS_PER_ROOM must be a whole number from 1 to 1000"
        );
    }

    #[test]
    fn flags_accept_one_grammar_and_default_closed() {
        for (value, expected) in [
            ("true", true),
            ("TRUE", true),
            ("1", true),
            ("false", false),
            ("0", false),
            ("", false),
        ] {
            let config = parse(&[
                ("REGISTRATION_ENABLED", value),
                ("ALLOW_AD_HOC_ROOMS", value),
            ])
            .unwrap();
            assert_eq!(
                config.auth.registration_enabled, expected,
                "value {value:?}"
            );
            assert_eq!(config.rooms.allow_ad_hoc, expected, "value {value:?}");
        }
        assert_eq!(
            error(&[("RUN_MIGRATIONS", "yes")]),
            "RUN_MIGRATIONS must be true, false, 1 or 0"
        );
    }

    #[test]
    fn secrets_need_thirty_two_bytes_and_are_never_echoed() {
        let short = "0123456789abcdef";
        let long = "0123456789abcdef0123456789abcdef";
        for name in ["JWT_SECRET", "METRICS_TOKEN", "TRUSTED_PROXY_SECRET"] {
            assert_eq!(
                error(&[(name, short)]),
                format!("{name} must contain at least 32 bytes")
            );
            let config = parse(&[(name, "")]).unwrap();
            assert_eq!(config.settings()[name], Value::Null);
            let config = config_with(name, long);
            assert_eq!(config.settings()[name], json!("configured"));
            assert!(!format!("{config:?}").contains(long));
        }
    }

    fn config_with(name: &str, value: &str) -> Config {
        parse(&[(name, value)]).unwrap()
    }

    #[test]
    fn a_public_bind_requires_exact_origins() {
        assert_eq!(
            error(&[("BIND_ADDR", "0.0.0.0")]),
            "ALLOWED_ORIGINS is required when BIND_ADDR is not loopback"
        );
        assert_eq!(
            error(&[("BIND_ADDR", "chat")]),
            "BIND_ADDR must be an IP address"
        );
        let config = parse(&[
            ("BIND_ADDR", "0.0.0.0"),
            (
                "ALLOWED_ORIGINS",
                " https://Chat.Example.com , http://localhost:5173 ",
            ),
        ])
        .unwrap();
        assert_eq!(
            config.network.allowed_origins,
            ["https://chat.example.com", "http://localhost:5173"]
        );
        for origin in [
            "https://chat.example.com/app",
            "https://user@chat.example.com",
            "https://chat.example.com/?x=1",
            "ftp://chat.example.com",
        ] {
            assert_eq!(
                error(&[("ALLOWED_ORIGINS", origin)]),
                "ALLOWED_ORIGINS entries must be exact http(s) origins without paths",
                "origin {origin}"
            );
        }
        assert_eq!(
            error(&[("ALLOWED_ORIGINS", "not a url")]),
            "ALLOWED_ORIGINS contains an invalid origin"
        );
    }

    #[test]
    fn announced_addresses_keep_one_listener_per_family() {
        let config = parse(&[
            ("ANNOUNCE_IP", "203.0.113.5"),
            ("ANNOUNCE_IPV6", "2001:db8::5"),
        ])
        .unwrap();
        assert_eq!(config.network.announce_ip, IpAddr::from([203, 0, 113, 5]));
        assert_eq!(
            config.network.announce_ipv6,
            Some("2001:db8::5".parse().unwrap())
        );
        assert_eq!(
            error(&[
                ("ANNOUNCE_IP", "2001:db8::5"),
                ("ANNOUNCE_IPV6", "2001:db8::6")
            ]),
            "ANNOUNCE_IPV6 goes beside an IPv4 ANNOUNCE_IP; an IPv6 ANNOUNCE_IP already announces that family"
        );
        assert_eq!(
            error(&[("ANNOUNCE_IPV6", "203.0.113.5")]),
            "ANNOUNCE_IPV6 must be an IPv6 address"
        );
        assert!(
            parse(&[("ANNOUNCE_IPV6", " ")])
                .unwrap()
                .network
                .announce_ipv6
                .is_none()
        );
    }

    #[test]
    fn passkeys_need_both_settings_and_a_secure_origin() {
        assert!(parse(&[]).unwrap().auth.webauthn.is_none());
        assert_eq!(
            error(&[("WEBAUTHN_RP_ID", "example.com")]),
            "WEBAUTHN_RP_ID and WEBAUTHN_ORIGIN must be configured together"
        );
        let config = parse(&[
            ("WEBAUTHN_RP_ID", "example.com"),
            ("WEBAUTHN_ORIGIN", "https://chat.example.com"),
        ])
        .unwrap();
        assert_eq!(
            config.auth.webauthn.unwrap().origin.as_str(),
            "https://chat.example.com/"
        );
        for origin in [
            "http://chat.example.com",
            "https://chat.example.com/app",
            "https://user:pw@chat.example.com",
        ] {
            assert_eq!(
                error(&[
                    ("WEBAUTHN_RP_ID", "example.com"),
                    ("WEBAUTHN_ORIGIN", origin)
                ]),
                "WEBAUTHN_ORIGIN must be an HTTPS origin without a path (HTTP localhost is allowed for development)",
                "origin {origin}"
            );
        }
        assert!(
            parse(&[
                ("WEBAUTHN_RP_ID", "localhost"),
                ("WEBAUTHN_ORIGIN", "http://localhost:3000")
            ])
            .unwrap()
            .auth
            .webauthn
            .is_some()
        );
    }

    #[test]
    fn database_pool_bounds_are_validated_without_a_url() {
        assert_eq!(
            error(&[("DATABASE_MIN_CONNECTIONS", "30")]),
            "DATABASE_MIN_CONNECTIONS must not exceed DATABASE_MAX_CONNECTIONS"
        );
        assert_eq!(
            error(&[("DATABASE_ACQUIRE_TIMEOUT_SECS", "61")]),
            "DATABASE_ACQUIRE_TIMEOUT_SECS must be a whole number from 1 to 60"
        );
        let config = parse(&[("DATABASE_URL", "postgres://app:pw@db/chat")]).unwrap();
        assert_eq!(config.settings()["DATABASE_URL"], json!("configured"));
        assert!(!format!("{config:?}").contains("app:pw"));
    }

    #[test]
    fn observability_settings_are_bounded_and_named() {
        assert_eq!(
            error(&[("LOG_FORMAT", "pretty")]),
            "LOG_FORMAT must be json or text"
        );
        assert_eq!(
            parse(&[("LOG_FORMAT", "text")])
                .unwrap()
                .observability
                .log_format,
            LogFormat::Text
        );
        assert_eq!(
            error(&[("SOURCE_REVISION", "abc")]),
            "SOURCE_REVISION must be a 40-character hexadecimal commit"
        );
        assert_eq!(
            error(&[("QUALITY_SAMPLE_INTERVAL_SECS", "4")]),
            "QUALITY_SAMPLE_INTERVAL_SECS must be a whole number from 5 to 300"
        );
        assert_eq!(
            parse(&[("QUALITY_SAMPLE_MAX_TRANSPORT_STATS", "0")])
                .unwrap()
                .observability
                .quality_sample_max_transport_stats,
            0
        );
    }

    #[test]
    fn turn_and_saturation_sections_compose_their_own_parsers() {
        let secret = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            error(&[("TURN_URLS", "turn:relay.example.com:3478")]),
            "TURN_SECRET is required when TURN_URLS is set"
        );
        let config = parse(&[
            (
                "TURN_URLS",
                "turn:relay.example.com:3478, turns:relay.example.com:5349",
            ),
            ("TURN_SECRET", secret),
            ("TURN_TTL", "3600"),
        ])
        .unwrap();
        let turn = config.turn.as_ref().unwrap();
        assert_eq!(turn.urls.len(), 2);
        assert_eq!(turn.ttl_secs, 3600);
        assert_eq!(config.settings()["TURN_SECRET"], json!("configured"));
        assert!(
            parse(&[("CPU_SATURATION_DISABLED", "true")])
                .unwrap()
                .saturation
                .is_none()
        );
        assert_eq!(
            parse(&[("CPU_SATURATION_DISABLED", "true")])
                .unwrap()
                .settings()["CPU_SATURATION_DISABLED"],
            json!(true)
        );
    }

    #[test]
    fn room_manager_defaults_match_an_empty_source() {
        assert_eq!(
            crate::room::ManagerSettings::default(),
            crate::room::ManagerSettings::from(&parse(&[]).unwrap())
        );
    }

    #[test]
    fn the_settings_summary_names_every_variable_once() {
        let settings = parse(&[]).unwrap().settings();
        for name in [
            "BIND_ADDR",
            "MEDIA_WORKERS",
            "WEBRTC_MAX_OUTGOING_BITRATE",
            "CPU_SATURATION_THROTTLED_FRACTION",
            "TURN_TTL",
            "DIAGNOSTICS_PATH",
            "QUALITY_SAMPLE_MAX_TRANSPORT_STATS",
        ] {
            assert!(settings.contains_key(name), "{name} missing");
        }
        assert_eq!(settings["TURN_TTL"], json!(86_400));
        assert_eq!(settings["ALLOWED_ORIGINS"], json!([] as [&str; 0]));
    }

    fn is_variable_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    }

    /// A docs table row: `| \`NAME\` | default | description |`.
    fn documented_defaults() -> BTreeMap<String, String> {
        let docs = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/docs/configuration.md"
        ));
        docs.lines()
            .filter_map(|line| {
                let mut cells = line.split('|').map(str::trim);
                let _ = cells.next()?;
                let name = cells.next()?;
                let default = cells.next()?;
                let name = name.strip_prefix('`')?.strip_suffix('`')?;
                is_variable_name(name).then(|| (name.to_owned(), default.to_owned()))
            })
            .collect()
    }

    fn rendered(value: &Value) -> Option<String> {
        match value {
            Value::Null => None,
            Value::Bool(flag) => Some(flag.to_string()),
            Value::Number(number) => number.as_f64().map(|number| number.to_string()),
            Value::String(text) => Some(text.clone()),
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            Value::Object(_) => None,
        }
    }

    #[test]
    fn the_configuration_guide_documents_every_setting_with_its_default() {
        let settings = parse(&[]).unwrap().settings();
        let documented = documented_defaults();
        let undocumented: Vec<_> = settings
            .keys()
            .filter(|name| !documented.contains_key(**name))
            .collect();
        assert!(
            undocumented.is_empty(),
            "undocumented settings: {undocumented:?}"
        );
        let unknown: Vec<_> = documented
            .keys()
            .filter(|name| *name != "RUST_LOG" && !settings.contains_key(name.as_str()))
            .collect();
        assert!(
            unknown.is_empty(),
            "documented settings the server never reads: {unknown:?}"
        );
        for (name, default) in &documented {
            let Some(value) = settings.get(name.as_str()) else {
                continue;
            };
            if default == "(none)" || default == "unset" {
                assert_eq!(*value, Value::Null, "{name} documents no default");
            } else if let Some(literal) =
                default.strip_prefix('`').and_then(|d| d.strip_suffix('`'))
            {
                assert_eq!(
                    rendered(value).as_deref(),
                    Some(literal),
                    "{name}: documented default {literal:?}"
                );
            }
        }
    }

    #[test]
    fn compose_defaults_equal_the_servers_own() {
        // Compose pins what the server would derive from the host for these two.
        const DERIVED: [&str; 2] = ["MEDIA_WORKERS", "MAX_PASSWORD_WORKERS"];
        let settings = parse(&[]).unwrap().settings();
        let compose = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docker-compose.yml"));
        let mut checked = 0;
        for line in compose.lines() {
            let Some((name, rest)) = line.trim().split_once(": \"${") else {
                continue;
            };
            if !is_variable_name(name) {
                continue;
            }
            let Some((variable, default)) = rest.trim_end_matches("}\"").split_once(":-") else {
                continue;
            };
            assert_eq!(
                name, variable,
                "compose default for {name} interpolates {variable}"
            );
            if DERIVED.contains(&name) {
                continue;
            }
            let Some(value) = settings.get(name) else {
                assert_eq!(
                    name, "RUST_LOG",
                    "compose sets {name}, which the server never reads"
                );
                continue;
            };
            assert_eq!(
                rendered(value).unwrap_or_default(),
                default,
                "docker-compose.yml default for {name}"
            );
            checked += 1;
        }
        assert!(checked >= 25, "only {checked} compose defaults compared");
    }

    #[test]
    fn no_other_module_reads_the_environment() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy();
                if !name.ends_with(".rs") || name.ends_with("_tests.rs") || name == "config.rs" {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let production = text.split("#[cfg(test)]").next().unwrap_or("");
                if production.contains("env::var") || production.contains("env::vars") {
                    offenders.push(path.strip_prefix(&root).unwrap().display().to_string());
                }
            }
        }
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "modules reading the environment directly: {offenders:?}"
        );
    }
}
