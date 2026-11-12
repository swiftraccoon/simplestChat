//! The relay playbook, its certificate timer and the relay's templates.
//! Certificate preparation, refresh and activation run through the
//! installed Ops executable, on a host a signed release has delivered it
//! to. Nothing connects to a host.

use super::ansible::{asserted, render_with};
use super::checkout_file;
use super::monitoring::{
    LOOK, OPS, REQUIRE, addressed, environment, named, names, play, requirement, section,
};
use super::release_playbook::base64;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const TURN: &str = "ops/ansible/turn.yml";
const UNIT: &str = "ops/ansible/templates/turn-certificate.service.j2";
const PREPARATION: &str = "Prepare the separately owned relay without advertising it to clients";
const GUARD: &str = "Require the explicitly enabled relay on a prepared public host";
const SELECTED: &str = "Read the selected relay identity before activation";
const IDENTITY: &str = "Require the selected relay identity before activation";
const ACTIVATION: &str = "Activate the externally verified relay with bounded application rollback";
const IMAGE: &str = "docker.io/coturn/coturn:4.18.0@sha256:";

/// A 4-vCPU host with a gigabit port, as the relay's templates and the
/// application's environment are rendered for it.
fn host() -> Value {
    let hexadecimal = |index: u8| format!("{index:064x}");
    json!({
        "scpub_domain": "chat.example.test",
        "scpub_webauthn_rp_id": "chat.example.test",
        "scpub_announce_ip": "192.0.2.10",
        "scpub_announce_ipv6": "2001:db8::10",
        "scpub_server_image": format!("sha256:{}", "a".repeat(64)),
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
        "scpub_turn_image": "pinned-fixture",
        "scpub_turn_memory_mib": 512,
        "scpub_turn_cpus": 1,
        "scpub_turn_bps_capacity": 62_500_000,
        "scpub_turn_max_bps": 500_000,
        "scpub_turn_user_quota": 4,
        "scpub_turn_total_quota": 525,
        "scpub_turn_relay_threads": 1,
        "scpub_turn_relay_port_min": 49160,
        "scpub_turn_relay_port_max": 50209,
        "scpub_turn_secret": "a".repeat(64),
        "scpub_secrets": {
            "database_password": hexadecimal(3),
            "metrics_token": hexadecimal(4),
            "proxy_secret": hexadecimal(5),
        },
    })
}

/// The assignments of a rendered environment file, values verbatim.
fn assignments(text: &str) -> BTreeMap<&str, &str> {
    let lines = text.lines().filter(|line| !line.starts_with('#'));
    lines.filter_map(|line| line.split_once('=')).collect()
}

/// Every guard runs whatever tags are selected: the explicit opt-in, the
/// executable a signed release delivered, the host verified through it, and
/// the activation's own identity check.
#[test]
fn relay_steps_need_the_signed_ops_executable_and_a_verified_host() {
    let play = play(TURN);
    assert_eq!(play["serial"], 1);
    let pre_tasks = section(&play, "pre_tasks");
    assert_eq!(
        names(pre_tasks),
        [
            GUARD,
            LOOK,
            REQUIRE,
            "Verify the prepared public host before any relay step",
            SELECTED,
            IDENTITY,
        ]
    );
    requirement(pre_tasks, "scpub_turn_ops");
    for task in pre_tasks {
        assert_eq!(task["tags"], json!(["always"]), "{}", task["name"]);
    }
    let preflight = &pre_tasks[3];
    assert_eq!(
        preflight["ansible.builtin.command"],
        json!({"argv": [OPS, "release", "preflight"]})
    );
    assert_eq!(preflight["environment"], environment());
    assert_eq!(preflight["changed_when"], false);
    assert_eq!(preflight["check_mode"], false);
    for softened in ["when", "failed_when", "ignore_errors"] {
        assert!(preflight.get(softened).is_none(), "{softened}");
    }
    // An activation is of the relay this inventory names: the hostname and
    // the pinned image recorded when it was prepared.
    let selected = named(pre_tasks, SELECTED);
    assert_eq!(
        selected["ansible.builtin.slurp"],
        json!({"src": "/etc/simplestchat-turn/settings.json"})
    );
    assert_eq!(selected["register"], "scpub_turn_selected");
    let identity = named(pre_tasks, IDENTITY);
    for task in [selected, identity] {
        assert_eq!(task["when"], "scpub_turn_activate | bool");
        assert!(task.get("ignore_errors").is_none() && task.get("failed_when").is_none());
    }
    let image = format!("{IMAGE}{}", "b".repeat(64));
    let recorded = |record: Value| {
        let content = base64(record.to_string().as_bytes());
        json!({
            "scpub_domain": "chat.example.test",
            "scpub_turn_image": image,
            "scpub_turn_selected": {"content": content},
        })
    };
    let cases = [
        recorded(json!({"domain": "chat.example.test", "image": image})),
        recorded(
            json!({"domain": "chat.example.test", "image": image, "configurationSha256": "c"}),
        ),
        recorded(json!({"domain": "other.example.test", "image": image})),
        recorded(
            json!({"domain": "chat.example.test", "image": format!("{IMAGE}{}", "c".repeat(64))}),
        ),
        recorded(json!({"domain": "chat.example.test"})),
        recorded(json!({"image": image})),
        recorded(json!([])),
    ];
    let rules = &identity["ansible.builtin.assert"]["that"];
    assert_eq!(
        asserted(rules, &cases),
        [true, true, false, false, false, false, false]
    );
}

