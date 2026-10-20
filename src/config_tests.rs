//! Tests of `src/config.rs`.

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
    let long = "disposable-fixture-value-of-32-bytes";
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
    let secret = "disposable-turn-relay-fixture-secret";
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
        } else if let Some(literal) = default.strip_prefix('`').and_then(|d| d.strip_suffix('`')) {
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
