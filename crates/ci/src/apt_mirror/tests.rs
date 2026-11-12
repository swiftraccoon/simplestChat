use super::{policy, refusal, run};
use serde_json::Value;
use simplestchat_toolkit::tooling::Environment;
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
    "  -- \"$GITHUB_WORKSPACE/target/debug/simplestchat-ci\" apt-mirror\n",
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
    assert_eq!(
        policy::admit(policy::BEFORE).unwrap().as_deref(),
        Some(policy::AFTER)
    );
    assert_eq!(policy::admit(policy::AFTER).unwrap(), None);
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

/// The list of an AWS-hosted image, which prefers its region's mirror.
fn regional(region: &str) -> String {
    format!(
        concat!(
            "http://{}.ec2.archive.ubuntu.com/ubuntu/\tpriority:1\n",
            "https://archive.ubuntu.com/ubuntu/\tpriority:2\n",
            "https://security.ubuntu.com/ubuntu/\tpriority:3\n",
        ),
        region
    )
}

/// An AWS-hosted image's list is reordered exactly as the Azure one is: the
/// two official archives first, the region's own mirror kept as the last.
#[test]
fn an_aws_hosted_list_demotes_its_regional_mirror_the_same_way() {
    // What a hosted image in AWS reported for its list: 152 bytes.
    assert_eq!(regional("us-east-2").len(), 152);
    let regions = [
        "us-east-2",
        "us-west-2",
        "eu-central-1",
        "ap-southeast-2",
        "us-gov-west-1",
        "ap-northeast-3",
    ];
    for region in regions {
        let original = regional(region);
        let preferred = format!(
            concat!(
                "https://archive.ubuntu.com/ubuntu/\tpriority:1\n",
                "https://security.ubuntu.com/ubuntu/\tpriority:2\n",
                "http://{}.ec2.archive.ubuntu.com/ubuntu/\tpriority:3\n",
            ),
            region
        );
        let admitted = policy::admit(original.as_bytes()).unwrap();
        assert_eq!(admitted.as_deref(), Some(preferred.as_bytes()), "{region}");
        assert_eq!(
            policy::admit(preferred.as_bytes()).unwrap(),
            None,
            "{region}"
        );
        assert!(original.len() as u64 <= policy::LIST_MAX, "{region}");
    }
    // The longest region name a list may carry fills the bound exactly.
    let longest = regional("ap-southeast-east-1");
    assert_eq!("ap-southeast-east-1".len(), 19);
    assert!(policy::admit(longest.as_bytes()).is_err());
    let longest = regional("us-gov-northwest-1");
    assert!(policy::admit(longest.as_bytes()).is_err());
    let longest = regional("us-gov-central-1");
    assert_eq!(longest.len() as u64, policy::LIST_MAX);
    assert!(policy::admit(longest.as_bytes()).unwrap().is_some());
}

/// Only a region's name stands before the fixed regional address, only over
/// the scheme a hosted image uses, and a list has one cloud's mirror once.
#[test]
fn a_mirror_that_is_no_aws_regional_archive_is_never_admitted() {
    for region in [
        "",
        "us",
        "us-east",
        "us-2",
        "us-east-22",
        "us-east-a",
        "u-east-2",
        "usa-east-2",
        "US-EAST-2",
        "us--2",
        "us-east-2-1",
        "us-a-b-c-2",
        "us-east-2.evil",
        "evil.example/us-east-2",
        "us-east-2 ",
        "us_east_2",
    ] {
        assert!(
            policy::admit(regional(region).as_bytes()).is_err(),
            "{region:?}"
        );
    }
    let original = regional("us-east-2");
    for value in [
        original.replace("http://us-east-2", "https://us-east-2"),
        original.replace(
            ".ec2.archive.ubuntu.com/ubuntu/",
            ".ec2.archive.ubuntu.com/other/",
        ),
        original.replace(
            ".ec2.archive.ubuntu.com",
            ".ec2.archive.ubuntu.com.evil.example",
        ),
        original.replace(".ec2.archive", ".archive"),
        original.replace("https://archive.", "http://us-west-2.ec2.archive."),
        original.replace("priority:2", "priority:1"),
        original.trim_end().to_owned(),
        format!("{original}http://azure.archive.ubuntu.com/ubuntu/\tpriority:4\n"),
        format!("http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n{original}"),
    ] {
        assert!(policy::admit(value.as_bytes()).is_err(), "{value:?}");
    }
    // A cloud's mirror in the middle is neither the original nor the
    // preferred order.
    let mut lines: Vec<&str> = original.lines().collect();
    lines.swap(0, 1);
    assert!(policy::admit(format!("{}\n", lines.join("\n")).as_bytes()).is_err());
}

