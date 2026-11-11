//! Host sizing in the deployment's group variables: the capacity expressions
//! against the answers recorded from the retired Python model, the defaults a
//! host's facts produce, and the bounds both configuration playbooks enforce.
//! Ansible resolves every value from the real variable file.

use super::ansible::{asserted, evaluate_in};
use super::{document, located};
use serde_json::{Value, json};

const GROUP: &str = "ops/ansible/group_vars/benchmark_hosts.yml";
const GUARD: &str = "Require sizing the capacity model and the server accept";

/// The answers the retired Python model gave, recorded before its removal.
fn recorded() -> Value {
    serde_json::from_str(include_str!("sizing_recorded_reference.json")).unwrap()
}

/// Each named group variable as text, once per case of facts and overrides.
fn resolve(names: &[&str], cases: &[Value]) -> Vec<Vec<String>> {
    let expressions = names.iter().map(|name| format!("{name} | string"));
    let expressions = expressions.collect::<Vec<_>>();
    let expressions = expressions.iter().map(String::as_str).collect::<Vec<_>>();
    let text = |value: Value| value.as_str().unwrap().to_owned();
    let rows = evaluate_in(&[GROUP], &expressions, cases);
    let rows = rows.into_iter();
    rows.map(|row| row.into_iter().map(text).collect())
        .collect()
}

/// The default-route facts of a host with both families on `eth0`. Ansible
/// always defines both facts, as empty objects without a route.
fn dual_stack() -> Value {
    json!({
        "ansible_default_ipv4": {"address": "192.0.2.10", "interface": "eth0"},
        "ansible_default_ipv6": {"address": "2001:db8::10", "interface": "eth0"},
    })
}

/// The default-route facts of an IPv4-only host on `ens3`.
fn four_only() -> Value {
    json!({
        "ansible_default_ipv4": {"address": "192.0.2.10", "interface": "ens3"},
        "ansible_default_ipv6": {},
    })
}

/// The default-route facts of an IPv6-only host on `eth0`.
fn six_only() -> Value {
    json!({
        "ansible_default_ipv4": {},
        "ansible_default_ipv6": {"address": "2001:db8::10", "interface": "eth0"},
    })
}

/// One host: its processor and memory facts, its routes and its inventory.
fn host(vcpus: u64, memory_mib: u64, routes: Value, inventory: Value) -> Value {
    let mut variables = json!({
        "ansible_processor_vcpus": vcpus,
        "ansible_memtotal_mb": memory_mib,
    });
    let variables_map = variables.as_object_mut().unwrap();
    variables_map.extend(routes.as_object().unwrap().clone());
    variables_map.extend(inventory.as_object().unwrap().clone());
    variables
}

/// The sizing guard in one section of a playbook: its position, the task and
/// the section's tasks.
fn guard(playbook: &str, section: &str) -> (usize, Value, Vec<Value>) {
    let play = document(playbook)[0].clone();
    let tasks = play[section].as_array().unwrap().clone();
    let (position, task) = located(&tasks, |task| task["name"] == GUARD);
    (position, task.clone(), tasks)
}

/// The group variables size the app from its workers, memory limit, port and
/// viewers per worker exactly as the Python model's `managed_limits` did, in
/// every limiting case: CPU, memory, network, the floors and the room cap.
#[test]
fn capacity_expressions_match_the_recorded_python_model() {
    let recorded = recorded();
    let rows = recorded["matrix"].as_array().unwrap();
    assert_eq!(rows.len(), 192);
    let case = |row: &Value| {
        json!({
            "scpub_media_workers": row[0][0],
            "scpub_app_memory_mib": row[0][1],
            "scpub_port_mbps": row[0][2],
            "scpub_webinar_viewers_per_worker": row[0][3],
        })
    };
    let names = [
        "scpub_max_connections",
        "scpub_max_rooms",
        "scpub_max_participants_per_room",
    ];
    let derived = resolve(&names, &rows.iter().map(case).collect::<Vec<_>>());
    for (row, derived) in rows.iter().zip(derived) {
        let derived = derived.iter().map(|value| value.parse::<u64>().unwrap());
        assert_eq!(json!(derived.collect::<Vec<_>>()), row[1], "{}", row[0]);
    }
}

