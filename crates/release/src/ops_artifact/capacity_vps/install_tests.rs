//! The benchmark host's delivery of the signed Ops executable: its playbook
//! imports the production delivery tasks unchanged, is given what a routine
//! release and the KVM guests give them, and the host is then served by the
//! installed file. These are source contracts; no Ansible, host or credential runs.

use super::{
    install::{self, Delivery},
    inventory::Target,
};
use serde_json::{Value, json};
use simplestchat_ops::capacity_vps::protocol::{Pin, Source};
use std::{collections::BTreeSet, ffi::OsString, fs, path::Path};

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
/// Every `scpub_*` name `text` mentions.
fn mentioned(text: &str) -> BTreeSet<String> {
    regex::Regex::new(r"\bscpub_[a-z_]+")
        .unwrap()
        .find_iter(text)
        .map(|name| name.as_str().to_owned())
        .collect()
}
/// What the production delivery tasks read but do not define: their inputs.
fn delivery_inputs() -> BTreeSet<String> {
    let text = fs::read_to_string(root().join(install::TASKS)).unwrap();
    let mut defined = BTreeSet::new();
    let mut pending = parse(install::TASKS).as_array().unwrap().clone();
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
/// The body of the one `fn variables(` in `source`, up to its own `end`.
fn variables_body<'a>(source: &'a str, end: &str) -> &'a str {
    assert_eq!(source.matches("fn variables(").count(), 1);
    let (_, body) = source.split_once("fn variables(").unwrap();
    body.split_once(end).unwrap().0
}
const INSTALLED: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";

