//! The release playbook's source selection and the public playbook's staged
//! image selection, carried over from the Python release playbook tests, and
//! what a release's image selection changes in the project the provisioned
//! Compose renders. The source guard is evaluated by Ansible's own assert
//! module.

use super::ansible::{Scratch, asserted};
use super::public_templates::{engineless, required_renderer, write_project};
use super::{checkout_file, document};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::process::Command;

/// The single play of one playbook.
fn play(name: &str) -> Value {
    document(&format!("ops/ansible/{name}"))[0].clone()
}

/// One section of a play: a list whose every task is a named mapping.
fn section<'a>(play: &'a Value, name: &str) -> &'a [Value] {
    let tasks = play[name].as_array().unwrap();
    assert!(tasks.iter().all(|task| task["name"].is_string()), "{name}");
    tasks
}

/// The one task with exactly this name.
fn named<'a>(tasks: &'a [Value], name: &str) -> &'a Value {
    let matches = tasks.iter().filter(|task| task["name"] == name);
    let [task] = matches.collect::<Vec<_>>()[..] else {
        panic!("exactly one task named {name:?}");
    };
    task
}

/// A list whose every item must be text.
fn strings(value: &Value) -> Vec<&str> {
    let items = value.as_array().unwrap().iter();
    items.map(|item| item.as_str().unwrap()).collect()
}

/// Valid fictional identities, not usable credentials or hosts.
fn github_values() -> Map<String, Value> {
    let values = json!({
        "scpub_release_repository": "example/simplestChat",
        "scpub_release_artifact_id": 1234,
        "scpub_release_expected_revision": "a".repeat(40),
        "scpub_release_ci_run": 5678,
        "ansible_host": "chat.example.test",
        "ansible_user": "root",
        "ansible_ssh_private_key_file": "/private/controller key",
    });
    values.as_object().unwrap().clone()
}

/// From `test_only_current_signed_github_artifacts_can_select_a_release`: an
/// unsigned local directory cannot bypass the source and run contract.
#[test]
fn only_current_signed_github_artifacts_can_select_a_release() {
    let release = play("release.yml");
    let guards = section(&release, "pre_tasks");
    let assertions = strings(&guards[0]["ansible.builtin.assert"]["that"]);
    assert!(assertions.contains(&"scpub_release_directory is not defined"));
    assert!(assertions.contains(&"scpub_release_artifact_id is defined"));
    let remote = named(guards, "Select the explicitly pinned GitHub revision");
    assert_eq!(
        remote["ansible.builtin.set_fact"]["scpub_release_revision"],
        "{{ scpub_release_expected_revision }}"
    );
    let local = "Select the verified local source revision";
    assert!(guards.iter().all(|task| task["name"] != local));
}

/// From `test_github_source_requires_exact_identities_and_explicit_supported_ssh`:
/// the real guard accepts only exact identities over key-based SSH.
#[test]
fn github_source_requires_exact_identities_and_explicit_supported_ssh() {
    let release = play("release.yml");
    let task = named(
        section(&release, "pre_tasks"),
        "Require an exact GitHub source and supported key-based SSH connection",
    );
    assert_eq!(task["when"], "scpub_release_artifact_id is defined");
    let with = |changes: &[(&str, Value)]| {
        let mut values = github_values();
        for (key, value) in changes {
            values.insert((*key).to_owned(), value.clone());
        }
        Value::Object(values)
    };
    let mut cases = vec![
        (with(&[]), true),
        (
            with(&[
                ("ansible_user", json!("deploy")),
                ("ansible_host", json!("chat-alias")),
                ("ansible_port", json!("2222")),
            ]),
            true,
        ),
    ];
    for key in github_values().keys() {
        let mut values = github_values();
        values.remove(key);
        cases.push((Value::Object(values), false));
    }
    for (key, value) in [
        (
            "scpub_release_repository",
            json!("https://github.com/example/repo"),
        ),
        ("scpub_release_repository", json!("example/repo/extra")),
        ("scpub_release_repository", json!("example/")),
        ("scpub_release_artifact_id", json!(0)),
        ("scpub_release_artifact_id", json!(true)),
        ("scpub_release_artifact_id", json!("01")),
        ("scpub_release_ci_run", json!(-1)),
        ("scpub_release_ci_run", json!("1.0")),
        ("scpub_release_expected_revision", json!("main")),
        ("scpub_release_expected_revision", json!("A".repeat(40))),
        ("ansible_connection", json!("local")),
        ("ansible_connection", json!("paramiko")),
        ("ansible_host", json!("")),
        ("ansible_host", json!("2001:db8::1")),
        ("ansible_host", json!("-oProxyCommand=command")),
        ("ansible_user", json!("")),
        ("ansible_ssh_private_key_file", json!("~/.ssh/id_ed25519")),
        ("ansible_port", json!(0)),
        ("ansible_port", json!(65536)),
        ("ansible_port", json!(true)),
        ("ansible_password", json!("")),
        ("ansible_ssh_pass", json!("")),
        ("ansible_ssh_password", json!("")),
        ("ansible_become_password", json!("")),
        ("ansible_become_pass", json!("")),
        ("ansible_ssh_common_args", json!("-J jump.example.test")),
        ("ansible_ssh_extra_args", json!("-F custom.conf")),
    ] {
        cases.push((with(&[(key, value)]), false));
    }
    let variables = cases.iter().map(|(case, _)| case.clone());
    let verdicts = asserted(
        &task["ansible.builtin.assert"]["that"],
        &variables.collect::<Vec<_>>(),
    );
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}

