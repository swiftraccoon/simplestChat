//! Actual policy and owned filesystem/process fixtures. No live installer is used.
//! Every test runs the same body over both plugins' independently written pins.

use super::{
    Plugin, execute, files,
    plan::{self, Checksum, Context, Machine},
};
use crate::{
    command::{Command, Program},
    process::Executor,
};
use serde_json::Value;
use simplestchat_toolkit::tooling::{self, Environment, Result};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[path = "native_tests.rs"]
mod native;

pub(super) const CONTENT: &[u8] = b"owned fixture plugin\n";
pub(super) const DIGEST: &str = "90ee5dfccb2872058e673f18d6da8bddb6e63d5e4bc8232bb48e85ad164a0917";

/// One plugin's published release, written out here rather than read from
/// the pins it checks.
pub(super) struct Expected {
    pub(super) plugin: Plugin,
    pub(super) name: &'static str,
    /// The name in the one public failure line.
    pub(super) title: &'static str,
    pub(super) version: &'static str,
    /// The release's download directory; each asset file is below it.
    pub(super) release: &'static str,
    /// `uname -m`, the release's architecture spelling, file and SHA-256.
    pub(super) assets: [(&'static str, &'static str, &'static str, &'static str); 3],
    pub(super) path: &'static str,
    /// The `docker` arguments that print the version, and the exact line.
    pub(super) command: &'static [&'static str],
    pub(super) answer: &'static str,
}

const COMPOSE_AMD64: &str = "40343e21ca777173e69cff5dbafeb37c6f81f3b0d57d9e597f036e95eb63e76a";
const COMPOSE_ARM64: &str = "733ec76717ceb59052a9609b9dadfb523b2df8eab57a54212872d10a58078ea2";
const BUILDX_AMD64: &str = "903090816da68f8bc1a2ff5346dd163a5adacfce4548d003c75df42cd47a9bf4";
const BUILDX_ARM64: &str = "05c88e0f9152825e609a1cdb391faf14d5f94d23d071eaecb87bae69d98da8b8";