/// The fixed shape of the benchmark delivery playbook.
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
        "benchmark-host",
    )?;
    let before = play["pre_tasks"].as_array().ok_or("pre-tasks")?;
    let tasks = play["tasks"].as_array().ok_or("tasks")?;
    require(
        before.len() == 5 && tasks.len() == 3,
        "fixed-delivery-steps",
    )?;
    let local = ["register", "delegate_to", "become", "no_log"];
    require(
        before
            .iter()
            .zip([
                ("assert", &[][..]),
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
                ])
                .all(|(task, (module, extras))| only_module(task, module, extras)),
        "delivery-only",
    )?;
    let binding = before[0]["ansible.builtin.assert"]["that"]
        .as_array()
        .ok_or("selected-host")?;
    for condition in [
        "inventory_hostname == capacity_host",
        "ansible_play_hosts_all | length == 1",
        "capacity_ops_binary.sha256 is match('^[a-f0-9]{64}$')",
    ] {
        require(binding.contains(&json!(condition)), "selected-host")?;
    }
    require(
        before[1]["ansible.builtin.file"]
            == json!({"path":"{{ item.path }}","state":"directory","owner":"root",
                "group":"root","mode":"{{ item.mode }}"})
            && before[1]["loop"]
                == json!([
                    {"path":"/usr/local/libexec/simplestchat-public","mode":"0755"},
                    {"path":"/etc/simplestchat-public","mode":"0700"},
                    {"path":"/srv/simplestchat-public","mode":"0700"}
                ]),
        "prepared-host-roots",
    )?;
    for (at, register, name) in [
        (2, "scpub_app_predicate_file", "release-predicate.json"),
        (3, "scpub_app_manifest_file", "release.json"),
        (4, "scpub_app_predicate_bytes", "release-predicate.json"),
    ] {
        let task = &before[at];
        let (module, field) = if at == 4 {
            ("ansible.builtin.slurp", "src")
        } else {
            ("ansible.builtin.stat", "path")
        };
        require(
            task["register"] == register
                && task["delegate_to"] == "localhost"
                && task["become"] == false
                && task[module][field]
                    == format!("{{{{ capacity_application_directory }}}}/{name}"),
            "authenticated-application-originals",
        )?;
    }
    require(
        tasks[0]["ansible.builtin.import_tasks"] == "tasks/signed-ops.yml",
        "production-delivery",
    )?;
    require(
        tasks[1]["ansible.builtin.stat"]
            == json!({"path":INSTALLED,"follow":false,"get_checksum":true,
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
        "ops_installed.stat.nlink == 1",
        "ops_installed.stat.uid == 0",
        "ops_installed.stat.gid == 0",
        "ops_installed.stat.mode == '0755'",
        "ops_installed.stat.size == capacity_ops_binary.bytes",
        "ops_installed.stat.checksum == capacity_ops_binary.sha256",
    ] {
        require(that.contains(&json!(condition)), "installed-identity")?;
    }
    Ok(())
}
fn delivery_variables() -> Value {
    let selection = Source {
        repository: "swiftraccoon/simplestChat".into(),
        revision: "a".repeat(40),
        package_version: "0.1.0".into(),
        protocol_version: 1,
        target: "x86_64-unknown-linux-musl".into(),
        build_run_id: 17,
        run_attempt: 2,
    };
    let binary = Pin {
        sha256: "c".repeat(64),
        bytes: 9,
    };
    Delivery {
        revision: &"a".repeat(40),
        ci_run: 17,
        artifact: 29,
        selection: &selection,
        controller: Path::new("/trusted/simplestchat-release"),
        gh: Path::new("/trusted/gh"),
        host: "private_host",
        run: &"b".repeat(32),
        binary: &binary,
        application: Path::new("/held/application/artifact"),
    }
    .variables()
}

#[test]
fn the_benchmark_playbook_imports_the_production_delivery_and_runs_nothing_of_its_own() {
    delivery_contract(&parse(install::PLAYBOOK)).unwrap();
}

#[test]
fn delivery_mutations_are_refused() {
    let original = parse(install::PLAYBOOK);
    for (list, at, field, value, cause) in [
        (
            "tasks",
            0,
            "ansible.builtin.import_tasks",
            json!("tasks/capacity-ops.yml"),
            "production-delivery",
        ),
        ("tasks", 2, "ignore_errors", json!(true), "delivery-only"),
        (
            "pre_tasks",
            2,
            "delegate_to",
            json!("private_host"),
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
        assert_eq!(delivery_contract(&changed), Err(cause), "{list} {at}");
    }
    for (list, at, module, condition, cause) in [
        (
            "tasks",
            2,
            "ansible.builtin.assert",
            "ops_installed.stat.nlink == 1",
            "installed-identity",
        ),
        (
            "tasks",
            2,
            "ansible.builtin.assert",
            "ops_installed.stat.checksum == capacity_ops_binary.sha256",
            "installed-identity",
        ),
        (
            "pre_tasks",
            0,
            "ansible.builtin.assert",
            "inventory_hostname == capacity_host",
            "selected-host",
        ),
    ] {
        let mut weakened = original.clone();
        weakened[0][list][at][module]["that"]
            .as_array_mut()
            .unwrap()
            .retain(|kept| kept != condition);
        assert_eq!(delivery_contract(&weakened), Err(cause), "{list} {at}");
    }
    let mut source = original.clone();
    source[0]["pre_tasks"][3]["ansible.builtin.stat"]["path"] =
        json!("{{ capacity_application_directory }}/release-predicate.json");
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

/// The production tasks' inputs are exactly what the capacity controller passes
/// and its playbook registers, and the same names `release deploy` and the KVM
/// controller pass, the optional pre-downloaded archive left out as a routine
/// release leaves it out.
#[test]
fn the_delivery_is_given_exactly_what_a_routine_release_and_the_guests_give_it() {
    let inputs = delivery_inputs();
    let variables = delivery_variables();
    let passed = variables
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| key.starts_with("scpub_"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let play = &parse(install::PLAYBOOK)[0];
    let registered = play["pre_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|task| task["register"].as_str())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut supplied = passed.union(&registered).cloned().collect::<BTreeSet<_>>();
    supplied.insert("scpub_ops_artifact_zip".into());
    assert_eq!(supplied, inputs);
    let release = include_str!("../deploy_frontend/execute.rs");
    let mut frontend = mentioned(variables_body(release, "\n}\n"))
        .intersection(&inputs)
        .cloned()
        .collect::<BTreeSet<_>>();
    // `release.yml` derives the revision from the expected one before its import.
    frontend.insert("scpub_release_revision".into());
    frontend.remove("scpub_ops_artifact_zip");
    assert_eq!(frontend, passed);
    let guests = include_str!("../maintain_runtime/execute.rs");
    let kvm = mentioned(variables_body(guests, "\n    }\n"))
        .intersection(&inputs)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(kvm, passed);
    // The three registrations use `release.yml`'s own module arguments.
    let release_tasks = parse("ops/ansible/release.yml")[0]["tasks"].clone();
    for name in &registered {
        let find = |tasks: &Value| {
            let found = tasks
                .as_array()
                .unwrap()
                .iter()
                .filter(|task| task["register"] == name.as_str())
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(found.len(), 1, "{name}");
            found[0].clone()
        };
        let ours = find(&play["pre_tasks"]);
        let theirs = find(&release_tasks);
        for module in ["ansible.builtin.stat", "ansible.builtin.slurp"] {
            for field in ["follow", "get_checksum", "checksum_algorithm"] {
                assert_eq!(ours[module][field], theirs[module][field], "{name} {field}");
            }
        }
        for field in ["delegate_to", "become", "no_log"] {
            assert_eq!(ours[field], theirs[field], "{name} {field}");
        }
    }
    assert_eq!(
        variables,
        json!({"scpub_release_repository":"swiftraccoon/simplestChat",
            "scpub_release_ci_run":17,"scpub_release_expected_revision":"a".repeat(40),
            "scpub_release_revision":"a".repeat(40),
            "scpub_ops_release_controller":"/trusted/simplestchat-release",
            "scpub_ops_gh_executable":"/trusted/gh","scpub_ops_artifact_id":29,
            "scpub_ops_selection":{"repository":"swiftraccoon/simplestChat",
                "revision":"a".repeat(40),"packageVersion":"0.1.0","protocolVersion":1,
                "target":"x86_64-unknown-linux-musl","buildRunId":17,"runAttempt":2},
            "capacity_host":"private_host","capacity_run":"b".repeat(32),
            "capacity_application_directory":"/held/application/artifact",
            "capacity_ops_binary":{"sha256":"c".repeat(64),"bytes":9}})
    );
}

/// The verifier runs exactly as `release.yml` runs it, but downloads itself: no
/// pre-verified directory is named.
#[test]
fn the_application_is_verified_by_the_production_verifier_as_release_yml_runs_it() {
    let argv = install::verification(&"a".repeat(40), 789, 123, Path::new("/output/application"));
    assert_eq!(
        argv,
        [
            "verify",
            "--repository",
            "swiftraccoon/simplestChat",
            "--artifact-id",
            "789",
            "--revision",
            &"a".repeat(40),
            "--ci-run",
            "123",
            "--output-parent",
            "/output/application",
            "--timeout-seconds",
            "600",
            "--finalization-seconds",
            "60",
        ]
        .map(OsString::from)
    );
    let release = parse("ops/ansible/release.yml");
    let verifying = release[0]["pre_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| {
            task["ansible.builtin.command"]["argv"]
                .as_array()
                .is_some_and(|argv| argv.get(1) == Some(&json!("verify")))
        })
        .unwrap();
    let flags = |values: Vec<String>| {
        values
            .into_iter()
            .filter(|value| value.starts_with("--"))
            .collect::<Vec<_>>()
    };
    let theirs = verifying["ansible.builtin.command"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .filter(|value| value != "--artifact-dir")
        .collect::<Vec<_>>();
    let ours = argv
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(flags(ours), flags(theirs));
    let listing = install::listing(
        123,
        &super::super::deploy_frontend::policy::Revision::named(&"a".repeat(40)).unwrap(),
    );
    assert_eq!(
        listing.last().unwrap(),
        &OsString::from(format!(
            "repos/swiftraccoon/simplestChat/actions/runs/123/artifacts?name=\
             simplestchat-production-{}&per_page=100",
            "a".repeat(40)
        ))
    );
}

/// The playbook runs once over the private inventory snapshot for the one host
/// the operator selected, with the written variables and the run's SSH trust.
#[test]
fn the_delivery_command_is_the_selected_host_and_the_written_variables() {
    let argv = install::command(
        Path::new("/output/source"),
        Path::new("/output/inventory.snapshot.json"),
        "private_host",
        Path::new("/output/ops-install-vars.json"),
        "-o BatchMode=yes",
    )
    .unwrap();
    assert_eq!(
        argv,
        [
            "-i",
            "/output/inventory.snapshot.json",
            "/output/source/ops/ansible/capacity-ops-install.yml",
            "--limit",
            "private_host",
            "--extra-vars",
            "@/output/ops-install-vars.json",
            "--extra-vars",
            r#"{"ansible_ssh_common_args":"-o BatchMode=yes"}"#,
        ]
        .map(OsString::from)
    );
}

/// Every request is served by the executable the production tasks install, under
/// the fixed environment the removed bootstrap gave it, with the header as one
/// quoted argument; another user reaches it only through noninteractive sudo.
#[test]
fn the_host_serves_every_request_with_the_installed_executable() {
    let served = "/usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin LC_ALL=C HOME=/root \
                  /usr/local/libexec/simplestchat-public/simplestchat-ops capacity-vps serve \
                  --transport-json '{\"run\":\"inert\"}'";
    for (user, remote) in [
        ("root", served.to_owned()),
        ("debian", format!("sudo -n -- {served}")),
    ] {
        let target = Target {
            host: "inert.invalid".into(),
            user: user.into(),
            port: 22,
            identity: "/private/key".into(),
            known_hosts: "/private/known_hosts".into(),
        };
        let argv = target.ssh(r#"{"run":"inert"}"#);
        assert_eq!(argv.last().unwrap(), &OsString::from(remote), "{user}");
        assert_eq!(
            argv[argv.len() - 2],
            OsString::from(format!("{user}@inert.invalid"))
        );
    }
    // The playbook's final check and the production tasks name the same file.
    let tasks = fs::read_to_string(root().join(install::TASKS)).unwrap();
    assert!(tasks.contains(&format!("path: {INSTALLED}\n")));
    assert!(served.contains(INSTALLED));
}

/// One verified application below a fresh parent, as the verifier leaves it.
struct Verified {
    fixture: super::tests::Fixture,
    parent: std::path::PathBuf,
    directory: std::path::PathBuf,
    manifest: Vec<u8>,
}
impl Verified {
    const REVISION: &'static str = "0123456789abcdef0123456789abcdef01234567";
    fn new() -> Self {
        let fixture = super::tests::Fixture::new();
        let parent = fixture.root.join("application");
        let directory = parent.join(format!("release-verification.{}", "0f".repeat(16)));
        let manifest = br#"{"release":"a manifest of the application"}"#.to_vec();
        let verified = Self {
            fixture,
            parent,
            directory: directory.join("artifact"),
            manifest,
        };
        verified.place(&verified.directory);
        verified
    }
    /// The verified files, also at `directory`, so that only where they are
    /// differs.
    fn place(&self, directory: &Path) -> std::path::PathBuf {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .unwrap();
        let predicate = json!({"runId": 7, "runAttempt": 2, "revision": Self::REVISION});
        for (name, bytes) in [
            ("release.json", self.manifest.clone()),
            ("release-predicate.json", predicate.to_string().into_bytes()),
        ] {
            let path = directory.join(name);
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        directory.to_owned()
    }
    fn predicate(self, predicate: Value) -> Self {
        let path = self.directory.join("release-predicate.json");
        fs::write(&path, predicate.to_string()).unwrap();
        self
    }
    fn receipt(&self) -> Value {
        json!({
            "directory": self.directory,
            "image.tar": "e".repeat(64),
            "release.json": super::super::sha(&self.manifest),
            "revision": Self::REVISION,
        })
    }
    /// The refusal of `receipt`, as its code and message, if it is refused.
    fn admitted(&self, receipt: &Value) -> Option<(u8, String)> {
        let expected = install::Expected {
            revision: Self::REVISION,
            ci_run: 7,
            attempt: 2,
        };
        let receipt = serde_json::to_vec(receipt).unwrap();
        let fixture = &self.fixture;
        install::admit(
            &receipt,
            &self.parent,
            &expected,
            &fixture.budget,
            &fixture.checkpoint,
        )
        .err()
        .map(|failure| (failure.code, failure.message))
    }
}

#[test]
fn the_verifier_receipt_and_predicate_must_name_this_run_and_its_own_fresh_attempt() {
    let verified = Verified::new();
    let receipt = verified.receipt();
    assert_eq!(verified.admitted(&receipt), None);
    let refused = Some(super::failure(1)).map(|failure| (failure.code, failure.message));
    let mut rows: Vec<(&str, Value)> = Vec::new();
    let mut changed = |name: &'static str, change: &dyn Fn(&mut Value)| {
        let mut receipt = receipt.clone();
        change(&mut receipt);
        rows.push((name, receipt));
    };
    changed("an extra key", &|r| r["size"] = json!(1));
    changed("a missing key", &|r| {
        r.as_object_mut().unwrap().remove("image.tar");
    });
    changed("another revision", &|r| {
        r["revision"] = json!("1".repeat(40))
    });
    changed("an image that is no digest", &|r| {
        r["image.tar"] = json!("an image")
    });
    changed("another manifest digest", &|r| {
        r["release.json"] = json!("d".repeat(64))
    });
    let directory = verified.directory.clone();
    let parent = verified.parent.clone();
    let elsewhere = parent.with_file_name("elsewhere");
    let placed = |path: std::path::PathBuf| {
        let path = if path.is_absolute() {
            verified.place(&path)
        } else {
            path
        };
        move |r: &mut Value| r["directory"] = json!(path)
    };
    changed(
        "a relative directory",
        &placed("application/artifact".into()),
    );
    changed(
        "a directory not named artifact",
        &placed(directory.with_file_name("output")),
    );
    changed(
        "an attempt beside the parent",
        &placed(
            elsewhere
                .join(format!("release-verification.{}", "0f".repeat(16)))
                .join("artifact"),
        ),
    );
    for (name, nonce) in [
        ("a short nonce", "0f".repeat(15) + "0"),
        ("a long nonce", "0f".repeat(16) + "0"),
        ("an upper-case nonce", "0F".repeat(16)),
    ] {
        let attempt = parent.join(format!("release-verification.{nonce}"));
        changed(name, &placed(attempt.join("artifact")));
    }
    let named = |prefix: &str| parent.join(format!("{prefix}{}", "0f".repeat(16)));
    changed(
        "another attempt prefix",
        &placed(named("verification.").join("artifact")),
    );
    for (name, receipt) in &rows {
        assert_eq!(verified.admitted(receipt), refused, "{name}");
    }
    assert_eq!(rows.len(), 12);
    for (name, predicate) in [
        (
            "another run",
            json!({"runId": 8, "runAttempt": 2, "revision": Verified::REVISION}),
        ),
        (
            "another attempt",
            json!({"runId": 7, "runAttempt": 1, "revision": Verified::REVISION}),
        ),
        (
            "another built revision",
            json!({"runId": 7, "runAttempt": 2, "revision": "1".repeat(40)}),
        ),
        (
            "no run",
            json!({"runAttempt": 2, "revision": Verified::REVISION}),
        ),
    ] {
        let verified = Verified::new().predicate(predicate);
        assert_eq!(verified.admitted(&verified.receipt()), refused, "{name}");
    }
}