/// The routine path installs no helper file and names no interpreted script:
/// every host step runs the delivered executable, and the private release
/// storage is ensured on every run.
#[test]
fn routine_release_installs_no_helper_and_always_ensures_release_storage() {
    let release = play("release.yml");
    let tasks = section(&release, "tasks");
    for task in section(&release, "pre_tasks").iter().chain(tasks) {
        let name = task["name"].as_str().unwrap();
        assert!(!name.starts_with("Install "), "{name}");
        let copied = task["ansible.builtin.copy"]["dest"].as_str();
        assert!(
            !copied.is_some_and(|path| path.starts_with("/usr/local/libexec/")),
            "{name}"
        );
    }
    let text = checkout_file("ops/ansible/release.yml");
    for retired in [
        "scpub_release_prepared",
        "scpub_release_native_verification",
        ".py",
        "ansible_playbook_python",
    ] {
        assert!(!text.contains(retired), "{retired}");
    }
    let storage = named(tasks, "Create private immutable release storage");
    assert!(storage.get("when").is_none());
    assert_eq!(release["vars"]["ansible_pipelining"], true);
    // The delegated controller's Python is never overridden with the host's.
    assert!(release["vars"].get("ansible_python_interpreter").is_none());
}

/// Standard base64 with padding, as Ansible's slurp reports file content.
pub(super) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0_u32, |word, (at, byte)| {
            word | u32::from(*byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            let sextet = (word >> (18 - 6 * at)) & 63;
            let symbol = char::from(ALPHABET[sextet as usize]);
            text.push(if at <= chunk.len() { symbol } else { '=' });
        }
    }
    text
}

/// The identity the delivery joins, decided by Ansible's own assert module:
/// the routine caller's verified originals and the shared join accept one
/// signed identity and refuse each way the originals or the selection differ.
#[test]
fn verified_originals_and_the_shared_join_accept_only_one_signed_identity() {
    let release = play("release.yml");
    let originals = named(
        section(&release, "tasks"),
        "Require the verified application originals before any Ops delivery",
    );
    let shared = document("ops/ansible/tasks/signed-ops.yml");
    let join = named(
        shared[0]["block"].as_array().unwrap(),
        "Join signed application bytes and exact Ops selection before export",
    );
    let revision = "a".repeat(40);
    let manifest = "b".repeat(64);
    let predicate = json!({
        "repository": "example/simplestChat",
        "revision": revision,
        "runId": 123,
        "runAttempt": 2,
        "fileDigests": {"release.json": manifest},
    });
    let bytes = predicate.to_string().into_bytes();
    let verified = json!({
        "directory": "/private/verified",
        "image.tar": "c".repeat(64),
        "release.json": manifest,
        "revision": revision,
    });
    let base = json!({
        "scpub_app_predicate_file": {"stat": {
            "isreg": true, "islnk": false, "nlink": 1, "size": bytes.len(),
            "checksum": crate::io::sha256(&bytes),
        }},
        "scpub_app_manifest_file": {"stat": {
            "isreg": true, "islnk": false, "checksum": manifest,
        }},
        "scpub_validated_release": {"stdout": verified.to_string()},
        "scpub_app_predicate_bytes": {"content": base64(&bytes)},
        "scpub_app_predicate": predicate,
        "scpub_ops_selection": {
            "repository": "example/simplestChat", "revision": revision,
            "buildRunId": 123, "runAttempt": 2,
        },
    });
    let with = |pointer: &str, value: Value| {
        let mut case = base.clone();
        *case.pointer_mut(pointer).unwrap() = value;
        case
    };
    let cases = [
        (base.clone(), true, true),
        // The controller no longer holds what it verified.
        (
            with("/scpub_app_predicate_file/stat/islnk", json!(true)),
            false,
            true,
        ),
        (
            with("/scpub_app_predicate_file/stat/nlink", json!(2)),
            false,
            true,
        ),
        (
            with("/scpub_app_predicate_file/stat/size", json!(0)),
            false,
            true,
        ),
        (
            with("/scpub_app_manifest_file/stat/isreg", json!(false)),
            false,
            true,
        ),
        // Another manifest than the verified one refuses both checks.
        (
            with(
                "/scpub_app_manifest_file/stat/checksum",
                json!("d".repeat(64)),
            ),
            false,
            false,
        ),
        // The read bytes are not the snapshotted file.
        (
            with(
                "/scpub_app_predicate_file/stat/checksum",
                json!("e".repeat(64)),
            ),
            true,
            false,
        ),
        // The Ops selection names another signed run, attempt, revision or repository.
        (
            with("/scpub_ops_selection/runAttempt", json!(3)),
            true,
            false,
        ),
        (
            with("/scpub_ops_selection/buildRunId", json!(124)),
            true,
            false,
        ),
        (
            with("/scpub_ops_selection/revision", json!("f".repeat(40))),
            true,
            false,
        ),
        (
            with(
                "/scpub_ops_selection/repository",
                json!("other/simplestChat"),
            ),
            true,
            false,
        ),
        // Text is not an integer identity.
        (
            with("/scpub_app_predicate/runId", json!("123")),
            true,
            false,
        ),
        (
            with("/scpub_app_predicate", json!("not a mapping")),
            true,
            false,
        ),
    ];
    let variables = cases.iter().map(|(case, _, _)| case.clone());
    let variables = variables.collect::<Vec<_>>();
    let held = asserted(&originals["ansible.builtin.assert"]["that"], &variables);
    let joined = asserted(&join["ansible.builtin.assert"]["that"], &variables);
    for (at, (case, originals, join)) in cases.iter().enumerate() {
        assert_eq!(held[at], *originals, "originals {at}: {case}");
        assert_eq!(joined[at], *join, "join {at}: {case}");
    }
}

