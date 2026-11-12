//! The public deployment's ordering and syntax, carried over from the Python
//! deployment tests. The launcher is never executed.

use super::ansible::evaluate;
use super::{checkout_file, document, located};
use serde_json::{Value, json};
use std::process::Command;

const SCRIPT: &str = "ops/ansible/files/deploy-public.sh";
/// The launcher's comparison of the database's migration ledger with the
/// verified source checkout.
const LEDGER: &str =
    r#""$ops" release ledger --revision "$revision" --ledger "$attempt/migration-ledger.txt""#;

/// The single play of the public playbook.
fn public() -> Value {
    document("ops/ansible/public.yml")[0].clone()
}

/// One section of a play: a list whose every task is a mapping.
fn section<'a>(play: &'a Value, name: &str) -> &'a [Value] {
    let tasks = play[name].as_array().unwrap();
    assert!(tasks.iter().all(Value::is_object), "{name}");
    tasks
}

/// From `test_passkey_preflight_rejects_unrelated_and_malformed_domains`: the
/// real guard is evaluated by Ansible against exact, parent, deceptive-suffix
/// and invalid relying-party identifiers.
#[test]
fn passkey_preflight_rejects_unrelated_and_malformed_domains() {
    let play = public();
    let guards = section(&play, "pre_tasks");
    let (guarded, guard) = located(guards, |task| {
        task["name"] == "Require the passkey identity to belong to the public domain"
    });
    assert!(guard.get("when").is_none());
    assert!(guard.get("ignore_errors").is_none());
    let (stopped, _) = located(guards, |task| task["register"] == "scpub_active_containers");
    assert!(guarded < stopped);
    // Rules are joined with `and`, so a later rule is not evaluated once an
    // earlier one refuses, exactly as the assert module stops at the first.
    let rules = guard["ansible.builtin.assert"]["that"].as_array().unwrap();
    let rules = rules
        .iter()
        .map(|rule| format!("({})", rule.as_str().unwrap()));
    let accepted = rules.collect::<Vec<_>>().join(" and ");
    let domain = "the.research.clinic";
    let oversized = ["a".repeat(63).as_str(); 4].join(".");
    let nested = format!("nested.{domain}");
    let cases = [
        (domain, json!(domain), true),
        (domain, json!("research.clinic"), true),
        (nested.as_str(), json!("research.clinic"), true),
        (domain, json!("unrelated.clinic"), false),
        (domain, json!("search.clinic"), false),
        (domain, json!("other.research.clinic"), false),
        (domain, json!("research.clinic.attacker.test"), false),
        (domain, json!("clinic"), false),
        (domain, json!("https://research.clinic"), false),
        (domain, json!("research.clinic:443"), false),
        (domain, json!("research.clinic."), false),
        (domain, json!("research..clinic"), false),
        (domain, json!("-research.clinic"), false),
        (domain, json!("research-.clinic"), false),
        (domain, json!("research_clinic.test"), false),
        (domain, json!("research.clinic\n"), false),
        (domain, json!(""), false),
        (domain, Value::Null, false),
        (domain, json!(123), false),
        (oversized.as_str(), json!(oversized), false),
    ];
    let variables = cases
        .iter()
        .map(|(origin, rp_id, _)| json!({"scpub_domain": origin, "scpub_webauthn_rp_id": rp_id}));
    let rows = evaluate(&[&accepted], &variables.collect::<Vec<_>>());
    for (row, (origin, rp_id, expected)) in rows.iter().zip(&cases) {
        assert_eq!(row[0], *expected, "{origin} with {rp_id}");
    }
}

/// From `test_installed_shell_commands_parse_and_pass_shellcheck`.
#[test]
fn installed_shell_commands_parse_and_pass_shellcheck() {
    let root = super::ansible::root().join("ops/ansible/files");
    let scripts = ["deploy-public.sh", "public-compose.sh"].map(|name| root.join(name));
    for script in &scripts {
        let parsed = Command::new("bash").arg("-n").arg(script).output().unwrap();
        assert!(parsed.status.success(), "{}", script.display());
    }
    let checked = Command::new("shellcheck").args(&scripts).output().unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stdout)
    );
}

