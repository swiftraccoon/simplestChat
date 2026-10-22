//! An owned fake engine and HTTP responses; no daemon, socket or service exists.

use super::*;
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Environment, Failure, Result},
};
use engine::Clock;
use plan::Asset;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[path = "oracle_tests.rs"]
mod oracle;

const ENDPOINT: &str = "unix:///owned/docker-$(literal).sock";
const APP: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PG: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const DBID: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const APPID: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

struct Fixture {
    root: PathBuf,
    context: Context,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "container smoke {} {} $(literal)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::create_dir_all(root.join("web/public")).unwrap();
        fs::create_dir(root.join("migrations")).unwrap();
        fs::write(root.join("migrations/1.sql"), "select 1").unwrap();
        fs::write(root.join("migrations/2.sql"), "select 2").unwrap();
        for asset in Asset::ALL {
            fs::write(
                root.join("web/public").join(asset.name()),
                format!("fixture {}\n", asset.name()),
            )
            .unwrap();
        }
        let environment = Environment::from([
            ("PRODUCTION_IMAGE".into(), "fixture:production".into()),
            (
                "CONTAINER_TEST_ARTIFACTS".into(),
                root.join("artifacts").into_os_string(),
            ),
        ]);
        let context = Context::new(&root, environment, Mode::Local, "Darwin").unwrap();
        Self { root, context }
    }
    fn fake(&self) -> Fake {
        Fake {
            root: self.root.clone(),
            assets: Asset::ALL
                .into_iter()
                .map(|asset| {
                    (
                        asset,
                        fs::read(self.root.join("web/public").join(asset.name())).unwrap(),
                    )
                })
                .collect(),
            ..Fake::default()
        }
    }
    fn summary(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.join("artifacts/container-smoke.json")).unwrap())
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
struct FakeClock(std::rc::Rc<std::cell::Cell<Duration>>);
impl Clock for FakeClock {
    fn elapsed(&self) -> Duration {
        self.0.get()
    }
    async fn sleep(&mut self, duration: Duration) {
        self.0.set(self.0.get() + duration);
    }
}