/// From `test_a_staged_release_replaces_the_on_host_build_as_the_image_source`:
/// with a release revision the staged image is selected and its label checked.
#[test]
fn a_staged_release_replaces_the_on_host_build_as_the_image_source() {
    let public = play("public.yml");
    let guards = section(&public, "pre_tasks");
    let assertions = strings(&guards[0]["ansible.builtin.assert"]["that"]);
    assert!(assertions.contains(
        &"scpub_release_revision is not defined or scpub_release_revision == scbench_revision"
    ));
    for name in [
        "Read the previously verified application image manifest",
        "Select the retained application artifact without rebuilding",
    ] {
        assert_eq!(
            named(guards, name)["when"],
            "scpub_release_revision is not defined",
            "{name}"
        );
    }
    let staged = named(
        guards,
        "Read the release the controller staged while chat was live",
    );
    assert_eq!(staged["when"], "scpub_release_revision is defined");
    assert_eq!(
        staged["ansible.builtin.slurp"]["src"],
        "{{ scpub_root }}/releases/{{ scpub_release_revision }}/staged.json"
    );
    let label = named(
        guards,
        "Require the staged image to carry the release revision",
    );
    assert_eq!(label["when"], "scpub_release_revision is defined");
    let argv = strings(&label["ansible.builtin.command"]["argv"]).join(" ");
    assert!(argv.contains("org.opencontainers.image.revision"));
    let refusal = label["failed_when"].as_str().unwrap();
    assert!(refusal.contains("scpub_release_revision"));
    let selection = named(guards, "Retain the verified application image selection");
    let image = &selection["ansible.builtin.set_fact"]["scpub_server_image"];
    assert!(image.as_str().unwrap().contains("scpub_staged_release"));
    let addressed = named(guards, "Require a content-addressed application image");
    assert_eq!(
        addressed["ansible.builtin.assert"]["that"],
        "scpub_server_image is match('^sha256:[a-f0-9]{64}$')"
    );
}

/// The services of two Compose models, and their other top-level fields, in
/// which the models differ: names only, never values, which hold a host's
/// secrets.
fn differing(left: &Value, right: &Value) -> Vec<String> {
    let keys = |value: &Value| -> BTreeSet<String> {
        let fields = value.as_object().map(|fields| fields.keys().cloned());
        fields.into_iter().flatten().collect()
    };
    let mut names = Vec::new();
    for key in keys(left).union(&keys(right)) {
        if key == "services" {
            let (one, other) = (&left[key], &right[key]);
            for name in keys(one).union(&keys(other)) {
                if one.get(name) != other.get(name) {
                    names.push(format!("services.{name}"));
                }
            }
        } else if left.get(key) != right.get(key) {
            names.push(key.clone());
        }
    }
    names
}

