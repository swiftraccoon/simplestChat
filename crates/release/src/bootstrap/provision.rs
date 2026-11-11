//! Host preparation through the checkout's `site.yml`, with one-run revision
//! and maintenance overrides, retained evidence and no retained sudo
//! credential.

use super::command::{Environment, ansible_environment};
use super::files::{local_path, metadata, write_new, write_record};
use super::secrets::{Terminal, password_value};
use super::ssh::Target;
use super::{Refusal, Result, require};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::fs::DirBuilder;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// What one provisioning run was asked for.
pub(super) struct Request<'a> {
    pub root: &'a Path,
    pub home: &'a Path,
    pub name: &'a str,
    pub revision: &'a str,
    pub initial_maintenance: bool,
    /// The sudo password file as the operator named it; resolved from
    /// `current` only when sudo needs a password.
    pub become_password_file: Option<&'a str>,
    pub current: &'a Path,
    /// The run's evidence directory.
    pub output: &'a Path,
}

/// Where the playbook runs; tests substitute a recorded status.
pub(super) trait Playbook {
    /// Runs `argv` with `environment`, keeping its output under `evidence`.
    fn run(&self, argv: &[String], environment: &Environment, evidence: &Path) -> Result<i32>;
}

/// The playbook invocation that advances the source revision without
/// editing the stored inventory's flags.
pub(super) fn site_command(
    root: &Path,
    inventory: &Path,
    name: &str,
    revision: &str,
    maintenance: bool,
) -> Result<Vec<String>> {
    let hexadecimal = Regex::new(r"^[a-f0-9]{40}$").unwrap();
    require(hexadecimal.is_match(revision), "invalid_revision")?;
    let alias = Regex::new(r"^[A-Za-z][A-Za-z0-9_-]{0,62}$").unwrap();
    require(alias.is_match(name), "invalid_inventory_name")?;
    let overrides = json!({
        "scbench_revision": revision,
        "scbench_upgrade_packages": maintenance,
        "scbench_reboot": maintenance,
    });
    let ansible = root.join("ops/ansible");
    Ok(vec![
        ansible
            .join(".venv/bin/ansible-playbook")
            .display()
            .to_string(),
        "-i".to_owned(),
        inventory.display().to_string(),
        ansible.join("site.yml").display().to_string(),
        "--limit".to_owned(),
        name.to_owned(),
        "--extra-vars".to_owned(),
        overrides.to_string(),
    ])
}

/// A private directory beside the identity that holds the sudo password for
/// one playbook run and is removed when the run ends.
struct Become(PathBuf);

impl Become {
    fn create(parent: &Path, password: &str) -> Result<Self> {
        let mut random = [0u8; 8];
        let mut source = std::fs::File::open("/dev/urandom").map_err(metadata)?;
        source.read_exact(&mut random).map_err(metadata)?;
        let suffix = random.iter().map(|byte| format!("{byte:02x}"));
        let name = format!(".simplestchat-become-{}", suffix.collect::<String>());
        let directory = parent.join(name);
        let mut builder = DirBuilder::new();
        builder.mode(0o700).create(&directory).map_err(metadata)?;
        let held = Self(directory);
        write_new(&held.password(), format!("{password}\n").as_bytes())?;
        Ok(held)
    }

    fn password(&self) -> PathBuf {
        self.0.join("password")
    }
}