struct Container {
    name: String,
    image: String,
    selected: String,
    label: String,
    running: bool,
}
struct Fake {
    root: PathBuf,
    assets: BTreeMap<Asset, Vec<u8>>,
    containers: BTreeMap<String, Container>,
    commands: Vec<Command>,
    removed: Vec<String>,
    restarted: Vec<String>,
    bad: &'static str,
    identity: String,
    interrupted: Option<u8>,
    cancel_after_app_removed: bool,
    clean: bool,
    database_probes: usize,
    room_probes: usize,
    header: Option<Vec<u8>>,
    clock: Option<std::rc::Rc<std::cell::Cell<Duration>>>,
    jump_last_room: bool,
    jump_final_ready: bool,
    final_ready_calls: usize,
    cold_database: bool,
    image_failure: Option<u8>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            assets: BTreeMap::new(),
            containers: BTreeMap::new(),
            commands: vec![],
            removed: vec![],
            restarted: vec![],
            bad: "",
            identity: "opaque engine:UUID-1234".into(),
            interrupted: None,
            cancel_after_app_removed: false,
            clean: true,
            database_probes: 0,
            room_probes: 0,
            header: None,
            clock: None,
            jump_last_room: false,
            jump_final_ready: false,
            final_ready_calls: 0,
            cold_database: false,
            image_failure: None,
        }
    }
}
impl Fake {
    fn fail(code: u8) -> Failure {
        Failure::new(code, "Owned fake command failure.")
    }
    fn strings(command: &Command) -> Vec<String> {
        command
            .arguments
            .iter()
            .map(|value| value.to_str().unwrap().into())
            .collect()
    }
    fn arg(args: &[String], name: &str) -> String {
        args[args.iter().position(|value| value == name).unwrap() + 1].clone()
    }
    fn docker(&mut self, args: &[String]) -> Result<Vec<u8>> {
        let phase = args[0].as_str();
        if phase == "info" {
            return Ok(format!("{}\n", self.identity).into_bytes());
        }
        if phase == "image" {
            let selected = args.last().unwrap();
            if selected == plan::POSTGRES_IMAGE {
                if let Some(code) = self.image_failure {
                    return Err(Self::fail(code));
                }
                if self.cold_database {
                    return Err(Self::fail(1));
                }
            }
            return Ok(format!(
                "{}\n",
                if selected == plan::POSTGRES_IMAGE || selected == PG {
                    PG
                } else {
                    APP
                }
            )
            .into_bytes());
        }
        if phase == "pull" {
            assert_eq!(args, ["pull", "--quiet", plan::POSTGRES_IMAGE]);
            assert!(self.containers.is_empty());
            assert!(!self.root.join("artifacts/ownership.json").exists());
            self.cold_database = false;
            return Ok(format!("{PG}\n").into_bytes());
        }
        if phase == "container" {
            match args[1].as_str() {
                "create" => {
                    assert!(
                        self.root.join("artifacts/ownership.json").is_file(),
                        "journal precedes all engine creation"
                    );
                    let name = Self::arg(args, "--name");
                    let database = name.contains("-database-");
                    assert!(
                        args.iter()
                            .any(|value| value == if database { PG } else { APP })
                    );
                    if database && self.bad == "db-create-before" {
                        return Err(Self::fail(37));
                    }
                    let id = if database { DBID } else { APPID };
                    let label = Self::arg(args, "--label")
                        .split_once('=')
                        .unwrap()
                        .1
                        .to_owned();
                    self.containers.insert(
                        id.into(),
                        Container {
                            name,
                            image: if database { PG } else { APP }.into(),
                            selected: if database { PG } else { APP }.into(),
                            label,
                            running: false,
                        },
                    );
                    if database && self.bad == "db-create-lost" {
                        return Err(Self::fail(37));
                    }
                    if !database && self.bad == "engine-swap" {
                        self.identity = "another engine identity".into();
                    }
                    return Ok(format!("{id}\n").into_bytes());
                }
                "inspect" => {
                    let id = args.last().unwrap();
                    let value = self.containers.get(id).ok_or_else(|| Self::fail(1))?;
                    return Ok(serde_json::to_vec(&json!({"Id":id,"Name":format!("/{}",value.name),"Image":value.image,"Config":{"Image":value.selected,"Labels":{plan::OWNER:value.label}},"State":{"Running":value.running}})).unwrap());
                }
                "start" => {
                    let id = args.last().unwrap();
                    self.containers.get_mut(id).unwrap().running =
                        !(id == APPID && self.bad == "app-stopped");
                    return Ok(format!("{id}\n").into_bytes());
                }
                "ls" => {
                    if let Some(app) = self.containers.get_mut(APPID) {
                        match self.bad {
                            "renamed" => app.name = "changed-owned-name".into(),
                            "foreign-image" => app.image = PG.into(),
                            "foreign-label" => app.label = "foreign".into(),
                            _ => {}
                        }
                    }
                    let filter = Self::arg(args, "--filter");
                    let found = self
                        .containers
                        .iter()
                        .filter(|(id, value)| {
                            if let Some(expected) = filter.strip_prefix("id=") {
                                id.as_str() == expected
                            } else {
                                filter == format!("name=^/{}$", value.name)
                            }
                        })
                        .map(|(id, _)| format!("{id}\n"))
                        .collect::<String>();
                    return Ok(found.into_bytes());
                }
                "rm" => {
                    let id = args.last().unwrap();
                    if self.bad == "app-rm" && id == APPID {
                        return Err(Self::fail(42));
                    }
                    self.containers.remove(id);
                    self.removed.push(id.clone());
                    if self.cancel_after_app_removed && id == APPID {
                        self.interrupted = Some(143);
                    }
                    return Ok(vec![]);
                }
                _ => {}
            }
        }
        if phase == "port" {
            return Ok(if self.bad == "port" {
                b"0.0.0.0:38001\n".to_vec()
            } else {
                b"127.0.0.1:38001\n".to_vec()
            });
        }
        if phase == "exec" {
            if args[2] == "pg_isready" {
                self.database_probes += 1;
                return if self.bad == "db-not-ready" {
                    Err(Self::fail(1))
                } else {
                    Ok(vec![])
                };
            }
            let sql = args.last().unwrap();
            if sql == plan::MIGRATIONS {
                return Ok(if self.bad == "migration-zero" {
                    b"0\n".to_vec()
                } else if self.bad == "migration-count" {
                    b"1\n".to_vec()
                } else {
                    b"2\n".to_vec()
                });
            }
            assert_eq!(sql, plan::REGISTERED);
            return Ok(if self.bad == "registered" {
                b"0\n".to_vec()
            } else {
                b"1\n".to_vec()
            });
        }
        if phase == "restart" {
            let id = args.last().unwrap();
            self.restarted.push(id.clone());
            if self.bad == "restart" {
                self.containers.get_mut(id).unwrap().running = false;
            }
            return Ok(vec![]);
        }
        if phase == "logs" {
            if self.bad == "logs" {
                return Err(Self::fail(24));
            }
            return Ok(b"PRIVATE owned guest log\n".to_vec());
        }
        panic!("unexpected owned Docker argv: {args:?}");
    }
    fn curl(&mut self, command: &Command, args: &[String]) -> Result<Vec<u8>> {
        assert_eq!(args.first().unwrap(), "--disable");
        assert_eq!(Self::arg(args, "--proxy"), "");
        let path = args
            .last()
            .unwrap()
            .strip_prefix("http://127.0.0.1:38001")
            .unwrap();
        if path == "/api/rooms" {
            self.room_probes += 1;
            if self.jump_last_room && !self.restarted.is_empty() {
                self.clock
                    .as_ref()
                    .unwrap()
                    .set(plan::WATCHDOG - plan::RESERVE);
            }
            return if self.bad == "app-not-ready" {
                Err(Self::fail(22))
            } else {
                Ok(if self.bad == "rooms" {
                    b"[{}]".to_vec()
                } else {
                    b"[]\n\n".to_vec()
                })
            };
        }
        if path == "/health" {
            if self.bad == "health-command" {
                return Err(Self::fail(22));
            }
            return Ok(if self.bad == "health" {
                b"wrong".to_vec()
            } else {
                b"{\"status\":\"ok\"}\n\n".to_vec()
            });
        }
        if path == "/" {
            if self.bad == "html-command" {
                return Err(Self::fail(22));
            }
            return Ok(if self.bad == "html" {
                b"not html".to_vec()
            } else {
                b"<!DOCTYPE HTML><title>owned</title>".to_vec()
            });
        }
        if path == "/api/auth/register" {
            assert_eq!(Self::arg(args, "--data"), plan::REGISTER);
            assert_eq!(Self::arg(args, "--output"), "/dev/null");
            assert_eq!(command.capture, Some(0));
            return Ok(vec![]);
        }
        let asset = Asset::ALL
            .into_iter()
            .find(|asset| path == format!("/{}", asset.name()))
            .unwrap();
        assert!(args.iter().any(|value| value == "--include"));
        let mime = if self.bad == "mime" {
            "text/plain"
        } else {
            match asset {
                Asset::Manifest => "application/manifest+json",
                Asset::Worker => "application/javascript",
                _ => "image/png",
            }
        };
        let mut response = self.header.clone().unwrap_or_else(|| {
            format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}; charset=utf-8\r\n\r\n").into_bytes()
        });
        response.extend(if self.bad == "asset-bytes" {
            b"wrong".as_slice()
        } else {
            &self.assets[&asset]
        });
        Ok(response)
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let combined = command.combined_log.take();
        let args = Self::strings(&command);
        self.commands.push(command.clone());
        let result = match command.program {
            Program::Docker => {
                assert_eq!(&args[..2], ["--host", ENDPOINT]);
                self.docker(&args[2..])
            }
            Program::Curl => self.curl(&command, &args),
            _ => panic!("unexpected executable role"),
        };
        if let Some(log) = combined {
            use std::io::Write;
            assert!(command.capture.is_none() && command.stdout.is_none());
            let log = std::sync::Arc::try_unwrap(log).unwrap();
            let mut writer = log.writer()?;
            if let Ok(bytes) = &result {
                writer.write_all(bytes).unwrap();
            }
            writer.write_all(b"PRIVATE owned stderr channel\n").unwrap();
            nix::unistd::close(writer).unwrap();
            let finished = log.finish().await?;
            assert!(finished.complete);
            result.map(|_| vec![])
        } else {
            result
        }
    }
    async fn ready(&mut self) -> Result<()> {
        if self.jump_final_ready && self.removed.len() == 2 {
            self.final_ready_calls += 1;
            if self.final_ready_calls == 2 {
                self.clock.as_ref().unwrap().set(plan::WATCHDOG);
            }
        }
        match self.interrupted {
            Some(code) => Err(Self::fail(code)),
            None => Ok(()),
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
}

#[tokio::test]
async fn all_reference_assertions_use_literal_images_owned_containers_and_private_artifacts() {
    let fixture = Fixture::new();
    let mut fake = fixture.fake();
    let mut clock = FakeClock::default();
    let result = execute::run(&fixture.context, ENDPOINT, &mut fake, &mut clock)
        .await
        .unwrap();
    result.result.unwrap();
    assert_eq!(fake.removed, [APPID, DBID]);
    assert_eq!(fake.restarted, [APPID]);
    assert!(fake.containers.is_empty());
    for role in [plan::Role::App, plan::Role::Database] {
        let log = fs::read_to_string(
            fixture
                .root
                .join("artifacts")
                .join(format!("{}.log", role.label())),
        )
        .unwrap();
        assert!(log.contains("PRIVATE owned guest log"));
        assert!(log.contains("PRIVATE owned stderr channel"));
    }
    let summary = fixture.summary();
    assert_eq!(summary["assertionsPassed"], true);
    assert_eq!(summary["engineCleanupPassed"], true);
    assert_eq!(summary["cliGroupsSettled"], true);
    assert_eq!(summary["sourceClosed"], true);
    assert_eq!(summary["appliedMigrations"], 2);
    assert_eq!(summary["registered"], true);
    assert_eq!(summary["restarted"], true);
    assert!(
        !fs::read_to_string(fixture.root.join("artifacts/container-smoke.json"))
            .unwrap()
            .contains("PRIVATE")
    );
    let create: Vec<_> = fake
        .commands
        .iter()
        .filter(|command| {
            command.program == Program::Docker
                && Fake::strings(command)
                    .get(3)
                    .is_some_and(|value| value == "create")
        })
        .collect();
    assert_eq!(create.len(), 2);
    let db = Fake::strings(create[0]);
    assert_eq!(Fake::arg(&db, "--publish"), "127.0.0.1::3000");
    assert_eq!(
        Fake::arg(&db, "--tmpfs"),
        "/var/lib/postgresql:rw,nosuid,nodev,size=512m"
    );
    assert!(db.iter().any(|value| value == PG));
    let app = Fake::strings(create[1]);
    assert_eq!(Fake::arg(&app, "--network"), format!("container:{DBID}"));
    assert_eq!(app.last().unwrap(), APP);
    assert!(app.iter().any(|value| value == "--read-only"));
    assert_eq!(Fake::arg(&app, "--cap-drop"), "ALL");
    assert_eq!(Fake::arg(&app, "--security-opt"), "no-new-privileges");
}

#[tokio::test]
async fn every_smoke_assertion_failure_still_retires_its_owned_resources() {
    for bad in [
        "health",
        "html",
        "mime",
        "asset-bytes",
        "rooms",
        "migration-zero",
        "migration-count",
        "registered",
        "restart",
        "app-stopped",
        "port",
        "db-not-ready",
        "app-not-ready",
    ] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        fake.bad = bad;
        let mut clock = FakeClock::default();
        let result = execute::run(&fixture.context, ENDPOINT, &mut fake, &mut clock)
            .await
            .unwrap();
        assert_eq!(result.result.unwrap_err().code, 1, "{bad}");
        assert!(!fake.containers.contains_key(DBID), "{bad}");
        assert!(!fake.containers.contains_key(APPID), "{bad}");
        assert_eq!(fixture.summary()["engineCleanupPassed"], true, "{bad}");
        if bad == "db-not-ready" {
            assert_eq!(fake.database_probes, 120);
            assert_eq!(clock.elapsed(), Duration::from_secs(60));
        }
        if bad == "app-not-ready" {
            assert_eq!(fake.room_probes, 120);
        }
    }
}

