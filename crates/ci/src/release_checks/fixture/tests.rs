//! Owned-fake fixture runs: a model engine answers every command the fixture
//! issues, so whole runs, each failure stage and each refusal are checked
//! without Docker, a browser, a network request or root.

use super::{
    execute::{Fixture, Inputs},
    host::{Commands, Execution, Paths, Request, Result, Runner, Stop, Window},
    images::{self, Kind},
    plan, policy,
    report::Phase,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

#[path = "native_tests.rs"]
mod native;
#[path = "oracle_tests.rs"]
mod oracle;
#[path = "render_tests.rs"]
mod render;

pub(super) const TOKEN: &str = "cccccccccccccccccccccccccccccccc";
const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";
const BASE_IMAGE: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

/// The checkout these tests were built from.
pub(super) fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A private directory of this test process, removed on drop.
pub(super) struct Scratch(pub PathBuf);
impl Scratch {
    pub(super) fn new(name: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-release-fixture-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn words(request: &Request) -> Vec<String> {
    request
        .argv
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect()
}

#[derive(Clone, Debug)]
struct Container {
    service: &'static str,
    image: String,
    running: bool,
    exit: i64,
    started: u32,
}

/// What the model answers instead of the normal response, at one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Injected {
    /// Exit with this status, writing nothing.
    Status(i32),
    /// The command outlives its bound.
    Timeout,
    /// A signal stops it.
    Interrupt,
}

/// The render command's added environment and its working directory.
type Rendering = (Vec<(String, String)>, Option<PathBuf>);
/// Answers replaced by operation and the n-th occurrence of it.
type Replies = Vec<(&'static str, usize, Value)>;

/// The model engine and host the fake runner answers from.
pub(super) struct World {
    pub paths: Paths,
    containers: BTreeMap<String, Container>,
    networks: BTreeMap<String, usize>,
    images: BTreeMap<String, String>,
    next: u32,
    /// Every command, by operation and words, in order.
    pub log: Vec<(&'static str, Vec<String>, Duration)>,
    /// A fault at the command of this index.
    pub fault: Option<(usize, Injected)>,
    /// Answers replaced by operation and the n-th occurrence of it.
    pub replies: Replies,
    pub chowned: Vec<(PathBuf, u32)>,
    /// A signal arrives once this many commands have run, in cleanup.
    pub late_signal: Option<usize>,
    /// The next release command's status, attempt count and outcome.
    pub prepared: Option<(i32, u64, Value)>,
    /// The render command's added environment and working directory.
    pub rendering: Option<Rendering>,
    /// Named departures of the model from a correct host.
    pub tamper: std::collections::BTreeSet<&'static str>,
    /// The services of the containers cleanup stopped, in order.
    pub stopped: Vec<&'static str>,
    /// Cleanup has listed the owned containers.
    cleaning: bool,
    migrations: usize,
    latched: bool,
    clock: Duration,
    started: Instant,
    rendered: BTreeMap<String, String>,
}

fn id(number: u32) -> String {
    format!("{number:064x}")
}

impl World {
    pub(super) fn new(paths: Paths) -> Self {
        Self {
            paths,
            containers: BTreeMap::new(),
            networks: BTreeMap::new(),
            images: BTreeMap::new(),
            next: 1,
            log: Vec::new(),
            fault: None,
            replies: Vec::new(),
            chowned: Vec::new(),
            late_signal: None,
            prepared: None,
            rendering: None,
            tamper: std::collections::BTreeSet::new(),
            stopped: Vec::new(),
            cleaning: false,
            migrations: 0,
            latched: false,
            clock: Duration::ZERO,
            started: Instant::now(),
            rendered: oracle::rendered_default(),
        }
    }

    fn count(&self, operation: &str) -> usize {
        self.log
            .iter()
            .filter(|(name, ..)| *name == operation)
            .count()
    }

    fn reply(&self, operation: &str) -> Option<Value> {
        let occurrence = self.count(operation).checked_sub(1)?;
        self.replies
            .iter()
            .find(|(name, at, _)| *name == operation && *at == occurrence)
            .map(|(.., value)| value.clone())
    }

    fn create(&mut self, service: &'static str, image: &str) -> String {
        let identity = id(self.next);
        self.next += 1;
        self.containers.insert(
            identity.clone(),
            Container {
                service,
                image: image.into(),
                running: true,
                exit: 0,
                started: self.next,
            },
        );
        if self.networks.is_empty() {
            self.networks.insert(id(9000), 0);
        }
        identity
    }

    fn running(&self, service: &str) -> Option<String> {
        self.containers
            .iter()
            .find(|(_, container)| container.service == service && container.running)
            .map(|(identity, _)| identity.clone())
    }

    fn inspection(&mut self, identity: &str) -> Value {
        let container = self.containers[identity].clone();
        let mut pid = if container.running { 4242 } else { 0 };
        let mut running = container.running;
        if container.service == "migrate" {
            self.migrations += 1;
            if self.tamper.contains("migration-stopped") {
                (running, pid) = (false, 0);
            }
            // Each half of a running migration: the flag and a real process.
            if self.tamper.contains("migration-not-running") {
                running = false;
            }
            if self.tamper.contains("migration-pid-one") {
                pid = 1;
            }
            // The second inspection of the first readiness probe.
            if self.tamper.contains("migration-pid") && self.migrations == 3 {
                pid = 99;
            }
        }
        json!({
            "id": identity,
            "image": container.image,
            "restarts": 0,
            "state": {
                "Running": running,
                "ExitCode": container.exit,
                "OOMKilled": false,
                "Error": "",
                "StartedAt": format!("2026-11-22T00:00:{:02}Z", container.started % 60),
                "Pid": pid,
            },
            "labels": {
                plan::LABEL: if self.cleaning && self.tamper.contains("foreign")
                    && container.service == "caddy" { "d".repeat(32) } else { TOKEN.to_owned() },
                "com.docker.compose.project": plan::PROJECT,
                "com.docker.compose.service": container.service,
            },
        })
    }

    fn image_of(&self, revision: &str) -> String {
        self.images[&images::tag(revision)].clone()
    }

    fn replace_app(&mut self, image: &str) {
        let previous = self.running("simplestchat").unwrap();
        self.containers.remove(&previous);
        self.create("simplestchat", image);
    }

    fn replace_proxy(&mut self) {
        let caddy = self.running("caddy").unwrap();
        self.containers.remove(&caddy);
        self.create("caddy", plan::CADDY);
    }

    /// Change the selection files without changing a container.
    fn reselect(&self) {
        let images = self.paths.config.join("images.json");
        let mut text = fs::read_to_string(&images).unwrap();
        text.push(' ');
        fs::write(images, text).unwrap();
    }

    fn attempt(&mut self, outcome: &Value) -> PathBuf {
        let results = self.paths.root.join("results");
        let attempt = results.join(format!("release.{:04}", self.next));
        self.next += 1;
        fs::create_dir(&attempt).unwrap();
        fs::write(
            attempt.join("outcome.json"),
            serde_json::to_vec(outcome).unwrap(),
        )
        .unwrap();
        attempt
    }

    fn release(&mut self, words: &[String]) -> (i32, String) {
        if let Some((code, attempts, outcome)) = self.prepared.take() {
            for _ in 0..attempts {
                self.attempt(&outcome);
            }
            return (code, String::new());
        }
        let action = words[2].as_str();
        let revision = words[4].clone();
        let failed = images::revision(TOKEN, Kind::Failed);
        if action == "stage" {
            self.attempt(&json!({"action": "stage", "revision": revision, "passed": true}));
            if self.tamper.contains("stage-selection") {
                self.reselect();
            }
            if self.tamper.contains("stage-proxy") {
                self.replace_proxy();
            }
            return (0, String::new());
        }
        if self.tamper.contains("restart-database") {
            let postgres = self.running("postgres").unwrap();
            self.containers.get_mut(&postgres).unwrap().started += 1;
        }
        if self.tamper.contains("deploy-proxy") {
            self.replace_proxy();
        }
        let dump: &[u8] = if self.tamper.contains("empty-dump") {
            b""
        } else {
            b"PGDMP owned fixture dump"
        };
        let digest = if self.tamper.contains("backup-digest") {
            "0".repeat(64)
        } else {
            super::files::sha256(dump)
        };
        let finalized = !self.tamper.contains("unfinished");
        let journal =
            |phase: &str| json!({"schemaVersion": 1, "finalized": finalized, "phase": phase});
        let (code, outcome, phase) = if revision == failed {
            let previous = self.containers[&self.running("simplestchat").unwrap()]
                .image
                .clone();
            let kept = if self.tamper.contains("keep-failed") {
                self.image_of(&failed)
            } else {
                previous
            };
            self.replace_app(&kept);
            if self.tamper.contains("rollback-selection") {
                self.reselect();
            }
            let outcome = json!({
                "action": "deploy", "revision": revision, "phase": "startup", "passed": false,
                "failure": "Candidate readiness failed",
                "rollbackAttempted": !self.tamper.contains("rollback-unattempted"),
                "rollbackPassed": !self.tamper.contains("rollback-failed"), "backupSha256": digest,
            });
            let rolled = if self.tamper.contains("journal-phase") {
                "complete"
            } else {
                "rolled_back"
            };
            (1, outcome, rolled)
        } else {
            let image = self.image_of(&revision);
            if self.tamper.contains("same-id") {
                let app = self.running("simplestchat").unwrap();
                self.containers.get_mut(&app).unwrap().image = image.clone();
            } else if self.tamper.contains("wrong-image") {
                // A new container, but of the image that was already running.
                let app = self.running("simplestchat").unwrap();
                let previous = self.containers[&app].image.clone();
                self.replace_app(&previous);
            } else if !self.tamper.contains("keep-app") {
                self.replace_app(&image);
            }
            let outcome = json!({
                "action": "deploy", "revision": revision, "phase": "complete", "passed": true,
                "rollbackAttempted": false, "backupSha256": digest,
            });
            let phase = if self.tamper.contains("success-phase") {
                "rolled_back"
            } else {
                "complete"
            };
            (0, outcome, phase)
        };
        let mut outcome = outcome;
        if self.tamper.contains("dump-link") {
            outcome.as_object_mut().unwrap().remove("backupSha256");
        }
        let attempt = self.attempt(&outcome);
        let backup = attempt.join("database-before.dump");
        if self.tamper.contains("dump-link") {
            // A non-empty name whose content cannot be hashed without following it.
            fs::write(attempt.join("dump.real"), dump).unwrap();
            std::os::unix::fs::symlink(attempt.join("dump.real"), &backup).unwrap();
        } else {
            fs::write(backup, dump).unwrap();
        }
        if code != 0 {
            let image = self.image_of(&failed);
            if !self.tamper.contains("no-marker") {
                fs::write(
                    attempt.join("command-007.stdout"),
                    format!("{}\n", plan::FAILURE_MARKER),
                )
                .unwrap();
            }
            if self.tamper.contains("failed-left") {
                self.create("migrate", &image);
            }
            let killed = self.tamper.contains("killed");
            let exit = if self.tamper.contains("float-exit") {
                json!(42.0)
            } else {
                json!(42)
            };
            let shown = if self.tamper.contains("other-image-state") {
                self.image_of(&images::revision(TOKEN, Kind::Baseline))
            } else {
                image.clone()
            };
            let state = json!({"image": shown, "state": {"ExitCode": exit, "OOMKilled": killed}});
            fs::write(attempt.join("command-008.stdout"), state.to_string()).unwrap();
            fs::write(
                attempt.join("validation-name.txt"),
                format!("scpub-release-validate-{TOKEN}\n"),
            )
            .unwrap();
        }
        let state = self.paths.root.join("release-state.json");
        let _ = fs::remove_file(&state);
        fs::write(state, journal(phase).to_string()).unwrap();
        (code, String::new())
    }

    /// The model's answer: exit status and stdout, writing any files the
    /// real command would have written.
    fn answer(&mut self, request: &Request, words: &[String]) -> (i32, String) {
        let docker = words[0] == plan::DOCKER[0];
        let compose = docker && words.get(3).is_some_and(|word| word == "compose");
        if compose {
            let arguments: Vec<&str> = words[12..].iter().map(String::as_str).collect();
            return self.compose(&arguments);
        }
        if docker {
            let arguments: Vec<&str> = words[3..].iter().map(String::as_str).collect();
            return self.docker(request, &arguments);
        }
        let program = words[0].as_str();
        match program {
            plan::GIT => (0, format!("{SOURCE}\n")),
            plan::OPENSSL => {
                for flag in ["-out", "-keyout"] {
                    if let Some(at) = words.iter().position(|word| word == flag) {
                        fs::write(&words[at + 1], "-----BEGIN FIXTURE-----\n").unwrap();
                    }
                }
                (0, String::new())
            }
            plan::UPDATE_TRUST => (0, String::new()),
            plan::CURL | plan::NSENTER => (0, "{\"status\":\"ready\"}".into()),
            plan::OPS => match words[2].as_str() {
                "assets" => (0, "{\"passed\":true,\"changed\":true}".into()),
                "archive" => (0, "{\"passed\":true}".into()),
                _ => self.release(words),
            },
            _ if program.ends_with("ansible-playbook") => {
                let environment = request.environment.iter().map(|(key, value)| {
                    (
                        key.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                });
                self.rendering = Some((environment.collect(), request.directory.clone()));
                if self.tamper.contains("plant") {
                    fs::write(self.paths.config.join("images.json"), "planted").unwrap();
                }
                let play: Value = serde_json::from_slice(&fs::read(&words[3]).unwrap()).unwrap();
                let destination = play[0]["tasks"][0]["ansible.builtin.template"]["dest"]
                    .as_str()
                    .unwrap();
                let staging = Path::new(destination).parent().unwrap().to_path_buf();
                let baseline = self.image_of(&images::revision(TOKEN, Kind::Baseline));
                for (name, template) in super::render::TEMPLATES {
                    let text = oracle::localized(&self.rendered[name], &self.paths, &baseline);
                    fs::write(staging.join(template), text).unwrap();
                }
                (0, String::new())
            }
            _ if program.ends_with("node") => {
                let original = images::revision(TOKEN, Kind::Baseline);
                let candidate = images::revision(TOKEN, Kind::Candidate);
                for revision in [&candidate, &original] {
                    let image = self.image_of(revision);
                    self.replace_app(&image);
                    self.attempt(
                        &json!({"action": "deploy", "revision": revision, "passed": true}),
                    );
                }
                if self.tamper.contains("authority") {
                    let trusted = fs::read_dir(&self.paths.trust)
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path();
                    fs::write(trusted, "replaced").unwrap();
                }
                let report = self.reply("node").unwrap_or_else(policy::rollover);
                fs::write(&words[4], report.to_string()).unwrap();
                (0, String::new())
            }
            _ => (0, policy::receipt().to_string()),
        }
    }

    fn docker(&mut self, request: &Request, arguments: &[&str]) -> (i32, String) {
        match arguments {
            ["ps", "--all", "--quiet"] => {
                let mut listed: Vec<String> = self.containers.keys().cloned().collect();
                if self.tamper.contains("leftover") && self.log.len() > 100 {
                    listed.push(id(8888));
                }
                (0, listed.iter().map(|id| format!("{id}\n")).collect())
            }
            ["ps", "--all", "--quiet", "--no-trunc", "--filter", _] => {
                self.cleaning = true;
                (
                    0,
                    self.containers.keys().map(|id| format!("{id}\n")).collect(),
                )
            }
            ["network", "ls", "--quiet", "--filter", _] => (0, String::new()),
            ["network", "ls", "--quiet", "--no-trunc", "--filter", _] => (
                0,
                self.networks.keys().map(|id| format!("{id}\n")).collect(),
            ),
            ["network", "inspect", "--format", _, identity] => {
                let attached =
                    self.containers.len() + usize::from(self.tamper.contains("attached"));
                let containers: serde_json::Map<String, Value> = (0..attached)
                    .map(|index| (index.to_string(), json!({})))
                    .collect();
                let mut value = json!({
                    "Id": identity,
                    "Labels": {plan::LABEL: TOKEN, "com.docker.compose.project": plan::PROJECT},
                    "Containers": containers,
                });
                let changes = [
                    ("network-id", "Id", None, json!(id(9999))),
                    (
                        "network-label",
                        "Labels",
                        Some(plan::LABEL),
                        json!("d".repeat(32)),
                    ),
                    (
                        "network-project",
                        "Labels",
                        Some("com.docker.compose.project"),
                        json!("other"),
                    ),
                    ("network-null", "Containers", None, Value::Null),
                    ("network-list", "Containers", None, json!([])),
                    ("network-list-attached", "Containers", None, json!([{}])),
                    ("network-text", "Containers", None, json!("x")),
                ];
                for (flag, key, label, changed) in changes {
                    if self.tamper.contains(flag) {
                        match label {
                            Some(label) => value[key][label] = changed,
                            None => value[key] = changed,
                        }
                    }
                }
                (0, value.to_string())
            }
            ["network", "rm", identity] => {
                self.networks.remove(*identity);
                (0, String::new())
            }
            ["image", "save", "--help"] => (
                0,
                "Usage: docker image save [OPTIONS] IMAGE\n\nOptions:\n  -o, --output string   \
                 Write to a file\n      --platform string   Export a specific platform\n"
                    .into(),
            ),
            ["version", ..] => (0, "1.51\n".into()),
            ["buildx", "inspect", "default"] => (0, "Name: default\nDriver: docker\n".into()),
            ["image", "inspect", "--format", format, selector] if *format == plan::IMAGE_FORMAT => {
                let value = json!({"id": BASE_IMAGE, "os": "linux", "architecture": "amd64",
                    "user": "10001:10001", "cmd": ["/app/simplestChat"], "entrypoint": null});
                let _ = selector;
                (0, value.to_string())
            }
            ["image", "inspect", "--format", "{{.Id}}", tag] => {
                (0, self.images.get(*tag).cloned().unwrap_or_default())
            }
            ["image", "ls", ..] | ["pull", _] => (0, String::new()),
            ["image", "tag", image, tag] => {
                self.images.insert((*tag).into(), (*image).into());
                (0, String::new())
            }
            ["buildx", "build", .., "--tag", tag, _] => {
                let image = format!("sha256:{}", super::files::sha256(tag.as_bytes()));
                self.images.insert((*tag).into(), image);
                (0, String::new())
            }
            [
                "image",
                "save",
                "--platform",
                "linux/amd64",
                "--output",
                archive,
                _,
            ] => {
                fs::write(archive, b"fixture archive").unwrap();
                (0, String::new())
            }
            ["inspect", "--format", format, identity] if *format == plan::CONTAINER_FORMAT => {
                (0, self.inspection(identity).to_string())
            }
            ["inspect", "--format", _, name] => {
                let failed = self.image_of(&images::revision(TOKEN, Kind::Failed));
                let value = json!({"id": id(7777), "name": format!("/{name}"), "image": failed,
                    "user": "10001:10001", "entrypoint": ["/usr/bin/timeout"], "network": "none",
                    "readOnly": true, "state": {"ExitCode": 1, "Status": "exited",
                    "OOMKilled": false, "Error": ""}});
                (0, value.to_string())
            }
            ["logs", ..] => (0, String::new()),
            ["stop", "--time", "30", identity] => {
                let killed = self.tamper.contains("unexpected-exit");
                let ordinary = self.tamper.contains("ordinary-42");
                let deliberate = if self.tamper.contains("failed-zero") {
                    0
                } else {
                    42
                };
                let failed = self.image_of(&images::revision(TOKEN, Kind::Failed));
                if let Some(container) = self.containers.get_mut(*identity) {
                    container.running = false;
                    self.stopped.push(container.service);
                    if killed && container.service == "postgres" {
                        container.exit = 137;
                    }
                    if ordinary && container.service == "postgres" {
                        container.exit = 42;
                    }
                    // The deliberately failed server exits 42 whenever it runs.
                    if container.image == failed {
                        container.exit = deliberate;
                    }
                }
                (0, String::new())
            }
            ["rm", identity] => {
                self.containers.remove(*identity);
                (0, String::new())
            }
            ["exec", .., "pg_restore", "--list"] => {
                let marker = if self.tamper.contains("listing") {
                    ""
                } else {
                    "; TABLE public release_fixture_marker\n"
                };
                let migrations = if self.tamper.contains("listing-migrations") {
                    ""
                } else {
                    "; TABLE public _sqlx_migrations\n"
                };
                (0, format!("{migrations}{marker}"))
            }
            ["exec", .., "--command", sql] if sql.starts_with("SELECT") => {
                let marker = if self.tamper.contains("marker") {
                    "other"
                } else {
                    TOKEN
                };
                (0, format!("{marker}\n"))
            }
            ["exec", ..] => {
                let _ = &request.input;
                (0, String::new())
            }
            _ => panic!("unexpected engine command"),
        }
    }

    fn compose(&mut self, arguments: &[&str]) -> (i32, String) {
        let baseline = self.image_of(&images::revision(TOKEN, Kind::Baseline));
        match arguments {
            ["config", "--quiet"] => (0, String::new()),
            ["up", .., "postgres"] => {
                self.create("postgres", plan::POSTGRES);
                (0, String::new())
            }
            ["--profile", "maintenance", "up", .., "migrate"] => {
                self.create("migrate", &baseline);
                (0, String::new())
            }
            [
                "--profile",
                "maintenance",
                "stop",
                "--timeout",
                "30",
                "migrate",
            ] => {
                let migration = self.running("migrate").unwrap();
                self.containers.get_mut(&migration).unwrap().running = false;
                (0, String::new())
            }
            ["up", .., "simplestchat", "caddy"] => {
                self.create("simplestchat", &baseline);
                self.create("caddy", plan::CADDY);
                (0, String::new())
            }
            ["ps", "--status", "running", "--quiet", service] => {
                (0, self.running(service).unwrap_or_default())
            }
            _ => panic!("unexpected compose command"),
        }
    }
}

/// The fake runner over one model world.
pub(super) struct Fake {
    pub world: World,
}
impl Runner for Fake {
    async fn execute(
        &mut self,
        request: &mut Request,
        streams: [fs::File; 2],
        timeout: Duration,
        _: Window,
    ) -> Execution {
        let words = words(request);
        let index = self.world.log.len();
        self.world
            .log
            .push((request.operation, words.clone(), timeout));
        self.world.clock += Duration::from_secs(1);
        if self.world.late_signal == Some(index) {
            self.world.latched = true;
        }
        let [mut stdout, mut stderr] = streams;
        match self.world.fault.clone() {
            Some((at, fault)) if at == index => {
                return match fault {
                    Injected::Status(status) => Execution {
                        status: Some(status),
                        stopped: false,
                        stop: None,
                    },
                    Injected::Timeout => Execution {
                        status: None,
                        stopped: true,
                        stop: Some(Stop::Timeout),
                    },
                    Injected::Interrupt => {
                        self.world.latched = true;
                        Execution {
                            status: None,
                            stopped: true,
                            stop: Some(Stop::Interrupted),
                        }
                    }
                };
            }
            _ => {}
        }
        let (status, text) = match self.world.reply(request.operation) {
            Some(Value::Array(reply)) => {
                if let Some(error) = reply.get(2).and_then(Value::as_str) {
                    stderr.write_all(error.as_bytes()).unwrap();
                }
                (
                    reply[0].as_i64().unwrap() as i32,
                    reply[1].as_str().unwrap().to_owned(),
                )
            }
            _ => self.world.answer(request, &words),
        };
        stdout.write_all(text.as_bytes()).unwrap();
        Execution {
            status: Some(status),
            stopped: false,
            stop: None,
        }
    }
    fn now(&self) -> Instant {
        self.world.started + self.world.clock
    }
    async fn pause(&mut self, duration: Duration, _: Window) -> Result<()> {
        self.world.clock += duration;
        Ok(())
    }
    async fn signalled(&mut self) -> bool {
        false
    }
    fn latched(&self) -> bool {
        self.world.latched
    }
    fn chown(&mut self, path: &Path, owner: u32) -> Result<()> {
        self.world.chowned.push((path.to_path_buf(), owner));
        Ok(())
    }
    fn namespace(&mut self, pid: i64) -> Result<OwnedFd> {
        assert!(pid == 4242);
        Ok(OwnedFd::from(fs::File::open("/dev/null").unwrap()))
    }
    fn retained(&mut self, _: &Path) -> Result<()> {
        Ok(())
    }
    fn preflight(&mut self, paths: &Paths) -> Result<()> {
        for path in paths.service() {
            assert!(!super::files::present(path));
        }
        Ok(())
    }
}

/// One owned fixture run over its own scratch paths.
pub(super) struct Run {
    pub scratch: Scratch,
    pub fixture: Fixture<Fake>,
}
impl Run {
    pub(super) fn new() -> Self {
        let scratch = Scratch::new("run");
        let base = &scratch.0;
        let paths = Paths {
            config: base.join("etc"),
            root: base.join("srv"),
            work: base.join("run"),
            bench: base.join("bench"),
            trust: base.join("trust"),
            checkout: checkout(),
        };
        fs::create_dir(&paths.trust).unwrap();
        let output = base.join("output");
        super::files::fresh_output(&output, &paths.service()).unwrap();
        let world = World::new(paths.clone());
        let started = world.started;
        let commands = Commands::new(
            Fake { world },
            &output.join("private"),
            started + super::WORK,
        );
        let inputs = Inputs {
            image: "simplestchat-ci:production".into(),
            node: "/fixture/node".into(),
            chromium: "/fixture/chromium".into(),
            http_policy: "/fixture/http-policy".into(),
            ansible: "/fixture/ansible-playbook".into(),
            token: TOKEN.into(),
            secrets: oracle::secrets(),
        };
        let fixture = Fixture::new(commands, paths, inputs, &output);
        Self { scratch, fixture }
    }
    pub(super) fn world(&mut self) -> &mut World {
        &mut self.fixture.commands.runner.world
    }
    /// The whole lifecycle; returns whether it passed.
    pub(super) fn complete(&mut self) -> bool {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(super::lifecycle(&mut self.fixture))
    }
    fn operations(&self) -> Vec<&'static str> {
        self.fixture
            .commands
            .runner
            .world
            .log
            .iter()
            .map(|(name, ..)| *name)
            .collect()
    }
}

/// The complete run's commands by operation, in order: bootstrap, the three
/// releases, the rollover, both cases and cleanup.
fn transcript() -> Vec<&'static str> {
    let mut expected = vec![
        "docker ps",
        "docker network ls",
        "docker image save",
        "docker version",
        "docker buildx inspect",
        "docker image inspect",
        "git",
        "docker image ls",
        "docker image tag",
        "docker pull",
        "docker pull",
    ];
    for _ in Kind::ALL {
        expected.extend([
            "docker image inspect",
            "docker buildx build",
            "docker image inspect",
            "docker image inspect",
        ]);
    }
    expected.extend([
        "ansible-playbook",
        "simplestchat-ops release assets",
        "openssl",
        "openssl",
        "openssl",
    ]);
    expected.extend([
        "update-ca-certificates",
        "docker compose",
        "docker compose",
        "docker compose",
    ]);
    let service = ["docker compose", "docker inspect"];
    expected.extend(service);
    expected.extend(["docker inspect", "docker inspect", "nsenter"]);
    expected.extend([
        "docker compose",
        "docker inspect",
        "docker logs",
        "docker rm",
    ]);
    expected.extend(service);
    expected.push("docker exec");
    expected.extend(service);
    expected.extend(["docker exec", "docker compose", "curl"]);
    expected.extend(service);
    expected.extend(service);
    expected.push("release-http-policy");
    for _ in Kind::ALL {
        expected.extend(["docker image save", "simplestchat-ops release archive"]);
    }
    // The rollover.
    expected.extend(service);
    expected.push("docker exec");
    let snapshot = [service, service, service].concat();
    expected.extend(snapshot.iter().copied());
    expected.push("node");
    expected.extend(snapshot.iter().copied());
    expected.extend(service);
    expected.push("docker exec");
    for (stage, deploy, failed) in [
        (
            "simplestchat-ops release stage",
            "simplestchat-ops release deploy",
            false,
        ),
        (
            "simplestchat-ops release stage",
            "simplestchat-ops release deploy",
            true,
        ),
    ] {
        expected.extend(snapshot.iter().copied());
        expected.push(stage);
        expected.extend(snapshot.iter().copied());
        expected.push(deploy);
        if failed {
            expected.extend(["docker inspect", "docker logs"]);
        }
        expected.extend(snapshot.iter().copied());
        expected.extend(service);
        expected.extend(["docker exec", "curl"]);
        expected.extend(service);
        expected.push("docker exec");
    }
    // Cleanup: the three running services, then the network.
    expected.extend([
        "docker ps",
        "docker inspect",
        "docker inspect",
        "docker inspect",
    ]);
    for _ in 0..3 {
        expected.extend(["docker logs", "docker stop", "docker inspect", "docker rm"]);
    }
    expected.extend([
        "docker network ls",
        "docker network inspect",
        "docker network rm",
        "docker ps",
    ]);
    expected.push("update-ca-certificates");
    expected
}

#[test]
fn a_complete_run_drives_every_phase_in_order_and_leaves_nothing_it_owns() {
    let mut run = Run::new();
    assert!(run.complete());
    assert!(run.operations() == transcript());
    let report = &run.fixture.report;
    assert!(report.phase == Phase::Complete);
    assert!(report.cleanup_passed && report.cleanup_failures.is_empty());
    assert!(report.failure.is_none());
    assert!(report.base_image.as_deref() == Some(BASE_IMAGE));
    assert!(report.source_revision.as_deref() == Some(SOURCE));
    assert!(report.cases.http_response_policy == Some(policy::receipt()));
    assert!(report.cases.browser_assets == Some(policy::rollover()));
    let candidate = report.cases.candidate.as_ref().unwrap();
    assert!(candidate.release_passed && candidate.rollback_passed == Value::Null);
    let failed = report.cases.failed.as_ref().unwrap();
    assert!(!failed.release_passed && failed.rollback_passed == json!(true));
    assert!(report.browser_asset_releases.as_ref().unwrap().len() == 2);
    let validator = report.validator_startup.as_ref().unwrap();
    assert!(validator["diagnosticUnavailable"] == json!(false));
    assert!(validator["exitCode"] == json!(1));
    // The failed release is the last nonzero command; its diagnostics are not.
    let last = report.last_nonzero_command.as_ref().unwrap();
    assert!(last.operation == "simplestchat-ops release deploy");
    assert!(last.exit_status == 1);
    let world = &run.fixture.commands.runner.world;
    assert!(world.containers.is_empty() && world.networks.is_empty());
    assert!(fs::read_dir(&world.paths.trust).unwrap().count() == 0);
    let owners: Vec<(String, u32)> = world
        .chowned
        .iter()
        .map(|(path, owner)| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                *owner,
            )
        })
        .collect();
    let expected = [
        ("postgres", 999),
        ("postgres-socket", 999),
        ("caddy-data", 10001),
        ("caddy-config", 10001),
        ("postgres-admin-password", 999),
        ("pg_hba.conf", 999),
        ("init-database.sql", 999),
        ("fixture.key", 10001),
        ("fixture.crt", 10001),
    ];
    assert!(owners == expected.map(|(name, owner)| (name.to_owned(), owner)));
    // Cleanup stops the application first and the database last.
    assert!(world.stopped == ["simplestchat", "caddy", "postgres"]);
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn every_command_of_a_complete_run_has_its_exact_arguments_and_bounds() {
    let mut run = Run::new();
    assert!(run.complete());
    let world = &run.fixture.commands.runner.world;
    let paths = &world.paths;
    let config = paths.config.display().to_string();
    let find = |operation: &str, word: &str| {
        world
            .log
            .iter()
            .find(|(name, words, _)| *name == operation && words.iter().any(|w| w == word))
            .expect("the command was run")
            .clone()
    };
    let compose = [
        plan::DOCKER[0],
        plan::DOCKER[1],
        plan::DOCKER[2],
        "compose",
        "--project-name",
        "simplestchat-public",
        "--project-directory",
        &config,
        "--env-file",
        &format!("{config}/app.env"),
        "-f",
        &format!("{config}/compose.public.yml"),
    ]
    .map(str::to_owned);
    let (_, postgres, timeout) = find("docker compose", "--wait");
    assert!(postgres[..12] == compose);
    assert!(
        postgres[12..]
            == [
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "--wait",
                "--wait-timeout",
                "120",
                "postgres"
            ]
    );
    assert!(timeout == Duration::from_secs(135));
    let (_, stop, timeout) = find("docker compose", "stop");
    assert!(
        stop[12..]
            == [
                "--profile",
                "maintenance",
                "stop",
                "--timeout",
                "30",
                "migrate"
            ]
    );
    assert!(timeout == Duration::from_secs(45));
    let revision = images::revision(TOKEN, Kind::Candidate);
    let (_, deploy, timeout) = find("simplestchat-ops release deploy", &revision);
    assert!(
        deploy
            == [
                plan::OPS,
                "release",
                "deploy",
                "--revision",
                &revision,
                "--timeout-seconds",
                "900",
                "--finalization-seconds",
                "60"
            ]
    );
    assert!(timeout == Duration::from_secs(240));
    let (_, stage, _) = find("simplestchat-ops release stage", &revision);
    assert!(stage[5..7] == ["--timeout-seconds", "600"]);
    let (_, policy, timeout) = find("release-http-policy", "release-http-policy");
    let ca = run
        .scratch
        .0
        .join("output/private/ca.crt")
        .display()
        .to_string();
    let caddy = &policy[5];
    assert!(
        policy[..5]
            == [
                "/fixture/http-policy",
                "release-http-policy",
                "--fixture-token",
                TOKEN,
                "--caddy-id"
            ]
    );
    assert!(
        policy[6..]
            == [
                "--app-id",
                &policy[7],
                "--ca",
                &ca,
                "--ca-sha256",
                &policy[11]
            ]
    );
    assert!(policy::identity(caddy) && policy::identity(&policy[7]) && policy[11].len() == 64);
    assert!(timeout == Duration::from_secs(210));
    let (_, node, timeout) = find("node", "/fixture/chromium");
    let checkout = paths
        .checkout
        .join("web/e2e/release-assets.cjs")
        .display()
        .to_string();
    assert!(node[..2] == ["/fixture/node".to_owned(), checkout]);
    assert!(timeout == Duration::from_secs(330));
    let (_, marker, _) = find("docker exec", "--command");
    assert!(
        marker[3..].starts_with(&["exec", "--interactive", "--user", "999:999"].map(str::to_owned))
    );
    assert!(
        marker.last().unwrap()
            == &format!(
                "CREATE TABLE public.release_fixture_marker (value text NOT NULL); INSERT INTO \
         public.release_fixture_marker VALUES ('{TOKEN}');"
            )
    );
    let (_, namespace, timeout) = find("nsenter", "--");
    assert!(namespace[1].starts_with("--net=/proc/self/fd/"));
    assert!(
        namespace[3..]
            == [
                "/usr/bin/curl",
                "--disable",
                "--noproxy",
                "*",
                "--proto",
                "=http",
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "2",
                "http://127.0.0.1:3000/ready"
            ]
    );
    assert!(timeout == Duration::from_secs(5));
    // Private evidence: every record and stream is the owner's alone.
    let private = run.scratch.0.join("output/private");
    for entry in fs::read_dir(&private).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // The model writes the browser report and certificates as their tools would.
        let tool = name.starts_with("asset-rollover-result")
            || name.starts_with("ca.")
            || name.starts_with("server.csr");
        if path.is_file() && !tool {
            assert!(mode(&path) == 0o600, "{}", path.display());
        }
    }
    let record: Value =
        serde_json::from_slice(&fs::read(private.join("command-001.json")).unwrap()).unwrap();
    assert!(
        record["command"]
            == json!([
                plan::DOCKER[0],
                plan::DOCKER[1],
                plan::DOCKER[2],
                "ps",
                "--all",
                "--quiet"
            ])
    );
    assert!(record["exitStatus"] == json!(0));
    assert!(record["timeoutSeconds"] == json!(30.0));
    for name in [
        "compose.public.yml",
        "app.env",
        "images.json",
        "fixture.key",
    ] {
        assert!(mode(&paths.config.join(name)) == 0o600, "{name}");
    }
    assert!(mode(&paths.config.join("Caddyfile")) == 0o644);
    assert!(mode(&paths.root.join("postgres-socket")) == 0o755);
    let owner: Value =
        serde_json::from_slice(&fs::read(paths.root.join("release-test-owner.json")).unwrap())
            .unwrap();
    assert!(owner == json!({"token": TOKEN}));
    let manifest: Value = serde_json::from_slice(
        &fs::read(
            paths
                .root
                .join("releases")
                .join(&revision)
                .join("release.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(manifest["imageTag"] == json!(images::tag(&revision)));
    assert!(manifest["archiveSha256"] == json!(super::files::sha256(b"fixture archive")));
}

/// The index of every command of a complete run that belongs to its work,
/// as opposed to cleanup.
fn work_commands() -> usize {
    let transcript = transcript();
    let last = transcript
        .iter()
        .rposition(|name| *name == "simplestchat-ops release deploy")
        .unwrap();
    let cleanup = last
        + transcript[last..]
            .iter()
            .position(|name| *name == "docker ps")
            .unwrap();
    assert_eq!(transcript.len() - cleanup, 21);
    cleanup
}

fn phase_at(index: usize) -> Phase {
    let transcript = transcript();
    let bootstrap = transcript
        .iter()
        .position(|name| *name == "docker compose")
        .unwrap()
        + 1;
    let archive = "simplestchat-ops release archive";
    let browser = transcript
        .iter()
        .rposition(|name| *name == archive)
        .unwrap()
        + 1;
    let at = |operation: &str| -> Vec<usize> {
        let found = transcript
            .iter()
            .enumerate()
            .filter(|(_, name)| **name == operation);
        found.map(|(at, _)| at).collect()
    };
    let (stages, deploys) = (
        at("simplestchat-ops release stage"),
        at("simplestchat-ops release deploy"),
    );
    match index {
        _ if index < bootstrap => Phase::Preflight,
        _ if index < browser => Phase::Bootstrap,
        _ if index < stages[0] => Phase::BrowserAssetRollover,
        _ if index < deploys[0] => Phase::StageCandidate,
        _ if index < stages[1] => Phase::DeployCandidate,
        _ if index < deploys[1] => Phase::StageFailed,
        _ => Phase::DeployFailed,
    }
}

#[test]
fn a_failure_at_any_work_command_fails_the_run_and_cleanup_removes_everything_it_owns() {
    let work = work_commands();
    for index in 0..work {
        let mut run = Run::new();
        run.world().fault = Some((index, Injected::Status(3)));
        let passed = run.complete();
        let operation = transcript()[index];
        let report = run.fixture.report.clone();
        let world = &run.fixture.commands.runner.world;
        // A failed readiness probe is asked again, and a failed validator
        // diagnostic only describes the release failure; either may pass.
        let failed = transcript()
            .iter()
            .rposition(|name| *name == "simplestchat-ops release deploy")
            .unwrap();
        if matches!(operation, "curl" | "nsenter") || index == failed + 1 || index == failed + 2 {
            assert!(passed, "{index} {operation}");
            continue;
        }
        assert!(!passed, "{index} {operation}");
        assert!(report.failure.is_some(), "{index}");
        let created = transcript()
            .iter()
            .position(|name| *name == "ansible-playbook")
            .unwrap();
        let cleaning = world
            .log
            .iter()
            .any(|(_, words, _)| words.iter().any(|word| word == "--no-trunc"));
        assert!(
            cleaning == (index >= created),
            "{index}: cleanup only of what was created"
        );
        assert!(report.phase == phase_at(index), "{index} {operation}");
        assert!(
            report.cleanup_passed,
            "{index} {operation} {:?}",
            report.cleanup_failures
        );
        assert!(
            world.containers.is_empty() && world.networks.is_empty(),
            "{index}"
        );
        assert!(
            fs::read_dir(&world.paths.trust).unwrap().count() == 0,
            "{index}"
        );
        let failed = report.last_nonzero_command.as_ref().unwrap();
        assert!(failed.exit_status == 3, "{index}");
        // Nothing of the work ran after the failure, except diagnostics.
        let after: Vec<&str> = world.log[index + 1..]
            .iter()
            .map(|(name, ..)| *name)
            .collect();
        let cleanup = after
            .iter()
            .position(|name| *name == "docker ps")
            .unwrap_or(after.len());
        assert!(
            after[..cleanup]
                .iter()
                .all(|name| matches!(*name, "docker inspect" | "docker logs")),
            "{index} {after:?}"
        );
    }
}

#[test]
fn a_checked_command_failure_names_its_number_and_its_operation() {
    let mut run = Run::new();
    run.world().fault = Some((9, Injected::Status(17)));
    assert!(!run.complete());
    let report = &run.fixture.report;
    assert!(report.failure.as_deref() == Some("Command 010 failed; inspect private logs"));
    let failed = report.last_nonzero_command.as_ref().unwrap();
    assert!((failed.number, failed.operation, failed.exit_status) == (10, "docker pull", 17));
    let text = serde_json::to_string(report).unwrap();
    assert!(text.contains("\"lastCommand\":{\"number\":"));
}

#[test]
fn a_stopped_command_leaves_every_resource_and_names_the_stop() {
    for (fault, message) in [
        (
            Injected::Timeout,
            "Command 031 exceeded its time limit; inspect private logs",
        ),
        (
            Injected::Interrupt,
            "Container release attempt interrupted or exceeded its work deadline",
        ),
    ] {
        let mut run = Run::new();
        run.world().fault = Some((30, fault.clone()));
        assert!(!run.complete());
        let report = run.fixture.report.clone();
        assert!(report.failure.as_deref() == Some(message));
        assert!(!report.cleanup_passed);
        assert!(report.cleanup_failures == ["CommandSettlementUnconfirmed"]);
        assert!(report.interrupted == (fault == Injected::Interrupt));
        let world = &run.fixture.commands.runner.world;
        assert!(
            world.log.len() == 31,
            "no cleanup command after an unsettled stop"
        );
        assert!(fs::read_dir(&world.paths.trust).unwrap().count() == 1);
        assert!(report.last_command.as_ref().unwrap().number == 31);
    }
}

#[test]
fn a_signal_during_cleanup_finishes_cleanup_but_never_passes() {
    let mut run = Run::new();
    run.world().late_signal = Some(transcript().len() - 3);
    assert!(!run.complete());
    let report = &run.fixture.report;
    assert!(report.cleanup_passed && report.interrupted && !report.passed);
    assert!(report.phase == Phase::Complete);
    assert!(run.operations() == transcript());
}

fn refused(replies: Replies) -> super::report::Report {
    let mut run = Run::new();
    run.world().replies = replies;
    assert!(!run.complete());
    run.fixture.report.clone()
}

fn reply(status: i32, text: &str) -> Value {
    json!([status, text])
}

#[test]
fn every_release_assertion_refuses_its_own_failure() {
    let world_cases: Vec<(&'static str, usize, Value, &str)> = vec![
        (
            "docker ps",
            0,
            reply(0, "abc\n"),
            "Existing containers; use an empty disposable engine",
        ),
        (
            "docker version",
            0,
            reply(0, "1.47"),
            "Docker server API 1.48 or newer is required",
        ),
        (
            "git",
            0,
            reply(0, "main"),
            "Source revision must be an exact Git commit",
        ),
        (
            "docker image ls",
            0,
            reply(0, "x"),
            "Fixture base tag already exists",
        ),
        (
            "simplestchat-ops release assets",
            0,
            reply(0, "{\"passed\":true,\"busy\":true}"),
            "Browser asset store was not initialized",
        ),
        (
            "simplestchat-ops release assets",
            0,
            reply(0, "{\"passed\":false,\"changed\":true}"),
            "Browser asset store was not initialized",
        ),
        (
            "release-http-policy",
            0,
            reply(0, "{\"passed\":true}"),
            "Fixture HTTP response policy did not pass completely",
        ),
        (
            "node",
            0,
            json!({"passed": true}),
            "Browser rollover proof incomplete",
        ),
        (
            "curl",
            0,
            reply(0, "[]"),
            "Readiness answer is not a JSON object",
        ),
    ];
    for (operation, at, value, message) in world_cases {
        let report = refused(vec![(operation, at, value)]);
        assert!(report.failure.as_deref() == Some(message), "{operation}");
        assert!(report.cleanup_passed, "{operation}");
    }
}

fn tampered(flag: &'static str) -> Run {
    let mut run = Run::new();
    run.world().tamper.insert(flag);
    assert!(!run.complete(), "{flag}");
    run
}

#[test]
fn every_release_case_refuses_a_deployment_that_did_not_do_what_it_said() {
    for (flag, message, phase) in [
        (
            "stage-selection",
            "Staging interrupted or changed the running deployment",
            Phase::StageCandidate,
        ),
        (
            "stage-proxy",
            "Staging interrupted or changed the running deployment",
            Phase::StageCandidate,
        ),
        (
            "restart-database",
            "A retained dependency changed",
            Phase::DeployCandidate,
        ),
        (
            "backup-digest",
            "Backup identity mismatch",
            Phase::DeployCandidate,
        ),
        (
            "unfinished",
            "Release cleanup remained unfinished",
            Phase::DeployCandidate,
        ),
        (
            "keep-app",
            "Successful upgrade did not replace the application",
            Phase::DeployCandidate,
        ),
        (
            "keep-failed",
            "Rollback did not restore the prior selection and image",
            Phase::DeployFailed,
        ),
        (
            "no-marker",
            "Failed startup log was not retained",
            Phase::DeployFailed,
        ),
        (
            "killed",
            "Failed candidate image/state evidence was not retained",
            Phase::DeployFailed,
        ),
        (
            "marker",
            "Durable database marker changed",
            Phase::BrowserAssetRollover,
        ),
        (
            "listing",
            "Live backup omitted expected tables",
            Phase::DeployCandidate,
        ),
        (
            "journal-phase",
            "Failed startup did not complete its single rollback",
            Phase::DeployFailed,
        ),
        (
            "same-id",
            "Successful upgrade did not replace the application",
            Phase::DeployCandidate,
        ),
        // Each remaining case breaks exactly one operand of its assertion.
        (
            "wrong-image",
            "Successful upgrade did not replace the application",
            Phase::DeployCandidate,
        ),
        (
            "success-phase",
            "Successful upgrade did not replace the application",
            Phase::DeployCandidate,
        ),
        (
            "rollback-unattempted",
            "Failed startup did not complete its single rollback",
            Phase::DeployFailed,
        ),
        (
            "rollback-failed",
            "Failed startup did not complete its single rollback",
            Phase::DeployFailed,
        ),
        (
            "rollback-selection",
            "Rollback did not restore the prior selection and image",
            Phase::DeployFailed,
        ),
        (
            "deploy-proxy",
            "A retained dependency changed",
            Phase::DeployCandidate,
        ),
        (
            "empty-dump",
            "Backup identity mismatch",
            Phase::DeployCandidate,
        ),
        (
            "dump-link",
            "Backup identity mismatch",
            Phase::DeployCandidate,
        ),
        (
            "listing-migrations",
            "Live backup omitted expected tables",
            Phase::DeployCandidate,
        ),
        (
            "float-exit",
            "Failed candidate image/state evidence was not retained",
            Phase::DeployFailed,
        ),
        (
            "other-image-state",
            "Failed candidate image/state evidence was not retained",
            Phase::DeployFailed,
        ),
    ] {
        let run = tampered(flag);
        let report = &run.fixture.report;
        assert!(report.failure.as_deref() == Some(message), "{flag}");
        assert!(report.phase == phase, "{flag}");
        // A failed image left running exits with its fixed 42 and is removed.
        assert!(
            report.cleanup_passed,
            "{flag} {:?}",
            report.cleanup_failures
        );
    }
}

#[test]
fn cleanup_retains_and_reports_whatever_it_cannot_prove_it_owns() {
    for (flag, failure, retained) in [
        (
            "attached",
            "Network ownership or cleanup is uncertain",
            "network",
        ),
        (
            "leftover",
            "Containers remain after fixture cleanup; inspect private evidence",
            "nothing",
        ),
        (
            "unexpected-exit",
            "Owned container did not exit as expected",
            "container",
        ),
        ("authority", "Fixture CA ownership changed", "authority"),
        (
            "network-id",
            "Network ownership or cleanup is uncertain",
            "network",
        ),
        (
            "network-label",
            "Network ownership or cleanup is uncertain",
            "network",
        ),
        (
            "network-project",
            "Network ownership or cleanup is uncertain",
            "network",
        ),
        (
            "network-list-attached",
            "Network ownership or cleanup is uncertain",
            "network",
        ),
        (
            "network-text",
            "Network ownership or cleanup is uncertain",
            "network",
        ),
    ] {
        let mut run = Run::new();
        run.world().tamper.insert(flag);
        let passed = run.complete();
        let report = &run.fixture.report;
        assert!(!passed && !report.cleanup_passed, "{flag}");
        assert!(report.phase == Phase::Complete, "{flag}");
        assert!(report.cleanup_failures == [failure], "{flag}");
        let world = &run.fixture.commands.runner.world;
        match retained {
            "network" => assert!(world.networks.len() == 1),
            "container" => assert!(world.containers.len() == 1),
            "authority" => assert!(fs::read_dir(&world.paths.trust).unwrap().count() == 1),
            _ => assert!(world.containers.is_empty()),
        }
        if retained != "authority" {
            assert!(
                fs::read_dir(&world.paths.trust).unwrap().count() == 0,
                "{flag}"
            );
        }
    }
    // A network without containers may also be described by null or an empty list.
    for flag in ["network-null", "network-list"] {
        let mut run = Run::new();
        run.world().tamper.insert(flag);
        assert!(run.complete(), "{flag}");
        assert!(run.fixture.commands.runner.world.networks.is_empty());
    }
}

#[test]
fn a_browser_failure_names_only_the_helpers_fixed_stage() {
    for (stderr, stage) in [
        (
            "Release asset rollover browser check failed: load-old\n",
            "load-old",
        ),
        ("PRIVATE_TOKEN raw browser error", "unavailable"),
    ] {
        let mut run = Run::new();
        run.world().replies = vec![("node", 0, json!([1, "", stderr]))];
        assert!(!run.complete());
        let report = &run.fixture.report;
        assert!(report.phase == Phase::BrowserAssetRollover);
        assert!(report.browser_asset_failure_stage == Some(stage));
        assert!(
            report.failure.as_deref()
                == Some(
                    "Browser rollover command failed; inspect fixed stage and private evidence"
                )
        );
        assert!(
            !serde_json::to_string(report)
                .unwrap()
                .contains("PRIVATE_TOKEN")
        );
        assert!(report.cases.browser_assets.is_none());
    }
}

#[test]
fn the_render_runs_the_controller_ansible_with_its_own_private_state_and_utf8_locale() {
    let mut run = Run::new();
    assert!(run.complete());
    let world = &run.fixture.commands.runner.world;
    let (environment, directory) = world.rendering.clone().unwrap();
    let playbooks = checkout().join("ops/ansible");
    let ansible = run.scratch.0.join("output/private/ansible");
    let expected = [
        ("ANSIBLE_CONFIG", playbooks.join("ansible.cfg")),
        ("ANSIBLE_HOME", ansible.join("home")),
        ("ANSIBLE_LOCAL_TEMP", ansible.join("local")),
        ("ANSIBLE_REMOTE_TEMP", ansible.join("remote")),
        ("ANSIBLE_NOCOLOR", "1".into()),
        ("LC_ALL", "C.UTF-8".into()),
    ]
    .map(|(key, value)| (key.to_owned(), value.display().to_string()));
    assert!(environment == expected);
    assert!(directory == Some(playbooks));
    let (_, render, timeout) = world
        .log
        .iter()
        .find(|(name, ..)| *name == "ansible-playbook")
        .unwrap();
    let play = run
        .scratch
        .0
        .join("output/private/render-play.json")
        .display()
        .to_string();
    assert!(
        render
            == &[
                "/fixture/ansible-playbook".to_owned(),
                "-i".into(),
                "localhost,".into(),
                play
            ]
    );
    assert!(*timeout == Duration::from_secs(120));
}

#[test]
fn a_fixture_never_replaces_an_existing_configuration_file() {
    let mut run = Run::new();
    // A file that appears while the templates render is never replaced.
    run.world().tamper.insert("plant");
    assert!(!run.complete());
    assert!(run.fixture.report.failure.as_deref() == Some("Fixture output already exists"));
    let planted = run.fixture.paths.config.join("images.json");
    assert!(fs::read_to_string(planted).unwrap() == "planted");
    assert!(!run.fixture.paths.config.join("app.env").exists());
}

#[test]
fn engine_image_and_readiness_refusals_stop_before_the_next_step() {
    let cases: Vec<(Replies, &str)> = vec![
        (
            vec![("docker image inspect", 1, reply(0, "sha256:"))],
            "Fixture base tag changed before build",
        ),
        (
            vec![("docker image inspect", 2, reply(0, "sha256:other"))],
            "Fixture base tag changed during build",
        ),
        (
            vec![("docker image inspect", 3, reply(0, "not-an-image"))],
            "Fixture image has no immutable identity",
        ),
    ];
    for (replies, message) in cases {
        let report = refused(replies);
        assert!(report.failure.as_deref() == Some(message));
        assert!(report.cleanup_passed, "{message}");
    }
    for (flag, message) in [
        (
            "migration-stopped",
            "Migration container stopped before readiness",
        ),
        ("migration-pid", "Migration namespace changed"),
        (
            "migration-not-running",
            "Migration container stopped before readiness",
        ),
        (
            "migration-pid-one",
            "Migration container stopped before readiness",
        ),
    ] {
        let run = tampered(flag);
        assert!(run.fixture.report.failure.as_deref() == Some(message));
        assert!(run.fixture.report.phase == Phase::Bootstrap);
    }
    let mut run = Run::new();
    run.world().replies = (0..200).map(|at| ("curl", at, reply(7, ""))).collect();
    assert!(!run.complete());
    assert!(run.fixture.report.failure.as_deref() == Some("Fixture readiness deadline exceeded"));
    // Each modelled probe takes one second and waits half a second after it.
    assert!(run.fixture.commands.runner.world.count("curl") == 31);
}

#[test]
fn only_the_deliberately_failed_image_may_exit_42_and_still_be_removed() {
    let mut run = Run::new();
    run.world().tamper.insert("failed-left");
    assert!(run.complete());
    let world = &run.fixture.commands.runner.world;
    assert!(world.stopped == ["simplestchat", "migrate", "caddy", "postgres"]);
    assert!(world.containers.is_empty());
    // The failed image must exit 42, and no other container may.
    for (flags, retained) in [
        (&["failed-left", "failed-zero"][..], 3),
        (&["ordinary-42"], 1),
    ] {
        let mut run = Run::new();
        for flag in flags {
            run.world().tamper.insert(flag);
        }
        assert!(!run.complete());
        let report = &run.fixture.report;
        assert!(report.cleanup_failures == ["Owned container did not exit as expected"]);
        assert!(run.fixture.commands.runner.world.containers.len() == retained);
    }
}

#[test]
fn the_fixture_runs_only_fixed_absolute_programs_and_no_interpreter() {
    let mut run = Run::new();
    assert!(run.complete());
    let allowed = [
        plan::DOCKER[0],
        plan::OPS,
        plan::CURL,
        plan::OPENSSL,
        plan::NSENTER,
        plan::UPDATE_TRUST,
        plan::GIT,
        "/fixture/node",
        "/fixture/http-policy",
        "/fixture/ansible-playbook",
    ];
    for (_, words, _) in &run.fixture.commands.runner.world.log {
        assert!(allowed.contains(&words[0].as_str()));
        assert!(
            words
                .iter()
                .all(|word| !word.contains(".py") && !word.contains("python"))
        );
    }
}

#[test]
fn only_explicit_disposable_intent_and_one_local_image_selector_are_accepted() {
    let arguments = |image: &str| -> Vec<String> {
        [
            "--disposable-host",
            "--image",
            image,
            "--output",
            "/o",
            "--node",
            "/n",
            "--chromium",
            "/c",
            "--http-policy-binary",
            "/h",
            "--ansible",
            "/a",
        ]
        .map(str::to_owned)
        .into()
    };
    let parsed = super::Options::parse(&arguments("simplestchat-ci:production")).unwrap();
    assert!(parsed.image == "simplestchat-ci:production");
    assert!(parsed.ansible == Path::new("/a"));
    let digest = format!("sha256:{}", "a".repeat(64));
    assert!(super::Options::parse(&arguments(&digest)).is_some());
    for image in [
        "",
        "-x",
        " x",
        "x y",
        "x;y",
        "x\n",
        "x$(y)",
        &"a".repeat(257),
    ] {
        assert!(
            super::Options::parse(&arguments(image)).is_none(),
            "{image:?}"
        );
    }
    assert!(super::Options::parse(&arguments(&"a".repeat(256))).is_some());
    let mut missing = arguments("x");
    missing.remove(0);
    assert!(super::Options::parse(&missing).is_none());
    for at in (1..arguments("x").len()).step_by(2) {
        let mut short = arguments("x");
        short.drain(at..at + 2);
        assert!(super::Options::parse(&short).is_none(), "{at}");
        let mut twice = arguments("x");
        twice.extend(arguments("x")[at..at + 2].to_vec());
        assert!(super::Options::parse(&twice).is_none(), "{at}");
    }
    let mut unknown = arguments("x");
    unknown.push("--other".into());
    assert!(super::Options::parse(&unknown).is_none());
}

#[test]
fn the_evidence_directory_is_new_private_and_outside_every_service_path() {
    let scratch = Scratch::new("output");
    let service = scratch.0.join("srv");
    fs::create_dir(&service).unwrap();
    let fresh = scratch.0.join("fresh");
    assert!(super::files::fresh_output(&fresh, &[&service]).unwrap() == fresh);
    assert!(mode(&fresh) == 0o700);
    assert!(mode(&fresh.join("private")) == 0o700);
    let existing = scratch.0.join("existing");
    fs::write(&existing, "retained evidence").unwrap();
    let dangling = scratch.0.join("dangling");
    std::os::unix::fs::symlink(scratch.0.join("missing"), &dangling).unwrap();
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink(&scratch.0, &link).unwrap();
    for (path, message) in [
        (fresh.clone(), "Output must be a fresh directory"),
        (existing.clone(), "Output must be a fresh directory"),
        (dangling.clone(), "Output must be a fresh directory"),
        (
            PathBuf::from("relative"),
            "Output must have an existing absolute, non-symlink parent",
        ),
        (
            scratch.0.join("missing/child"),
            "Output must have an existing absolute, non-symlink parent",
        ),
        (
            link.join("child"),
            "Output must have an existing absolute, non-symlink parent",
        ),
        (
            existing.join("child"),
            "Output must have an existing absolute, non-symlink parent",
        ),
        (scratch.0.join("control\u{1}"), "Invalid output path"),
        (scratch.0.join("control\u{7f}"), "Invalid output path"),
        (
            service.join("child"),
            "Output must be separate from fixture service paths",
        ),
    ] {
        assert!(
            super::files::fresh_output(&path, &[&service]) == Err(message),
            "{}",
            path.display()
        );
    }
    assert!(fs::read_to_string(&existing).unwrap() == "retained evidence");
    assert!(
        fs::symlink_metadata(&dangling)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(fs::read_dir(&service).unwrap().count() == 0);
    let private = scratch.0.join("private.json");
    super::files::write_new(&private, b"first").unwrap();
    assert!(mode(&private) == 0o600);
    assert!(super::files::write_new(&private, b"second").is_err());
    assert!(super::files::write_new(&dangling, b"x").is_err());
    assert!(fs::read_to_string(&private).unwrap() == "first");
}

#[test]
fn only_the_sanitized_report_becomes_readable_by_other_users() {
    let mut run = Run::new();
    assert!(run.complete());
    let output = run.scratch.0.join("output");
    super::publish(&output, &run.fixture.report).unwrap();
    assert!(mode(&output.join("report.json")) == 0o644);
    assert!(mode(&output) == 0o755);
    assert!(mode(&output.join("private")) == 0o700);
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
    assert!(report["passed"] == json!(true));
    assert!(report["phase"] == json!("complete"));
    assert!(report["schemaVersion"] == json!(1));
    assert!(report["execution"] == json!("real-containers"));
    let text = report.to_string();
    for private in super::tests::oracle::secrets() {
        assert!(!text.contains(&private));
    }
    assert!(!text.contains(TOKEN), "the ownership token stays private");
    assert!(
        super::publish(&output, &run.fixture.report).is_err(),
        "never replaced"
    );
}

#[test]
fn nothing_is_cleaned_before_the_fixture_created_anything() {
    let transcript = transcript();
    let created = transcript
        .iter()
        .position(|name| *name == "ansible-playbook")
        .unwrap();
    for index in [0, 6, created - 1] {
        let mut run = Run::new();
        run.world().fault = Some((index, Injected::Status(1)));
        assert!(!run.complete());
        let world = &run.fixture.commands.runner.world;
        assert!(world.log.len() == index + 1, "{index}: no cleanup command");
        assert!(run.fixture.report.cleanup_passed);
    }
}

#[test]
fn a_failed_trust_refresh_keeps_cleanup_failed_after_removing_the_authority() {
    let mut run = Run::new();
    run.world().fault = Some((transcript().len() - 1, Injected::Status(1)));
    assert!(!run.complete());
    let report = &run.fixture.report;
    assert!(!report.cleanup_passed);
    let number = transcript().len();
    assert!(
        report.cleanup_failures == [format!("Command {number:03} failed; inspect private logs")]
    );
    let world = &run.fixture.commands.runner.world;
    assert!(fs::read_dir(&world.paths.trust).unwrap().count() == 0);
}

#[test]
fn cleanup_verifies_every_container_before_stopping_or_removing_any() {
    let mut run = Run::new();
    run.world().tamper.insert("foreign");
    assert!(!run.complete());
    let report = &run.fixture.report;
    assert!(report.cleanup_failures == ["Container ownership does not match this fixture"]);
    let world = &run.fixture.commands.runner.world;
    assert!(world.stopped.is_empty());
    assert!(world.containers.len() == 3);
    assert!(fs::read_dir(&world.paths.trust).unwrap().count() == 0);
}