/// From `test_enrollment_is_private_and_inventory_policy_precedes_public_start`.
#[test]
fn enrollment_is_private_and_inventory_policy_precedes_public_start() {
    let script = checkout_file(SCRIPT);
    let at = |text: &str| script.find(text).unwrap();
    assert!(at("flock -n 9") < at("phase=proxy_validation"));
    assert!(script.contains("Stop the public application and proxy before maintenance."));
    assert!(at("REGISTRATION_ENABLED=true compose up") < at("phase=lobby"));
    assert!(at("phase=lobby") < at("phase=runtime_policy"));
    assert!(at("phase=runtime_policy") < at("phase=public_proxy"));
    assert!(script.contains("--profile maintenance stop caddy simplestchat migrate"));
    assert!(!script.contains("down --volumes"));
    assert!(!script.contains("docker system prune"));
}

/// The launcher judges the runtime profile of the resolved model and of the
/// image, and the database's migration ledger, through the same installed
/// executable: two receipts before anything is validated or started, and
/// the ledger exactly as the database printed it. It runs no interpreter of
/// its own.
#[test]
fn the_launcher_judges_profile_and_ledger_through_the_installed_ops_executable() {
    let script = checkout_file(SCRIPT);
    let lines: Vec<&str> = script.lines().collect();
    let position = |line: &str| {
        let found = lines.iter().position(|found| *found == line);
        found.unwrap_or_else(|| panic!("{line}"))
    };
    let compose = concat!(
        r#"compose --profile maintenance config --format json | timeout 15s "$ops" "#,
        r#"release profile compose --image-id "$server_image" >"$attempt/runtime-compose.json""#
    );
    let image = concat!(
        r#"docker_owned image inspect --format '{{json .Config}}' "$server_image" | "#,
        r#"timeout 15s "$ops" release profile image >"$attempt/runtime-image.json""#
    );
    // Both receipts are written before anything is validated or started.
    assert_eq!(position(image), position(compose) + 1);
    assert!(position(image) < position("phase=proxy_validation"));
    // The ledger is the one the database printed on the line before.
    let printed = lines[position(LEDGER) - 1];
    assert!(printed.ends_with(r#"ORDER BY version" >"$attempt/migration-ledger.txt""#));
    assert!(script.find("ops=/usr/local/libexec").unwrap() < script.find(compose).unwrap());
    assert_eq!(script.matches(r#""$ops" release "#).count(), 4);
    assert!(!script.contains("python"));
}

/// The lobby is seeded by the Ops executable a signed release delivers: the
/// launcher requires it, root-owned and not a link, before it takes the
/// workload lock or touches anything, runs its seeding command while only
/// the private backend is up, and accepts nothing but a passed report. The
/// playbook installs no seeding helper.
#[test]
fn the_lobby_is_seeded_by_the_installed_ops_executable() {
    let script = checkout_file(SCRIPT);
    let at = |text: &str| script.find(text).unwrap();
    let lines: Vec<&str> = script.lines().collect();
    let installed = "ops=/usr/local/libexec/simplestchat-public/simplestchat-ops";
    let required = concat!(
        r#"[[ -f "$ops" && ! -L "$ops" && $(stat -c '%u:%a' "$ops") == 0:755 ]] || {"#,
        "\n  echo 'Deliver the signed Ops executable first: stage a signed release, ",
        "then deploy again.' >&2; exit 1;\n}\n"
    );
    assert_eq!(lines.iter().filter(|line| **line == installed).count(), 1);
    assert!(at(installed) < at(required) && at(required) < at("exec 9<>"));
    assert!(at(required) < at("mkdir -m 700 /run/simplestchat-bench"));
    let seeding = concat!(
        r#"timeout --signal=TERM --kill-after=2s 90s "$ops" release seed-lobby "#,
        r#">"$attempt/lobby.json""#
    );
    let step = lines.iter().position(|line| *line == seeding).unwrap();
    assert_eq!(lines[step - 1], "phase=lobby");
    let accepted = r#"jq -e '.passed == true' "$attempt/lobby.json" >/dev/null"#;
    assert_eq!(lines[step + 1], accepted);
    assert_eq!(
        script.matches("seed").count(),
        2,
        "one comment and one command"
    );
    let play = public();
    let (_, install) = located(section(&play, "tasks"), |task| {
        task["name"] == "Install the explicit deployment and Compose commands"
    });
    let installs = json!([
        {"source": "public-compose.sh", "destination": "/usr/local/bin/simplestchat-public"},
        {"source": "deploy-public.sh", "destination": "/usr/local/bin/simplestchat-public-deploy"},
    ]);
    assert_eq!(install["loop"], installs);
    let text = play.to_string();
    for retired in ["seed", "simplestchat-bench/release_json.py"] {
        assert!(!text.contains(retired), "{retired}");
    }
}

/// From `test_verified_artifacts_and_migrations_precede_runtime_privileges`.
#[test]
fn verified_artifacts_and_migrations_precede_runtime_privileges() {
    let script = checkout_file(SCRIPT);
    let at = |text: &str| script.find(text).unwrap();
    for text in [
        "git -C \"$source_root\" status --porcelain --untracked-files=all",
        "org.opencontainers.image.revision",
        "--cap-drop ALL --cap-add NET_BIND_SERVICE",
        LEDGER,
        "has_table_privilege('simplestchat_app','public._sqlx_migrations'",
    ] {
        assert!(script.contains(text), "{text}");
    }
    for proof in ["runtime-compose.json", "runtime-image.json"] {
        assert!(at(proof) < at("phase=proxy_validation"), "{proof}");
    }
    assert!(at(LEDGER) < at("\"$config/runtime-grants.sql\""));
    assert!(at(".ExitCode == 0") < at("docker_owned rm \"$migration_id\""));
}

/// From `test_preparation_requires_opt_in_and_does_not_start_services`.
#[test]
fn preparation_requires_opt_in_and_does_not_start_services() {
    let play = public();
    let opted = play["pre_tasks"][0]["ansible.builtin.assert"]["that"]
        .as_array()
        .unwrap();
    assert!(opted.iter().any(|rule| rule == "scpub_enabled | bool"));
    for task in section(&play, "tasks") {
        assert!(task.get("ansible.builtin.systemd_service").is_none());
        let Some(command) = task.get("ansible.builtin.command") else {
            continue;
        };
        assert!(command.is_object());
        let Some(argv) = command.get("argv") else {
            continue;
        };
        for word in argv.as_array().unwrap() {
            assert!(word != "up" && word != "start");
        }
    }
    let text = checkout_file("ops/ansible/public.yml");
    for expected in [
        "os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600",
        "cacheable: false",
        "no_log: true",
    ] {
        assert!(text.contains(expected), "{expected}");
    }
}

/// From `test_unfinished_release_preflight_precedes_public_configuration`.
#[test]
fn unfinished_release_preflight_precedes_public_configuration() {
    let play = public();
    let guards = section(&play, "pre_tasks");
    let (released, release) = located(guards, |task| {
        task.to_string().contains("release-state.json")
    });
    let (stopped, _) = located(guards, |task| task["register"] == "scpub_active_containers");
    assert!(released < stopped);
    assert_eq!(release["tags"], json!(["always"]));
    assert_eq!(release["changed_when"], false);
    assert_eq!(release["check_mode"], false);
    let script = checkout_file(SCRIPT);
    let at = |text: &str| script.find(text).unwrap();
    let journal = at(
        "/bin/sh -eu <<'GUARD'\nroot=/srv/simplestchat-public\nrecord=$root/release-state.json\n",
    );
    assert!(at("flock -n 9") < journal);
    assert!(journal < at("compose()"));
}