#[tokio::test]
async fn lost_create_response_is_recovered_but_current_absence_cannot_prove_an_inflight_create_closed()
 {
    for bad in ["db-create-lost", "db-create-before"] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        fake.bad = bad;
        let result = execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            &mut FakeClock::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.result.unwrap_err().code, 37);
        assert_eq!(
            fixture.summary()["engineCleanupPassed"],
            bad == "db-create-lost"
        );
        assert_eq!(fake.removed.len(), usize::from(bad == "db-create-lost"));
    }
}

#[tokio::test]
async fn changed_container_or_engine_identity_never_authorizes_foreign_removal() {
    for bad in ["renamed", "foreign-image", "foreign-label", "engine-swap"] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        fake.bad = bad;
        let result = execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            &mut FakeClock::default(),
        )
        .await
        .unwrap();
        assert!(result.result.is_err(), "{bad}");
        assert!(!fake.removed.iter().any(|id| id == APPID), "{bad}");
        assert_eq!(fixture.summary()["engineCleanupPassed"], false, "{bad}");
    }
}

#[tokio::test]
async fn cleanup_errors_interrupts_and_unproven_cli_closure_cannot_pass_zero() {
    for (bad, interrupted, expected) in [
        ("app-rm", false, 42),
        ("logs", false, 24),
        ("", true, 143),
        ("html-command", true, 22),
    ] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        fake.bad = bad;
        fake.cancel_after_app_removed = interrupted;
        let result = execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            &mut FakeClock::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.result.unwrap_err().code, expected);
        assert!(fake.removed.iter().any(|id| id == DBID));
    }
    let fixture = Fixture::new();
    let mut fake = fixture.fake();
    fake.clean = false;
    assert!(
        execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            &mut FakeClock::default()
        )
        .await
        .unwrap()
        .result
        .is_err()
    );
    assert_eq!(fixture.summary()["cliGroupsSettled"], false);
}