/// A refusal names the check that refused, in fixed words.
#[test]
fn a_refusal_names_only_the_check_that_refused() {
    let named = |check: &str| format!("Unsupported hosted Ubuntu archive profile: {check}.");
    let refused = policy::admit(b"PRIVATE").unwrap_err();
    assert_eq!((refused.code, refused.message), (2, named("mirror list")));
    let refused = policy::sources(b"PRIVATE").unwrap_err();
    assert_eq!((refused.code, refused.message), (2, named("sources")));
    let refused = policy::sources(&[0xff]).unwrap_err();
    assert_eq!(refused.message, named("sources"));
    let refused = policy::os_release(b"PRIVATE").unwrap_err();
    assert_eq!((refused.code, refused.message), (2, named("os-release")));
    let refused = policy::os_release(&[0xff]).unwrap_err();
    assert_eq!(refused.message, named("os-release"));
    let refused = policy::host(&Environment::new(), "Linux", "x86_64", 0).unwrap_err();
    assert_eq!(
        refusal(&refused),
        "Hosted Ubuntu mirror preparation refused. \
         Unsupported hosted Ubuntu archive profile: host."
    );
    assert_eq!((refused.code, refused.message), (2, named("host")));
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

/// An AWS-hosted image names the official security archive itself in its
/// security stanza. That and the list are the only addresses a stanza may
/// name, and the list must still be named by one.
#[test]
fn a_stanza_may_name_the_official_security_archive_directly() {
    let listed = "URIs: mirror+file:/etc/apt/apt-mirrors.txt\nSuites: noble-security";
    assert_eq!(SOURCES.matches(listed).count(), 1);
    for address in [
        "http://security.ubuntu.com/ubuntu/",
        "http://security.ubuntu.com/ubuntu",
    ] {
        let direct = SOURCES.replace(listed, &format!("URIs: {address}\nSuites: noble-security"));
        policy::sources(direct.as_bytes()).unwrap();
        // Without any stanza that names the list there is nothing to prefer.
        let unlisted = direct.replace("mirror+file:/etc/apt/apt-mirrors.txt", address);
        assert!(policy::sources(unlisted.as_bytes()).is_err(), "{address}");
    }
    for address in [
        "https://security.ubuntu.com/ubuntu/",
        "http://security.ubuntu.com/other/",
        "http://security.ubuntu.com/ubuntu//",
        "http://security.ubuntu.com.evil.example/ubuntu/",
        "http://archive.ubuntu.com/ubuntu/",
        "http://us-east-2.ec2.archive.ubuntu.com/ubuntu/",
        "http://security.ubuntu.com/ubuntu/ http://foreign.example/ubuntu/",
    ] {
        let direct = SOURCES.replace(listed, &format!("URIs: {address}\nSuites: noble-security"));
        assert!(policy::sources(direct.as_bytes()).is_err(), "{address}");
    }
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
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
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
    assert!(commands.starts_with(concat!(
        "cargo build --locked -p simplestchat-release --bin simplestchat-release\n",
        "cargo build --locked -p simplestchat-ci --bin simplestchat-ci\n",
    )));
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
