//! The guest delivery of the signed Ops executable: its playbook imports the production
//! delivery tasks unchanged and is given what a routine release gives them. These are
//! source contracts; no Ansible, guest or credential runs.
use super::{
    catalog,
    control::Control,
    execute::{self, Delivery},
    files::{self, Original},
    options::Purpose,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, ffi::OsString, fs, path::Path, time::Instant};

type Check<T> = std::result::Result<T, &'static str>;
fn require(ok: bool, cause: &'static str) -> Check<()> {
    if ok { Ok(()) } else { Err(cause) }
}
fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
}
fn parse(path: &str) -> Value {
    let bytes = fs::read(root().join(path)).unwrap();
    serde_saphyr::from_slice(&bytes).unwrap()
}
/// Whether `task` is a name, the one builtin `module`, and nothing else but `extras`.
fn only_module(task: &Value, module: &str, extras: &[&str]) -> bool {
    let module = format!("ansible.builtin.{module}");
    task.as_object().is_some_and(|fields| {
        fields.contains_key(&module)
            && fields
                .keys()
                .all(|key| key == "name" || *key == module || extras.contains(&key.as_str()))
    })
}
/// The task of `tasks` that registers `name`.
fn registering<'a>(tasks: &'a [Value], name: &str) -> Check<&'a Value> {
    let found = tasks
        .iter()
        .filter(|task| task["register"] == name)
        .collect::<Vec<_>>();
    require(found.len() == 1, "unique-registration")?;
    Ok(found[0])
}
/// Every `scpub_*` name `text` mentions.
fn mentioned(text: &str) -> BTreeSet<String> {
    regex::Regex::new(r"\bscpub_[a-z_]+")
        .unwrap()
        .find_iter(text)
        .map(|name| name.as_str().to_owned())
        .collect()
}
/// What the delivery tasks read but do not define: their inputs.
fn delivery_inputs() -> BTreeSet<String> {
    let text = fs::read_to_string(root().join(catalog::DELIVERY_TASKS)).unwrap();
    let tasks = parse(catalog::DELIVERY_TASKS);
    let mut defined = BTreeSet::new();
    let mut pending = tasks.as_array().unwrap().clone();
    while let Some(task) = pending.pop() {
        for key in ["block", "always", "rescue"] {
            if let Some(inner) = task[key].as_array() {
                pending.extend(inner.iter().cloned());
            }
        }
        if let Some(name) = task["register"].as_str() {
            defined.insert(name.to_owned());
        }
        if let Some(facts) = task["ansible.builtin.set_fact"].as_object() {
            defined.extend(facts.keys().filter(|key| *key != "cacheable").cloned());
        }
    }
    mentioned(&text).difference(&defined).cloned().collect()
}
/// The fixed shape of the guest delivery playbook.
fn delivery_contract(value: &Value) -> Check<()> {
    let plays = value.as_array().ok_or("plays")?;
    require(plays.len() == 1, "one-delivery")?;
    let play = &plays[0];
    require(
        play.as_object()
            .ok_or("play")?
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "name",
                "hosts",
                "become",
                "gather_facts",
                "pre_tasks",
                "tasks",
            ]),
        "delivery-only",
    )?;
    require(
        play["hosts"] == "benchmark_hosts"
            && play["become"] == true
            && play["gather_facts"] == false,
        "owned-guest",
    )?;
    let before = play["pre_tasks"].as_array().ok_or("pre-tasks")?;
    let tasks = play["tasks"].as_array().ok_or("tasks")?;
    require(
        before.len() == 7 && tasks.len() == 5,
        "fixed-delivery-steps",
    )?;
    let local = ["register", "delegate_to", "become", "no_log"];
    require(
        before
            .iter()
            .zip([
                ("assert", &[][..]),
                ("slurp", &["register", "no_log"][..]),
                ("assert", &["no_log"][..]),
                ("file", &["loop"][..]),
                ("stat", &local[..]),
                ("stat", &local[..]),
                ("slurp", &local[..]),
            ])
            .all(|(task, (module, extras))| only_module(task, module, extras))
            && tasks
                .iter()
                .zip([
                    ("import_tasks", &[][..]),
                    ("stat", &["register"][..]),
                    ("assert", &[][..]),
                    ("file", &["when"][..]),
                    ("copy", &["loop", "when"][..]),
                ])
                .all(|(task, (module, extras))| only_module(task, module, extras)),
        "delivery-only",
    )?;
    require(
        before[1]["ansible.builtin.slurp"]["src"] == "/etc/simplestchat-vm-fixture"
            && before[2]["ansible.builtin.assert"]["that"]
                == json!(["ops_fixture_marker.content | b64decode == vm_fixture_run_id + '\\n'"]),
        "original-marker",
    )?;
    require(
        before[3]["ansible.builtin.file"]
            == json!({"path":"{{ item.path }}","state":"directory","owner":"root",
                "group":"root","mode":"{{ item.mode }}"})
            && before[3]["loop"]
                == json!([
                    {"path":"/usr/local/libexec/simplestchat-public","mode":"0755"},
                    {"path":"/etc/simplestchat-public","mode":"0700"},
                    {"path":"/srv/simplestchat-public","mode":"0700"}
                ]),
        "prepared-host-roots",
    )?;
    for (at, register, name) in [
        (4, "scpub_app_predicate_file", "release-predicate.json"),
        (5, "scpub_app_manifest_file", "release.json"),
        (6, "scpub_app_predicate_bytes", "release-predicate.json"),
    ] {
        let task = &before[at];
        let module = &task[if at == 6 {
            "ansible.builtin.slurp"
        } else {
            "ansible.builtin.stat"
        }];
        let source = &module[if at == 6 { "src" } else { "path" }];
        require(
            task["register"] == register
                && task["delegate_to"] == "localhost"
                && task["become"] == false
                && *source == format!("{{{{ vm_current_artifact_dir }}}}/{name}"),
            "authenticated-application-originals",
        )?;
    }
    require(
        tasks[0]["ansible.builtin.import_tasks"] == "tasks/signed-ops.yml",
        "production-delivery",
    )?;
    let installed = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
    require(
        tasks[1]["ansible.builtin.stat"]
            == json!({"path":installed,"follow":false,"get_checksum":true,
                "checksum_algorithm":"sha256"})
            && tasks[1]["register"] == "ops_installed",
        "installed-snapshot",
    )?;
    let that = tasks[2]["ansible.builtin.assert"]["that"]
        .as_array()
        .ok_or("installed-identity")?;
    for condition in [
        "ops_installed.stat.isreg | default(false)",
        "not (ops_installed.stat.islnk | default(false))",
        "ops_installed.stat.uid == 0",
        "ops_installed.stat.gid == 0",
        "ops_installed.stat.mode == '0755'",
        "ops_installed.stat.size == vm_ops_binary.bytes",
        "ops_installed.stat.checksum == vm_ops_binary.sha256",
    ] {
        require(that.contains(&json!(condition)), "installed-identity")?;
    }
    require(
        tasks[3]["when"] == "vm_ops_upgrade_input"
            && tasks[4]["when"] == "vm_ops_upgrade_input"
            && tasks[3]["ansible.builtin.file"]
                == json!({"path":"/root/simplestchat-vm-fixture/ops-input","state":"directory",
                    "owner":"root","group":"root","mode":"0700"})
            && tasks[4]["ansible.builtin.copy"]
                == json!({"src":"{{ scpub_ops_remote }}/input/{{ item.name }}",
                    "dest":"/root/simplestchat-vm-fixture/ops-input/{{ item.name }}",
                    "remote_src":true,"owner":"root","group":"root","mode":"{{ item.mode }}",
                    "force":false})
            && tasks[4]["loop"]
                == json!([
                    {"name":"ops-release.json","mode":"0600"},
                    {"name":"ops-selection.json","mode":"0600"},
                    {"name":"simplestchat-ops","mode":"0755"}
                ]),
        "upgrade-input-only-when-asked",
    )?;
    Ok(())
}
fn delivery_variables(purpose: Purpose) -> Value {
    let directory = std::env::temp_dir().join(format!("maintain-delivery-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let mut control = Control::new(Instant::now()).unwrap();
    let path = directory.join("simplestchat-ops");
    files::write(&path, b"candidate", &mut control).unwrap();
    let mut binary = Original::open(&path, 64, &mut control).unwrap();
    let selection = json!({"repository":"swiftraccoon/simplestChat","revision":"a".repeat(40)});
    let delivery = Delivery {
        revision: &"a".repeat(40),
        run: 17,
        artifact: 29,
        selection: &selection,
        controller: Path::new("/trusted/simplestchat-release"),
        gh: Path::new("/trusted/gh"),
        server: Path::new("/held/server/artifact"),
        binary: &binary,
        purpose,
    };
    let value = delivery.variables();
    binary.finish(&mut control).unwrap();
    control.settle();
    control.finish().unwrap();
    fs::remove_dir_all(directory).unwrap();
    value
}

#[test]
fn the_guest_playbook_imports_the_production_delivery_and_runs_nothing_of_its_own() {
    delivery_contract(&parse(catalog::DELIVERY_PLAYBOOK)).unwrap();
}
#[test]
fn delivery_mutations_are_refused() {
    let original = parse(catalog::DELIVERY_PLAYBOOK);
    for (list, at, field, value, cause) in [
        (
            "tasks",
            0,
            "ansible.builtin.import_tasks",
            json!("tasks/fixture-ops.yml"),
            "production-delivery",
        ),
        (
            "tasks",
            0,
            "ansible.builtin.command",
            json!({"argv":["tar","-x"]}),
            "delivery-only",
        ),
        ("tasks", 2, "ignore_errors", json!(true), "delivery-only"),
        (
            "tasks",
            4,
            "when",
            json!(true),
            "upgrade-input-only-when-asked",
        ),
        (
            "pre_tasks",
            4,
            "delegate_to",
            json!("vmfixture"),
            "authenticated-application-originals",
        ),
        (
            "pre_tasks",
            0,
            "ansible.builtin.shell",
            json!("true"),
            "delivery-only",
        ),
    ] {
        let mut changed = original.clone();
        changed[0][list][at][field] = value;
        assert_eq!(
            delivery_contract(&changed),
            Err(cause),
            "{list} {at} {field}"
        );
    }
    let mut weakened = original.clone();
    weakened[0]["tasks"][2]["ansible.builtin.assert"]["that"]
        .as_array_mut()
        .unwrap()
        .retain(|condition| condition != "ops_installed.stat.checksum == vm_ops_binary.sha256");
    assert_eq!(delivery_contract(&weakened), Err("installed-identity"));
    let mut source = original.clone();
    source[0]["pre_tasks"][5]["ansible.builtin.stat"]["path"] =
        json!("{{ vm_current_artifact_dir }}/release-predicate.json");
    assert_eq!(
        delivery_contract(&source),
        Err("authenticated-application-originals")
    );
    let mut extra = original;
    extra[0]["tasks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"x","ansible.builtin.command":"true"}));
    assert_eq!(delivery_contract(&extra), Err("fixed-delivery-steps"));
}
#[test]
fn the_delivery_is_given_exactly_the_inputs_a_routine_release_gives_it() {
    let inputs = delivery_inputs();
    // The tasks file's inputs, as a release supplies them: variables of the frontend,
    // the revision the release playbook selects, and three facts its caller registers.
    assert_eq!(
        inputs,
        [
            "scpub_app_manifest_file",
            "scpub_app_predicate_bytes",
            "scpub_app_predicate_file",
            "scpub_ops_artifact_id",
            "scpub_ops_artifact_zip",
            "scpub_ops_gh_executable",
            "scpub_ops_release_controller",
            "scpub_ops_selection",
            "scpub_release_ci_run",
            "scpub_release_expected_revision",
            "scpub_release_repository",
            "scpub_release_revision",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect()
    );
    let variables = delivery_variables(Purpose::Host);
    let passed = variables
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| key.starts_with("scpub_"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let play = &parse(catalog::DELIVERY_PLAYBOOK)[0];
    let registered = play["pre_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|task| task["register"].as_str())
        .filter(|name| name.starts_with("scpub_"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    // Only the optional pre-downloaded archive is left out, as a routine release does.
    let mut supplied = passed.union(&registered).cloned().collect::<BTreeSet<_>>();
    supplied.insert("scpub_ops_artifact_zip".into());
    assert_eq!(supplied, inputs);
    // The release frontend passes the same delivery variables, with the archive optional;
    // the release playbook derives the revision and registers the same three facts.
    let frontend = include_str!("../deploy_frontend/execute.rs");
    let (_, body) = frontend.split_once("\nfn variables(").unwrap();
    let (body, _) = body.split_once("\n}\n").unwrap();
    let production = mentioned(body)
        .intersection(&inputs)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut controller = passed.clone();
    controller.remove("scpub_release_revision");
    controller.insert("scpub_ops_artifact_zip".into());
    assert_eq!(production, controller);
    let release = parse("ops/ansible/release.yml");
    let release_tasks = release[0]["tasks"].as_array().unwrap();
    assert!(release[0]["pre_tasks"].as_array().unwrap().iter().any(
        |task| task["ansible.builtin.set_fact"]["scpub_release_revision"]
            == "{{ scpub_release_expected_revision }}"
    ));
    for name in &registered {
        let ours = registering(play["pre_tasks"].as_array().unwrap(), name).unwrap();
        let theirs = registering(release_tasks, name).unwrap();
        for module in ["ansible.builtin.stat", "ansible.builtin.slurp"] {
            for field in ["follow", "get_checksum", "checksum_algorithm"] {
                assert_eq!(ours[module][field], theirs[module][field], "{name} {field}");
            }
        }
        assert_eq!(ours["delegate_to"], theirs["delegate_to"], "{name}");
        assert_eq!(ours["become"], theirs["become"], "{name}");
        assert_eq!(ours["no_log"], theirs["no_log"], "{name}");
    }
    assert_eq!(
        variables,
        json!({"scpub_release_repository":"swiftraccoon/simplestChat",
            "scpub_release_ci_run":17,"scpub_release_expected_revision":"a".repeat(40),
            "scpub_release_revision":"a".repeat(40),
            "scpub_ops_release_controller":"/trusted/simplestchat-release",
            "scpub_ops_gh_executable":"/trusted/gh","scpub_ops_artifact_id":29,
            "scpub_ops_selection":{"repository":"swiftraccoon/simplestChat",
                "revision":"a".repeat(40)},
            "vm_current_artifact_dir":"/held/server/artifact",
            "vm_ops_binary":{"sha256":files::hex(&<sha2::Sha256 as sha2::Digest>::digest(
                b"candidate")),"bytes":9},
            "vm_ops_upgrade_input":true})
    );
    // Only the prepared-host flow keeps the delivered members for its upgrade stage.
    for purpose in [Purpose::Prerequisite, Purpose::Deploy] {
        let mut expected = variables.clone();
        expected["vm_ops_upgrade_input"] = false.into();
        assert_eq!(delivery_variables(purpose), expected, "{purpose:?}");
    }
}
#[test]
fn the_delivery_runs_once_with_its_credential_between_boot_and_fixture_setup() {
    let command = execute::delivery_command(
        Path::new("/checkout"),
        Path::new("/work/inventory.json"),
        Path::new("/output/ops-install-vars.json"),
    );
    assert_eq!(
        command,
        [
            "-i",
            "/work/inventory.json",
            "/checkout/ops/ansible/security-vm-ops-install.yml",
            "--extra-vars",
            "@/output/ops-install-vars.json",
        ]
        .map(OsString::from)
    );
    let source = include_str!("execute.rs");
    let (_, body) = source.split_once("pub async fn prepare(").unwrap();
    let (body, _) = body.split_once("\n    async fn body(").unwrap();
    let at = |needle: &str| {
        assert_eq!(body.matches(needle).count(), 1, "{needle}");
        body.find(needle).unwrap()
    };
    assert!(at("checkpoint(report, \"boot\")") < at("checkpoint(report, \"install\")"));
    assert!(
        at("checkpoint(report, \"install\")") < at(".credential(")
            && at(".credential(") < at("checkpoint(report, \"fixture-setup\")")
    );
    let (_, call) = body.split_once(".credential(").unwrap();
    let (call, _) = call.split_once(".await").unwrap();
    assert_eq!(
        call.split_whitespace().collect::<Vec<_>>(),
        [
            "control,",
            "\"ops-install\",",
            "\"ansible-playbook\",",
            "argv,",
            "Duration::from_secs(600),",
            ")",
        ]
    );
    assert_eq!(body.matches("purpose: options.purpose,").count(), 1);
    for absent in ["python", ".py", "run_input", "ops-stage"] {
        assert!(!body.contains(absent), "{absent}");
    }
}