#[tokio::test]
async fn header_subset_rejects_folded_duplicate_control_and_invalid_name_forms() {
    for header in [
        "HTTP/1.1 200 OK\r\nContent-Type: application/manifest+json\r\n text/plain\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Type: application/manifest+json\r\nContent-Type: text/plain\r\n\r\n",
        "HTTP/1.1 200 OK\r\nBad Name: x\r\nContent-Type: application/manifest+json\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Type:\tapplication/manifest+json\r\n\r\n",
    ] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        fake.header = Some(header.as_bytes().to_vec());
        assert!(
            execute::run(
                &fixture.context,
                ENDPOINT,
                &mut fake,
                &mut FakeClock::default()
            )
            .await
            .unwrap()
            .result
            .is_err()
        );
    }
}

#[test]
fn local_and_ci_selection_have_separate_admission_and_bounded_loopback_grammars() {
    let fixture = Fixture::new();
    let environment = fixture.context.environment.clone();
    assert!(Context::new(&fixture.root, environment.clone(), Mode::Ci, "Linux").is_err());
    let mut hosted = environment;
    hosted.insert("GITHUB_ACTIONS".into(), "true".into());
    hosted.insert("RUNNER_ENVIRONMENT".into(), "github-hosted".into());
    hosted.insert("PRODUCTION_IMAGE".into(), APP.into());
    assert!(Context::new(&fixture.root, hosted, Mode::Ci, "Linux").is_ok());
    for endpoint in [
        "tcp://127.0.0.1:2375",
        "ssh://example.test",
        "unix://relative",
        "unix:///owned\nsocket",
    ] {
        assert!(engine::endpoint(endpoint).is_err());
    }
    for port in [
        "0.0.0.0:3000",
        "127.0.0.1:0",
        "127.0.0.1:65536",
        "127.0.0.1:1\nextra",
        "127.0.0.1:22\r\n",
    ] {
        assert!(http::Origin::parse(port.as_bytes()).is_err());
    }
    assert!(http::Origin::parse(b"127.0.0.1:65535\n").is_ok());
}

