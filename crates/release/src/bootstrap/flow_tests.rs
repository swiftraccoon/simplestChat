//! The whole command against recorded answers: the order of its stages and
//! the evidence each one leaves.

use super::files::tests::Scratch;
use super::options::tests::{REVISION, line};
use super::secrets::tests::Typed;
use super::tests::{PUBLIC, Recorded};
use super::*;
use serde_json::Value;
use std::cell::RefCell;

const RECORD: &str = "SIMPLESTCHAT_BOOTSTRAP_FACT ";

/// No password session and no playbook may run unless a test allows it.
struct Sealed {
    playbooks: RefCell<Vec<Vec<String>>>,
    status: i32,
}

impl Session for Sealed {
    fn password_session(
        &self,
        _: &[String],
        _: &secrets::Password,
        _: &secrets::Password,
    ) -> Result<enroll::Outcome> {
        panic!("a key that works needs no password session");
    }
}

impl Playbook for Sealed {
    fn run(&self, argv: &[String], _: &Environment, _: &Path) -> Result<i32> {
        self.playbooks.borrow_mut().push(argv.to_vec());
        Ok(self.status)
    }
}

/// A checkout and a private directory with a key and a trusted host.
struct Layout {
    checkout: Scratch,
    private: Scratch,
}

impl Layout {
    fn new() -> Self {
        let checkout = Scratch::new();
        std::fs::create_dir_all(checkout.0.join("ops/ansible")).unwrap();
        let private = Scratch::new();
        private.file("key", b"private fixture\n", 0o600);
        let known = format!("test.example.invalid {PUBLIC}\n");
        private.file("known_hosts", known.as_bytes(), 0o600);
        Self { checkout, private }
    }

    fn options(&self, extra: &[&str]) -> Options {
        let mut arguments = line(extra);
        let place = |arguments: &mut Vec<String>, flag: &str, value: String| {
            let at = arguments.iter().position(|word| word == flag).unwrap();
            arguments[at + 1] = value;
        };
        let private = |name: &str| self.private.0.join(name).display().to_string();
        place(&mut arguments, "--identity", private("key"));
        place(&mut arguments, "--output", private("evidence"));
        arguments.extend(["--known-hosts".to_owned(), private("known_hosts")]);
        match options::parse(&arguments).unwrap() {
            options::Request::Run(options) => *options,
            other => panic!("{other:?}"),
        }
    }

    fn run(&self, options: &Options, runner: &Recorded, sealed: &Sealed) -> Result<PathBuf> {
        let world = World {
            runner,
            session: sealed,
            playbook: sealed,
            terminal: &Typed(None),
            places: Places {
                root: &self.checkout.0,
                home: &self.private.0,
                current: &self.checkout.0,
            },
        };
        bootstrap(options, &world)
    }

