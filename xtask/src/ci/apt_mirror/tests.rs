use super::{policy, run};
use crate::tooling::Environment;
use serde_json::Value;
use std::{ffi::OsStr, fs, path::Path, process::ExitCode};

const OS_RELEASE: &[u8] =
    b"NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nVERSION_CODENAME=noble\n";
const SOURCES: &str = concat!(
    "# Existing repository definitions are never rewritten.\n",
    "Types: deb\n",
    "URIs: mirror+file:/etc/apt/apt-mirrors.txt\n",
    "Suites: noble noble-updates noble-backports\n",
    "Components: main universe restricted multiverse\n",
    "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\n",
    "Types: deb\n",
    "URIs: mirror+file:/etc/apt/apt-mirrors.txt\n",
    "Suites: noble-security\n",
    "Components: main universe restricted multiverse\n",
    "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n",
);
const INVOCATION: &str = concat!(
    "sudo --preserve-env=GITHUB_ACTIONS,ImageOS,RUNNER_OS,RUNNER_ARCH,RUNNER_ENVIRONMENT,ACT \\\n",
    "  -- \"$GITHUB_WORKSPACE/target/debug/xtask\" ci apt-mirror\n",
);

fn environment() -> Environment {
    [
        ("GITHUB_ACTIONS", "true"),
        ("RUNNER_ENVIRONMENT", "github-hosted"),
        ("RUNNER_OS", "Linux"),
        ("RUNNER_ARCH", "X64"),
        ("ImageOS", "ubuntu24"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect()
}

fn entries(bytes: &[u8]) -> std::collections::BTreeMap<&str, &str> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| line.split_once('\t').unwrap())
        .collect()
}

#[test]
fn root_hosted_noble_admission_excludes_local_and_ambiguous_contexts() {
    let environment = environment();
    policy::host(&environment, "Linux", "x86_64", 0).unwrap();
    for key in environment.keys() {
        let mut missing = environment.clone();
        missing.remove(key);
        assert!(policy::host(&missing, "Linux", "x86_64", 0).is_err());
        let mut changed = environment.clone();
        changed.insert(key.clone(), "unsupported".into());
        assert!(policy::host(&changed, "Linux", "x86_64", 0).is_err());
    }
    for value in ["", "false", "true"] {
        let mut local = environment.clone();
        local.insert("ACT".into(), value.into());
        local.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
        assert!(policy::host(&local, "Linux", "x86_64", 0).is_err());
    }
    for (system, machine, uid) in [
        ("Darwin", "x86_64", 0),
        ("Linux", "aarch64", 0),
        ("Linux", "x86_64", 1001),
    ] {
        assert!(policy::host(&environment, system, machine, uid).is_err());
    }
    assert!(!environment.contains_key(OsStr::new("ACT")));
}

#[test]
fn exact_priorities_prefer_https_and_preserve_every_original_mirror() {
    assert_eq!(policy::BEFORE.len(), 144);
    assert_eq!(policy::AFTER.len(), 144);
    assert!(policy::admit(policy::BEFORE).unwrap());
    assert!(!policy::admit(policy::AFTER).unwrap());
    let before = entries(policy::BEFORE);
    let after = entries(policy::AFTER);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    assert_eq!(after["https://archive.ubuntu.com/ubuntu/"], "priority:1");
    assert_eq!(after["https://security.ubuntu.com/ubuntu/"], "priority:2");
    assert_eq!(
        after["http://azure.archive.ubuntu.com/ubuntu/"],
        "priority:3"
    );
    let mut lines = std::str::from_utf8(policy::BEFORE)
        .unwrap()
        .lines()
        .collect::<Vec<_>>();
    lines.reverse();
    assert!(policy::admit(format!("{}\n", lines.join("\n")).as_bytes()).is_err());
}

#[test]
fn unknown_or_ambiguous_mirror_lists_are_never_templated() {
    let original = std::str::from_utf8(policy::BEFORE).unwrap();
    for value in [
        String::new(),
        original.trim_end().to_owned(),
        original.replace('\t', " "),
        original.replace("priority:2", "priority:1"),
        original.replace("archive.ubuntu.com/ubuntu/", "archive.ubuntu.com/other/"),
        original.replace("https://archive.", "http://archive."),
        format!("{original}https://unapproved.example/ubuntu/\tpriority:4\n"),
        format!("{original}{}", original.lines().next().unwrap()),
        format!("# additional configuration\n{original}"),
        "x".repeat(16 * 1024 + 1),
    ] {
        assert!(policy::admit(value.as_bytes()).is_err());
    }
    assert!(policy::admit(&[0xff]).is_err());
}

#[test]
fn actual_os_identity_is_required_without_executing_os_release() {
    policy::os_release(OS_RELEASE).unwrap();
    policy::os_release(b"ID=\"ubuntu\"\nVERSION_ID=24.04\nVERSION_CODENAME=\"noble\"\n").unwrap();
    let original = std::str::from_utf8(OS_RELEASE).unwrap();
    for value in [
        original.replace("ID=ubuntu", "ID=debian"),
        original.replace("24.04", "22.04"),
        original.replace("noble", "jammy"),
        original.replace("ID=ubuntu\n", ""),
        format!("{original}ID=ubuntu\n"),
        original.replace("ID=ubuntu", "ID=$(unapproved)"),
        format!("{original}\0"),
        "x".repeat(16 * 1024 + 1),
    ] {
        assert!(policy::os_release(value.as_bytes()).is_err());
    }
    assert!(policy::os_release(&[0xff]).is_err());
}