#[tokio::test]
async fn late_success_after_command_or_final_close_deadline_cannot_return_zero() {
    for phase in ["last-room", "final-ready", "known-failure"] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        let mut clock = FakeClock::default();
        fake.clock = Some(clock.0.clone());
        fake.jump_last_room = phase == "last-room";
        fake.jump_final_ready = phase != "last-room";
        if phase == "known-failure" {
            fake.bad = "html-command";
        }
        let result = execute::run(&fixture.context, ENDPOINT, &mut fake, &mut clock)
            .await
            .unwrap();
        assert_eq!(
            result.result.unwrap_err().code,
            if phase == "known-failure" { 22 } else { 124 },
            "{phase}"
        );
        assert_eq!(fake.removed, [APPID, DBID]);
    }
}

struct Discovery {
    commands: Vec<Command>,
    output: Vec<u8>,
}
impl Executor for Discovery {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        self.commands.push(command);
        Ok(self.output.clone())
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn selected_context_is_resolved_once_and_never_silently_falls_back_to_a_different_engine() {
    let fixture = Fixture::new();
    let mut context = Context::new(
        &fixture.root,
        fixture.context.environment.clone(),
        Mode::Local,
        "Darwin",
    )
    .unwrap();
    context
        .environment
        .insert("DOCKER_HOST".into(), ENDPOINT.into());
    let mut discovery = Discovery {
        commands: vec![],
        output: b"tcp://example.test:2375\n".to_vec(),
    };
    assert_eq!(
        engine::discover(&context, &mut discovery).await.unwrap(),
        ENDPOINT
    );
    assert!(discovery.commands.is_empty());
    context
        .environment
        .insert("DOCKER_CONTEXT".into(), "remote-context".into());
    assert!(engine::discover(&context, &mut discovery).await.is_err());
    assert_eq!(discovery.commands.len(), 1);
    assert_eq!(
        Fake::strings(&discovery.commands[0]),
        [
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}"
        ]
    );
    assert_eq!(discovery.commands[0].capture, Some(4096));
    assert_eq!(discovery.commands[0].timeout, Duration::from_secs(30));
    discovery.output = format!("{ENDPOINT}\n").into_bytes();
    assert_eq!(
        engine::discover(&context, &mut discovery).await.unwrap(),
        ENDPOINT
    );
    for key in [
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "DOCKER_API_VERSION",
        "BUILDX_BUILDER",
        "BUILDKIT_HOST",
    ] {
        context.environment.insert(key.into(), "fixture".into());
    }
    let frozen = context.frozen_environment();
    for key in [
        "DOCKER_CONTEXT",
        "DOCKER_HOST",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "DOCKER_API_VERSION",
        "BUILDX_BUILDER",
        "BUILDKIT_HOST",
    ] {
        assert!(!frozen.contains_key(std::ffi::OsStr::new(key)));
    }
}

