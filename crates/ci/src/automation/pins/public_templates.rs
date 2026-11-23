//! The public service's rendered configuration, carried over from the Python
//! template tests: what the Compose project, the environment files, the
//! database's access rules and its grants say once a deployment renders
//! them. Nothing is started and no network is used.

use super::ansible::render_with;
use super::{checkout_file, document};
use serde_json::{Map, Value, json};
use simplestchat_ops::release::maintain::IDENTITY_KEYS;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

const COMPOSE: &str = "public-compose.yml.j2";
const APP: &str = "public-app.env.j2";
const MIGRATION: &str = "public-migration.env.j2";
const PROXY: &str = "public-proxy.env.j2";
const HBA: &str = "public-pg_hba.conf.j2";
const BOOTSTRAP: &str = "public-init-database.sql.j2";
const GRANTS: &str = "public-runtime-grants.sql.j2";
const RELAY: &str = "turnserver.conf.j2";
/// The templates that need only a host's own values.
const HOST_TEMPLATES: [&str; 8] = [
    COMPOSE,
    APP,
    MIGRATION,
    PROXY,
    "public-postgres-admin-password.j2",
    HBA,
    BOOTSTRAP,
    GRANTS,
];
const SERVER_IMAGE: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Six secrets, each a number in 64 hexadecimal digits, counted from `first`.
fn secrets(first: u32) -> Value {
    let names = [
        "postgres_admin_password",
        "migration_password",
        "database_password",
        "metrics_token",
        "proxy_secret",
        "owner_password",
    ];
    let secret =
        |(index, name): (u32, &str)| (name.to_owned(), json!(format!("{:064x}", first + index)));
    Value::Object((0..).zip(names).map(secret).collect())
}

/// A host's values, with sizing as the group variables derive it for four
/// processors and 11967 MiB, and `overrides` in place of its own.
fn host(overrides: &Value) -> Value {
    let mut values = json!({
        "scpub_config": "/etc/simplestchat-public",
        "scpub_root": "/srv/simplestchat-public",
        "scpub_server_image": SERVER_IMAGE,
        "scpub_postgres_image": format!("postgres:18.6-bookworm@sha256:{}", "b".repeat(64)),
        "scpub_caddy_image": format!("caddy:2.11.2-alpine@sha256:{}", "c".repeat(64)),
        "scpub_domain": "chat.example.test",
        "scpub_webauthn_rp_id": "chat.example.test",
        "scpub_announce_ip": "192.0.2.10",
        "scpub_announce_ipv6": "2001:db8::10",
        "scpub_ipv6_network": "fd5c:5c68:a7d1::/64",
        "scpub_registration_enabled": false,
        "scpub_app_cpus": 3,
        "scpub_media_workers": 3,
        "scpub_app_memory_mib": 8976,
        "scpub_max_connections": 525,
        "scpub_max_rooms": 525,
        "scpub_max_participants_per_room": 525,
        "scpub_max_broadcasters_per_room": 30,
        "scpub_max_users": 100,
        "scpub_max_persisted_rooms": 100,
        "scpub_port_mbps": 1000,
        "scpub_reserved_cpus": 1,
        "scpub_reserved_memory_mib": 2991,
        "scpub_postgres_memory_mib": 1495,
        "scpub_postgres_shared_buffers_mib": 373,
        "scpub_postgres_cpus": 1,
        "scpub_caddy_memory_mib": 373,
        "scpub_caddy_cpus": 0.5,
        "scpub_turn_memory_mib": 512,
        "scpub_turn_cpus": 1,
        "scpub_turn_bps_capacity": 62_500_000,
        "scpub_turn_max_bps": 500_000,
        "scpub_turn_user_quota": 4,
        "scpub_turn_total_quota": 525,
        "scpub_turn_relay_threads": 1,
        "scpub_turn_relay_port_min": 49160,
        "scpub_turn_relay_port_max": 50209,
        "scpub_secrets": secrets(1),
    });
    let overrides = overrides.as_object().unwrap().clone();
    values.as_object_mut().unwrap().extend(overrides);
    values
}

