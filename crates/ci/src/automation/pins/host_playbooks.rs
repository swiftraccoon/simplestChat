//! The host hardening and proxy configuration playbooks, carried over from
//! the Python playbook tests. Each pin reads the playbook offline; nothing is
//! contacted.

use super::{checkout_file, document, first, located};
use serde_json::{Value, json};

/// The single play of one maintained playbook.
fn play(name: &str) -> Value {
    let document = document(&format!("ops/ansible/{name}"));
    let [play] = &document.as_array().unwrap()[..] else {
        panic!("{name}: one play");
    };
    play.clone()
}

/// One section of a play: a list whose every task is a mapping.
fn section<'a>(play: &'a Value, name: &str) -> &'a [Value] {
    let tasks = play[name].as_array().unwrap();
    assert!(tasks.iter().all(Value::is_object), "{name}");
    tasks
}

/// A list whose every item must be text.
fn strings(value: &Value) -> Vec<&str> {
    let items = value.as_array().unwrap().iter();
    items.map(|item| item.as_str().unwrap()).collect()
}

/// The position of the one task with exactly this name.
fn named(tasks: &[Value], name: &str) -> usize {
    let matches = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task["name"] == name);
    let [(position, _)] = matches.collect::<Vec<_>>()[..] else {
        panic!("exactly one task named {name:?}");
    };
    position
}

/// From `test_sshd_drop_in_sorts_first_and_is_verified_before_the_reload`:
/// the drop-in precedes provider files, and `sshd -T` is asserted before any
/// reload. Passwords are refused only after a proved key login.
#[test]
fn sshd_drop_in_sorts_first_and_is_verified_before_the_reload() {
    let hardening = play("host-hardening.yml");
    assert_eq!(hardening["hosts"], "benchmark_hosts");
    assert_eq!(hardening["become"], true);
    assert_eq!(hardening["serial"], 1);
    let dropin_path = hardening["vars"]["schost_sshd_dropin"].as_str().unwrap();
    assert!(dropin_path.starts_with("/etc/ssh/sshd_config.d/00-"));
    let guards = section(&hardening, "pre_tasks");
    let enabled = strings(&guards[0]["ansible.builtin.assert"]["that"]);
    assert!(enabled.contains(&"scpub_enabled | bool"));
    let proof = &guards[guards.len() - 1];
    assert_eq!(proof["delegate_to"], "localhost");
    assert_eq!(proof["become"], false);
    assert_eq!(proof["changed_when"], false);
    let login = &guards[guards.len() - 2]["ansible.builtin.set_fact"]["schost_login_proof"];
    let login = login.as_str().unwrap();
    for option in [
        "BatchMode=yes",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
    ] {
        assert!(login.contains(option), "{option}");
    }
    let tasks = section(&hardening, "tasks");
    assert!(tasks.iter().all(|task| task["name"].is_string()));
    let dropin = &tasks[0];
    let content = dropin["ansible.builtin.copy"]["content"].as_str().unwrap();
    assert_eq!(dropin["ansible.builtin.copy"]["mode"], "0600");
    let lines = content.lines().collect::<Vec<_>>();
    for line in [
        "PasswordAuthentication no",
        "KbdInteractiveAuthentication no",
        "PermitRootLogin {{ schost_permit_root_login }}",
        "PubkeyAuthentication yes",
        "AuthenticationMethods publickey",
        "MaxAuthTries 3",
        "LoginGraceTime 20",
        "AllowAgentForwarding no",
        "X11Forwarding no",
    ] {
        assert!(lines.contains(&line), "{line}");
    }
    assert!(content.contains("AllowUsers {{ schost_allow_users | join(' ') }}"));
    assert_eq!(
        tasks[1]["ansible.builtin.command"]["argv"],
        json!(["/usr/sbin/sshd", "-T"])
    );
    let effective = strings(&tasks[2]["ansible.builtin.assert"]["that"]);
    for keyword in [
        "'passwordauthentication no' in",
        "permitrootlogin",
        "authenticationmethods publickey",
        "maxauthtries 3",
        "allowagentforwarding no",
        "x11forwarding no",
        "allowusers ",
    ] {
        assert!(
            effective.iter().any(|rule| rule.contains(keyword)),
            "{keyword}"
        );
    }
    assert_eq!(dropin["notify"], "Reload the SSH daemon");
    // The reload runs right after the assertion, and a fresh login is proved
    // after the reload.
    assert_eq!(tasks[3]["ansible.builtin.meta"], "flush_handlers");
    assert_eq!(tasks[4]["delegate_to"], "localhost");
    assert!(
        named(
            tasks,
            "Reload the SSH daemon now; sessions already open stay connected"
        ) < named(
            tasks,
            "Prove that key-only login still works after the reload"
        )
    );
    let handlers = section(&hardening, "handlers");
    let reload = first(handlers, |handler| {
        handler["name"] == "Reload the SSH daemon"
    });
    let service = &reload["ansible.builtin.systemd_service"];
    assert_eq!(service["state"], "reloaded");
    assert_eq!(service["name"], "ssh.service");
    for task in tasks {
        assert!(task.get("ansible.builtin.apt").is_none());
        assert!(task.get("ansible.builtin.lineinfile").is_none());
    }
}