/// An IPv6-only host is refused before any relay step: the relay's primary
/// address is a public IPv4 one, and a second IPv6 listener is optional.
#[test]
fn an_ipv6_only_host_is_refused_before_any_relay_step() {
    let play = play(TURN);
    let guard = named(section(&play, "pre_tasks"), GUARD);
    assert!(guard.get("when").is_none());
    let assertion = &guard["ansible.builtin.assert"];
    assert!(
        assertion["fail_msg"]
            .as_str()
            .unwrap()
            .contains("IPv6-only")
    );
    let rules = assertion["that"].as_array().unwrap();
    let (facts, rules): (Vec<&Value>, Vec<&Value>) = rules
        .iter()
        .partition(|rule| rule.as_str().unwrap().starts_with("ansible_facts."));
    assert_eq!(
        json!(facts),
        json!([
            "ansible_facts.distribution == 'Debian'",
            "ansible_facts.distribution_major_version == '13'",
            "ansible_facts.architecture == 'x86_64'",
        ])
    );
    let with = |changes: &[(&str, Value)]| {
        let mut values = json!({
            "scpub_enabled": true,
            "scpub_turn_enabled": true,
            "scpub_config": "/etc/simplestchat-public",
            "scpub_root": "/srv/simplestchat-public",
            "scpub_domain": "chat.example.test",
            "scpub_announce_ip": "203.0.113.5",
            "scpub_announce_ipv6": "",
            "scpub_turn_image": format!("{IMAGE}{}", "b".repeat(64)),
        });
        for (name, value) in changes {
            values[*name] = value.clone();
        }
        values
    };
    let cases = [
        (with(&[]), true),
        (with(&[("scpub_announce_ipv6", json!("2001:db8::5"))]), true),
        (with(&[("scpub_announce_ip", json!("2001:db8::5"))]), false),
        (with(&[("scpub_announce_ip", json!(""))]), false),
        (
            with(&[("scpub_announce_ipv6", json!("2001:DB8::5"))]),
            false,
        ),
        (with(&[("scpub_turn_enabled", json!(false))]), false),
        (with(&[("scpub_enabled", json!(false))]), false),
        (with(&[("scpub_domain", json!("Chat.example.test"))]), false),
        (
            with(&[("scpub_turn_image", json!("docker.io/coturn/coturn:4.18.0"))]),
            false,
        ),
        (with(&[("scpub_config", json!("/etc/other"))]), false),
        (with(&[("scpub_root", json!("/srv/other"))]), false),
    ];
    let (values, expected): (Vec<Value>, Vec<bool>) = cases.into_iter().unzip();
    assert_eq!(asserted(&json!(rules), &values), expected);
}

