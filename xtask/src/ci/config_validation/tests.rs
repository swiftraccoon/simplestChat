//! Owned fake engine/files only: no daemon, network or validator is executed.

use super::*;
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Environment, Failure, Result},
};
use engine::Clock;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[path = "native_tests.rs"]
mod native;
#[path = "oracle_tests.rs"]
mod oracle;

const ENDPOINT: &str = "unix:///owned/config-docker.sock";
const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CID: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) context: Context,
}
impl Fixture {
    pub(super) fn new(kind: Kind) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "config validation {} {} literal $(text)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        for directory in ["temporary", "ops/ansible/tests", "ops/ansible/files"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("Caddyfile"), "fixture Caddyfile\n").unwrap();
        fs::write(
            root.join("ops/ansible/tests/monitoring-rules.test.yml"),
            "rule_files:\n  - ../files/monitoring-alerts.yml\n",
        )
        .unwrap();
        fs::write(
            root.join("ops/ansible/files/monitoring-alerts.yml"),
            "groups: []\n",
        )
        .unwrap();
        let environment = Environment::from([
            ("GITHUB_ACTIONS".into(), "true".into()),
            ("RUNNER_ENVIRONMENT".into(), "github-hosted".into()),
            (
                "RUNNER_TEMP".into(),
                root.join("temporary").into_os_string(),
            ),
            ("CADDY_IMAGE".into(), plan::CADDY.into()),
            ("TRUSTED_PROXY_SECRET".into(), plan::PROXY_SECRET.into()),
        ]);
        let context = Context::new(&root, environment, kind, "Linux", Instant::now()).unwrap();
        Self { root, context }
    }
    pub(super) fn artifacts(&self) -> PathBuf {
        let paths = fs::read_dir(self.root.join("temporary"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 1);
        paths[0].clone()
    }
    fn evidence(&self) -> Value {
        serde_json::from_slice(&fs::read(self.artifacts().join("config-validation.json")).unwrap())
            .unwrap()
    }
    fn fake(&self) -> Fake {
        Fake {
            root: self.root.clone(),
            kind: self.context.kind,
            ..Fake::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
struct FakeClock(Rc<Cell<Duration>>);
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
    label: String,
    running: bool,
}
struct Fake {
    kind: Kind,
    root: PathBuf,
    container: Option<Container>,
    commands: Vec<Command>,
    removed: Vec<String>,
    bad: &'static str,
    guest: u8,
    pending: Option<u8>,
    clean: bool,
    cold: bool,
    clock: Option<Rc<Cell<Duration>>>,
    after_terminal: bool,
    advance_cleanup: bool,
    cleanup_calls: usize,
    replayed: Vec<u8>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            kind: Kind::Caddy,
            root: PathBuf::new(),
            container: None,
            commands: vec![],
            removed: vec![],
            bad: "",
            guest: 0,
            pending: None,
            clean: true,
            cold: false,
            clock: None,
            after_terminal: false,
            advance_cleanup: false,
            cleanup_calls: 0,
            replayed: vec![],
        }
    }
}
impl Fake {
    fn strings(command: &Command) -> Vec<String> {
        command
            .arguments
            .iter()
            .map(|v| v.to_str().unwrap().into())
            .collect()
    }
    fn arg(args: &[String], flag: &str) -> String {
        args[args.iter().position(|v| v == flag).unwrap() + 1].clone()
    }
    fn error(code: u8) -> Failure {
        Failure::new(code, "Private fake engine failure.")
    }
    fn folder(&self) -> PathBuf {
        fs::read_dir(self.root.join("temporary"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }
    fn execute(&mut self, args: &[String], cleanup: bool) -> Result<Vec<u8>> {
        assert_eq!(&args[..2], ["--host", ENDPOINT]);
        let args = &args[2..];
        if cleanup {
            self.cleanup_calls += 1;
            if self.advance_cleanup {
                let clock = self.clock.as_ref().unwrap();
                clock.set(clock.get() + Duration::from_secs(26));
            }
        }
        match args[0].as_str() {
            "info" => Ok(if self.bad == "daemon" && self.after_terminal {
                b"different daemon\n".to_vec()
            } else {
                b"opaque daemon UUID\n".to_vec()
            }),
            "image" => {
                if args[1] == "ls" {
                    assert_eq!(args.last().unwrap(), self.kind.repository());
                    if self.bad == "catalog" {
                        return Err(Self::error(1));
                    }
                    if self.cold {
                        return Ok(vec![]);
                    }
                    let mut row = json!({"repository":self.kind.repository(),
                        "digest":self.kind.image().split_once('@').unwrap().1,
                        "id":IMAGE,"tag":"selected"});
                    if self.bad == "catalog-mapping" {
                        row["id"] = json!(format!("sha256:{}", "b".repeat(64)));
                    }
                    if self.bad == "catalog-private" {
                        row["privatePayload"] = json!("PRIVATE");
                    }
                    match self.bad {
                        "catalog-digest" => row["digest"] = json!("PRIVATE_BAD_DIGEST"),
                        "catalog-empty" => row["repository"] = json!(""),
                        "catalog-tag" => row["tag"] = json!(""),
                        _ => {}
                    }
                    let mut bytes = serde_json::to_vec(&row).unwrap();
                    bytes.push(b'\n');
                    if self.bad == "catalog-duplicate" {
                        bytes.extend_from_slice(&bytes.clone());
                    }
                    if matches!(self.bad, "catalog-alias" | "catalog-conflict") {
                        row["tag"] = json!("another-tag");
                        if self.bad == "catalog-conflict" {
                            row["id"] = json!(format!("sha256:{}", "b".repeat(64)));
                        }
                        bytes.extend_from_slice(&serde_json::to_vec(&row).unwrap());
                        bytes.push(b'\n');
                    }
                    return Ok(bytes);
                }
                Ok(format!("{IMAGE}\n").into_bytes())
            }
            "pull" => {
                assert!(matches!(args[2].as_str(), plan::PROMETHEUS | plan::CADDY));
                self.cold = false;
                Ok(b"PRIVATE_DAEMON_PULL\n".to_vec())
            }
            "logs" => {
                assert_eq!(args[1], CID);
                Ok(b"PRIVATE_CONFIG_LOG_DATA\n".to_vec())
            }
            "container" => match args[1].as_str() {
                "create" => {
                    let intent: Value = serde_json::from_slice(
                        &fs::read(self.folder().join("ownership.json")).unwrap(),
                    )
                    .unwrap();
                    let name = Self::arg(args, "--name");
                    let label = Self::arg(args, "--label");
                    assert_eq!(intent["name"], name);
                    assert_eq!(intent["image"], IMAGE);
                    assert_eq!(
                        label,
                        format!("{}={}", plan::OWNER, intent["labelValue"].as_str().unwrap())
                    );
                    if self.bad != "lost-empty" {
                        self.container = Some(Container {
                            name,
                            image: IMAGE.into(),
                            label,
                            running: false,
                        });
                    }
                    if matches!(self.bad, "lost" | "lost-empty") {
                        return Err(Self::error(42));
                    }
                    if self.bad == "malformed-id" {
                        return Ok(b"PRIVATE_BAD_ID\nextra\n".to_vec());
                    }
                    Ok(format!("{CID}\n").into_bytes())
                }
                "start" => {
                    assert_eq!(args[2], CID);
                    self.container.as_mut().unwrap().running = true;
                    Ok(format!("{CID}\n").into_bytes())
                }
                "wait" => {
                    assert_eq!(args[2], CID);
                    self.container.as_mut().unwrap().running = false;
                    if self.bad == "source" {
                        fs::write(self.root.join("Caddyfile"), "changed").unwrap();
                    }
                    if let Some(code) = self.bad.strip_prefix("wait-") {
                        let code = code.parse::<u8>().unwrap();
                        if matches!(code, 130 | 143) {
                            self.pending = Some(code);
                        }
                        return Err(Self::error(code));
                    }
                    Ok(format!("{}\n", self.guest).into_bytes())
                }
                "inspect" => {
                    assert_eq!(args.last().unwrap(), CID);
                    let container = self.container.as_ref().unwrap();
                    let mut name = format!("/{}", container.name);
                    let mut image = container.image.clone();
                    let mut label = container.label.split_once('=').unwrap().1.to_owned();
                    if cleanup {
                        match self.bad {
                            "foreign-name" => name = "/foreign replacement".into(),
                            "foreign-image" => image = format!("sha256:{}", "b".repeat(64)),
                            "foreign-label" => label = "foreign nonce".into(),
                            _ => {}
                        }
                    }
                    let mut state = json!({"Running":container.running,
                        "Status":if container.running {"running"} else {"exited"},
                        "ExitCode":self.guest});
                    if self.bad == "no-terminal" {
                        state.as_object_mut().unwrap().remove("ExitCode");
                    }
                    if self.bad == "wrong-terminal" {
                        state["ExitCode"] = json!(17);
                    }
                    if self
                        .commands
                        .iter()
                        .any(|v| Self::strings(v).get(3).is_some_and(|s| s == "wait"))
                    {
                        self.after_terminal = true;
                    }
                    Ok(
                        serde_json::to_vec(&json!({"Id":CID,"Name":name,"Image":image,
                        "Config":{"Image":container.image,"Labels":{plan::OWNER:label}},
                        "State":state}))
                        .unwrap(),
                    )
                }
                "ls" => Ok(if self.container.is_some() {
                    format!("{CID}\n").into_bytes()
                } else {
                    vec![]
                }),
                "rm" => {
                    assert_eq!(args, ["container", "rm", "--force", "--volumes", CID]);
                    if self.bad == "remove" {
                        return Err(Self::error(13));
                    }
                    self.removed.push(CID.into());
                    self.container = None;
                    if self.bad == "late-cancel" {
                        self.pending = Some(143);
                    }
                    Ok(format!("{CID}\n").into_bytes())
                }
                _ => panic!("unexpected fake command"),
            },
            _ => panic!("unexpected fake command"),
        }
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut request: Command) -> Result<Vec<u8>> {
        assert_eq!(request.program, Program::Docker);
        assert_eq!(request.capture, None);
        let log = std::sync::Arc::try_unwrap(request.combined_log.take().unwrap()).unwrap();
        let args = Self::strings(&request);
        let cleanup = request.cleanup;
        self.commands.push(request);
        let result = self.execute(&args, cleanup);
        let bytes = result
            .as_ref()
            .map_or(b"PRIVATE_DAEMON_ERROR\n".as_slice(), Vec::as_slice)
            .to_vec();
        let writer = log.writer().unwrap();
        let written = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut writer = writer;
            let written = writer.write_all(&bytes);
            let closed = nix::unistd::close(writer);
            written.map_err(|_| Self::error(1))?;
            closed.map_err(|_| Self::error(1))
        })
        .await
        .unwrap();
        let finished = log.finish().await;
        result?;
        written?;
        assert!(finished?.complete);
        Ok(vec![])
    }
    async fn ready(&mut self) -> Result<()> {
        if self.advance_cleanup && self.after_terminal && self.cleanup_calls == 0 {
            self.clock.as_ref().unwrap().set(Duration::from_secs(120));
        }
        match self.pending {
            Some(code) => Err(Self::error(code)),
            None => Ok(()),
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
    async fn replay(&mut self, bytes: &[u8], _: bool, _: Instant) -> Result<()> {
        self.replayed.extend_from_slice(bytes);
        if self.advance_cleanup {
            let clock = self.clock.as_ref().unwrap();
            clock.set(clock.get() + Duration::from_secs(30));
        }
        self.ready().await
    }
}

async fn transaction(fixture: &Fixture, fake: &mut Fake, clock: &mut FakeClock) -> Result<()> {
    let inputs = files::Inputs::read(&fixture.context).unwrap();
    let artifacts = files::Artifacts::new(&fixture.context).unwrap();
    execute::run(&fixture.context, ENDPOINT, fake, clock, inputs, artifacts).await
}

#[tokio::test]
async fn both_complete_kinds_keep_private_diagnostics_and_retire_only_their_exact_resource() {
    for kind in [Kind::Promtool, Kind::Caddy] {
        let fixture = Fixture::new(kind);
        let mut fake = fixture.fake();
        transaction(&fixture, &mut fake, &mut FakeClock::default())
            .await
            .unwrap();
        assert_eq!(fake.removed, [CID]);
        assert!(fake.container.is_none());
        let evidence = fixture.evidence();
        assert_eq!(evidence["guestExitCode"], 0);
        for key in [
            "guestTerminalAgreement",
            "logsCollected",
            "resourceAbsenceObserved",
            "cliGroupsSettled",
            "sourceClosed",
        ] {
            assert_eq!(evidence[key], true, "{key}");
        }
        assert!(
            !String::from_utf8(fake.replayed)
                .unwrap()
                .contains("PRIVATE")
        );
        assert!(
            !fs::read_to_string(fixture.artifacts().join("config-validation.json"))
                .unwrap()
                .contains("PRIVATE")
        );
    }
}

#[tokio::test]
async fn partial_create_reconciles_exact_identity_but_empty_uncertain_lookup_is_never_proof() {
    for bad in ["lost", "lost-empty", "malformed-id"] {
        let fixture = Fixture::new(Kind::Caddy);
        let mut fake = fixture.fake();
        fake.bad = bad;
        let error = transaction(&fixture, &mut fake, &mut FakeClock::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, if bad == "malformed-id" { 1 } else { 42 });
        assert_eq!(fake.removed.is_empty(), bad == "lost-empty");
        assert_eq!(
            fixture.evidence()["resourceAbsenceObserved"],
            bad != "lost-empty"
        );
    }
}

#[tokio::test]
async fn foreign_ownership_daemon_or_terminal_changes_cannot_pass_or_delete_foreign_resources() {
    for bad in [
        "foreign-name",
        "foreign-image",
        "foreign-label",
        "daemon",
        "no-terminal",
        "wrong-terminal",
        "source",
    ] {
        let fixture = Fixture::new(Kind::Caddy);
        let mut fake = fixture.fake();
        fake.bad = bad;
        assert!(
            transaction(&fixture, &mut fake, &mut FakeClock::default())
                .await
                .is_err(),
            "{bad}"
        );
        if bad.starts_with("foreign") || bad == "daemon" {
            assert!(fake.removed.is_empty(), "{bad}");
            assert!(fake.container.is_some());
        } else {
            assert_eq!(fake.removed, [CID]);
        }
    }
}

#[tokio::test]
async fn known_guest_tool_signal_or_timeout_status_survives_later_cleanup_failures() {
    for guest in [1, 42, 124, 130, 143] {
        let fixture = Fixture::new(Kind::Caddy);
        let mut fake = fixture.fake();
        fake.guest = guest;
        fake.bad = "remove";
        assert_eq!(
            transaction(&fixture, &mut fake, &mut FakeClock::default())
                .await
                .unwrap_err()
                .code,
            guest
        );
    }
    for bad in ["wait-42", "wait-124", "wait-130", "wait-143", "late-cancel"] {
        let fixture = Fixture::new(Kind::Caddy);
        let mut fake = fixture.fake();
        fake.bad = bad;
        let expected = bad
            .strip_prefix("wait-")
            .map_or(143, |value| value.parse().unwrap());
        assert_eq!(
            transaction(&fixture, &mut fake, &mut FakeClock::default())
                .await
                .unwrap_err()
                .code,
            expected
        );
        assert_eq!(fake.removed, [CID]);
    }
    let fixture = Fixture::new(Kind::Caddy);
    let mut fake = fixture.fake();
    fake.clean = false;
    assert_eq!(
        transaction(&fixture, &mut fake, &mut FakeClock::default())
            .await
            .unwrap_err()
            .code,
        1
    );
}

#[tokio::test]
async fn the_full_fifteen_cli_cleanup_and_replay_allowance_fits_the_fixed_entry_policy() {
    let fixture = Fixture::new(Kind::Caddy);
    let mut fake = fixture.fake();
    let mut clock = FakeClock::default();
    fake.clock = Some(clock.0.clone());
    fake.advance_cleanup = true;
    transaction(&fixture, &mut fake, &mut clock).await.unwrap();
    assert_eq!(fake.cleanup_calls, 15);
    assert_eq!(clock.elapsed(), Duration::from_secs(540));
    for command in fake.commands.iter().filter(|command| command.cleanup) {
        assert!(command.timeout <= Duration::from_secs(5));
        assert_eq!(command.grace, Duration::from_secs(1));
    }
}

#[tokio::test]
async fn cold_pull_uses_only_the_fixed_selector_and_unfit_commands_do_not_dispatch() {
    let fixture = Fixture::new(Kind::Promtool);
    let mut fake = fixture.fake();
    fake.cold = true;
    transaction(&fixture, &mut fake, &mut FakeClock::default())
        .await
        .unwrap();
    let pulls = fake
        .commands
        .iter()
        .map(Fake::strings)
        .filter(|args| args.get(2).is_some_and(|value| value == "pull"))
        .collect::<Vec<_>>();
    assert_eq!(
        pulls,
        [vec![
            "--host",
            ENDPOINT,
            "pull",
            "--quiet",
            plan::PROMETHEUS
        ]]
    );
    for cleanup in [false, true] {
        let fixture = Fixture::new(Kind::Caddy);
        let artifacts = files::Artifacts::new(&fixture.context).unwrap();
        let mut fake = fixture.fake();
        let mut clock = FakeClock::default();
        let elapsed = clock.0.clone();
        let mut capture = capture::Capture {
            inner: &mut fake,
            artifacts: &artifacts,
            started: Instant::now(),
        };
        let mut core = engine::Core::bind(
            ENDPOINT,
            &mut capture,
            &mut clock,
            engine::Policy::validation(),
        )
        .await
        .unwrap();
        let limit = if cleanup { 570 } else { 120 };
        elapsed.set(Duration::from_secs(limit - 21));
        let result = core
            .run(core.command(&["info", "--format", "{{.ID}}"], cleanup))
            .await;
        assert_eq!(result.unwrap_err().code, 124);
        assert_eq!(fake.commands.len(), 1);
        artifacts.finish().unwrap();
    }
}

#[tokio::test]
async fn failed_or_malformed_catalog_never_authorizes_a_pull_or_create() {
    for bad in [
        "catalog",
        "catalog-private",
        "catalog-duplicate",
        "catalog-mapping",
        "catalog-conflict",
        "catalog-digest",
        "catalog-empty",
        "catalog-tag",
    ] {
        let fixture = Fixture::new(Kind::Caddy);
        let mut fake = fixture.fake();
        fake.bad = bad;
        assert!(
            transaction(&fixture, &mut fake, &mut FakeClock::default())
                .await
                .is_err()
        );
        assert!(
            !fake
                .commands
                .iter()
                .map(Fake::strings)
                .any(|args| args.get(2).is_some_and(|value| value == "pull")
                    || args.get(3).is_some_and(|value| value == "create")),
            "{bad}"
        );
        assert!(fake.container.is_none());
    }
}

#[tokio::test]
async fn distinct_repository_tags_may_share_the_same_digest_and_immutable_id() {
    let fixture = Fixture::new(Kind::Caddy);
    let mut fake = fixture.fake();
    fake.bad = "catalog-alias";
    transaction(&fixture, &mut fake, &mut FakeClock::default())
        .await
        .unwrap();
    assert_eq!(fake.removed, [CID]);
    assert!(
        !fake
            .commands
            .iter()
            .map(Fake::strings)
            .any(|args| args[2] == "pull")
    );
}

#[test]
fn closed_kind_context_and_mount_grammars_refuse_before_any_resource_acquisition() {
    let fixture = Fixture::new(Kind::Caddy);
    for bad in ["", "unknown", "caddy --private", "promtool;private"] {
        assert!(Kind::parse(bad).is_none());
    }
    for system in ["Darwin", "Windows"] {
        assert!(
            Context::new(
                &fixture.root,
                fixture.context.environment.clone(),
                Kind::Caddy,
                system,
                Instant::now()
            )
            .is_err()
        );
    }
    for field in [
        "GITHUB_ACTIONS",
        "RUNNER_ENVIRONMENT",
        "CADDY_IMAGE",
        "TRUSTED_PROXY_SECRET",
    ] {
        let mut environment = fixture.context.environment.clone();
        environment.insert(field.into(), "PRIVATE".into());
        let error = Context::new(
            &fixture.root,
            environment,
            Kind::Caddy,
            "Linux",
            Instant::now(),
        )
        .err()
        .unwrap();
        assert!(!error.message.contains("PRIVATE"));
    }
    for (kind, suffix) in [(Kind::Caddy, "bad,root"), (Kind::Promtool, "bad:root")] {
        assert!(
            Context::new(
                &fixture.root.join(suffix),
                fixture.context.environment.clone(),
                kind,
                "Linux",
                Instant::now()
            )
            .is_err()
        );
    }
}