pub(super) static EXPECTED: [Expected; 2] = [
    Expected {
        plugin: Plugin::Compose,
        name: "compose",
        title: "Compose",
        version: "5.6.0",
        release: "https://github.com/docker/compose/releases/download/v5.6.0/",
        assets: [
            (
                "x86_64",
                "x86_64",
                "docker-compose-linux-x86_64",
                COMPOSE_AMD64,
            ),
            (
                "aarch64",
                "aarch64",
                "docker-compose-linux-aarch64",
                COMPOSE_ARM64,
            ),
            (
                "arm64",
                "aarch64",
                "docker-compose-linux-aarch64",
                COMPOSE_ARM64,
            ),
        ],
        path: "/usr/local/lib/docker/cli-plugins/docker-compose",
        command: &["compose", "version", "--short"],
        answer: "5.6.0",
    },
    Expected {
        plugin: Plugin::Buildx,
        name: "buildx",
        title: "Buildx",
        version: "0.39.1",
        release: "https://github.com/docker/buildx/releases/download/v0.39.1/",
        assets: [
            (
                "x86_64",
                "amd64",
                "buildx-v0.39.1.linux-amd64",
                BUILDX_AMD64,
            ),
            (
                "aarch64",
                "arm64",
                "buildx-v0.39.1.linux-arm64",
                BUILDX_ARM64,
            ),
            ("arm64", "arm64", "buildx-v0.39.1.linux-arm64", BUILDX_ARM64),
        ],
        path: "/usr/local/lib/docker/cli-plugins/docker-buildx",
        command: &["buildx", "version"],
        answer: "github.com/docker/buildx v0.39.1 334b63fe2bb0f0e89a8fd067704a0d113f57ec25",
    },
];

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) context: Context,
    name: &'static str,
}
impl Fixture {
    pub(super) fn new(expected: &Expected) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "plugin fixture {} {} $(literal)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("runner temp")).unwrap();
        let mut environment = Environment::new();
        for (key, value) in [
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
        ] {
            environment.insert(key.into(), value.into());
        }
        environment.insert(
            "RUNNER_TEMP".into(),
            root.join("runner temp").into_os_string(),
        );
        let mut context =
            Context::new(&root, environment, "Linux", "x86_64", expected.plugin).unwrap();
        context.asset.digest = Checksum::new(DIGEST);
        Self {
            root,
            context,
            name: expected.name,
        }
    }
    fn report(&self) -> Value {
        let prefix = format!("{}-install-", self.name);
        let paths = fs::read_dir(&self.context.temporary)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&prefix)
                    .then_some(path)
            })
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 1);
        let bytes = fs::read(&paths[0]).unwrap();
        assert!(
            !bytes
                .windows(b"PRIVATE-SENTINEL".len())
                .any(|part| part == b"PRIVATE-SENTINEL")
        );
        assert!(!String::from_utf8_lossy(&bytes).contains(self.root.to_str().unwrap()));
        serde_json::from_slice(&bytes).unwrap()
    }
    fn staging(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.context.temporary)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct Fake {
    root: PathBuf,
    expected: &'static Expected,
    commands: Vec<Command>,
    mode: &'static str,
    download: Option<PathBuf>,
}
impl Fake {
    fn new(fixture: &Fixture, expected: &'static Expected) -> Self {
        Self {
            root: fixture.root.clone(),
            expected,
            commands: Vec::new(),
            mode: "",
            download: None,
        }
    }
    fn sudo_count(&self) -> usize {
        self.commands
            .iter()
            .filter(|c| c.program == Program::Sudo)
            .count()
    }
}
impl Executor for Fake {
    fn cleanup_passed(&self) -> bool {
        self.mode != "unproven_cleanup"
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let args = command
            .arguments
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>();
        let stage = if command.program == Program::Curl {
            "download"
        } else if command.program == Program::Docker {
            "caller_version"
        } else if args[0] == "env" {
            "root_version"
        } else if args.get(1) == Some(&"-d") {
            "install_directory"
        } else {
            "install_file"
        };
        if stage == "download" {
            let prefix = fs::read_dir(self.root.join("runner temp"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.is_dir())
                .unwrap();
            let staging = format!("simplestchat-{}-", self.expected.name);
            assert!(
                prefix
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&staging)
            );
            let path = prefix.join("download");
            self.download = Some(path.clone());
            // Every fixture selects the x86_64 asset and substitutes its digest.
            let intent = fs::read(path.parent().unwrap().join("intent.json")).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&intent).unwrap(),
                serde_json::json!({
                    "version": self.expected.version,
                    "architecture": self.expected.assets[0].1,
                    "expectedSha256": DIGEST,
                    "maxBytes": 268_435_456_u64,
                })
            );
            let bytes = if self.mode == "bad_hash" {
                b"PRIVATE-SENTINEL".as_slice()
            } else {
                CONTENT
            };
            let sink = std::sync::Arc::try_unwrap(command.stdout.take().unwrap()).unwrap();
            use std::io::Write;
            (&sink).write_all(bytes).unwrap();
            nix::unistd::close(sink).unwrap();
            match self.mode {
                "empty" => fs::write(&path, []).unwrap(),
                "oversize" => fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(plan::MAX_BYTES + 1)
                    .unwrap(),
                "link" => {
                    let target = self.root.join("private-target");
                    fs::write(&target, b"private").unwrap();
                    fs::remove_file(&path).unwrap();
                    std::os::unix::fs::symlink(target, &path).unwrap();
                }
                "hardlink" => fs::hard_link(&path, self.root.join("external-copy")).unwrap(),
                "loose_prefix" => {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                "extra_entry" => {
                    fs::write(path.parent().unwrap().join("unexpected"), b"private").unwrap()
                }
                "replace_prefix" => {
                    let prefix = path.parent().unwrap();
                    fs::rename(prefix, self.root.join("renamed-owned")).unwrap();
                    fs::create_dir(prefix).unwrap();
                    fs::write(prefix.join("foreign"), b"private").unwrap();
                }
                _ => {}
            }
        }
        if stage == "install_file" {
            assert_eq!(args[0..3], ["install", "-m", "0755"]);
            assert_eq!(args.last().unwrap(), &self.expected.path);
            assert!(args[3].ends_with("/verified"));
            assert_eq!(fs::read(args[3]).unwrap(), CONTENT);
        }
        if stage == "install_directory" && self.mode == "changed_copy" {
            fs::write(
                self.download
                    .as_ref()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("verified"),
                b"PRIVATE-SENTINEL",
            )
            .unwrap();
        }
        self.commands.push(command);
        if self.mode == stage {
            return Err(tooling::Failure::new(42, "Fixed fixture failure."));
        }
        if stage.ends_with("version") {
            if self.mode == "wrong_caller" && stage == "caller_version"
                || self.mode == "wrong_root" && stage == "root_version"
            {
                Ok(b"PRIVATE-SENTINEL".to_vec())
            } else {
                Ok(format!("{}\n", self.expected.answer).into_bytes())
            }
        } else {
            Ok(Vec::new())
        }
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
}