impl Drop for Become {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `site.yml` against a private snapshot of the validated host.
pub(super) fn provision(
    playbook: &dyn Playbook,
    terminal: &dyn Terminal,
    target: &Target,
    host: &Map<String, Value>,
    facts: &Value,
    request: &Request<'_>,
) -> Result<()> {
    let snapshot = request.output.join("inventory.snapshot.json");
    let stored = json!({"benchmark_hosts": {"hosts": {request.name: host}}});
    write_record(&snapshot, &stored)?;
    let mut argv = site_command(
        request.root,
        &snapshot,
        request.name,
        request.revision,
        request.initial_maintenance,
    )?;
    let transport = json!({"ansible_ssh_common_args": target.common()});
    argv.extend(["--extra-vars".to_owned(), transport.to_string()]);
    let evidence = request.output.join("provision");
    let mut builder = DirBuilder::new();
    builder.mode(0o700).create(&evidence).map_err(metadata)?;
    let mut held = None;
    if facts["passwordlessSudo"] != true {
        let prompt = "Sudo password: ";
        let named = request.become_password_file;
        let source = named.map(|named| local_path(named, request.home, request.current));
        let source = source.transpose()?;
        let password = password_value(source.as_deref(), prompt, request.root, terminal)?;
        let private = target
            .identity
            .parent()
            .ok_or(Refusal("invalid_private_parent"))?;
        let sudo = Become::create(private, password.expose())?;
        let file = sudo.password().display().to_string();
        argv.extend(["--become-password-file".to_owned(), file]);
        held = Some(sudo);
    }
    let environment = ansible_environment(request.root, request.home);
    let status = playbook.run(&argv, &environment, &evidence);
    drop(held);
    require(status? == 0, "provisioning_failed_inspect_host")
}

#[cfg(test)]
mod tests {
    use super::super::files::tests::Scratch;
    use super::super::secrets::tests::Typed;
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::MetadataExt;

    const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// One playbook invocation, with what the sudo password file held and
    /// its mode at that moment.
    type Call = (Vec<String>, Environment, PathBuf, Option<(String, u32)>);

    /// A playbook double that records its invocations.
    struct Observed {
        status: Result<i32>,
        seen: RefCell<Vec<Call>>,
    }

    impl Playbook for Observed {
        fn run(&self, argv: &[String], environment: &Environment, evidence: &Path) -> Result<i32> {
            let flag = argv
                .iter()
                .position(|word| word == "--become-password-file");
            let secret = flag.map(|at| {
                let path = Path::new(&argv[at + 1]);
                let mode = path.metadata().unwrap().mode() & 0o7777;
                (std::fs::read_to_string(path).unwrap(), mode)
            });
            let call = (
                argv.to_vec(),
                environment.clone(),
                evidence.to_owned(),
                secret,
            );
            self.seen.borrow_mut().push(call);
            self.status
        }
    }

    fn target(private: &Scratch) -> Target {
        Target {
            host: "test.example.invalid".to_owned(),
            user: "debian".to_owned(),
            port: 22,
            identity: private.0.join("key"),
            known_hosts: private.0.join("known_hosts"),
        }
    }

    /// The overrides advance the revision and switch maintenance on for one
    /// run; a malformed revision or alias never reaches a command line.
    #[test]
    fn site_command_overrides_revision_and_maintenance_only() {
        let root = Path::new("/checkout");
        let inventory = Path::new("/evidence/inventory.snapshot.json");
        let argv = site_command(root, inventory, "test_vps", REVISION, true).unwrap();
        assert_eq!(
            argv[..7],
            [
                "/checkout/ops/ansible/.venv/bin/ansible-playbook",
                "-i",
                "/evidence/inventory.snapshot.json",
                "/checkout/ops/ansible/site.yml",
                "--limit",
                "test_vps",
                "--extra-vars",
            ]
        );
        let overrides: Value = serde_json::from_str(&argv[7]).unwrap();
        let expected = json!({
            "scbench_revision": REVISION,
            "scbench_upgrade_packages": true,
            "scbench_reboot": true,
        });
        assert_eq!(overrides, expected);
        let quiet = site_command(root, inventory, "test_vps", REVISION, false).unwrap();
        let overrides: Value = serde_json::from_str(&quiet[7]).unwrap();
        assert_eq!(overrides["scbench_reboot"], false);
        let short = site_command(root, inventory, "test_vps", "abc", false);
        assert_eq!(short.unwrap_err(), Refusal("invalid_revision"));
        let alias = site_command(root, inventory, "-e @x", REVISION, false);
        assert_eq!(alias.unwrap_err(), Refusal("invalid_inventory_name"));
    }