/// From `test_real_compose_preview_and_installed_selection_change_only_images_and_image_metadata`:
/// a release selects another image as `release deploy` does, replacing the
/// two image lines of the old image in the Compose file and its one line in
/// the application's settings. The provisioned Compose, with no engine,
/// renders the preview that the release judges, which still reads the
/// installed settings, with only the two images changed; once installed, the
/// project also hands the application the new image setting, and the
/// database and proxy keep their configuration hashes while the
/// application's hash changes. Without the provisioned Compose the case is
/// skipped, unless `VM_COMPOSE_REQUIRED=1` (the deployment job) requires it.
#[test]
fn a_release_selection_changes_only_the_application_images_of_the_compose_model() {
    let scratch = Scratch::new();
    let directory = scratch.path().canonicalize().unwrap();
    let environment = engineless(&directory);
    let Some(renderer) = required_renderer(&environment) else {
        return;
    };
    let (config, attempt) = (directory.join("config"), directory.join("attempt"));
    std::fs::create_dir(&config).unwrap();
    std::fs::create_dir(&attempt).unwrap();
    let old = format!("sha256:{}", "a".repeat(64));
    let new = format!("sha256:{}", "d".repeat(64));
    let overrides = json!({
        "scpub_config": config,
        "scpub_root": directory.join("data"),
        "scpub_server_image": old,
    });
    write_project(&config, &overrides);
    // Compose as the release runs it: from the installed project directory,
    // with the installed files or the candidate's.
    let compose = |file: &str, settings: &str, arguments: &[&str]| {
        let output = Command::new(&renderer[0])
            .args(&renderer[1..])
            .args([
                "--project-name",
                "simplestchat-public",
                "--project-directory",
            ])
            .arg(&config)
            .arg("--env-file")
            .arg(directory.join(settings))
            .arg("-f")
            .arg(directory.join(file))
            .args(arguments)
            .current_dir(&directory)
            .env_clear()
            .envs(
                environment
                    .iter()
                    .map(|(name, value)| (*name, value.as_str())),
            )
            .output()
            .unwrap();
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{errors}");
        output.stdout
    };
    let installed = ("config/compose.public.yml", "config/app.env");
    let model = |(file, settings): (&str, &str)| -> Value {
        let arguments = ["--profile", "maintenance", "config", "--format", "json"];
        serde_json::from_slice(&compose(file, settings, &arguments)).unwrap()
    };
    let hashes = || {
        ["simplestchat", "postgres", "caddy"].map(|service| {
            let printed = compose(installed.0, installed.1, &["config", "--hash", service]);
            let printed = String::from_utf8(printed).unwrap();
            let prefix = format!("{service} ");
            let digest = printed.trim_end().strip_prefix(prefix.as_str()).unwrap();
            let hexadecimal = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
            assert!(
                digest.len() == 64 && digest.bytes().all(hexadecimal),
                "{printed}"
            );
            digest.to_owned()
        })
    };
    let before = model(installed);
    let application = &before["services"]["simplestchat"]["environment"];
    assert_eq!(application["SIMPLESTCHAT_IMAGE"], json!(old));
    let hashed = hashes();
    let read = |name: &str| std::fs::read_to_string(directory.join(name)).unwrap();
    let image = format!("image: \"{old}\"");
    let file = read(installed.0);
    assert_eq!(
        file.matches(&image).count(),
        2,
        "the app and migration images"
    );
    let preview = file.replace(&image, &format!("image: \"{new}\""));
    let setting = format!("SIMPLESTCHAT_IMAGE={old}");
    let settings = read(installed.1);
    assert_eq!(settings.lines().filter(|line| *line == setting).count(), 1);
    let selected = settings.lines().map(|line| {
        if line == setting {
            format!("SIMPLESTCHAT_IMAGE={new}")
        } else {
            line.to_owned()
        }
    });
    let selected = selected.collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(attempt.join("candidate-compose.yml"), &preview).unwrap();
    std::fs::write(attempt.join("candidate.env"), &selected).unwrap();
    let mut expected = before.clone();
    for service in ["simplestchat", "migrate"] {
        expected["services"][service]["image"] = json!(new);
    }
    let previewed = model(("attempt/candidate-compose.yml", "attempt/candidate.env"));
    let changed = differing(&previewed, &expected);
    assert!(previewed == expected, "the preview differs in {changed:?}");
    std::fs::write(directory.join(installed.0), &preview).unwrap();
    std::fs::write(directory.join(installed.1), &selected).unwrap();
    expected["services"]["simplestchat"]["environment"]["SIMPLESTCHAT_IMAGE"] = json!(new);
    let replaced = model(installed);
    let changed = differing(&replaced, &expected);
    assert!(
        replaced == expected,
        "the installed project differs in {changed:?}"
    );
    let rehashed = hashes();
    assert_ne!(rehashed[0], hashed[0]);
    assert_eq!(rehashed[1..], hashed[1..]);
}