/// From `test_resolver_drop_in_closes_link_local_listeners_and_is_verified`:
/// LLMNR and mDNS are disabled through a drop-in, applied by a restart and
/// verified.
#[test]
fn resolver_drop_in_closes_link_local_listeners_and_is_verified() {
    let hardening = play("host-hardening.yml");
    let tasks = section(&hardening, "tasks");
    let (position, resolved) = located(tasks, |task| {
        task["name"]
            .as_str()
            .unwrap()
            .starts_with("Stop answering LLMNR")
    });
    let content = resolved["ansible.builtin.copy"]["content"]
        .as_str()
        .unwrap();
    let lines = content.lines().collect::<Vec<_>>();
    assert!(lines.contains(&"LLMNR=no"));
    assert!(lines.contains(&"MulticastDNS=no"));
    assert_eq!(resolved["notify"], "Restart the stub resolver");
    assert_eq!(
        tasks[position + 1]["ansible.builtin.meta"],
        "flush_handlers"
    );
    let restart = first(section(&hardening, "handlers"), |handler| {
        handler["name"] == "Restart the stub resolver"
    });
    let service = &restart["ansible.builtin.systemd_service"];
    assert_eq!(service["name"], "systemd-resolved.service");
    assert_eq!(service["state"], "restarted");
    let verification = strings(&tasks[tasks.len() - 1]["ansible.builtin.assert"]["that"]);
    assert!(verification.contains(&"'+LLMNR' not in schost_resolver.stdout"));
    assert!(verification.contains(&"'+mDNS' not in schost_resolver.stdout"));
}

/// From `test_candidate_is_validated_before_the_in_place_write_and_reload`:
/// validation precedes the in-place write, a stale mount restarts, and the
/// reload precedes the probe.
#[test]
fn candidate_is_validated_before_the_in_place_write_and_reload() {
    let proxy = play("proxy-config.yml");
    assert_eq!(proxy["hosts"], "benchmark_hosts");
    assert_eq!(proxy["become"], true);
    let guards = section(&proxy, "pre_tasks");
    let rules = strings(&guards[0]["ansible.builtin.assert"]["that"]);
    assert!(rules.contains(&"scpub_enabled | bool"));
    assert!(
        rules
            .iter()
            .any(|rule| rule.contains("scpub_caddy_image is match"))
    );
    let tasks = section(&proxy, "tasks");
    assert!(tasks.iter().all(|task| task["name"].is_string()));
    let validate = &tasks[1];
    let argv = strings(&validate["ansible.builtin.command"]["argv"]);
    assert_eq!(
        argv[argv.len() - 6..],
        [
            "caddy",
            "validate",
            "--config",
            "/etc/caddy/Caddyfile",
            "--adapter",
            "caddyfile"
        ]
    );
    let after = |flag: &str| argv[argv.iter().position(|item| *item == flag).unwrap() + 1];
    assert!(after("--network").contains("none"));
    assert_eq!(after("--cap-add"), "NET_BIND_SERVICE");
    assert!(argv.contains(&"{{ scproxy_candidate }}:/etc/caddy/Caddyfile:ro"));
    assert_eq!(validate["changed_when"], false);
    // An in-place write keeps the mounted inode.
    let write = &tasks[3];
    let write_argv = strings(&write["ansible.builtin.command"]["argv"]);
    assert_eq!(
        write_argv[..2],
        ["cp", "--no-preserve=mode,ownership,timestamps"]
    );
    assert_eq!(
        write_argv[write_argv.len() - 1],
        "{{ scpub_config }}/Caddyfile"
    );
    assert_eq!(write["notify"], "Reload the running proxy");
    let (restarted, restart) = located(tasks, |task| {
        task["name"]
            .as_str()
            .unwrap()
            .starts_with("Restart the proxy once")
    });
    assert!(strings(&restart["ansible.builtin.command"]["argv"]).contains(&"restart"));
    let stale = restart["when"].as_str().unwrap();
    assert!(stale.contains("scproxy_mounted.stdout.split() | first !="));
    let reloaded = named(tasks, "Reload the running proxy now");
    assert!(restarted < reloaded);
    assert!(reloaded < named(tasks, "Read a response the proxy answers itself"));
    let reload = &section(&proxy, "handlers")[0];
    assert_eq!(reload["name"], "Reload the running proxy");
    assert!(strings(&reload["ansible.builtin.command"]["argv"]).contains(&"reload"));
    assert!(
        named(
            tasks,
            "Validate the candidate with the exact pinned proxy image, offline"
        ) < named(
            tasks,
            "Write the Caddyfile in place so the container's bind mount sees it"
        )
    );
    let probe = &tasks[tasks.len() - 2]["ansible.builtin.uri"];
    assert_eq!(probe["url"], "https://{{ scpub_domain }}/metrics");
    assert_eq!(probe["status_code"], json!([404]));
    let checks = strings(&tasks[tasks.len() - 1]["ansible.builtin.assert"]["that"]);
    assert!(
        checks.contains(&"scproxy_probe.content_security_policy | default('') == scproxy_policy")
    );
}

/// From `test_policy_extraction_matches_the_single_default_only_line`: the
/// playbook's expression finds exactly the Caddyfile's one default-only policy.
#[test]
fn policy_extraction_matches_the_single_default_only_line() {
    let caddyfile = checkout_file("Caddyfile");
    let policies = caddyfile
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("Content-Security-Policy") && !line.starts_with('#'));
    assert_eq!(policies.count(), 1);
    let policy = regex::Regex::new(r#"\?Content-Security-Policy "([^"]+)""#).unwrap();
    let found = policy.captures(&caddyfile).unwrap();
    assert!(found[1].starts_with("default-src 'self'; "));
    assert!(!found[1].contains("upgrade-insecure-requests"));
}