/// The text of every argument of a recorded command.
fn arguments(command: &Command) -> Vec<&str> {
    command
        .arguments
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .collect()
}

#[test]
fn exact_architecture_pins_guard_context_before_any_operation() {
    for expected in &EXPECTED {
        let release = expected.plugin.release();
        assert_eq!(release.name, expected.name);
        assert_eq!(release.title, expected.title);
        assert_eq!(release.version, expected.version);
        assert_eq!(release.path(), expected.path);
        for (machine, architecture, file, digest) in expected.assets {
            let asset = release.asset(Machine::from_uname(machine).unwrap());
            assert_eq!(asset.architecture, architecture);
            assert_eq!(asset.digest, Checksum::new(digest));
            let sink = std::sync::Arc::new(fs::File::open("/dev/null").unwrap());
            let request = plan::download(release, asset, sink);
            let url = format!("{}{file}", expected.release);
            assert_eq!(arguments(&request).last(), Some(&url.as_str()));
        }
        let fixture = Fixture::new(expected);
        let environment = &fixture.context.environment;
        for (system, machine) in [("Linux", "riscv64"), ("Linux", "x86"), ("Darwin", "arm64")] {
            assert!(
                Context::new(
                    &fixture.root,
                    environment.clone(),
                    system,
                    machine,
                    expected.plugin
                )
                .is_err()
            );
        }
        for (key, value) in [
            ("ACT", "true"),
            ("GITHUB_ACTIONS", ""),
            ("RUNNER_ENVIRONMENT", "self-hosted"),
            ("RUNNER_TEMP", "relative"),
        ] {
            let mut env = environment.clone();
            env.insert(key.into(), value.into());
            let refused = Context::new(&fixture.root, env, "Linux", "x86_64", expected.plugin);
            assert_eq!(refused.err().map(|error| error.code), Some(2));
        }
        let mut env = environment.clone();
        env.insert("ACT".into(), "true".into());
        env.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
        assert!(Context::new(&fixture.root, env, "Linux", "aarch64", expected.plugin).is_ok());
        assert!(fixture.staging().is_empty());
    }
}

#[test]
fn each_subcommand_selects_only_its_own_plugin() {
    for expected in &EXPECTED {
        let subcommand = format!("install-{}", expected.name);
        assert_eq!(Plugin::from_subcommand(&subcommand), Some(expected.plugin));
        for other in [
            expected.name.to_owned(),
            format!("install-{}", expected.title),
            format!("{subcommand} "),
            format!("install-install-{}", expected.name),
        ] {
            assert_eq!(Plugin::from_subcommand(&other), None, "{other}");
        }
    }
    for other in ["", "install-", "install-podman"] {
        assert_eq!(Plugin::from_subcommand(other), None, "{other}");
    }
}

#[test]
fn a_digest_pin_admits_only_sixty_four_lowercase_hexadecimal_digits() {
    let valid = "0123456789abcdef".repeat(4);
    for text in [
        valid.clone(),
        "0123456789ABCDEF".repeat(4),
        "0".repeat(63),
        "0".repeat(65),
        "g".repeat(64),
    ] {
        let accepted = text == valid;
        let text: &'static str = text.leak();
        assert_eq!(
            std::panic::catch_unwind(|| Checksum::new(text)).is_ok(),
            accepted,
            "{text}"
        );
    }
}

