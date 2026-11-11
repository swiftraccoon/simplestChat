//! `simplestchat-release bootstrap`: enroll trusted SSH access to one private
//! Debian benchmark host and prepare it through the checkout's `site.yml`.
//!
//! Passwords are accepted only through a protected file or a terminal prompt.
//! The controller preserves existing authentication, never accepts an
//! unverified host key, and reports failures as fixed codes that cannot carry
//! credentials or child output.

mod command;
mod dialogue;
mod enroll;
mod files;
mod identity;
mod inventory;
mod options;
mod preflight;
mod provision;
mod secrets;
mod ssh;
mod trust;

use command::{Environment, Finished, capture, controller_environment, logged};
use enroll::{Credentials, OpenSsh, Session, enroll};
use files::{local_path, metadata, write_record};
use inventory::{Places, prepare_inventory};
use options::Options;
use provision::Playbook;
use secrets::{Console, Terminal};
use serde_json::json;
use ssh::Target;
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use options::USAGE;

/// A fixed failure code; it never contains a transcript or a secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Refusal(pub &'static str);

type Result<T> = std::result::Result<T, Refusal>;

/// Refuses an unmet boundary condition without disclosing its input.
fn require(condition: bool, code: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Refusal(code))
    }
}

/// Where the controller's bounded commands run. Tests substitute recorded
/// answers, so no test contacts a host or generates a key.
trait Runner {
    /// Runs `argv` to its exit within `timeout`.
    fn run(&self, argv: &[String], timeout: Duration) -> Result<Finished>;
}

/// Everything outside this process that one run touches.
struct World<'a> {
    runner: &'a dyn Runner,
    session: &'a dyn Session,
    playbook: &'a dyn Playbook,
    terminal: &'a dyn Terminal,
    places: Places<'a>,
}

/// The checkout's own commands, with a closed environment.
struct Local {
    root: PathBuf,
    home: PathBuf,
}

impl Runner for Local {
    fn run(&self, argv: &[String], timeout: Duration) -> Result<Finished> {
        capture(
            argv,
            &self.root,
            &controller_environment(&self.home),
            timeout,
        )
    }
}

impl Playbook for Local {
    fn run(&self, argv: &[String], environment: &Environment, evidence: &Path) -> Result<i32> {
        let limit = Duration::from_secs(7200);
        logged(
            argv,
            &self.root,
            environment,
            limit,
            evidence,
            "01-ansible-playbook",
        )
    }
}

/// Performs the selected access and provisioning stages, keeping private
/// evidence of each, and returns the evidence directory.
fn bootstrap(options: &Options, world: &World<'_>) -> Result<PathBuf> {
    let places = world.places;
    let resolve = |value: &str| local_path(value, places.home, places.current);
    let target = Target {
        host: options.host.clone(),
        user: options.user.clone(),
        port: options.port,
        identity: resolve(&options.identity)?,
        known_hosts: resolve(&options.known_hosts)?,
    };
    let (inventory, output) = (resolve(&options.inventory)?, resolve(&options.output)?);
    let fresh = output.symlink_metadata().is_err();
    require(fresh, "evidence_directory_already_exists")?;
    let git = |arguments: &[&str]| {
        let argv = [&["/usr/bin/git"], arguments].concat();
        let argv = argv.iter().map(|word| (*word).to_owned());
        let argv = argv.collect::<Vec<String>>();
        world.runner.run(&argv, Duration::from_secs(30))
    };
    let head = git(&["rev-parse", "HEAD"])?;
    let selected = head.status == 0 && head.text == options.revision;
    require(selected, "checkout_revision_mismatch")?;
    let dirty = git(&["status", "--porcelain", "--untracked-files=all"])?;
    let clean = dirty.status == 0 && dirty.text.is_empty();
    require(clean, "checkout_must_be_clean")?;
    let root = places.root.canonicalize().map_err(metadata)?;
    if output.starts_with(&root) {
        let plan = output.join("plan.json").display().to_string();
        let ignored = git(&["check-ignore", "-q", "--", &plan])?;
        require(ignored.status == 0, "evidence_directory_not_ignored")?;
    }
    let mut builder = DirBuilder::new();
    builder.mode(0o700).create(&output).map_err(metadata)?;
    let plan = json!({
        "schemaVersion": 1,
        "host": target.host,
        "user": target.user,
        "port": target.port,
        "name": options.name,
        "revision": options.revision,
        "inventory": inventory.display().to_string(),
        "identity": target.identity.display().to_string(),
        "knownHosts": target.known_hosts.display().to_string(),
        "provision": options.provision,
        "initialMaintenance": options.initial_maintenance,
        "minimumFreeGiB": options.minimum_free_gib,
    });
    write_record(&output.join("plan.json"), &plan)?;
    let stages = stages(options, world, &target, &inventory, &output);
    let outcome = match stages {
        Ok(()) => json!({
            "completed": true,
            "provisioned": options.provision,
            "revision": options.revision,
        }),
        Err(_) => json!({"completed": false, "revision": options.revision}),
    };
    let recorded = write_record(&output.join("outcome.json"), &outcome);
    // The first refusal is the one reported; a failed record is the next.
    stages.and(recorded).map(|()| output)
}