#[tokio::test]
async fn fixed_database_digest_is_resolved_before_intent_and_cold_pull_uses_remaining_budget() {
    let fixture = Fixture::new();
    let mut fake = fixture.fake();
    fake.cold_database = true;
    let mut clock = FakeClock::default();
    clock.0.set(Duration::from_secs(40));
    execute::run(&fixture.context, ENDPOINT, &mut fake, &mut clock)
        .await
        .unwrap()
        .result
        .unwrap();
    let pull = fake
        .commands
        .iter()
        .find(|command| {
            Fake::strings(command)
                .get(2)
                .is_some_and(|value| value == "pull")
        })
        .unwrap();
    assert_eq!(pull.timeout, Duration::from_secs(175));
    let journal: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("artifacts/ownership.json")).unwrap())
            .unwrap();
    assert_eq!(journal["database"]["image"], PG);
    assert_eq!(journal["database"]["sourceReference"], plan::POSTGRES_IMAGE);
    for code in [124, 130, 143, 37] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        fake.image_failure = Some(code);
        let outcome = execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            &mut FakeClock::default(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.result.unwrap_err().code, code);
        assert!(!fake.commands.iter().any(|command| {
            Fake::strings(command)
                .get(2)
                .is_some_and(|value| value == "pull")
        }));
        assert!(fake.containers.is_empty());
    }
    let fixture = Fixture::new();
    let mut fake = fixture.fake();
    fake.image_failure = Some(1);
    fake.clean = false;
    assert!(
        execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            &mut FakeClock::default()
        )
        .await
        .unwrap()
        .result
        .is_err()
    );
    assert!(!fake.commands.iter().any(|command| {
        Fake::strings(command)
            .get(2)
            .is_some_and(|value| value == "pull")
    }));
}