/// Activation is one bounded unit of the installed executable: it refreshes
/// the certificate and then restarts the application as the release it
/// already runs. The unit outlives the refresh and the whole replacement,
/// and the playbook step outlives the unit and its stop. The playbook
/// installs no helper module and its activation runs no interpreter.
#[test]
fn the_relay_is_activated_by_the_installed_ops_executable_in_one_bounded_unit() {
    let play = play(TURN);
    let tasks = section(&play, "tasks");
    assert_eq!(names(tasks), [PREPARATION, ACTIVATION]);
    let activation = named(tasks, ACTIVATION);
    assert_eq!(activation["when"], "scpub_turn_activate | bool");
    let unit = [
        "systemd-run",
        "--wait",
        "--collect",
        "--unit=simplestchat-turn-activate",
        "--property=Type=exec",
        "--property=RuntimeMaxSec=1980",
        "--property=TimeoutStopSec=240",
        OPS,
        "turn",
        "activate",
    ];
    let mut overridden = unit.to_vec();
    overridden.extend(["--verify-address", "192.0.2.45"]);
    assert_eq!(
        addressed(activation),
        [json!(unit), json!(unit), json!(overridden)]
    );
    assert_eq!(activation["environment"], environment());
    assert_eq!(activation["changed_when"], true);
    // 1980 s of unit, 240 s for it to stop, and a minute to report.
    assert_eq!(activation["timeout"], 2280);
    for softened in ["failed_when", "ignore_errors", "ignore_unreachable"] {
        assert!(activation.get(softened).is_none(), "{softened}");
    }
    let text = checkout_file(TURN);
    for absent in [".py", "libexec/simplestchat-public/{{", "helpers"] {
        assert!(!text.contains(absent), "{absent}");
    }
    // Two preparation steps still run a script of their own on the host:
    // the public IPv4 listener check and the generation of the secret.
    assert_eq!(text.matches("/usr/bin/python3").count(), 2);
    let preparation = serde_json::to_string(named(tasks, PREPARATION)).unwrap();
    assert_eq!(preparation.matches("/usr/bin/python3").count(), 2);
}

/// The relay is a project of its own: one service of the pinned image, run
/// as its own user from a read-only root, with no capability but the one
/// its executable carries, and with read-only mounts.
#[test]
fn the_relay_project_has_no_privileged_network_or_filesystem_access() {
    let rendered = render_with(&["turn-compose.yml.j2"], &host());
    let project: Value = serde_saphyr::from_str(&rendered["turn-compose.yml.j2"]).unwrap();
    assert_eq!(project["name"], "simplestchat-turn");
    let services = project["services"].as_object().unwrap();
    assert_eq!(services.keys().collect::<Vec<_>>(), ["turn"]);
    let service = &services["turn"];
    assert_eq!(service["image"], "pinned-fixture");
    assert_eq!(service["user"], "10002:10002");
    assert_eq!(service["entrypoint"], json!(["/usr/bin/turnserver"]));
    assert_eq!(service["read_only"], true);
    assert_eq!(service["cap_drop"], json!(["ALL"]));
    assert_eq!(service["cap_add"], json!(["NET_BIND_SERVICE"]));
    assert_eq!(service["security_opt"], json!(["no-new-privileges:true"]));
    let volumes = service["volumes"].as_array().unwrap();
    assert_eq!(volumes.len(), 2);
    assert!(
        volumes
            .iter()
            .all(|volume| volume.as_str().unwrap().ends_with(":ro"))
    );
    assert_eq!(service["logging"]["driver"], "local");
    assert!(service.get("privileged").is_none() && service.get("devices").is_none());
}

/// The relay authenticates every allocation with the shared secret and
/// relays only to the selected media host, within its quotas; nothing
/// unauthenticated, administrative or older than TLS 1.2 is enabled.
#[test]
fn the_relay_authenticates_and_relays_only_to_the_selected_media_host() {
    let rendered = render_with(&["turnserver.conf.j2"], &host());
    let lines: Vec<&str> = rendered["turnserver.conf.j2"].lines().collect();
    let secret = format!("static-auth-secret={}", "a".repeat(64));
    for required in [
        "use-auth-secret",
        secret.as_str(),
        "no-stun",
        "no-tcp-relay",
        "no-multicast-peers",
        "denied-peer-ip=0.0.0.0-255.255.255.255",
        "denied-peer-ip=::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        "allowed-peer-ip=192.0.2.10",
        "listening-ip=192.0.2.10",
        "listening-ip=2001:db8::10",
        "relay-ip=192.0.2.10",
        "realm=chat.example.test",
        "min-port=49160",
        "max-port=50209",
        "user-quota=4",
        "total-quota=525",
        "max-bps=500000",
        "bps-capacity=62500000",
        "log-min-level=warning",
    ] {
        assert!(lines.contains(&required), "{required}");
    }
    for forbidden in [
        "no-auth",
        "allow-loopback-peers",
        "server-relay",
        "cli",
        "web-admin",
        "tlsv1",
        "tlsv1_1",
        "dtls",
        "prometheus",
    ] {
        assert!(!lines.contains(&forbidden), "{forbidden}");
    }
    let allowed = lines
        .iter()
        .filter(|line| line.starts_with("allowed-peer-ip="));
    assert_eq!(allowed.count(), 1);
}