/// Everything after the plan is recorded, in its fixed order.
fn stages(
    options: &Options,
    world: &World<'_>,
    target: &Target,
    inventory: &Path,
    output: &Path,
) -> Result<()> {
    let places = world.places;
    let prepared = || {
        let (name, revision) = (options.name.as_str(), options.revision.as_str());
        prepare_inventory(world.runner, inventory, target, name, revision, places)
    };
    if inventory.symlink_metadata().is_ok() {
        // Validate local selectors before any remote authentication changes.
        prepared()?;
    }
    let pinned = options.host_fingerprint.as_deref();
    let trust = trust::host_trust(world.runner, target, pinned)?;
    let public = identity::ensure_identity(world.runner, target, &options.name, places.root)?;
    let credentials = Credentials {
        password_file: options.password_file.as_deref(),
        replacement_file: options.replacement_password_file.as_deref(),
        home: places.home,
        current: places.current,
    };
    let access = enroll(
        world.runner,
        world.session,
        world.terminal,
        target,
        &public,
        credentials,
        places.root,
    )?;
    let access = json!({"mode": access, "hostTrust": trust});
    write_record(&output.join("access.json"), &access)?;
    let facts = preflight::preflight(world.runner, target, options.minimum_free_gib)?;
    write_record(&output.join("host-before.json"), &facts)?;
    let host = prepared()?;
    if options.provision {
        let request = provision::Request {
            root: places.root,
            home: places.home,
            name: &options.name,
            revision: &options.revision,
            initial_maintenance: options.initial_maintenance,
            become_password_file: options.become_password_file.as_deref(),
            current: places.current,
            output,
        };
        provision::provision(
            world.playbook,
            world.terminal,
            target,
            &host,
            &facts,
            &request,
        )?;
        let after = preflight::preflight(world.runner, target, options.minimum_free_gib)?;
        write_record(&output.join("host-after.json"), &after)?;
    }
    Ok(())
}

/// Runs the command line after `bootstrap` against the checkout at `root`.
pub(crate) fn run(arguments: &[String], root: &Path) -> ExitCode {
    let options = match options::parse(arguments) {
        Ok(options::Request::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(options::Request::Usage(reason)) => {
            eprintln!("bootstrap: {reason}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        Ok(options::Request::Run(options)) => options,
        Err(Refusal(code)) => {
            eprintln!("bootstrap: {code}");
            return ExitCode::FAILURE;
        }
    };
    // A core file could hold a password; this process must not leave one.
    let none = rustix::process::Rlimit {
        current: Some(0),
        maximum: Some(0),
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let current = std::env::current_dir().ok();
    let prepared = rustix::process::setrlimit(rustix::process::Resource::Core, none).ok();
    let (Some(home), Some(current), Some(())) = (home, current, prepared) else {
        eprintln!("bootstrap: invalid_local_or_remote_metadata");
        return ExitCode::FAILURE;
    };
    let local = Local {
        root: root.to_owned(),
        home: home.clone(),
    };
    let places = Places {
        root,
        home: &home,
        current: &current,
    };
    // The askpass material lives beside the identity, outside the checkout.
    let private = local_path(&options.identity, &home, &current);
    let private = private
        .ok()
        .and_then(|path| path.parent().map(Path::to_owned));
    let session = OpenSsh {
        home: home.clone(),
        private: private.unwrap_or_else(|| home.clone()),
        timeout: Duration::from_secs(120),
    };
    let world = World {
        runner: &local,
        session: &session,
        playbook: &local,
        terminal: &Console,
        places,
    };
    match bootstrap(&options, &world) {
        Ok(output) => {
            println!("Bootstrap evidence: {}", output.display());
            println!(
                "Maintenance flags remain false; build and test with \
                 simplestchat-release vps-capacity."
            );
            ExitCode::SUCCESS
        }
        Err(Refusal(code)) => {
            eprintln!("bootstrap: {code}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// A synthetic Ed25519 public key line; its blob is not a real key.
    pub(super) const PUBLIC: &str = "ssh-ed25519 Zml4dHVyZS1wdWJsaWMta2V5";

    /// A runner that answers each command with the next recorded result and
    /// keeps what it was asked.
    pub(super) struct Recorded {
        answers: RefCell<VecDeque<(i32, String)>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Recorded {
        pub fn new(answers: &[(i32, &str)]) -> Self {
            let answers = answers
                .iter()
                .map(|(status, text)| (*status, (*text).to_owned()));
            Self {
                answers: RefCell::new(answers.collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        pub fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl Runner for Recorded {
        fn run(&self, argv: &[String], _: Duration) -> Result<Finished> {
            self.calls.borrow_mut().push(argv.to_vec());
            let next = self.answers.borrow_mut().pop_front();
            let (status, text) = next.expect("a command without a recorded answer");
            Ok(Finished { status, text })
        }
    }
}

#[cfg(test)]
#[path = "bootstrap/flow_tests.rs"]
mod flow_tests;