#[derive(Default)]
struct AdmissionClock(std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<Duration>>>);
impl Clock for AdmissionClock {
    fn elapsed(&self) -> Duration {
        self.0.borrow_mut().pop_front().unwrap_or_default()
    }
    async fn sleep(&mut self, _: Duration) {
        panic!("no sleep in admission fixture");
    }
}

#[tokio::test]
async fn preforward_engine_budget_and_interrupt_failures_explicitly_settle_admitted_logs() {
    use std::os::fd::AsRawFd;
    for (phase, expected) in [("engine", 1), ("budget", 124), ("interrupt", 143)] {
        let fixture = Fixture::new();
        let artifacts = files::Artifacts::new(&fixture.context).unwrap();
        let (file, stamp) = artifacts.create_log(plan::Role::App).unwrap();
        let output = file.as_raw_fd();
        let log = crate::ci::bounded_log::Log::start(file, files::MAX_LOG).unwrap();
        let pipes = log.pipe_fds();
        let mut command = plan::docker(ENDPOINT, &["logs", "never-forwarded"], false);
        command.capture = None;
        command.combined_log = Some(std::sync::Arc::new(log));
        let mut fake = fixture.fake();
        let mut clock = AdmissionClock::default();
        let values = clock.0.clone();
        let mut engine = engine::Engine::bind(ENDPOINT, &mut fake, &mut clock)
            .await
            .unwrap();
        match phase {
            "engine" => engine.runner.identity = "changed".into(),
            "budget" => {
                values
                    .borrow_mut()
                    .extend([Duration::ZERO, Duration::ZERO, plan::WATCHDOG])
            }
            "interrupt" => engine.runner.interrupted = Some(143),
            _ => unreachable!(),
        }
        assert_eq!(engine.run(command).await.unwrap_err().code, expected);
        for fd in [output, pipes[0], pipes[1]] {
            assert!(
                !std::path::Path::new(&format!("/dev/fd/{fd}")).exists(),
                "{phase}"
            );
        }
        assert!(!engine.runner.commands.iter().any(|command| {
            Fake::strings(command)
                .iter()
                .any(|value| value == "never-forwarded")
        }));
        artifacts.check_log(plan::Role::App, &stamp).unwrap();
        artifacts.finish().unwrap();
    }
}

#[tokio::test]
async fn every_dispatch_reserves_grace_and_complete_cross_platform_settlement_allowance() {
    for cleanup in [false, true] {
        for remaining in [25, 26] {
            let fixture = Fixture::new();
            let mut fake = fixture.fake();
            let mut clock = FakeClock::default();
            let elapsed = clock.0.clone();
            let mut engine = engine::Engine::bind(ENDPOINT, &mut fake, &mut clock)
                .await
                .unwrap();
            let limit = if cleanup {
                plan::WATCHDOG
            } else {
                plan::WATCHDOG - plan::RESERVE
            };
            elapsed.set(limit - Duration::from_secs(remaining));
            let result = engine
                .run(plan::docker(
                    ENDPOINT,
                    &["info", "--format", "{{.ID}}"],
                    cleanup,
                ))
                .await;
            if remaining == 25 {
                assert_eq!(result.unwrap_err().code, 124);
                assert_eq!(
                    engine.runner.commands.len(),
                    1,
                    "no preflight or body may dispatch"
                );
            } else {
                assert!(result.is_ok());
                assert_eq!(engine.runner.commands.len(), 3);
                for command in &engine.runner.commands[1..] {
                    assert_eq!(command.timeout, Duration::from_secs(1));
                    assert_eq!(command.grace, Duration::from_secs(5));
                    assert!(
                        command.timeout + command.grace + plan::SETTLEMENT_ALLOWANCE
                            <= Duration::from_secs(remaining)
                    );
                }
            }
        }
    }
}