#[tokio::test]
async fn verified_install_preserves_exact_tls_bounds_fixed_sudo_and_both_version_checks() {
    use std::os::unix::fs::MetadataExt;
    for expected in &EXPECTED {
        let fixture = Fixture::new(expected);
        let mut fake = Fake::new(&fixture, expected);
        execute::install(&fixture.context, &mut fake).await.unwrap();
        assert_eq!(fake.commands.len(), 5);
        assert_eq!(fake.sudo_count(), 3);
        let url = format!("{}{}", expected.release, expected.assets[0].2);
        assert_eq!(
            arguments(&fake.commands[0]),
            [
                "--disable",
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                "10",
                "--max-time",
                "60",
                "--max-filesize",
                "268435456",
                url.as_str(),
            ]
        );
        assert_eq!(fake.commands[0].timeout, Duration::from_secs(65));
        assert_eq!(
            arguments(&fake.commands[1]),
            [
                "install",
                "-d",
                "-m",
                "0755",
                "/usr/local/lib/docker/cli-plugins"
            ]
        );
        assert_eq!(arguments(&fake.commands[3]), expected.command);
        assert_eq!(fake.commands[3].program, Program::Docker);
        assert_eq!(fake.commands[3].timeout, Duration::from_secs(10));
        assert_eq!(fake.commands[3].grace, Duration::from_secs(2));
        let mut root = vec![
            "env",
            "-i",
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            "LC_ALL=C",
            "timeout",
            "--signal=TERM",
            "--kill-after=2s",
            "10s",
            "/usr/bin/docker",
            "--host",
            "unix:///var/run/docker.sock",
        ];
        root.extend(expected.command);
        assert_eq!(arguments(&fake.commands[4]), root);
        assert_eq!(fake.commands[4].timeout, Duration::from_secs(15));
        assert_eq!(fake.commands[4].grace, Duration::from_secs(2));
        assert!(fixture.staging().is_empty());
        let report = fixture.report();
        assert_eq!(report["passed"], true);
        assert_eq!(report["version"], expected.version);
        assert_eq!(report["architecture"], expected.assets[0].1);
        assert_eq!(report["verified"], true);
        assert_eq!(report["cleanupPassed"], true);
        assert_eq!(report["downloadBytes"], CONTENT.len());
        for entry in fs::read_dir(&fixture.context.temporary).unwrap() {
            assert_eq!(entry.unwrap().metadata().unwrap().mode() & 0o777, 0o600);
        }
    }
}

#[tokio::test]
async fn failed_authentication_never_reaches_sudo_and_closes_all_owned_staging() {
    for expected in &EXPECTED {
        for mode in ["bad_hash", "empty", "oversize", "link", "hardlink"] {
            let fixture = Fixture::new(expected);
            let mut fake = Fake::new(&fixture, expected);
            fake.mode = mode;
            assert!(
                execute::install(&fixture.context, &mut fake).await.is_err(),
                "{mode}"
            );
            assert_eq!(fake.sudo_count(), 0);
            assert!(fixture.staging().is_empty());
            let report = fixture.report();
            assert_eq!(report["passed"], false);
            assert_eq!(report["failure"], "authenticate");
            assert_eq!(report["cleanupPassed"], true);
            if mode == "link" {
                assert_eq!(
                    fs::read(fixture.root.join("private-target")).unwrap(),
                    b"private"
                );
            }
        }
    }
}

#[tokio::test]
async fn every_command_and_version_failure_preserves_status_and_cleanup_evidence() {
    for expected in &EXPECTED {
        for mode in [
            "download",
            "install_directory",
            "install_file",
            "caller_version",
            "root_version",
            "wrong_caller",
            "wrong_root",
        ] {
            let fixture = Fixture::new(expected);
            let mut fake = Fake::new(&fixture, expected);
            fake.mode = mode;
            let error = execute::install(&fixture.context, &mut fake)
                .await
                .unwrap_err();
            assert_eq!(error.code, if mode.starts_with("wrong_") { 1 } else { 42 });
            assert!(fixture.staging().is_empty());
            let report = fixture.report();
            assert_eq!(report["passed"], false);
            assert_eq!(report["cleanupPassed"], true);
            assert_eq!(report["exitCode"], error.code);
            if mode == "download" {
                assert_eq!(report["installationAttempted"], false);
            }
            if mode == "install_file" {
                assert_eq!(report["installationCompleted"], false);
            }
            if mode == "wrong_caller" {
                assert_eq!(report["failure"], "caller_version");
                assert_eq!(report["callerVersionPassed"], false);
            }
            if mode == "wrong_root" {
                assert_eq!(report["failure"], "root_version");
                assert_eq!(report["callerVersionPassed"], true);
                assert_eq!(report["rootVersionPassed"], false);
            }
        }
    }
}