/// From `test_sizing_defaults_follow_the_host_and_yield_to_the_inventory`.
#[test]
fn sizing_defaults_follow_the_host_and_yield_to_the_inventory() {
    let names = [
        "scpub_app_cpus",
        "scpub_media_workers",
        "scpub_app_memory_mib",
        "scpub_max_connections",
        "scpub_max_rooms",
        "scpub_max_participants_per_room",
        "scpub_reserved_memory_mib",
        "scpub_postgres_memory_mib",
        "scpub_postgres_cpus",
        "scpub_postgres_shared_buffers_mib",
        "scpub_caddy_memory_mib",
        "scpub_caddy_cpus",
        "scpub_turn_bps_capacity",
        "scpub_turn_total_quota",
        "scpub_turn_relay_port_max",
    ];
    // Overrides carry through: a pinned worker count, a bigger reserve, a
    // slower port.
    let pinned = json!({
        "scpub_media_workers": 2,
        "scpub_reserved_cpus": 2,
        "scpub_port_mbps": 100,
    });
    let hosts = [
        host(4, 11967, dual_stack(), json!({})),
        host(16, 65536, six_only(), json!({})),
        host(1, 1024, four_only(), json!({})),
        host(4, 11967, dual_stack(), pinned),
    ];
    let rows = resolve(&names, &hosts);
    let value = |row: usize, name: &str| {
        let column = names.iter().position(|known| *known == name).unwrap();
        rows[row][column].as_str()
    };
    let vps = |name: &str| value(0, name);
    assert_eq!(vps("scpub_app_cpus"), "3");
    assert_eq!(vps("scpub_media_workers"), "3");
    assert_eq!(vps("scpub_app_memory_mib"), (11967 - 2991).to_string());
    assert_eq!(vps("scpub_max_connections"), "525");
    assert_eq!(vps("scpub_max_rooms"), "525");
    assert_eq!(vps("scpub_max_participants_per_room"), "525");
    assert_eq!(vps("scpub_reserved_memory_mib"), "2991");
    assert_eq!(vps("scpub_postgres_memory_mib"), "1495");
    assert_eq!(vps("scpub_postgres_cpus"), "1");
    assert_eq!(vps("scpub_postgres_shared_buffers_mib"), "373");
    assert_eq!(vps("scpub_caddy_memory_mib"), "373");
    assert_eq!(vps("scpub_caddy_cpus"), "0.5");
    assert_eq!(vps("scpub_turn_bps_capacity"), "62500000");
    assert_eq!(vps("scpub_turn_total_quota"), "525");
    assert_eq!(vps("scpub_turn_relay_port_max"), "50209");
    let large = |name: &str| value(1, name);
    assert_eq!(large("scpub_app_cpus"), "15");
    assert_eq!(large("scpub_media_workers"), "15");
    assert_eq!(large("scpub_app_memory_mib"), (65536 - 16384).to_string());
    assert_eq!(large("scpub_max_connections"), "800");
    assert_eq!(large("scpub_postgres_memory_mib"), "4096");
    assert_eq!(large("scpub_caddy_memory_mib"), "2048");
    assert_eq!(large("scpub_turn_relay_port_max"), "50759");
    let small = |name: &str| value(2, name);
    assert_eq!(small("scpub_app_cpus"), "1");
    assert_eq!(small("scpub_media_workers"), "1");
    assert_eq!(small("scpub_app_memory_mib"), "512");
    assert_eq!(small("scpub_max_connections"), "175");
    assert_eq!(small("scpub_max_rooms"), "175");
    assert_eq!(small("scpub_postgres_memory_mib"), "1024");
    assert_eq!(small("scpub_caddy_memory_mib"), "256");
    let pinned = |name: &str| value(3, name);
    assert_eq!(pinned("scpub_app_cpus"), "2");
    assert_eq!(pinned("scpub_media_workers"), "2");
    assert_eq!(pinned("scpub_max_connections"), "80");
    assert_eq!(pinned("scpub_postgres_cpus"), "2");
    assert_eq!(pinned("scpub_caddy_cpus"), "1.0");
    assert_eq!(pinned("scpub_turn_bps_capacity"), "6250000");
}

/// From `test_managed_and_adviser_limits_agree_across_host_shapes`: both
/// setup paths obey memory, bandwidth and runtime limits using one model. The
/// adviser's settings for each host are the recorded ones.
#[test]
fn managed_and_adviser_limits_agree_across_host_shapes() {
    let recorded = recorded();
    let shapes = recorded["adviser"].as_array().unwrap();
    assert_eq!(shapes.len(), 6);
    let case = |shape: &Value| {
        let (vcpus, memory) = (shape["host"][0].as_u64(), shape["host"][1].as_u64());
        let inventory = json!({"scpub_port_mbps": shape["host"][2]});
        host(vcpus.unwrap(), memory.unwrap(), four_only(), inventory)
    };
    let names = [
        "scpub_media_workers",
        "scpub_max_connections",
        "scpub_max_participants_per_room",
        "scpub_turn_bps_capacity",
        "scpub_turn_max_bps",
    ];
    let derived = resolve(&names, &shapes.iter().map(case).collect::<Vec<_>>());
    for (shape, managed) in shapes.iter().zip(derived) {
        let settings = &shape["settings"];
        assert_eq!(json!(managed[0]), settings["MEDIA_WORKERS"], "{shape}");
        assert_eq!(json!(managed[1]), settings["MAX_CONNECTIONS"], "{shape}");
        assert_eq!(
            json!(managed[2]),
            settings["MAX_PARTICIPANTS_PER_ROOM"],
            "{shape}"
        );
        let number = |text: &String| text.parse::<u64>().unwrap();
        assert!(number(&managed[0]) <= 64);
        assert!(number(&managed[2]) <= 10000);
        // coturn accepts bytes/second; its aggregate cap is half the sold port.
        let port = shape["host"][2].as_u64().unwrap();
        assert_eq!(number(&managed[3]) * 8, port * 500_000);
        assert_eq!(number(&managed[4]) * 8, 4_000_000);
    }
}