/// The templates a host renders, once, with the fixture's own values.
fn rendered(name: &str) -> &'static str {
    static RENDERED: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    &RENDERED.get_or_init(|| render_with(&HOST_TEMPLATES, &host(&json!({}))))[name]
}

/// One template rendered for a host with `overrides`.
fn rendered_with(name: &str, overrides: &Value) -> String {
    render_with(&[name], &host(overrides)).remove(name).unwrap()
}

/// A rendered Compose project.
fn project(text: &str) -> Value {
    serde_saphyr::from_str(text).unwrap()
}

/// The services of the project a host renders.
fn services() -> Map<String, Value> {
    project(rendered(COMPOSE))["services"]
        .as_object()
        .unwrap()
        .clone()
}

/// The assignments of a rendered environment file, their values as written.
fn environment(text: &str) -> BTreeMap<String, String> {
    let assigned = |line: &str| {
        let (name, value) = line.split_once('=').unwrap();
        (name.to_owned(), value.to_owned())
    };
    let lines = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    lines.map(assigned).collect()
}

/// The strings of a list.
fn strings(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect()
}

/// The provisioned Compose release: the upstream part of the host's
/// `docker-compose-plugin` package version.
fn provisioned_compose() -> String {
    let defaults = document("ops/ansible/group_vars/benchmark_hosts.yml");
    let package = defaults["scbench_docker_compose_version"].as_str().unwrap();
    package.split_once('-').unwrap().0.to_owned()
}

/// The provisioned Compose renderer on `PATH`, as a standalone binary or as
/// the Docker CLI plugin, asked for its version under `environment`.
fn compose_renderer(environment: &[(&str, String)]) -> Option<Vec<std::path::PathBuf>> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = |name: &str| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    };
    let mut candidates = Vec::new();
    if let Some(standalone) = found("docker-compose") {
        candidates.push(vec![standalone]);
    }
    if let Some(docker) = found("docker") {
        candidates.push(vec![docker, "compose".into()]);
    }
    let expected = provisioned_compose();
    candidates.into_iter().find(|command| {
        let output = std::process::Command::new(&command[0])
            .args(&command[1..])
            .args(["version", "--short"])
            .env_clear()
            .envs(
                environment
                    .iter()
                    .map(|(name, value)| (*name, value.as_str())),
            )
            .output();
        output.is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == expected
        })
    })
}