#[test]
fn readonly_repository_guard_requires_the_effective_fixed_mirror_list() {
    let original = SOURCES.as_bytes().to_vec();
    policy::sources(&original).unwrap();
    assert_eq!(original, SOURCES.as_bytes());
    for value in [
        SOURCES.replace(
            "mirror+file:/etc/apt/apt-mirrors.txt",
            "https://archive.ubuntu.com/ubuntu/",
        ),
        SOURCES.replace("URIs:", "# URIs:"),
        SOURCES.replace("URIs:", " URIs:"),
        SOURCES.replace("URIs:", "URIs: https://foreign.example/\nURIs:"),
        SOURCES.replace("URIs:", "uris: mirror+file:/etc/apt/apt-mirrors.txt\nURIs:"),
        SOURCES.replace("URIs:", "URIs:\n "),
        SOURCES.replace("Types: deb", "invalid line without a field"),
        format!("{SOURCES}\0"),
        "x".repeat(16 * 1024 + 1),
    ] {
        assert!(policy::sources(value.as_bytes()).is_err());
    }
    assert!(policy::sources(&[0xff]).is_err());
}

#[test]
fn cli_help_and_unknown_arguments_cannot_reach_privileged_files() {
    assert_eq!(run(&["--help".into()]).unwrap(), ExitCode::SUCCESS);
    for arguments in [
        vec!["--path".into(), "/tmp/unapproved".into()],
        vec!["--help".into(), "extra".into()],
        vec!["--apply".into()],
    ] {
        assert_eq!(run(&arguments).unwrap(), ExitCode::from(2));
    }
}

fn workflow(name: &str) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    serde_saphyr::from_str(&fs::read_to_string(root.join(".github/workflows").join(name)).unwrap())
        .unwrap()
}

fn before<'a>(steps: &'a [Value], consumer: &str) -> (&'a Value, &'a Value) {
    let index = steps
        .iter()
        .position(|step| step["name"] == consumer)
        .unwrap();
    let preparation = &steps[index - 1];
    assert_eq!(preparation["if"], "${{ !env.ACT }}");
    assert_eq!(preparation["timeout-minutes"], 1);
    assert_eq!(preparation["run"], INVOCATION);
    assert!(!preparation["run"].as_str().unwrap().contains("cargo"));
    (preparation, &steps[index])
}

#[test]
fn ci_browser_preparation_keeps_all_packages_requests_and_existing_deadlines() {
    let value = workflow("ci.yml");
    assert_eq!(value["jobs"]["browser"]["timeout-minutes"], 45);
    assert_eq!(value["jobs"]["deployment"]["timeout-minutes"], 95);
    let browser = value["jobs"]["browser"]["steps"].as_array().unwrap();
    let (_, install) = before(browser, "Build UI and install pinned browser tooling");
    assert!(
        install["run"].as_str().unwrap().contains(
            "npm --prefix web/e2e exec -- playwright install --with-deps chromium firefox"
        )
    );
    let deployment = value["jobs"]["deployment"]["steps"].as_array().unwrap();
    let (_, rollover) = before(deployment, "Install pinned rollover browser tooling");
    assert_eq!(rollover["timeout-minutes"], 10);
    assert!(
        rollover["run"]
            .as_str()
            .unwrap()
            .contains("npm --prefix web/e2e exec -- playwright install --with-deps chromium")
    );
    assert_eq!(
        value["jobs"]["browser"]["strategy"]["matrix"]["group"],
        serde_json::json!(["accounts", "media", "stress"])
    );
}

#[test]
fn maintenance_compiles_without_root_before_apt_and_credentials() {
    let value = workflow("maintenance-launcher-runtime.yml");
    let job = &value["jobs"]["maintain-launcher-runtime"];
    assert_eq!(job["timeout-minutes"], 45);
    let steps = job["steps"].as_array().unwrap();
    let (preparation, packages) = before(
        steps,
        "Prepare the free owned KVM and pinned controller dependencies",
    );
    let prep_index = steps
        .iter()
        .position(|step| std::ptr::eq(step, preparation))
        .unwrap();
    let compile = &steps[prep_index - 1];
    assert_eq!(
        compile["name"],
        "Compile trusted acquisition and passive ELF inspection before credentials"
    );
    let commands = compile["run"].as_str().unwrap();
    assert!(commands.starts_with("cargo build --locked -p xtask --bin xtask\n"));
    assert!(!commands.contains("sudo"));
    assert!(!commands.contains("--features"));
    assert!(commands.contains("--target x86_64-unknown-linux-gnu"));
    let install = packages["run"].as_str().unwrap();
    assert!(install.contains(concat!(
        "sudo apt-get install -y --no-install-recommends \\\n",
        "  qemu-system-x86 qemu-utils cloud-image-utils seabios ipxe-qemu \\\n",
        "  openssh-client curl ca-certificates acl\n",
    )));
    assert!(
        steps[..=prep_index]
            .iter()
            .all(|step| step["env"]["GH_TOKEN"].is_null())
    );
    let auth = steps
        .iter()
        .position(|step| !step["env"]["GH_TOKEN"].is_null())
        .unwrap();
    assert!(auth > prep_index);
    assert_eq!(steps[auth]["timeout-minutes"], 35);
}