    /// From `test_provision_retains_evidence_and_sudo_file_is_removed`: only
    /// a private file name reaches Ansible, logs have an evidence directory,
    /// and the sudo password is gone when the run ends.
    #[test]
    fn provision_retains_evidence_and_the_sudo_file_is_removed() {
        let private = Scratch::new();
        let checkout = Scratch::new();
        let output = Scratch::new();
        let sudo = private.file("sudo", b"sudo fixture\n", 0o600);
        let target = target(&private);
        let host = json!({"ansible_host": "test.example.invalid", "scbench_reboot": false});
        let request = Request {
            root: &checkout.0,
            home: &private.0,
            name: "test_vps",
            revision: REVISION,
            initial_maintenance: false,
            become_password_file: sudo.to_str(),
            current: &checkout.0,
            output: &output.0,
        };
        let playbook = Observed {
            status: Ok(0),
            seen: RefCell::new(Vec::new()),
        };
        let facts = json!({"passwordlessSudo": false});
        let host = host.as_object().unwrap();
        provision(&playbook, &Typed(None), &target, host, &facts, &request).unwrap();
        let seen = playbook.seen.borrow();
        assert_eq!(seen.len(), 1);
        let (argv, environment, evidence, secret) = &seen[0];
        assert_eq!(evidence, &output.0.join("provision"));
        assert_eq!(evidence.metadata().unwrap().mode() & 0o7777, 0o700);
        assert_eq!(
            secret.as_ref().unwrap(),
            &("sudo fixture\n".to_owned(), 0o600)
        );
        assert!(!argv.join(" ").contains("sudo fixture"));
        let flag = argv
            .iter()
            .position(|word| word == "--become-password-file")
            .unwrap();
        let transport: Value = serde_json::from_str(&argv[flag - 1]).unwrap();
        assert_eq!(transport["ansible_ssh_common_args"], target.common());
        assert_eq!(environment["ANSIBLE_HOST_KEY_CHECKING"], "True");
        let snapshot = std::fs::read(output.0.join("inventory.snapshot.json")).unwrap();
        let snapshot: Value = serde_json::from_slice(&snapshot).unwrap();
        let stored = &snapshot["benchmark_hosts"]["hosts"]["test_vps"];
        assert_eq!(stored["ansible_host"], "test.example.invalid");
        assert_eq!(
            argv[2],
            output
                .0
                .join("inventory.snapshot.json")
                .display()
                .to_string()
        );
        let left = std::fs::read_dir(&private.0).unwrap();
        let left = left.map(|entry| entry.unwrap().file_name().into_string().unwrap());
        assert_eq!(
            left.filter(|name| name.starts_with(".simplestchat-become-"))
                .count(),
            0
        );
    }

    /// With passwordless sudo no password is read or passed; a failed or
    /// interrupted playbook is reported as such and the sudo file still goes.
    #[test]
    fn passwordless_sudo_needs_no_file_and_failures_keep_their_code() {
        let private = Scratch::new();
        let checkout = Scratch::new();
        let host = Map::new();
        let run = |status: Result<i32>, sudo: bool| {
            let output = Scratch::new();
            let request = Request {
                root: &checkout.0,
                home: &private.0,
                name: "test_vps",
                revision: REVISION,
                initial_maintenance: true,
                become_password_file: None,
                current: &checkout.0,
                output: &output.0,
            };
            let playbook = Observed {
                status,
                seen: RefCell::new(Vec::new()),
            };
            let facts = json!({"passwordlessSudo": sudo});
            let typed = Typed(Some("typed sudo"));
            let result = provision(
                &playbook,
                &typed,
                &target(&private),
                &host,
                &facts,
                &request,
            );
            let seen = playbook.seen.borrow().clone();
            (result, seen)
        };
        let (passed, seen) = run(Ok(0), true);
        passed.unwrap();
        assert!(!seen[0].0.contains(&"--become-password-file".to_owned()));
        let (typed, seen) = run(Ok(0), false);
        typed.unwrap();
        assert_eq!(seen[0].3.as_ref().unwrap().0, "typed sudo\n");
        let (failed, _) = run(Ok(2), false);
        assert_eq!(
            failed.unwrap_err(),
            Refusal("provisioning_failed_inspect_host")
        );
        let (stopped, _) = run(Err(Refusal("provisioning_interrupted_inspect_host")), false);
        assert_eq!(
            stopped.unwrap_err(),
            Refusal("provisioning_interrupted_inspect_host")
        );
        let left = std::fs::read_dir(&private.0).unwrap();
        assert_eq!(left.count(), 0, "no sudo password directory remains");
    }
}