    fn record(&self, name: &str) -> Value {
        let path = self.private.0.join("evidence").join(name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }
}

fn facts() -> String {
    let pairs = [
        ("distribution", "debian"),
        ("version", "13"),
        ("architecture", "x86_64"),
        ("kernel", "6.12.48"),
        ("cpus", "6"),
        ("freeBytes", "42949672960"),
        ("freeInodes", "5000000"),
        ("systemd", "true"),
        ("passwordlessSudo", "true"),
        ("dockerInstalled", "false"),
        ("python", "3.13.5"),
    ];
    let lines = pairs
        .iter()
        .map(|(name, value)| format!("{RECORD}{name}={value}"));
    lines.collect::<Vec<_>>().join("\n")
}

fn idle() -> String {
    format!("{RECORD}held=false\n{RECORD}active=false\n{RECORD}journal=")
}

/// A host whose key already works: trust, identity, key probe, facts and
/// the inventory, in that order, with a record of each and no provisioning.
#[test]
fn an_enrolled_host_is_checked_in_order_and_leaves_its_evidence() {
    let layout = Layout::new();
    let known = format!("test.example.invalid {PUBLIC}");
    let public = format!("{PUBLIC} simplestchat-bootstrap:test_vps");
    let (facts, idle) = (facts(), idle());
    let runner = Recorded::new(&[
        (0, REVISION),
        (0, ""),
        (0, &known),
        (0, &public),
        (0, "SIMPLESTCHAT_KEY_READY"),
        (0, &facts),
        (0, &idle),
        (0, ""),
    ]);
    let sealed = Sealed {
        playbooks: RefCell::new(Vec::new()),
        status: 0,
    };
    let output = layout.run(&layout.options(&[]), &runner, &sealed).unwrap();
    assert_eq!(output, layout.private.0.join("evidence"));
    let programs = runner.calls();
    let programs = programs.iter().map(|argv| argv[..2].join(" "));
    assert_eq!(
        programs.collect::<Vec<_>>(),
        [
            "/usr/bin/git rev-parse",
            "/usr/bin/git status",
            "/usr/bin/ssh-keygen -F",
            "/usr/bin/ssh-keygen -y",
            "/usr/bin/ssh -F",
            "/usr/bin/ssh -F",
            "/usr/bin/ssh -F",
            "/usr/bin/git check-ignore",
        ]
    );
    let plan = layout.record("plan.json");
    assert_eq!(plan["schemaVersion"], 1);
    assert_eq!(plan["host"], "test.example.invalid");
    assert_eq!(
        (&plan["port"], &plan["minimumFreeGiB"]),
        (&json!(22), &json!(20))
    );
    assert_eq!(plan["provision"], false);
    let access = json!({"mode": "existing_key", "hostTrust": "existing_known_hosts"});
    assert_eq!(layout.record("access.json"), access);
    assert_eq!(layout.record("host-before.json")["distribution"], "debian");
    let outcome = json!({"completed": true, "provisioned": false, "revision": REVISION});
    assert_eq!(layout.record("outcome.json"), outcome);
    assert!(sealed.playbooks.borrow().is_empty());
    assert!(
        layout
            .checkout
            .0
            .join("ops/ansible/inventory.local.test.yml")
            .is_file()
    );
}

/// With `--provision` the playbook runs once after the inventory check and
/// the host is inspected again; a failing playbook is recorded as incomplete.
#[test]
fn provisioning_runs_once_and_a_failure_is_recorded() {
    let known = format!("test.example.invalid {PUBLIC}");
    let public = format!("{PUBLIC} simplestchat-bootstrap:test_vps");
    let (facts, idle) = (facts(), idle());
    let answers = [
        (0, REVISION),
        (0, ""),
        (0, known.as_str()),
        (0, public.as_str()),
        (0, "SIMPLESTCHAT_KEY_READY"),
        (0, facts.as_str()),
        (0, idle.as_str()),
        (0, ""),
        (0, facts.as_str()),
        (0, idle.as_str()),
    ];
    let layout = Layout::new();
    let sealed = Sealed {
        playbooks: RefCell::new(Vec::new()),
        status: 0,
    };
    let options = layout.options(&["--provision", "--initial-maintenance"]);
    layout
        .run(&options, &Recorded::new(&answers), &sealed)
        .unwrap();
    let playbooks = sealed.playbooks.borrow();
    assert_eq!(playbooks.len(), 1);
    assert!(playbooks[0][0].ends_with("ops/ansible/.venv/bin/ansible-playbook"));
    let overrides: Value = serde_json::from_str(&playbooks[0][7]).unwrap();
    assert_eq!(overrides["scbench_reboot"], true);
    assert_eq!(layout.record("host-after.json")["version"], "13");
    assert_eq!(layout.record("outcome.json")["provisioned"], true);
    let failing = Layout::new();
    let sealed = Sealed {
        playbooks: RefCell::new(Vec::new()),
        status: 2,
    };
    let options = failing.options(&["--provision"]);
    let refused = failing.run(&options, &Recorded::new(&answers[..8]), &sealed);
    assert_eq!(
        refused.unwrap_err(),
        Refusal("provisioning_failed_inspect_host")
    );
    let outcome = json!({"completed": false, "revision": REVISION});
    assert_eq!(failing.record("outcome.json"), outcome);
}

/// Nothing is created and no host is contacted for another revision, a
/// dirty checkout, existing evidence or evidence inside an unignored path.
#[test]
fn local_gates_refuse_before_any_evidence_or_host_contact() {
    let other = "b".repeat(40);
    for (answers, code) in [
        (vec![(0, other.as_str())], "checkout_revision_mismatch"),
        (vec![(128, "")], "checkout_revision_mismatch"),
        (
            vec![(0, REVISION), (0, " M file")],
            "checkout_must_be_clean",
        ),
    ] {
        let layout = Layout::new();
        let sealed = Sealed {
            playbooks: RefCell::new(Vec::new()),
            status: 0,
        };
        let refused = layout.run(&layout.options(&[]), &Recorded::new(&answers), &sealed);
        assert_eq!(refused.unwrap_err(), Refusal(code));
        assert!(!layout.private.0.join("evidence").exists());
    }
    let layout = Layout::new();
    let sealed = Sealed {
        playbooks: RefCell::new(Vec::new()),
        status: 0,
    };
    std::fs::create_dir(layout.private.0.join("evidence")).unwrap();
    let silent = Recorded::new(&[]);
    let refused = layout.run(&layout.options(&[]), &silent, &sealed);
    assert_eq!(
        refused.unwrap_err(),
        Refusal("evidence_directory_already_exists")
    );
    assert!(silent.calls().is_empty());
    let inside = Layout::new();
    let mut options = inside.options(&[]);
    options.output = inside.checkout.0.join("evidence").display().to_string();
    let runner = Recorded::new(&[(0, REVISION), (0, ""), (1, "")]);
    let refused = inside.run(&options, &runner, &sealed);
    assert_eq!(
        refused.unwrap_err(),
        Refusal("evidence_directory_not_ignored")
    );
    assert!(!inside.checkout.0.join("evidence").exists());
}

/// A refusal after the plan was written is recorded before it is reported.
#[test]
fn a_refused_stage_records_an_incomplete_outcome() {
    let layout = Layout::new();
    let sealed = Sealed {
        playbooks: RefCell::new(Vec::new()),
        status: 0,
    };
    std::fs::remove_file(layout.private.0.join("known_hosts")).unwrap();
    let runner = Recorded::new(&[(0, REVISION), (0, "")]);
    let refused = layout.run(&layout.options(&[]), &runner, &sealed);
    assert_eq!(
        refused.unwrap_err(),
        Refusal("verified_host_fingerprint_required")
    );
    assert_eq!(layout.record("outcome.json")["completed"], false);
    assert!(!layout.private.0.join("evidence/access.json").exists());
}