/// A full maintenance render of the application's environment advertises
/// the relay in the same three lines an activation appends, and a host
/// without the relay has none of them.
#[test]
fn full_maintenance_renders_the_three_settings_an_activation_appends() {
    let name = "public-app.env.j2";
    let mut enabled = host();
    enabled["scpub_turn_enabled"] = json!(true);
    let rendered = render_with(&[name], &enabled);
    let told = [
        concat!(
            "TURN_URLS=turn:chat.example.test:3478?transport=udp,",
            "turn:chat.example.test:3478?transport=tcp,",
            "turns:chat.example.test:5349?transport=tcp"
        )
        .to_owned(),
        format!("TURN_SECRET={}", "a".repeat(64)),
        "TURN_TTL=86400".to_owned(),
    ];
    let lines: Vec<&str> = rendered[name].lines().collect();
    let relay: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.starts_with("TURN_"))
        .collect();
    assert_eq!(relay, told);
    assert!(rendered[name].ends_with(&format!("{}\n", told.join("\n"))));
    let without = render_with(&[name], &host());
    let settings = assignments(&without[name]);
    assert!(settings.keys().all(|name| !name.starts_with("TURN_")));
    assert_eq!(settings["RUN_MIGRATIONS"], "false");
}

/// The certificate is published before the image is pulled and the relay
/// started, and the relay must present it before the timer is installed.
/// Both steps are the installed executable with a fixed environment; the
/// address override reaches only the refresh.
#[test]
fn the_relay_certificate_is_prepared_and_verified_through_the_ops_executable() {
    let play = play(TURN);
    let preparation = named(section(&play, "tasks"), PREPARATION);
    assert_eq!(preparation["when"], "not scpub_turn_activate | bool");
    let block = section(preparation, "block");
    let order = names(block);
    let at = |name: &str| order.iter().position(|known| *known == name).unwrap();
    let prepare = "Prepare a verified copy of the current Caddy certificate";
    let pull = "Pull the checksum-pinned coturn image without building on the VPS";
    let start = "Start only the relay after configuration and certificate validation";
    let verify = "Verify trusted relay TLS before scheduling certificate maintenance";
    let timer = "Install hourly certificate maintenance";
    assert!(at(prepare) < at(pull) && at(pull) < at(start));
    assert!(at(start) < at(verify) && at(verify) < at(timer));
    let prepared = named(block, prepare);
    assert_eq!(
        prepared["ansible.builtin.command"],
        json!({"argv": [OPS, "turn", "prepare-certificate"]})
    );
    let verified = named(block, verify);
    assert_eq!(
        addressed(verified),
        [
            json!([OPS, "turn", "refresh-certificate"]),
            json!([OPS, "turn", "refresh-certificate"]),
            json!([
                OPS,
                "turn",
                "refresh-certificate",
                "--verify-address",
                "192.0.2.45"
            ]),
        ]
    );
    for step in [prepared, verified] {
        assert_eq!(step["environment"], environment(), "{}", step["name"]);
        assert_eq!(step["changed_when"], true, "{}", step["name"]);
        for softened in ["when", "failed_when", "ignore_errors", "ignore_unreachable"] {
            assert!(step.get(softened).is_none(), "{}", step["name"]);
        }
    }
}

/// The hourly unit runs the refresh as the installed executable, once, in
/// the sandbox it had: only the relay's configuration, the results and the
/// lock directory are writable.
#[test]
fn the_certificate_timer_runs_the_ops_executable_in_its_sandbox() {
    let text = checkout_file(UNIT);
    let lines: Vec<&str> = text.lines().collect();
    let execution = format!("ExecStart={OPS} turn refresh-certificate");
    assert!(lines.contains(&execution.as_str()));
    let executions = lines.iter().filter(|line| line.starts_with("Exec"));
    assert_eq!(executions.count(), 1);
    for kept in [
        "Type=oneshot",
        "Requires=docker.service",
        "UMask=0077",
        "TimeoutStartSec=60",
        "NoNewPrivileges=true",
        "PrivateTmp=true",
        "ProtectSystem=strict",
        "ProtectHome=true",
        concat!(
            "ReadWritePaths=/etc/simplestchat-turn ",
            "/srv/simplestchat-public/results /run/simplestchat-bench",
        ),
        "MemoryMax=128M",
        "TasksMax=32",
        "RestrictSUIDSGID=true",
        "LockPersonality=true",
    ] {
        assert!(lines.contains(&kept), "{kept}");
    }
    assert!(!text.contains("python") && !text.contains("{{") && !text.contains("{%"));
}
