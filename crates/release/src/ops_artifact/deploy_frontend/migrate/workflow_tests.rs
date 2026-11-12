//! The production move over an inert scripted executor; no tool, network or
//! host runs. The recorded scenarios are the Python controller's own answers
//! for the same checkout, inventories, hosts, DNS and GitHub conditions.

use super::*;
use simplestchat_ci::{
    bounded_log::{Captures, Seal},
    command::{Command, ReleaseRole},
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SOURCE_PATHS: [&str; 4] = [
    "build/tracked-controller-input",
    "ops/ansible/ansible.cfg",
    "ops/ansible/migration-data.yml",
    "ops/ansible/migration-prepare.yml",
];
const RUN: u64 = 789;
const APPLICATION: u64 = 123;
const OPS: u64 = 457;

fn recorded() -> Value {
    serde_json::from_str(include_str!("recorded_controller.json")).unwrap()
}
fn revision() -> String {
    "a".repeat(40)
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn private(path: &Path, content: &[u8], mode: u32) {
    let _ = fs::remove_file(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn repository() -> Value {
    json!({"id": 1155881649u64, "full_name": policy::REPOSITORY, "fork": false,
        "owner": {"id": 47705353u64, "login": "swiftraccoon"}})
}
fn run_record(revision: &str, conclusion: &str) -> Value {
    json!({"id": RUN, "run_attempt": 2, "head_sha": revision, "head_branch": "main",
        "name": "CI", "path": ".github/workflows/ci.yml", "event": "push",
        "status": "completed", "conclusion": conclusion,
        "repository": repository(), "head_repository": repository()})
}
fn artifact_record(ops: bool, revision: &str, digest: &str) -> Value {
    let (id, name) = match ops {
        true => (
            OPS,
            format!("simplestchat-ops-{revision}-x86_64-unknown-linux-musl"),
        ),
        false => (APPLICATION, format!("simplestchat-production-{revision}")),
    };
    let url = format!(
        "https://api.github.com/repos/{}/actions/artifacts/{id}",
        policy::REPOSITORY
    );
    json!({"id": id, "name": name, "url": url, "archive_download_url": format!("{url}/zip"),
        "expired": false, "size_in_bytes": 12345, "digest": format!("sha256:{}", digest.repeat(64)),
        "workflow_run": {"id": RUN, "head_sha": revision, "repository_id": 1155881649u64,
            "head_repository_id": 1155881649u64}})
}
/// Every maximal run of forty lower-case hexadecimal digits in `text`.
fn revisions(text: &str) -> Vec<String> {
    let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    let runs = text.as_bytes().split(|byte| !digit(*byte));
    let exact = runs.filter(|run| run.len() == 40);
    exact
        .map(|run| String::from_utf8(run.to_vec()).unwrap())
        .collect()
}

/// One play as the controller issued it.
#[derive(Clone, Debug)]
struct Played {
    action: String,
    playbook: String,
    inventory: String,
    limit: String,
    variables: Value,
    hosts: Vec<String>,
    ceiling: Duration,
}

/// One inert checkout with both inventories and stub tools.
struct Fixture {
    root: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new(scenario: &Value) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "migrate-workflow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("results")).unwrap();
        fs::set_permissions(root.join("results"), fs::Permissions::from_mode(0o700)).unwrap();
        for name in [
            "git",
            "gh",
            "ansible-playbook",
            "ansible-inventory",
            "curl",
            "dig",
        ] {
            private(&root.join("bin").join(name), b"#!/bin/sh\nexit 99\n", 0o700);
        }
        private(
            &root.join("bin/simplestchat-release"),
            b"#!/bin/sh\nexit 99\n",
            0o700,
        );
        for name in SOURCE_PATHS {
            private(&root.join(name), b"inert private tracked source\n", 0o600);
        }
        let origin = "[remote \"origin\"]\nurl = git@github.com:swiftraccoon/simplestChat.git\n";
        for (name, content) in [
            (".git/HEAD", "ref: refs/heads/main\n".to_owned()),
            (".git/index", "inert private index\n".to_owned()),
            (".git/config", origin.to_owned()),
            (".git/refs/heads/main", format!("{}\n", revision())),
        ] {
            private(&root.join(name), content.as_bytes(), 0o600);
        }
        // An inventory that ends world-readable and unreplaced began that way.
        let kept = scenario["inventory"]["mode"] == "0o644";
        private(
            &root.join("source.yml"),
            b"offline fixture\n",
            if kept { 0o644 } else { 0o600 },
        );
        private(&root.join("target.yml"), b"offline fixture\n", 0o600);
        let environment = Environment::from([
            ("PATH".into(), root.join("bin").into_os_string()),
            ("HOME".into(), root.clone().into_os_string()),
            ("GH_TOKEN".into(), "PRIVATE_GH_SENTINEL".into()),
            ("HTTPS_PROXY".into(), "https://PRIVATE.invalid".into()),
        ]);
        Self { root, environment }
    }
    fn text(&self, value: &str) -> String {
        value.replace("<root>", self.root.to_str().unwrap())
    }
    /// The recorded command line, with this fixture's paths and tools.
    fn arguments(&self, scenario: &Value) -> Vec<String> {
        let recorded = scenario["arguments"].as_array().unwrap().iter();
        let mut arguments: Vec<String> = recorded
            .map(|word| self.text(word.as_str().unwrap()))
            .collect();
        for (flag, tool) in [("--ansible-playbook", "ansible-playbook"), ("--gh", "gh")] {
            arguments.extend([flag.to_owned(), self.text(&format!("<root>/bin/{tool}"))]);
        }
        arguments
    }
    /// The one private evidence directory, unless the move was refused
    /// before it had any.
    fn evidence(&self) -> Option<PathBuf> {
        let listed = fs::read_dir(self.root.join("results")).unwrap();
        let mut paths: Vec<PathBuf> = listed.map(|entry| entry.unwrap().path()).collect();
        assert!(paths.len() <= 1, "{paths:?}");
        paths.pop()
    }
    async fn run(&self, script: &mut Script, arguments: &[String]) -> (Result<Completed>, Value) {
        let options = Arc::new(super::super::options::parse(arguments).unwrap().unwrap());
        let plan = Plan::phased(Instant::now(), phases(&options));
        let controller = self.root.join("bin/simplestchat-release");
        let environment = self.environment.clone();
        let root = self.root.clone();
        let result = transaction(root, options, environment, controller, None, &plan, script).await;
        let report = match self.evidence() {
            Some(evidence) => {
                let outcome = fs::read(evidence.join("outcome.json")).unwrap();
                serde_json::from_slice(&outcome).unwrap()
            }
            None => result.as_ref().unwrap().summary.clone(),
        };
        (result, report)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// The world outside the controller, answering from one scenario.
struct Script {
    scenario: Value,
    root: PathBuf,
    selected: String,
    reads: [usize; 2],
    next: usize,
    canaries: usize,
    queries: usize,
    programs: Vec<Program>,
    plays: Vec<Played>,
    requests: Vec<(String, Option<String>, Vec<String>)>,
    lookups: Vec<Vec<String>>,
    notices: Vec<Value>,
    /// What a native-only case makes the verification step leave behind.
    application: Option<Value>,
    /// What a native-only case makes the name service print, or its failure.
    resolver: Option<std::result::Result<String, u8>>,
    /// A play during which an operator's own inventories are rewritten.
    rewrite_during: Option<&'static str>,
    /// A play during which a tracked controller input is edited.
    tracked_during: Option<&'static str>,
    /// What a native-only case changes between the first observation of it
    /// and the next: the checkout's head, published main or an artifact.
    moves: Option<&'static str>,
    /// Whether git ignores the private results directory.
    results_ignored: bool,
    heads: usize,
    mains: usize,
    listings: usize,
}
impl Script {
    fn new(fixture: &Fixture, scenario: &Value) -> Self {
        Self {
            scenario: scenario.clone(),
            root: fixture.root.clone(),
            selected: revision(),
            reads: [0; 2],
            next: 0,
            canaries: 0,
            queries: 0,
            programs: Vec::new(),
            plays: Vec::new(),
            requests: Vec::new(),
            lookups: Vec::new(),
            notices: Vec::new(),
            application: None,
            resolver: None,
            rewrite_during: None,
            tracked_during: None,
            moves: None,
            results_ignored: true,
            heads: 0,
            mains: 0,
            listings: 0,
        }
    }
    fn count(&self, program: Program) -> usize {
        self.programs
            .iter()
            .filter(|observed| **observed == program)
            .count()
    }
    fn git(&mut self, text: &[&str]) -> (Vec<u8>, u8) {
        let checkout = &self.scenario["checkout"];
        let line = |value: &str| format!("{value}\n").into_bytes();
        let field = |name: &str| checkout[name].as_str().unwrap().to_owned();
        let answer = match text {
            ["rev-parse", "HEAD"] => {
                self.heads += 1;
                let moved = self.moves == Some("head") && self.heads > 1;
                line(&if moved { "c".repeat(40) } else { revision() })
            }
            ["branch", "--show-current"] => line(&field("branch")),
            ["status", "--porcelain", "--untracked-files=all"] => match field("status").as_str() {
                "" => Vec::new(),
                changed => line(changed),
            },
            ["remote", "get-url", "origin"] => line(&field("remote")),
            ["check-ignore", "--", "results/migration.evidence"] => {
                if !self.results_ignored {
                    return (Vec::new(), 1);
                }
                line(text[2])
            }
            ["check-ignore", "--", path] => {
                assert_eq!(Path::new(path), self.root.join("source.yml"));
                if checkout["inventoryIgnored"] == false {
                    return (Vec::new(), 1);
                }
                line(path)
            }
            ["ls-files", "-z", "--", "ops/ansible", "build"] => {
                format!("{}\0", SOURCE_PATHS.join("\0")).into_bytes()
            }
            ["merge-base", "--is-ancestor", selected, tools] => {
                assert_eq!((selected.len(), *tools), (40, revision().as_str()));
                return (Vec::new(), u8::from(checkout["ancestor"] == false));
            }
            other => panic!("unexpected git command {other:?}"),
        };
        (answer, 0)
    }
    fn github(&mut self, text: &[&str]) -> (Vec<u8>, u8) {
        if text == ["--version"] {
            return (b"gh version 2.102.0 (inert fixture)\n".to_vec(), 0);
        }
        let canary = &self.scenario["canary"];
        if text[0] == "variable" {
            let repository = policy::REPOSITORY;
            let origin = self.scenario["report"]["destinationOrigin"]
                .as_str()
                .unwrap();
            let set = [
                "variable",
                "set",
                "CANARY_ORIGIN",
                "--repo",
                repository,
                "--body",
                origin,
            ];
            assert_eq!(text, set);
            return (Vec::new(), u8::from(canary["setFails"] == true));
        }
        assert_eq!(text[0], "api");
        let endpoint = *text.last().unwrap();
        let conclusion = if self.scenario["ci"].is_null() {
            "success"
        } else {
            "failure"
        };
        let answer = if endpoint.ends_with("/git/ref/heads/main") {
            let published = self.scenario["checkout"]["published"].as_str().unwrap();
            self.mains += 1;
            let moved = self.moves == Some("main") && self.mains > 1;
            let sha = if moved {
                "c".repeat(40)
            } else {
                published.to_owned()
            };
            json!({"ref": "refs/heads/main", "object": {"type": "commit", "sha": sha}})
        } else if endpoint.contains("/actions/workflows/ci.yml/runs?") {
            let (_, query) = endpoint.split_once("head_sha=").unwrap();
            self.selected = query[..40].to_owned();
            // The run is selected for the application's revision, not the tools'.
            let moved = &self.scenario["report"]["revision"];
            assert!(
                moved.is_null() || moved == self.selected.as_str(),
                "{moved}"
            );
            json!({"total_count": 1, "workflow_runs": [run_record(&self.selected, conclusion)]})
        } else if endpoint.contains("/artifacts?name=") {
            let ops = endpoint.contains("name=simplestchat-ops-");
            self.listings += 1;
            let moved = self.moves == Some("artifact") && self.listings > 2 && ops;
            let digest = if moved { "e" } else { "b" };
            json!({"total_count": 1, "artifacts": [artifact_record(ops, &self.selected, digest)]})
        } else if endpoint.ends_with("/actions/variables/CANARY_ORIGIN") {
            let values = canary["values"].as_array().unwrap();
            let value = &values[self.canaries.min(values.len() - 1)];
            self.canaries += 1;
            json!({"name": "CANARY_ORIGIN", "value": value})
        } else {
            let run = format!("repos/{}/actions/runs/{RUN}", policy::REPOSITORY);
            assert_eq!(endpoint, run);
            run_record(&self.selected, conclusion)
        };
        (bytes(&answer), 0)
    }
    fn inventory(&mut self, text: &[&str]) -> (Vec<u8>, u8) {
        assert_eq!((text.len(), text[0], text[2]), (3, "-i", "--list"));
        let side = usize::from(text[1].ends_with("target.yml"));
        let name = ["source", "target"][side];
        assert_eq!(Path::new(text[1]), self.root.join(format!("{name}.yml")));
        let answers = self.scenario["inventories"][name].as_array().unwrap();
        let answer = &answers[self.reads[side].min(answers.len() - 1)];
        self.reads[side] += 1;
        (bytes(answer), 0)
    }
    fn play(&mut self, text: &[&str], ceiling: Duration) -> (Vec<u8>, u8) {
        let [first, inventory, playbook, limit, hosts, extra, file] = text else {
            panic!("unexpected play {text:?}");
        };
        assert_eq!((*first, *limit, *extra), ("-i", "--limit", "--extra-vars"));
        let file = Path::new(file.strip_prefix('@').unwrap());
        let (inventory, playbook) = (Path::new(inventory), Path::new(playbook));
        let evidence = inventory.parent().unwrap();
        assert_eq!(
            (evidence, playbook.parent().unwrap()),
            (file.parent().unwrap(), &*self.root.join("ops/ansible"))
        );
        for path in [inventory, file] {
            assert_eq!(
                fs::metadata(path).unwrap().mode() & 0o7777,
                0o600,
                "{path:?}"
            );
        }
        let variables: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        assert_eq!(
            Path::new(variables["scmig_evidence"].as_str().unwrap()),
            evidence
        );
        let frozen: Value = serde_json::from_slice(&fs::read(inventory).unwrap()).unwrap();
        let members = frozen["benchmark_hosts"]["hosts"].as_object().unwrap();
        let name = |path: &Path| path.file_name().unwrap().to_str().unwrap().to_owned();
        let stem = name(playbook).trim_end_matches(".yml").to_owned();
        let action = variables["scmig_action"]
            .as_str()
            .map_or(stem, str::to_owned);
        self.plays.push(Played {
            action: action.clone(),
            playbook: name(playbook),
            inventory: name(inventory),
            limit: (*hosts).to_owned(),
            variables: variables.clone(),
            hosts: members.keys().cloned().collect(),
            ceiling,
        });
        if self.tracked_during == Some(action.as_str()) {
            let tracked = self.root.join(SOURCE_PATHS[2]);
            private(&tracked, b"changed while this step ran\n", 0o600);
        }
        if self.rewrite_during == Some(action.as_str()) {
            for inventory in ["source.yml", "target.yml"] {
                private(
                    &self.root.join(inventory),
                    b"independent operator update\n",
                    0o600,
                );
            }
        }
        if action == "verify" {
            // Controller-local: the native verification's own summary.
            if !self.scenario["attestation"].is_null() {
                return (Vec::new(), 2);
            }
            let directory = self.root.join("results/verify.fixture/artifact");
            let summary = json!({"directory": directory, "image.tar": "c".repeat(64),
                "release.json": "d".repeat(64), "revision": variables["scmig_revision"]});
            let summary = self.application.clone().unwrap_or(summary);
            if !summary.is_null() {
                private(&evidence.join("application.json"), &bytes(&summary), 0o600);
            }
            return (Vec::new(), 0);
        }
        let recorded = self.scenario["plays"]
            .get(self.next)
            .cloned()
            .unwrap_or(Value::Null);
        self.next += 1;
        if let Some(inspection) = recorded.get("inspection").filter(|value| !value.is_null()) {
            private(&evidence.join("inspection.json"), &bytes(inspection), 0o600);
        }
        if let Some(text) = recorded.get("rewrites").and_then(Value::as_str) {
            private(&self.root.join("source.yml"), text.as_bytes(), 0o600);
        }
        (Vec::new(), if recorded["fails"] == true { 2 } else { 0 })
    }
    /// The served revision: the one the recorded bundle named, when it named
    /// exactly one, and otherwise an answer without a usable one.
    fn served(&self) -> Value {
        let requests = self.scenario["requests"].as_array().unwrap();
        let asset = |request: &&Value| request["path"].as_str().unwrap().starts_with("/assets/");
        let Some(bundle) = requests.iter().find(asset) else {
            return json!({"revision": self.selected});
        };
        match revisions(bundle["body"].as_str().unwrap()).as_slice() {
            [one] => json!({"revision": one}),
            [] => json!({"build": "unknown"}),
            several => json!({"revision": several}),
        }
    }
    fn request(&mut self, text: &[&str]) -> (Vec<u8>, u8) {
        let url = *text.last().unwrap();
        let origin = self.scenario["report"]["destinationOrigin"]
            .as_str()
            .unwrap();
        let path = url.strip_prefix(origin).unwrap().to_owned();
        let pinned = text.iter().position(|word| *word == "--resolve");
        let pinned = pinned.map(|at| text[at + 1].to_owned());
        let words = text.iter().map(|word| (*word).to_owned()).collect();
        self.requests.push((path.clone(), pinned, words));
        let requests = self.scenario["requests"].as_array().unwrap();
        if requests.iter().any(|request| request["fails"] == true) {
            return (Vec::new(), 7);
        }
        let recorded = requests
            .iter()
            .find(|request| request["path"] == path.as_str());
        let body = match (path.as_str(), recorded) {
            ("/app-version.json", _) => self.served().to_string(),
            (_, Some(recorded)) => recorded["body"].as_str().unwrap().to_owned(),
            ("/health", None) => "{\"status\": \"ok\"}".to_owned(),
            ("/ready", None) => "{\"status\": \"ready\"}".to_owned(),
            ("/", None) => "<!doctype html><html></html>".to_owned(),
            (_, None) => "{\"auth\": true}".to_owned(),
        };
        let code = requests.iter().find_map(|request| request["code"].as_str());
        (format!("{body}\n{}", code.unwrap_or("200")).into_bytes(), 0)
    }
    fn lookup(&mut self, text: &[&str]) -> (Vec<u8>, u8) {
        self.lookups
            .push(text.iter().map(|word| (*word).to_owned()).collect());
        let wide = *text.last().unwrap() == "AAAA";
        let observation = self.queries / 2;
        self.queries += 1;
        match &self.resolver {
            Some(Ok(printed)) => return (printed.clone().into_bytes(), 0),
            Some(Err(status)) => return (Vec::new(), *status),
            None => {}
        }
        let observations = self.scenario["dns"].as_array().unwrap();
        let Some(last) = observations.len().checked_sub(1) else {
            return (Vec::new(), 0);
        };
        let addresses = observations[observation.min(last)]
            .as_array()
            .unwrap()
            .iter();
        let family = addresses
            .filter_map(Value::as_str)
            .filter(|address| address.contains(':') == wide);
        (
            family
                .map(|address| format!("{address}\n"))
                .collect::<String>()
                .into_bytes(),
            0,
        )
    }
    fn response(
        &mut self,
        program: Program,
        arguments: &[OsString],
        ceiling: Duration,
    ) -> (Vec<u8>, u8) {
        let text: Vec<&str> = arguments
            .iter()
            .map(|word| word.to_str().unwrap())
            .collect();
        match program {
            Program::FrontendGit => self.git(&text),
            Program::FrontendGh => self.github(&text),
            Program::FrontendInventory => self.inventory(&text),
            Program::FrontendAnsible => self.play(&text, ceiling),
            Program::FrontendCurl => self.request(&text),
            Program::FrontendDig => self.lookup(&text),
            other => panic!("unexpected program {other:?}"),
        }
    }
}
impl Executor for Script {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let proof = command.release.clone();
        let proof = proof
            .filter(|proof| proof.role() == ReleaseRole::Frontend)
            .unwrap();
        proof.begin(&command)?;
        // Each role runs its own tool, the name service's included.
        let tool = proof.validate(command.program, &command.arguments)?;
        let named = match command.program {
            Program::FrontendGit => "git",
            Program::FrontendGh => "gh",
            Program::FrontendInventory => "ansible-inventory",
            Program::FrontendAnsible => "ansible-playbook",
            Program::FrontendCurl => "curl",
            Program::FrontendDig => "dig",
            other => panic!("unexpected program {other:?}"),
        };
        assert_eq!(tool, self.root.join("bin").join(named));
        let (_, environment) = proof.context(command.program, &command.arguments)?.unwrap();
        let credentials = matches!(
            command.program,
            Program::FrontendGh | Program::FrontendAnsible
        );
        assert_eq!(
            environment.contains_key(OsStr::new("GH_TOKEN")),
            credentials
        );
        assert!(!environment.contains_key(OsStr::new("HTTPS_PROXY")));
        self.programs.push(command.program);
        let (output, status) = self.response(command.program, &command.arguments, command.timeout);
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap())
            .ok()
            .unwrap();
        let mut stdout = logs.stdout.writer()?;
        stdout.write_all(&output).map_err(|_| failure(1))?;
        nix::unistd::close(stdout).map_err(|_| failure(1))?;
        nix::unistd::close(logs.stderr.writer()?).map_err(|_| failure(1))?;
        let (out, err) = tokio::join!(logs.stdout.finish(), logs.stderr.finish());
        let seal = |done: simplestchat_ci::bounded_log::Finished| Seal {
            sha256: done.sha256,
            bytes: done.bytes,
            metadata: done.metadata,
        };
        proof.observed_io(Some(Captures {
            stdout: seal(out?),
            stderr: seal(err?),
        }));
        proof.observed_exit(Some(i32::from(status)), true, true);
        proof.completed()?;
        match status {
            0 => Ok(Vec::new()),
            status => Err(Failure::child_exit(status)),
        }
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
    async fn replay(&mut self, notice: &[u8], stderr: bool, _: Instant) -> Result<()> {
        assert!(!stderr);
        self.notices.push(serde_json::from_slice(notice).unwrap());
        Ok(())
    }
}

/// Where the native controller answers a recorded condition with its own
/// vocabulary or at its own step: the scenario, the native class and phase.
const RENAMED: [(&str, &str, &str); 10] = [
    // One admission judges the whole checkout; its parts are not named apart.
    (
        "a dirty checkout contacts no host",
        "checkout_identity_invalid",
        "preflight",
    ),
    (
        "another branch contacts no host",
        "checkout_identity_invalid",
        "preflight",
    ),
    (
        "another repository contacts no host",
        "checkout_identity_invalid",
        "preflight",
    ),
    // The published tools are proven through the API with the selection.
    (
        "an unpublished checkout contacts no host",
        "published_main_mismatch",
        "ci",
    ),
    // The signed application is verified by the first, controller-local step.
    (
        "a failed attestation precedes every host call",
        "migration_step_failed",
        "verify",
    ),
    (
        "an inventory that changes during attestation is refused",
        "inventory_target_changed",
        "recheck",
    ),
    // A missing media address is a named refusal, not an unnamed fault.
    (
        "a kept hostname without a media address",
        "migration_literal_addresses_required",
        "preflight",
    ),
    // The served revision is read from the application's own version record.
    (
        "a frontend built from another revision",
        "migration_https_revision",
        "https",
    ),
    (
        "a frontend that names no revision",
        "migration_https_revision",
        "https",
    ),
    (
        "a frontend that names two revisions",
        "migration_https_revision",
        "https",
    ),
];
/// The home page's script layout is no longer read, so this move completes.
const ADMITTED: &str = "a home page without a module script";

fn checkpoints(evidence: &Path) -> Vec<Value> {
    let mut names: Vec<String> = fs::read_dir(evidence)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("phase-"))
        .collect();
    names.sort();
    let read = |name: &String| serde_json::from_slice(&fs::read(evidence.join(name)).unwrap());
    names.iter().map(|name| read(name).unwrap()).collect()
}

/// Compare one native move with what the Python controller did under the
/// same conditions: the verdict, the boundaries, every play and checkpoint.
fn compare(fixture: &Fixture, scenario: &Value, script: &Script, report: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let expected = &scenario["report"];
    let renamed = RENAMED.iter().find(|(scenario, ..)| *scenario == name);
    if name == ADMITTED {
        assert_eq!(
            (&report["passed"], &report["phase"]),
            (&json!(true), &json!("complete"))
        );
        return;
    }
    let (class, failed) = match renamed {
        Some((_, class, phase)) => (json!(class), json!(phase)),
        None => (
            expected["failureClass"].clone(),
            expected["failedPhase"].clone(),
        ),
    };
    assert_eq!(report["failureClass"], class, "{name}");
    assert_eq!(
        report.get("failedPhase").unwrap_or(&Value::Null),
        &failed,
        "{name}"
    );
    for field in [
        "passed",
        "phase",
        "recovery",
        "sourceOrigin",
        "destinationOrigin",
        "repository",
        "sourceFreezeAttempted",
        "destinationStartupAttempted",
        "destinationStarted",
        "inventoryActivated",
        "inventoryActivationRequested",
        "canaryUpdateAttempted",
        "canaryUpdated",
        "canaryUpdateRequested",
        "dnsCutoverVerified",
        "revision",
        "toolRevision",
    ] {
        // The two controllers learn the checkout's identities at different
        // moments of an early refusal; where both report one, they agree.
        let identity = ["repository", "revision", "toolRevision"].contains(&field);
        if identity && (report.get(field).is_none() || expected.get(field).is_none()) {
            continue;
        }
        assert_eq!(report.get(field), expected.get(field), "{name}: {field}");
    }
    // The plays, after the controller-local verification the native move adds.
    let plays = scenario["plays"].as_array().unwrap();
    let verified = script.plays.first().filter(|play| play.action == "verify");
    let native = &script.plays[usize::from(verified.is_some())..];
    let actions = |plays: &[Played]| {
        plays
            .iter()
            .map(|play| play.action.clone())
            .collect::<Vec<_>>()
    };
    let recorded: Vec<&str> = plays
        .iter()
        .map(|play| play["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions(native), recorded, "{name}");
    assert_eq!(verified.is_some(), report.get("runId").is_some(), "{name}");
    let base = scenario["variables"].as_object();
    for (play, expected) in native.iter().zip(plays) {
        let context = format!("{name}: {}", play.action);
        assert_eq!(expected["playbook"], play.playbook.as_str(), "{context}");
        assert_eq!(expected["inventory"], play.inventory.as_str(), "{context}");
        assert_eq!(expected["limit"], play.limit.as_str(), "{context}");
        let both = ["migration_source", "migration_target"];
        let members = if play.limit.contains(':') {
            &both[..]
        } else {
            &both[1..]
        };
        assert_eq!(play.hosts, members, "{context}");
        // Staging also verifies and delivers the signed Ops executable now.
        let seconds = if play.action == "stage" {
            json!(5400)
        } else {
            expected["timeout"].clone()
        };
        assert_eq!(json!(play.ceiling.as_secs()), seconds, "{context}");
        for (key, value) in expected["extra"].as_object().unwrap() {
            assert_eq!(&play.variables[key], value, "{context}: {key}");
        }
        for key in expected["absent"].as_array().unwrap() {
            assert!(
                play.variables.get(key.as_str().unwrap()).is_none(),
                "{context}: {key}"
            );
        }
        let extras = expected["extra"].as_object().unwrap();
        for key in [
            "scmig_source_origin",
            "scmig_destination_origin",
            "scmig_revision",
            "scbench_revision",
            "scbench_repository",
            "scpub_release_revision",
            "scpub_release_expected_revision",
            "scpub_release_repository",
            "scpub_release_artifact_id",
            "scpub_release_ci_run",
            "scpub_webauthn_rp_id",
            "scpub_turn_verify_address",
        ] {
            let recorded = base.and_then(|base| base.get(key));
            let absent = expected["absent"].as_array().unwrap().contains(&json!(key));
            if extras.contains_key(key) || absent {
                continue;
            }
            assert_eq!(play.variables.get(key), recorded, "{context}: {key}");
        }
        assert_eq!(play.variables["scmig_run_id"], report["runId"], "{context}");
    }
    // Every checkpoint, in order, with the boundaries it recorded.
    let written = fixture
        .evidence()
        .map_or(Vec::new(), |evidence| checkpoints(&evidence));
    let written = &written[usize::from(verified.is_some())..];
    let recorded = scenario["checkpoints"].as_array().unwrap();
    assert_eq!(written.len(), recorded.len(), "{name}");
    for (written, recorded) in written.iter().zip(recorded) {
        for (field, value) in recorded.as_object().unwrap() {
            assert_eq!(&written[field], value, "{name}: {}", recorded["phase"]);
        }
    }
    // Anonymous requests reach the destination by address only while its
    // kept name still resolves to the source.
    let rounds: Vec<&Value> = scenario["requests"].as_array().unwrap().iter().collect();
    for (index, (path, pinned, _)) in script.requests.iter().enumerate() {
        let round = index / PATHS.len();
        assert_eq!(path, PATHS[index % PATHS.len()], "{name}");
        let recorded = rounds
            .get(round * PATHS.len())
            .map(|request| &request["resolve"]);
        if let Some(recorded) = recorded {
            assert_eq!(&json!(pinned), recorded, "{name}: request {index}");
        }
    }
    // The canary variable is read exactly as often, and a wait of zero is
    // one observation of both families.
    let reads = scenario["canary"]["values"].as_array().unwrap().len();
    assert_eq!(script.canaries, reads, "{name}");
    let waits = scenario["arguments"].as_array().unwrap();
    let once = waits
        .windows(2)
        .any(|pair| pair == [json!("--dns-wait-seconds"), json!("0")]);
    if once {
        assert_eq!(script.lookups.len(), 2, "{name}");
    }
    // The inventory's original bytes are retained exactly when it may be replaced.
    if let Some(evidence) = fixture.evidence() {
        let retained = fs::read(evidence.join("original-inventory")).ok();
        let requested = expected["inventoryActivationRequested"] == true;
        let original = requested.then_some(&b"offline fixture\n"[..]);
        assert_eq!(retained.as_deref(), original, "{name}");
    }
    // The operator's notice and own inventory.
    let notice = scenario["notice"].as_str().unwrap();
    let notices: Vec<Value> = notice
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(script.notices, notices, "{name}");
    let inventory = fs::read_to_string(fixture.root.join("source.yml")).unwrap();
    let mode = fs::metadata(fixture.root.join("source.yml"))
        .unwrap()
        .mode()
        & 0o7777;
    assert_eq!(
        json!(format!("0o{mode:o}")),
        scenario["inventory"]["mode"],
        "{name}"
    );
    let kept = scenario["inventory"]["text"].as_str().unwrap();
    if expected["inventoryActivated"] == true {
        let switched: Value = serde_json::from_str(&inventory).unwrap();
        assert_eq!(
            switched,
            serde_json::from_str::<Value>(kept).unwrap(),
            "{name}"
        );
        assert!(
            inventory.ends_with("}\n") && inventory.starts_with("{\n  \""),
            "{name}"
        );
    } else {
        assert_eq!(inventory, kept, "{name}");
    }
    let siblings = fs::read_dir(&fixture.root).unwrap();
    let left = siblings.filter(|entry| {
        let name = entry.as_ref().unwrap().file_name();
        name.to_str().unwrap().starts_with(".migration-inventory-")
    });
    assert_eq!(left.count(), 0, "{name}");
}

#[tokio::test]
async fn the_recorded_moves_replay_through_the_native_controller() {
    let recorded = recorded();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 61);
    let (mut moved, mut refused, mut complete) = (0, 0, 0);
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let fixture = Fixture::new(scenario);
        let arguments = fixture.arguments(scenario);
        if scenario["report"].is_null() {
            // Refused on the command line, before any work.
            assert!(scenario["refused"].is_string(), "{name}");
            let error = super::super::options::parse(&arguments).unwrap_err();
            assert_eq!(error.code, 2, "{name}");
            refused += 1;
            continue;
        }
        let mut script = Script::new(&fixture, scenario);
        let (result, report) = fixture.run(&mut script, &arguments).await;
        let completed = result.unwrap();
        assert_eq!(completed.summary, report, "{name}");
        assert_eq!(completed.status == 0, report["passed"] == true, "{name}");
        compare(&fixture, scenario, &script, &report);
        // No host is contacted before both targets are frozen.
        if report.get("runId").is_none() {
            assert_eq!(script.count(Program::FrontendAnsible), 0, "{name}");
        }
        complete += usize::from(report["passed"] == true);
        moved += 1;
    }
    // Thirteen recorded moves completed; the native one admits one more.
    assert_eq!((moved, refused, complete), (55, 6, 14));
}

fn scenario(recorded: &Value, name: &str) -> Value {
    let scenarios = recorded["scenarios"].as_array().unwrap();
    let found = scenarios.iter().find(|scenario| scenario["name"] == name);
    found
        .unwrap_or_else(|| panic!("no scenario named {name:?}"))
        .clone()
}

/// What the recording cannot know: the typed signed selections every play
/// receives, the verified originals later plays reuse, and the private
/// evidence a move leaves.
#[tokio::test]
async fn a_move_carries_its_signed_selection_and_leaves_private_evidence() {
    let usual = scenario(
        &recorded(),
        "a move to another hostname with every service enabled",
    );
    let fixture = Fixture::new(&usual);
    let mut script = Script::new(&fixture, &usual);
    let (result, report) = fixture.run(&mut script, &fixture.arguments(&usual)).await;
    assert_eq!(result.unwrap().status, 0);
    let expected = json!({"ciRunId": RUN, "runAttempt": 2, "applicationArtifactId": APPLICATION,
        "opsArtifactId": OPS, "githubAttested": true, "passed": true, "phase": "complete"});
    for (field, value) in expected.as_object().unwrap() {
        assert_eq!(&report[field], value, "{field}");
    }
    let evidence = fixture.evidence().unwrap();
    assert_eq!(report["evidence"], json!(evidence));
    assert!(
        evidence
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("migration.")
    );
    // The first step contacts no host and has no verified originals yet.
    let first = &script.plays[0];
    let both = "migration_source:migration_target";
    assert_eq!(
        (first.action.as_str(), first.limit.as_str()),
        ("verify", both)
    );
    assert_eq!(first.playbook, "migration-prepare.yml");
    assert!(
        first
            .variables
            .get("scpub_release_verified_directory")
            .is_none()
    );
    assert!(first.variables.get("scpub_webauthn_rp_id").is_none());
    let directory = json!(fixture.root.join("results/verify.fixture/artifact"));
    let selection = json!({"repository": policy::REPOSITORY, "revision": revision(),
        "packageVersion": env!("CARGO_PKG_VERSION"), "protocolVersion": 1,
        "target": "x86_64-unknown-linux-musl", "buildRunId": RUN, "runAttempt": 2});
    for play in &script.plays {
        let variables = &play.variables;
        assert_eq!(
            variables["scpub_ops_selection"], selection,
            "{}",
            play.action
        );
        assert_eq!(variables["scpub_ops_artifact_id"], OPS, "{}", play.action);
        let controller = json!(fixture.root.join("bin/simplestchat-release"));
        assert_eq!(variables["scpub_ops_release_controller"], controller);
        assert_eq!(
            variables["scpub_ops_gh_executable"],
            json!(fixture.root.join("bin/gh"))
        );
        assert!(variables.get("scpub_ops_artifact_zip").is_none());
        assert!(variables.get("scpub_turn_verify_address").is_none());
        if play.action != "verify" {
            assert_eq!(variables["scpub_release_verified_directory"], directory);
        }
    }
    let ceilings: Vec<(&str, u64)> = script
        .plays
        .iter()
        .map(|play| (play.action.as_str(), play.ceiling.as_secs()))
        .collect();
    assert_eq!(
        &ceilings[..6],
        [
            ("verify", 2400),
            ("inspect", 2400),
            ("site", 2400),
            ("bootstrap", 2400),
            ("stage", 5400),
            ("public", 2400)
        ]
    );
    // One window of bounded, retried, anonymous requests by name.
    let (path, pinned, words) = &script.requests[0];
    assert_eq!((path.as_str(), pinned), ("/health", &None));
    let fixed = [
        "--disable",
        "--silent",
        "--show-error",
        "--fail",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--noproxy",
        "*",
        "--connect-timeout",
        "5",
        "--max-time",
        "15",
        "--retry",
        "30",
        "--retry-delay",
        "1",
        "--retry-max-time",
    ];
    assert_eq!(&words[..fixed.len()], fixed);
    let retries: u64 = words[fixed.len()].parse().unwrap();
    assert!((70..=75).contains(&retries), "{retries}");
    let rest = [
        "--retry-all-errors",
        "--max-filesize",
        "131072",
        "--request",
        "GET",
        "--header",
        "Accept-Encoding: identity",
        "--write-out",
        "\n%{http_code}",
        "https://next.chat.example.test/health",
    ];
    assert_eq!(&words[fixed.len() + 1..], rest);
    assert_eq!(script.requests.len(), PATHS.len());
    assert_eq!(script.count(Program::FrontendDig), 0);
    // Everything retained is private, and the frozen inventories are exact.
    let mut names: Vec<String> = fs::read_dir(&evidence)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    for name in &names {
        let mode = fs::metadata(evidence.join(name)).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o600, "{name}");
    }
    let kept = |prefix: &str| names.iter().filter(|name| name.starts_with(prefix)).count();
    assert_eq!((kept("phase-"), kept("play-")), (14, 13));
    for name in [
        "request.json",
        "variables.json",
        "outcome.json",
        "application.json",
        "inspection.json",
        "source-inventory.json",
        "target-inventory.json",
        "migration-inventory.json",
    ] {
        assert!(names.iter().any(|kept| kept == name), "{name}");
    }
    assert!(!names.iter().any(|name| name == "original-inventory"));
    let read = |name: &str| -> Value {
        serde_json::from_slice(&fs::read(evidence.join(name)).unwrap()).unwrap()
    };
    let source = &usual["inventories"]["source"][0]["_meta"]["hostvars"]["public"];
    let target = &usual["inventories"]["target"][0]["_meta"]["hostvars"]["public"];
    let frozen = |hosts: Value| json!({"benchmark_hosts": {"hosts": hosts}});
    assert_eq!(
        read("source-inventory.json"),
        frozen(json!({"migration_source": source}))
    );
    assert_eq!(
        read("target-inventory.json"),
        frozen(json!({"migration_target": target}))
    );
    let pair = json!({"migration_source": source, "migration_target": target});
    assert_eq!(read("migration-inventory.json"), frozen(pair));
    assert_eq!(read("variables.json"), script.plays[2].variables);
    let request = read("request.json");
    assert_eq!(request["operation"], "migrate");
    assert_eq!(request["tools"]["gh"], json!(fixture.root.join("bin/gh")));
    assert_eq!(read("outcome.json"), report);
}

/// The verification's own summary must name the originals of this revision
/// before anything is installed anywhere.
#[tokio::test]
async fn a_verification_that_left_no_exact_record_stops_before_any_host() {
    let usual = scenario(
        &recorded(),
        "a move to another hostname with every service enabled",
    );
    let other = json!({"directory": "/private/verified", "image.tar": "c".repeat(64),
        "release.json": "d".repeat(64), "revision": "b".repeat(40)});
    for (left, class) in [
        (other, "migration_application_record"),
        (
            json!({"directory": "/private/verified"}),
            "migration_application_record",
        ),
        (Value::Null, "migration_step_failed"),
    ] {
        let fixture = Fixture::new(&usual);
        let mut script = Script::new(&fixture, &usual);
        script.application = Some(left.clone());
        let (result, report) = fixture.run(&mut script, &fixture.arguments(&usual)).await;
        assert_eq!(result.unwrap().status, 1, "{left}");
        let verdict = (
            &report["failureClass"],
            &report["failedPhase"],
            &report["recovery"],
        );
        assert_eq!(
            verdict,
            (&json!(class), &json!("verify"), &json!("source_untouched"))
        );
        assert_eq!(report["githubAttested"], false);
        let actions: Vec<&str> = script
            .plays
            .iter()
            .map(|play| play.action.as_str())
            .collect();
        assert_eq!(actions, ["verify"], "{left}");
    }
}

/// After the freeze every play reads the private copies, so an operator who
/// edits their own inventories mid-move disturbs nothing; the same edit made
/// while the release is still being selected stops the move before any host.
#[tokio::test]
async fn only_the_frozen_inventories_matter_once_the_hosts_are_selected() {
    let usual = scenario(
        &recorded(),
        "a move to another hostname with every service enabled",
    );
    let fixture = Fixture::new(&usual);
    let mut script = Script::new(&fixture, &usual);
    script.rewrite_during = Some("bootstrap");
    let (result, report) = fixture.run(&mut script, &fixture.arguments(&usual)).await;
    assert_eq!(result.unwrap().status, 0);
    assert_eq!(
        (&report["passed"], &report["phase"]),
        (&json!(true), &json!("complete"))
    );
    let updated = fs::read(fixture.root.join("source.yml")).unwrap();
    assert_eq!(updated, b"independent operator update\n");
    assert_eq!(script.plays.len(), 13);
    // A tracked controller input that changes mid-move is another matter:
    // the step it changed under is not accepted and no further one is issued.
    let fixture = Fixture::new(&usual);
    let mut script = Script::new(&fixture, &usual);
    script.tracked_during = Some("site");
    let (result, report) = fixture.run(&mut script, &fixture.arguments(&usual)).await;
    assert_eq!(result.unwrap().status, 1);
    let verdict = (
        &report["failureClass"],
        &report["failedPhase"],
        &report["recovery"],
    );
    let expected = (
        &json!("migration_step_failed"),
        &json!("site"),
        &json!("source_untouched"),
    );
    assert_eq!(verdict, expected);
    assert_eq!(report["sourceFreezeAttempted"], false);
    let actions: Vec<&str> = script
        .plays
        .iter()
        .map(|play| play.action.as_str())
        .collect();
    assert_eq!(actions, ["verify", "inspect", "site"]);
}

/// Retained signed inputs an operator supplies reach the plays that verify
/// them: the application's directory from the first step on, the Ops ZIP in
/// every play, both recorded in the request.
#[tokio::test]
async fn retained_signed_inputs_reach_the_steps_that_verify_them() {
    let usual = scenario(&recorded(), "only backups enabled");
    let fixture = Fixture::new(&usual);
    let retained = fixture.root.join("retained");
    let (application, ops) = (retained.join("application"), retained.join("ops.zip"));
    fs::create_dir(&retained).unwrap();
    fs::create_dir(&application).unwrap();
    for directory in [&retained, &application] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    private(&ops, b"inert retained candidate\n", 0o600);
    let mut arguments = fixture.arguments(&usual);
    let supplied = [
        ("--application-artifact-dir", &application),
        ("--ops-artifact-zip", &ops),
    ];
    for (flag, path) in supplied {
        arguments.extend([flag.to_owned(), path.to_str().unwrap().to_owned()]);
    }
    let mut script = Script::new(&fixture, &usual);
    // The native verification names the directory it was given.
    script.application = Some(
        json!({"directory": application, "image.tar": "c".repeat(64),
        "release.json": "d".repeat(64), "revision": revision()}),
    );
    let (result, report) = fixture.run(&mut script, &arguments).await;
    assert_eq!(result.unwrap().status, 0);
    assert_eq!(report["passed"], true);
    assert_eq!(script.plays[0].action, "verify");
    for play in &script.plays {
        let variables = &play.variables;
        assert_eq!(
            variables["scpub_release_verified_directory"],
            json!(application)
        );
        assert_eq!(
            variables["scpub_ops_artifact_zip"],
            json!(ops),
            "{}",
            play.action
        );
    }
    let evidence = fixture.evidence().unwrap();
    let request: Value =
        serde_json::from_slice(&fs::read(evidence.join("request.json")).unwrap()).unwrap();
    assert_eq!(request["applicationArtifactDirectory"], json!(application));
    assert_eq!(request["opsArtifactZip"], json!(ops));
    // A retained input that is not private is refused before anything runs.
    let fixture = Fixture::new(&usual);
    let loose = fixture.root.join("retained.zip");
    private(&loose, b"inert retained candidate\n", 0o644);
    let mut arguments = fixture.arguments(&usual);
    arguments.extend([
        "--ops-artifact-zip".to_owned(),
        loose.to_str().unwrap().to_owned(),
    ]);
    let mut script = Script::new(&fixture, &usual);
    let (result, report) = fixture.run(&mut script, &arguments).await;
    assert_eq!(result.unwrap().status, 1);
    assert_eq!(report["failureClass"], "migration_step_failed");
    assert!(script.programs.is_empty() && fixture.evidence().is_none());
}

/// A kept hostname is reached by the destination's address until its name
/// follows, and the name service's answers are read strictly: both families
/// must match, a failing query is asked again until the wait ends, and
/// nonsense ends the move.
#[tokio::test]
async fn a_kept_hostname_is_pinned_by_address_and_its_answers_are_read_strictly() {
    let recorded = recorded();
    let wide = scenario(&recorded, "a kept hostname with an IPv6 address");
    let fixture = Fixture::new(&wide);
    let mut script = Script::new(&fixture, &wide);
    let (result, report) = fixture.run(&mut script, &fixture.arguments(&wide)).await;
    assert_eq!(result.unwrap().status, 0);
    assert_eq!(report["dnsCutoverVerified"], true);
    let pinned = Some("chat.example.test:443:192.0.2.11".to_owned());
    let pins: Vec<&Option<String>> = script.requests.iter().map(|(_, pin, _)| pin).collect();
    assert_eq!(
        pins,
        [vec![&pinned; PATHS.len()], vec![&None; PATHS.len()]].concat()
    );
    let words = &script.requests[0].2;
    let at = words.iter().position(|word| word == "--resolve").unwrap();
    assert_eq!(at, words.len() - 3);
    let relay = script
        .plays
        .iter()
        .find(|play| play.action == "turn")
        .unwrap();
    assert_eq!(relay.variables["scpub_turn_verify_address"], "192.0.2.11");
    let query = ["+short", "+timeout=2", "+tries=1", "chat.example.test"];
    let asked: Vec<Vec<&str>> = script
        .lookups
        .iter()
        .map(|words| words.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        asked,
        [
            [&query[..], &["A"]].concat(),
            [&query[..], &["AAAA"]].concat()
        ]
    );
    assert_eq!(
        script.notices,
        [json!({"phase": "awaiting_dns_cutover",
        "domain": "chat.example.test", "addresses": ["192.0.2.11", "2001:db8::11"],
        "waitSeconds": 600})]
    );
    // Only one of the two families following is not a cutover.
    let mut half = wide.clone();
    half["dns"] = json!([["192.0.2.11"]]);
    let waiting = [json!("--dns-wait-seconds"), json!("0")];
    half["arguments"].as_array_mut().unwrap().extend(waiting);
    let fixture = Fixture::new(&half);
    let mut script = Script::new(&fixture, &half);
    let (result, report) = fixture.run(&mut script, &fixture.arguments(&half)).await;
    assert_eq!(result.unwrap().status, 1);
    let timeout = "migration_dns_cutover_timeout_source_remains_sealed";
    assert_eq!(
        (&report["failureClass"], script.lookups.len()),
        (&json!(timeout), 2)
    );

    let kept = scenario(&recorded, "a hostname that resolves to both hosts");
    let cases: [(std::result::Result<&str, u8>, &str); 3] = [
        (Ok("not an address\n"), "migration_dns_response_invalid"),
        (
            Ok("2001:db8::11\n"),
            "migration_dns_address_family_mismatch",
        ),
        (
            Err(9),
            "migration_dns_cutover_timeout_source_remains_sealed",
        ),
    ];
    for (printed, class) in cases {
        let fixture = Fixture::new(&kept);
        let mut script = Script::new(&fixture, &kept);
        script.resolver = Some(printed.map(str::to_owned));
        let (result, report) = fixture.run(&mut script, &fixture.arguments(&kept)).await;
        assert_eq!(result.unwrap().status, 1, "{class}");
        let verdict = (&report["failureClass"], &report["failedPhase"]);
        assert_eq!(verdict, (&json!(class), &json!("awaiting_dns_cutover")));
        let frozen = "manual_destination_recovery_source_must_remain_frozen";
        assert_eq!(report["recovery"], frozen);
        // The first family's answer already decided each of these.
        assert_eq!(script.lookups.len(), 1, "{class}");
    }
}

/// Without the name service's query tool a kept hostname cannot be moved,
/// and a move to a new hostname never needs the tool.
#[tokio::test]
async fn the_name_service_tool_is_required_only_for_a_kept_hostname() {
    let recorded = recorded();
    let kept = scenario(&recorded, "a move that keeps the hostname waits for DNS");
    let fixture = Fixture::new(&kept);
    fs::remove_file(fixture.root.join("bin/dig")).unwrap();
    let mut script = Script::new(&fixture, &kept);
    let (result, report) = fixture.run(&mut script, &fixture.arguments(&kept)).await;
    assert_eq!(result.unwrap().status, 1);
    assert_eq!(report["failureClass"], "migration_dns_tool_missing");
    assert!(script.programs.is_empty() && fixture.evidence().is_none());
    let usual = scenario(&recorded, "only backups enabled");
    let fixture = Fixture::new(&usual);
    fs::remove_file(fixture.root.join("bin/dig")).unwrap();
    let mut script = Script::new(&fixture, &usual);
    let (result, _) = fixture.run(&mut script, &fixture.arguments(&usual)).await;
    assert_eq!(result.unwrap().status, 0);
}

#[test]
fn every_step_has_its_own_share_of_one_envelope() {
    let arguments = |extra: &[&str]| -> Vec<String> {
        let base = [
            "--source-inventory",
            "/private/source.yml",
            "--source-origin",
            "https://chat.example.test",
            "--inventory",
            "/private/target.yml",
            "--origin",
            "https://chat.example.test",
            "--repository",
            policy::REPOSITORY,
        ];
        base.iter()
            .chain(extra)
            .map(|word| (*word).to_owned())
            .collect()
    };
    let parse = |extra: &[&str]| {
        super::super::options::parse(&arguments(extra))
            .unwrap()
            .unwrap()
    };
    let seconds =
        |options: &Options| -> Vec<u64> { phases(options).iter().map(Duration::as_secs).collect() };
    let usual = seconds(&parse(&[]));
    let play = 2400 + 290;
    let data = 5400 + 290;
    assert_eq!(
        usual,
        [
            540, 3960, 540, play, play, play, play, data, play, data, data, 150, play, play, play,
            play, data, 660, 150, 60, 150, data, data
        ]
    );
    assert_eq!(usual.len(), Step::ALL.len());
    for (index, step) in Step::ALL.iter().enumerate() {
        assert_eq!(step.index(), index);
    }
    let short = seconds(&parse(&["--wait-seconds", "0", "--dns-wait-seconds", "0"]));
    assert_eq!(
        (short[Step::Select.index()], short[Step::Cutover.index()]),
        (360, 70)
    );
    let playbooks: Vec<&str> = Step::ALL
        .iter()
        .filter_map(|step| step.play())
        .map(|play| play.0)
        .collect();
    assert_eq!(playbooks.len(), 15);
    assert!(playbooks.iter().all(|name| name.ends_with(".yml")));
}

/// Conditions no recorded scenario has: two inventories reaching one host,
/// results git does not ignore, an inspection too large to be one, and a
/// checkout, published main or artifact that moved while the release was
/// being selected. Each stops the move before any host is frozen or changed.
#[tokio::test]
async fn nothing_is_frozen_when_the_selection_or_the_checkout_cannot_be_trusted() {
    let usual = scenario(
        &recorded(),
        "a move to another hostname with every service enabled",
    );
    let mut same = usual.clone();
    for inventory in same["inventories"]["target"].as_array_mut().unwrap() {
        inventory["_meta"]["hostvars"]["public"]["ansible_host"] = json!("192.0.2.10");
    }
    let mut large = usual.clone();
    large["plays"][0]["inspection"]["padding"] = json!("x".repeat(INSPECTION));
    type Setup = fn(&mut Script);
    let cases: [(&Value, Setup, &str, &str, &[&str]); 6] = [
        (
            &same,
            |_| {},
            "migration_distinct_hosts_required",
            "preflight",
            &[],
        ),
        (
            &usual,
            |script| script.results_ignored = false,
            "results_must_be_git_ignored",
            "preflight",
            &[],
        ),
        (
            &usual,
            |script| script.moves = Some("head"),
            "checkout_changed",
            "recheck",
            &[],
        ),
        (
            &usual,
            |script| script.moves = Some("main"),
            "published_main_mismatch",
            "recheck",
            &[],
        ),
        (
            &usual,
            |script| script.moves = Some("artifact"),
            "migration_step_failed",
            "recheck",
            &[],
        ),
        (
            &large,
            |_| {},
            "migration_inspection_file",
            "inspect",
            &["verify", "inspect"],
        ),
    ];
    for (conditions, setup, class, phase, played) in cases {
        let fixture = Fixture::new(conditions);
        let mut script = Script::new(&fixture, conditions);
        setup(&mut script);
        let (result, report) = fixture
            .run(&mut script, &fixture.arguments(conditions))
            .await;
        assert_eq!(result.unwrap().status, 1, "{class}");
        let verdict = (
            &report["failureClass"],
            &report["failedPhase"],
            &report["recovery"],
        );
        assert_eq!(
            verdict,
            (&json!(class), &json!(phase), &json!("source_untouched"))
        );
        let actions: Vec<&str> = script
            .plays
            .iter()
            .map(|play| play.action.as_str())
            .collect();
        assert_eq!(actions, played, "{class}");
        assert_eq!(report.get("runId").is_some(), !played.is_empty(), "{class}");
    }
}