/// From `test_announced_addresses_follow_the_default_routes`: a dual-stack
/// host announces both families, a single-stack host its one; overrides win.
#[test]
fn announced_addresses_follow_the_default_routes() {
    let names = [
        "scpub_announce_ip",
        "scpub_announce_ipv6",
        "scpub_transfer_interface",
    ];
    let hosts = [
        host(4, 11967, dual_stack(), json!({})),
        host(4, 11967, six_only(), json!({})),
        host(4, 11967, four_only(), json!({})),
        host(4, 11967, dual_stack(), json!({"scpub_announce_ipv6": ""})),
    ];
    let rows = resolve(&names, &hosts);
    assert_eq!(rows[0], ["192.0.2.10", "2001:db8::10", "eth0"]);
    assert_eq!(rows[1][..2], ["2001:db8::10", ""]);
    assert_eq!(rows[2][1..], ["", "ens3"]);
    assert_eq!(rows[3][1], "");
}

/// From `test_passkey_identity_can_remain_at_the_parent_without_widening_origins`:
/// the relying-party identity defaults to the domain, and an inventory keeps
/// an existing parent identity.
#[test]
fn passkey_identity_defaults_to_the_domain_and_yields_to_the_inventory() {
    let domain = json!({"scpub_domain": "the.research.clinic"});
    let kept = json!({
        "scpub_domain": "the.research.clinic",
        "scpub_webauthn_rp_id": "research.clinic",
    });
    let hosts = [
        host(4, 11967, dual_stack(), domain),
        host(4, 11967, dual_stack(), kept),
    ];
    let rows = resolve(&["scpub_webauthn_rp_id"], &hosts);
    assert_eq!(rows[0], ["the.research.clinic"]);
    assert_eq!(rows[1], ["research.clinic"]);
}

/// The Python model refused more than 64 workers, less than 512 MiB, a port
/// that is not positive and viewers per worker outside 1 to 10000. Both
/// playbooks that render the application's environment refuse the same
/// inventory before they render it.
#[test]
fn both_configuration_playbooks_refuse_sizing_the_model_refused() {
    let (guarded, public, tasks) = guard("ops/ansible/public.yml", "pre_tasks");
    let (stopped, _) = located(&tasks, |task| task["register"] == "scpub_active_containers");
    assert!(guarded < stopped);
    let (guarded, maintenance, tasks) = guard("ops/ansible/maintenance.yml", "tasks");
    let (rendered, _) = located(&tasks, |task| {
        task["ansible.builtin.template"]["src"] == "public-app.env.j2"
    });
    assert!(guarded < rendered);
    assert_eq!(public, maintenance);
    assert!(public.get("when").is_none());
    assert!(public.get("ignore_errors").is_none());
    let sizing = |workers: i64, memory: i64, port: f64, viewers: i64| {
        json!({
            "scpub_media_workers": workers,
            "scpub_app_memory_mib": memory,
            "scpub_port_mbps": port,
            "scpub_webinar_viewers_per_worker": viewers,
        })
    };
    let cases = [
        (sizing(3, 8976, 1000.0, 175), true),
        (sizing(1, 512, 0.125, 1), true),
        (sizing(64, 262_144, 40000.0, 10000), true),
        (sizing(65, 4096, 1000.0, 175), false),
        (sizing(0, 4096, 1000.0, 175), false),
        (sizing(1, 1, 1000.0, 175), false),
        (sizing(1, 511, 1000.0, 175), false),
        (sizing(1, 4096, 0.0, 175), false),
        (sizing(1, 4096, -1.0, 175), false),
        (sizing(1, 4096, 1000.0, 0), false),
        (sizing(1, 4096, 1000.0, 10001), false),
    ];
    let variables = cases.iter().map(|(case, _)| case.clone());
    let rules = &public["ansible.builtin.assert"]["that"];
    let verdicts = asserted(rules, &variables.collect::<Vec<_>>());
    for (verdict, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(verdict, expected, "{case}");
    }
}