#[tokio::test]
async fn changed_verified_copy_refuses_privileged_file_install() {
    for expected in &EXPECTED {
        let fixture = Fixture::new(expected);
        let mut fake = Fake::new(&fixture, expected);
        fake.mode = "changed_copy";
        assert!(execute::install(&fixture.context, &mut fake).await.is_err());
        assert_eq!(fake.sudo_count(), 1);
        assert!(fixture.staging().is_empty());
        assert_eq!(fixture.report()["installationCompleted"], false);
    }
}

#[tokio::test]
async fn cleanup_refuses_unknown_entries_and_replaced_prefix_without_deleting_foreign_data() {
    for expected in &EXPECTED {
        for mode in ["extra_entry", "replace_prefix"] {
            let fixture = Fixture::new(expected);
            let mut fake = Fake::new(&fixture, expected);
            fake.mode = mode;
            assert!(execute::install(&fixture.context, &mut fake).await.is_err());
            let report = fixture.report();
            assert_eq!(report["passed"], false);
            assert_eq!(report["cleanupPassed"], false);
            if mode == "extra_entry" {
                assert_eq!(report["failure"], "cleanup");
                assert!(fixture.staging()[0].join("unexpected").is_file());
            } else {
                assert!(fixture.staging()[0].join("foreign").is_file());
                assert_eq!(fake.sudo_count(), 0);
            }
        }
    }
}

#[test]
fn staging_is_private_unique_and_never_clobbers_existing_output() {
    use std::os::unix::fs::MetadataExt;
    for expected in &EXPECTED {
        let fixture = Fixture::new(expected);
        let mut owned = files::Owned::new(&fixture.context).unwrap();
        let other = files::Owned::new(&fixture.context).unwrap();
        assert_ne!(owned.path, other.path);
        assert_eq!(fs::metadata(&owned.path).unwrap().mode() & 0o777, 0o700);
        owned.prepare(&fixture.context).unwrap();
        assert!(owned.prepare(&fixture.context).is_err());
        let (reporter, cleanup) = owned.cleanup();
        cleanup.unwrap();
        reporter.write(&serde_json::json!({})).unwrap();
        let (reporter, cleanup) = other.cleanup();
        cleanup.unwrap();
        reporter.write(&serde_json::json!({})).unwrap();
    }
}

#[test]
fn cli_help_and_unrecognized_arguments_need_no_runtime_or_host_mutation() {
    let root = std::path::Path::new("/nonexistent/plugin-help");
    for expected in &EXPECTED {
        assert_eq!(
            super::run(root, expected.plugin, &["--help".into()]).unwrap(),
            std::process::ExitCode::SUCCESS
        );
        assert_eq!(
            super::run(
                root,
                expected.plugin,
                &["--digest".into(), "override".into()]
            )
            .unwrap(),
            std::process::ExitCode::from(2)
        );
    }
}

#[tokio::test]
async fn loosened_owned_directory_permissions_refuse_privileged_reads_and_clean_success() {
    for expected in &EXPECTED {
        let fixture = Fixture::new(expected);
        let mut fake = Fake::new(&fixture, expected);
        fake.mode = "loose_prefix";
        assert!(execute::install(&fixture.context, &mut fake).await.is_err());
        assert_eq!(fake.sudo_count(), 0);
        let report = fixture.report();
        assert_eq!(report["passed"], false);
        assert_eq!(report["cleanupPassed"], false);
        assert_eq!(report["failure"], "authenticate");
    }
}

#[tokio::test]
async fn unproven_process_cleanup_cannot_publish_a_successful_install_report() {
    for expected in &EXPECTED {
        let fixture = Fixture::new(expected);
        let mut fake = Fake::new(&fixture, expected);
        fake.mode = "unproven_cleanup";
        assert!(execute::install(&fixture.context, &mut fake).await.is_err());
        let report = fixture.report();
        assert_eq!(report["passed"], false);
        assert_eq!(report["processCleanupPassed"], false);
        assert_eq!(report["stagingCleanupPassed"], true);
        assert_eq!(report["cleanupPassed"], false);
        assert_eq!(report["failure"], "cleanup");
    }
}