/// The disposable guest's snapshot checks the Compose model the provisioned
/// renderer makes of the real templates: rendered as the fixture's inventory
/// renders them, `config --format json` with the maintenance profile and no
/// engine, and the guest's own check of the project, its four services and
/// each one's isolation and bounds. The memory limits arrive as the decimal
/// byte strings the check requires. Without the provisioned Compose the case
/// is skipped, unless `VM_COMPOSE_REQUIRED=1` (the deployment job) requires it.
#[test]
fn the_provisioned_compose_model_of_the_templates_passes_the_guest_checks() {
    let scratch = super::ansible::Scratch::new();
    let directory = scratch.path().canonicalize().unwrap();
    let environment = [
        ("PATH", "/usr/bin:/bin".to_owned()),
        ("HOME", directory.to_str().unwrap().to_owned()),
        ("LC_ALL", "C".to_owned()),
        // No engine exists at this private path; config stays daemon-free.
        (
            "DOCKER_HOST",
            format!("unix://{}", directory.join("absent.sock").display()),
        ),
    ];
    let Some(renderer) = compose_renderer(&environment) else {
        let required = std::env::var("VM_COMPOSE_REQUIRED").is_ok_and(|value| value == "1");
        assert!(
            !required,
            "the provisioned Compose is required for this check"
        );
        eprintln!(
            "skipped: the provisioned Compose {} is absent",
            provisioned_compose()
        );
        return;
    };
    let overrides = json!({
        "scpub_config": directory,
        "scpub_root": directory.join("data"),
        "scpub_domain": "vm-fixture.test",
        "scpub_webauthn_rp_id": "vm-fixture.test",
        "scpub_announce_ip": "127.0.0.1",
        "scpub_announce_ipv6": "",
        "scpub_media_workers": 1,
        "scpub_app_cpus": 1,
        "scpub_app_memory_mib": 1024,
        "scpub_postgres_memory_mib": 1024,
        "scpub_postgres_shared_buffers_mib": 256,
    });
    let templates = [
        (COMPOSE, "compose.public.yml"),
        (APP, "app.env"),
        (MIGRATION, "migration.env"),
        (PROXY, "proxy.env"),
    ];
    let names: Vec<&str> = templates.iter().map(|(template, _)| *template).collect();
    let rendered = render_with(&names, &host(&overrides));
    for (template, name) in templates {
        std::fs::write(directory.join(name), format!("{}\n", rendered[template])).unwrap();
    }
    std::fs::write(
        directory.join("compose.base.yml"),
        checkout_file("docker-compose.yml"),
    )
    .unwrap();
    let output = std::process::Command::new(&renderer[0])
        .args(&renderer[1..])
        .arg("--env-file")
        .arg(directory.join("app.env"))
        .arg("-f")
        .arg(directory.join("compose.public.yml"))
        .args(["--profile", "maintenance", "config", "--format", "json"])
        .current_dir(&directory)
        .env_clear()
        .envs(
            environment
                .iter()
                .map(|(name, value)| (*name, value.as_str())),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let checked = simplestchat_ops::release::infrastructure::compose_model(&output.stdout);
    assert_eq!(checked, Ok(4), "{checked:?}");
    let model: Value = serde_json::from_slice(&output.stdout).unwrap();
    for name in ["simplestchat", "migrate", "postgres"] {
        assert_eq!(model["services"][name]["mem_limit"], "1073741824", "{name}");
    }
}

/// From `test_database_logs_omit_bound_values_in_slow_and_error_paths`.
#[test]
fn database_logs_omit_bound_values_in_slow_and_error_paths() {
    let services = services();
    let command = strings(&services["postgres"]["command"]);
    assert!(command.contains(&"log_min_duration_statement=500ms"));
    assert!(command.contains(&"log_parameter_max_length=0"));
    assert!(command.contains(&"log_parameter_max_length_on_error=0"));
}

/// From `test_all_public_templates_render_without_missing_values`.
#[test]
fn all_public_templates_render_without_missing_values() {
    for name in HOST_TEMPLATES {
        assert!(!rendered(name).contains("{{"), "{name}");
    }
}

/// From `test_database_and_maintenance_have_no_network_or_ports`.
#[test]
fn database_and_maintenance_have_no_network_or_ports() {
    let services = services();
    for name in ["postgres", "migrate"] {
        let service = &services[name];
        assert_eq!(service["network_mode"], "none", "{name}");
        assert!(service.get("ports").is_none(), "{name}");
        assert_eq!(service["read_only"], true, "{name}");
        assert_eq!(service["cap_drop"], json!(["ALL"]), "{name}");
    }
    let database = &services["postgres"];
    assert_eq!(database["user"], "999:999");
    let command = strings(&database["command"]);
    assert!(command.contains(&"listen_addresses="));
    assert!(command.contains(&"password_encryption=scram-sha-256"));
    let sockets = "unix_socket_directories=/var/run/postgresql,/run/simplestchat-postgres";
    assert!(command.contains(&sockets));
    let health = database["healthcheck"]["test"][1].as_str().unwrap();
    assert!(health.contains("/proc/1/comm"));
    let volumes = strings(&database["volumes"]);
    assert!(volumes.contains(&"/srv/simplestchat-public/postgres:/var/lib/postgresql"));
    let maintenance = &services["migrate"];
    assert_eq!(maintenance["profiles"], json!(["maintenance"]));
    assert_eq!(maintenance["restart"], "no");
    assert_eq!(maintenance["image"], SERVER_IMAGE);
}

/// From `test_the_base_compose_file_carries_the_servers_limits`: Compose's
/// `environment` outranks `app.env`, so its defaults must be the server's.
#[test]
fn the_base_compose_file_carries_the_servers_limits() {
    let compose = document("docker-compose.yml");
    let base = &compose["services"]["simplestchat"]["environment"];
    // The server's own default; 16 blanked a browser's tiles beyond eight peers.
    assert_eq!(
        base["MAX_CONSUMERS_PER_PARTICIPANT"],
        "${MAX_CONSUMERS_PER_PARTICIPANT:-64}"
    );
    // Forwarded, and empty when unset, which the server reads as no ceiling.
    assert_eq!(
        base["MAX_PARTICIPANTS_PER_ROOM"],
        "${MAX_PARTICIPANTS_PER_ROOM:-}"
    );
    assert_eq!(
        base["MAX_BROADCASTERS_PER_ROOM"],
        "${MAX_BROADCASTERS_PER_ROOM:-}"
    );
    // The managed host interpolates the base from app.env, which sets the ceiling.
    let app = environment(rendered(APP));
    assert_eq!(app["MAX_PARTICIPANTS_PER_ROOM"], "525");
    assert_eq!(app["MAX_BROADCASTERS_PER_ROOM"], "30");
}

/// From `test_a_dual_stack_host_gets_an_ipv6_network_and_listeners`: IPv6 is
/// announced, carried natively by the project network and offered by TURN.
#[test]
fn a_dual_stack_host_gets_an_ipv6_network_and_listeners() {
    let network = project(rendered(COMPOSE))["networks"]["default"].clone();
    assert_eq!(network["enable_ipv6"], true);
    assert_eq!(
        network["ipam"]["config"][0]["subnet"],
        "fd5c:5c68:a7d1::/64"
    );
    let single = json!({"scpub_announce_ipv6": ""});
    assert!(
        project(&rendered_with(COMPOSE, &single))
            .get("networks")
            .is_none()
    );
    assert_eq!(environment(rendered(APP))["ANNOUNCE_IPV6"], "2001:db8::10");
    assert_eq!(
        environment(&rendered_with(APP, &single))["ANNOUNCE_IPV6"],
        ""
    );
    let shared = "a".repeat(64);
    let relay = rendered_with(RELAY, &json!({"scpub_turn_secret": shared}));
    let listening = |address: &str| relay.lines().filter(|line| *line == address).count();
    assert_eq!(listening("listening-ip=192.0.2.10"), 1);
    assert_eq!(listening("listening-ip=2001:db8::10"), 1);
    let without = json!({"scpub_turn_secret": shared, "scpub_announce_ipv6": ""});
    let single = rendered_with(RELAY, &without);
    assert!(!single.contains("listening-ip=2001"));
    // One listener for the one address, and none for an address that is not set.
    let listeners = single
        .lines()
        .filter(|line| line.starts_with("listening-ip="));
    assert_eq!(listeners.collect::<Vec<_>>(), ["listening-ip=192.0.2.10"]);
}

/// From `test_app_extends_existing_compose_and_proxy_is_nonroot`.
#[test]
fn app_extends_existing_compose_and_proxy_is_nonroot() {
    let services = services();
    let app = &services["simplestchat"];
    assert_eq!(
        app["extends"],
        json!({"file": "./compose.base.yml", "service": "simplestchat"})
    );
    assert_eq!(app["env_file"], json!(["./app.env"]));
    assert_eq!(app["user"], "10001:10001");
    assert_eq!(app["image"], SERVER_IMAGE);
    let socket = "/srv/simplestchat-public/postgres-socket:/run/simplestchat-postgres:ro";
    assert_eq!(app["volumes"], json!([socket]));
    let proxy = &services["caddy"];
    assert_eq!(proxy["user"], "10001:10001");
    assert_eq!(proxy["cap_add"], json!(["NET_BIND_SERVICE"]));
    assert_eq!(
        proxy["ports"],
        json!(["80:80/tcp", "443:443/tcp", "443:443/udp"])
    );
    assert_eq!(proxy["sysctls"]["net.ipv4.ip_unprivileged_port_start"], "0");
    let journal = json!({
        "driver": "journald",
        "options": {"tag": "simplestchat.app", "mode": "non-blocking", "max-buffer-size": "4m"},
    });
    assert_eq!(app["logging"], journal);
    for (name, service) in &services {
        if name == "simplestchat" {
            continue;
        }
        assert_eq!(service["logging"]["driver"], "local", "{name}");
        let options = json!({"max-size": "10m", "max-file": "3"});
        assert_eq!(service["logging"]["options"], options, "{name}");
    }
}

/// From `test_app_configuration_is_bounded_and_secrets_are_separated`.
#[test]
fn app_configuration_is_bounded_and_secrets_are_separated() {
    let app = environment(rendered(APP));
    let migration = environment(rendered(MIGRATION));
    let proxy = environment(rendered(PROXY));
    let expected = [
        ("RUN_MIGRATIONS", "false"),
        ("REGISTRATION_ENABLED", "false"),
        ("ALLOW_AD_HOC_ROOMS", "false"),
        ("MEDIA_WORKERS", "3"),
        ("RTC_PORT_END", "40002"),
        ("SIMPLESTCHAT_CPUS", "3"),
        ("SIMPLESTCHAT_MEMORY_LIMIT", "8976m"),
        ("MAX_CONNECTIONS", "525"),
        ("MAX_USERS", "100"),
        ("MAX_ROOMS", "525"),
        ("MAX_PERSISTED_ROOMS", "100"),
        ("WEBAUTHN_RP_ID", "chat.example.test"),
        ("WEBAUTHN_ORIGIN", "https://chat.example.test"),
    ];
    for (name, value) in expected {
        assert_eq!(app[name], value, "{name}");
    }
    let open = rendered_with(APP, &json!({"scpub_registration_enabled": true}));
    assert_eq!(environment(&open)["REGISTRATION_ENABLED"], "true");
    assert!(app["DATABASE_URL"].contains("simplestchat_app:"));
    assert!(migration["DATABASE_URL"].contains("simplestchat_migrate:"));
    for values in [&app, &migration] {
        let socket = "?host=/run/simplestchat-postgres&sslmode=disable";
        assert!(values["DATABASE_URL"].ends_with(socket));
    }
    assert_eq!(migration["RUN_MIGRATIONS"], "true");
    assert_eq!(migration["BIND_ADDR"], "127.0.0.1");
    assert_eq!(migration["REGISTRATION_ENABLED"], "false");
    assert_eq!(proxy["TRUSTED_PROXY_SECRET"], app["TRUSTED_PROXY_SECRET"]);
    assert_eq!(proxy["CADDY_UPSTREAM"], "simplestchat:3000");
    assert!(!migration.contains_key("METRICS_TOKEN"));
    assert!(!proxy.contains_key("DATABASE_URL"));
    let owner = format!("{:064x}", 6);
    assert!(app.values().all(|value| !value.contains(&owner)));
}

/// From `test_passkey_identity_can_remain_at_the_parent_without_widening_origins`:
/// inventory retains an existing identity while HTTP and WebAuthn use one new
/// origin.
#[test]
fn passkey_identity_can_remain_at_the_parent_without_widening_origins() {
    let domain = "the.research.clinic";
    for identity in [domain, "research.clinic"] {
        let overrides = json!({"scpub_domain": domain, "scpub_webauthn_rp_id": identity});
        let app = environment(&rendered_with(APP, &overrides));
        assert_eq!(app["WEBAUTHN_RP_ID"], identity);
        assert_eq!(app["WEBAUTHN_ORIGIN"], format!("https://{domain}"));
        assert_eq!(app["ALLOWED_ORIGINS"], format!("https://{domain}"));
    }
}

/// From `test_a_pinned_worker_count_sets_the_media_port_range`: each media
/// worker takes one port from 40000 upwards.
#[test]
fn a_pinned_worker_count_sets_the_media_port_range() {
    let app = environment(&rendered_with(APP, &json!({"scpub_media_workers": 2})));
    assert_eq!(app["RTC_PORT_END"], "40001");
    assert_eq!(app["MEDIA_WORKERS"], "2");
}

/// From `test_ipv6_only_network_carries_the_primary_announced_address`: an
/// IPv6 primary needs a native Compose network even without a second address.
#[test]
fn ipv6_only_network_carries_the_primary_announced_address() {
    let only = json!({"scpub_announce_ip": "2001:db8::10", "scpub_announce_ipv6": ""});
    let compose = project(&rendered_with(COMPOSE, &only));
    assert_eq!(compose["networks"]["default"]["enable_ipv6"], true);
}

/// The lines of `app.env` that a maintenance release refuses to change.
fn identity_keys() -> BTreeSet<String> {
    IDENTITY_KEYS.map(str::to_owned).into()
}

/// From `test_the_maintenance_helper_protects_every_secret_and_identity_line`:
/// a candidate `app.env` may change sizing; a maintenance release refuses
/// one that changes a secret or identity line.
#[test]
fn the_maintenance_helper_protects_every_secret_and_identity_line() {
    let relayed =
        |letter: &str| json!({"scpub_turn_enabled": true, "scpub_turn_secret": letter.repeat(64)});
    let first = environment(&rendered_with(APP, &relayed("a")));
    let mut other = relayed("b");
    other.as_object_mut().unwrap().extend(
        json!({
            "scpub_secrets": secrets(101),
            "scpub_domain": "other.example.test",
            "scpub_webauthn_rp_id": "other.example.test",
            "scpub_announce_ip": "192.0.2.99",
            "scpub_announce_ipv6": "2001:db8::99",
            "scpub_server_image": format!("sha256:{}", "d".repeat(64)),
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let second = environment(&rendered_with(APP, &other));
    let changed = |name: &&String| first[*name] != second[*name];
    let identity: BTreeSet<String> = first.keys().filter(changed).cloned().collect();
    assert!(!identity.is_empty(), "the fixture changed nothing");
    assert!(identity.contains("ANNOUNCE_IPV6"));
    let protected = identity_keys();
    assert!(identity.is_subset(&protected), "{identity:?}");
    for name in ["RUN_MIGRATIONS", "BIND_ADDR", "PORT"] {
        assert!(protected.contains(name), "{name}");
    }
    // Sizing lines stay free for the candidate to follow the host.
    let sizing = [
        "MEDIA_WORKERS",
        "RTC_PORT_END",
        "SIMPLESTCHAT_CPUS",
        "SIMPLESTCHAT_MEMORY_LIMIT",
        "MAX_CONNECTIONS",
        "MAX_ROOMS",
        "MAX_PARTICIPANTS_PER_ROOM",
        "MAX_BROADCASTERS_PER_ROOM",
    ];
    assert!(sizing.iter().all(|name| !protected.contains(*name)));
}

/// From `test_database_authentication_and_grants_are_explicit`.
#[test]
fn database_authentication_and_grants_are_explicit() {
    let rule = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let rules = rendered(HBA)
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let expected = [
        "local all postgres peer",
        "local simplestchat simplestchat_migrate scram-sha-256",
        "local simplestchat simplestchat_app scram-sha-256",
        "local all all reject",
        "host all all 0.0.0.0/0 reject",
        "host all all ::/0 reject",
    ];
    assert_eq!(rules.map(rule).collect::<Vec<_>>(), expected.map(rule));
    // The statements, without the comments: a statement that is commented
    // out is not made.
    let statements = rendered(BOOTSTRAP)
        .lines()
        .filter(|line| !line.starts_with("--"));
    let bootstrap = statements.collect::<Vec<_>>().join("\n");
    let plain = "NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS";
    assert_eq!(bootstrap.matches(plain).count(), 2);
    assert!(bootstrap.contains("REVOKE ALL ON DATABASE simplestchat FROM PUBLIC"));
    assert!(bootstrap.contains("REVOKE ALL ON SCHEMA public FROM PUBLIC"));
    assert!(bootstrap.contains("GRANT USAGE, CREATE ON SCHEMA public TO simplestchat_migrate"));
    assert!(!bootstrap.contains("GRANT USAGE, CREATE ON SCHEMA public TO simplestchat_app"));
    assert!(bootstrap.contains("CREATE EXTENSION pg_trgm WITH SCHEMA public"));
    let grants = rendered(GRANTS);
    assert!(!grants.contains("ALL TABLES"));
    assert!(grants.contains("GRANT SELECT, INSERT, UPDATE\n  ON public.users\n"));
    for table in [
        "sessions",
        "rooms",
        "room_roles",
        "room_states",
        "room_reports",
    ] {
        assert!(grants.contains(&format!("public.{table}")), "{table}");
    }
    let statements = grants.lines().filter(|line| !line.starts_with("--"));
    assert!(
        statements
            .into_iter()
            .all(|line| !line.contains("_sqlx_migrations"))
    );
}

/// The tokens of Rust `source` as the grant scanner tells them apart, each by
/// where it begins and ends: a comment, a raw, plain or byte string, a
/// character, a word, or any other single character that is no white space.
fn tokens(source: &str) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    // A quoted literal from `open`, one character or any number, with
    // backslash escapes.
    let quoted = |open: usize, quote: u8, one: bool| {
        let mut at = open + 1;
        let mut held = 0;
        while let Some(byte) = bytes.get(at) {
            match *byte {
                found if found == quote => return (!one || held == 1).then_some(at + 1),
                b'\\' => at += 1 + source[at + 1..].chars().next()?.len_utf8(),
                _ => at += source[at..].chars().next()?.len_utf8(),
            }
            held += 1;
        }
        None
    };
    let raw = |at: usize| {
        let prefix = if source[at..].starts_with("br") { 2 } else { 1 };
        let hashes = bytes[at + prefix..]
            .iter()
            .take_while(|byte| **byte == b'#')
            .count();
        let open = at + prefix + hashes;
        if hashes > 32 || bytes.get(open) != Some(&b'"') {
            return None;
        }
        let close = format!("\"{}", "#".repeat(hashes));
        Some(open + 1 + source[open + 1..].find(&close)? + close.len())
    };
    let (mut found, mut at) = (Vec::new(), 0);
    while let Some(next) = source[at..].chars().next() {
        let rest = &source[at..];
        let literal = usize::from(next == 'b');
        let end = if rest.starts_with("//") {
            Some(at + rest.find('\n').unwrap_or(rest.len()))
        } else if let Some(close) = rest.strip_prefix("/*").and_then(|inside| inside.find("*/")) {
            Some(at + close + 4)
        } else if let Some(end) = (rest.starts_with("br") || next == 'r')
            .then(|| raw(at))
            .flatten()
        {
            Some(end)
        } else if bytes.get(at + literal) == Some(&b'"')
            && quoted(at + literal, b'"', false).is_some()
        {
            quoted(at + literal, b'"', false)
        } else if bytes.get(at + literal) == Some(&b'\'')
            && quoted(at + literal, b'\'', true).is_some()
        {
            quoted(at + literal, b'\'', true)
        } else if next.is_ascii_alphabetic() || next == '_' {
            Some(at + bytes[at..].iter().take_while(|byte| word(**byte)).count())
        } else if !next.is_whitespace() {
            Some(at + next.len_utf8())
        } else {
            None
        };
        match end {
            Some(end) => {
                found.push((at, end));
                at = end;
            }
            None => at += next.len_utf8(),
        }
    }
    found
}

/// Rust `source` without its items for tests only: what follows each
/// `#[cfg(test)]` up to the end of the item it marks, a block or a statement.
/// Production declarations that follow such an item stay.
fn production_rust(source: &str) -> String {
    let tokens = tokens(source);
    let text = |index: usize| tokens.get(index).map(|(start, end)| &source[*start..*end]);
    let marker = ["#", "[", "cfg", "(", "test", ")", "]"];
    let (mut result, mut cursor, mut index) = (String::new(), 0, 0);
    while index < tokens.len() {
        let marked = (0..marker.len()).all(|offset| text(index + offset) == Some(marker[offset]));
        if !marked {
            index += 1;
            continue;
        }
        result.push_str(&source[cursor..tokens[index].0]);
        index += marker.len();
        let mut depth = 0;
        while let Some(token) = text(index) {
            index += 1;
            match token {
                "{" => depth += 1,
                "}" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                ";" if depth == 0 => break,
                _ => {}
            }
        }
        cursor = tokens[index - 1].1;
    }
    result.push_str(&source[cursor..]);
    result
}

/// From `test_runtime_grant_scanner_keeps_production_after_test_items`:
/// test-only imports, functions and modules cannot hide later runtime SQL.
#[test]
fn runtime_grant_scanner_keeps_production_after_test_items() {
    let source = r##"
#[cfg(test)] use fixture::owner;
fn first() { query("INSERT INTO rooms VALUES (1)"); }
#[cfg(test)]
mod tests { fn fixture() { query(r#"DELETE FROM users; }"#); } }
#[cfg(test)]
fn helper() { query("UPDATE users SET name = 'fixture'"); }
fn runtime() { query("UPDATE rooms SET name = 'current'"); }
"##;
    let production = production_rust(source);
    assert!(production.contains("INSERT INTO rooms"));
    assert!(production.contains("UPDATE rooms"));
    assert!(!production.contains("DELETE FROM users"));
    assert!(!production.contains("UPDATE users"));
    assert!(!production.contains("fixture::owner"));
}

/// A raw string is one token whatever it holds: a quote or a brace inside it
/// neither ends it nor ends the item for tests around it.
#[test]
fn runtime_grant_scanner_reads_a_raw_string_whole() {
    let source = r###"
#[cfg(test)]
fn fixture() {
    query(r#"DELETE FROM users WHERE name = "a" } "#);
    query(br##"UPDATE users "# }"##);
}
fn runtime() { query(r#"UPDATE rooms SET name = "current""#); }
"###;
    let kept = "\n\nfn runtime() { query(r#\"UPDATE rooms SET name = \"current\"\"#); }\n";
    assert_eq!(production_rust(source), kept);
}

/// Every Rust source below `directory` that is no file of tests.
fn sources(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if path.is_dir() {
            sources(&path, found);
        } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
            found.push(path);
        }
    }
}

/// From `test_runtime_grants_cover_every_statement_the_server_runs`: every
/// table the server inserts into, updates or deletes from is granted that
/// right.
#[test]
fn runtime_grants_cover_every_statement_the_server_runs() {
    let grant =
        regex::Regex::new(r"GRANT ([A-Z, ]+)\n\s+ON ([^;]+?)\n\s+TO simplestchat_app;").unwrap();
    let mut granted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for found in grant.captures_iter(rendered(GRANTS)) {
        let rights: BTreeSet<String> = found[1]
            .split(',')
            .map(|right| right.trim().to_owned())
            .collect();
        for table in found[2].split(',') {
            let table = table.trim();
            let table = table.strip_prefix("public.").unwrap_or(table);
            granted.insert(table.to_owned(), rights.clone());
        }
    }
    let tables = "users|webauthn_credentials|sessions|rooms|room_roles|room_states|room_reports\
                  |moderation_events|invites|invite_redemptions|session_access_tokens";
    let statement = regex::Regex::new(&format!(
        r"\b(INSERT INTO|UPDATE|DELETE FROM)\s+({tables})\b"
    ))
    .unwrap();
    let mut needed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut files = Vec::new();
    sources(&super::ansible::root().join("src"), &mut files);
    assert!(!files.is_empty());
    for path in files {
        let production = production_rust(&std::fs::read_to_string(path).unwrap());
        for found in statement.captures_iter(&production) {
            let right = found[1].split(' ').next().unwrap().to_owned();
            needed.entry(found[2].to_owned()).or_default().insert(right);
        }
    }
    let rights = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        needed["webauthn_credentials"],
        rights(&["INSERT", "UPDATE", "DELETE"])
    );
    // Access tokens are issued and pruned, never rewritten.
    assert_eq!(
        needed["session_access_tokens"],
        rights(&["INSERT", "DELETE"])
    );
    assert!(
        !needed
            .get("users")
            .is_some_and(|rights| rights.contains("DELETE"))
    );
    for (table, rights) in &needed {
        let held = granted.get(table).cloned().unwrap_or_default();
        assert!(rights.is_subset(&held), "{table}");
    }
}
