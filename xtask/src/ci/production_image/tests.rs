//! Typed execution plans and failure fixtures. No container daemon is used.

use super::{
    execute, files,
    options::Context,
    plan::{self, Command, Program},
    process::Executor,
};
use crate::tooling::{self, Environment, Result};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[path = "native_tests.rs"]
mod native;

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IMAGE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DRIVER: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

struct Fixture {
    root: PathBuf,
    context: Context,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "image policy {} {} $(literal)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("runner temp")).unwrap();
        fs::create_dir(root.join("migrations")).unwrap();
        fs::write(root.join("migrations/001 first.sql"), "select 1").unwrap();
        fs::write(root.join("migrations/002 $(literal).sql"), "select 2").unwrap();
        fs::write(root.join("environment"), "EXISTING=value\n").unwrap();
        let mut environment = Environment::new();
        for (key, value) in [
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("GITHUB_SHA", REVISION),
            ("PRODUCTION_IMAGE", "fixture:production"),
            ("CACHE_PRIMARY_HIT", "false"),
        ] {
            environment.insert(key.into(), value.into());
        }
        environment.insert(
            "RUNNER_TEMP".into(),
            root.join("runner temp").into_os_string(),
        );
        environment.insert(
            "GITHUB_ENV".into(),
            root.join("environment").into_os_string(),
        );
        let context = Context::new(&root, environment, "Linux", "x86_64", false).unwrap();
        Self { root, context }
    }
    fn output(&self) -> String {
        fs::read_to_string(self.root.join("environment")).unwrap()
    }
    fn cache(&self) {
        fs::create_dir(self.context.temporary.join("buildx-cache")).unwrap();
        fs::write(
            self.context.temporary.join("buildx-cache/index.json"),
            "old",
        )
        .unwrap();
        fs::write(self.context.temporary.join("buildx-cache/old-layer"), "old").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

const CONTAINER: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

struct Fake {
    context: Context,
    commands: Vec<Command>,
    fail: &'static str,
    dirty: bool,
    head: String,
    image: String,
    driver_id: String,
    platform: String,
    user: String,
    label: String,
    source_env: Vec<String>,
    inspect_id: String,
    journal_seen: bool,
    ready_failure: bool,
    container_name: String,
    container_alive: bool,
    started: bool,
    foreign: bool,
    guest_exit: i32,
    mutate_source: bool,
    rename_on_cleanup: bool,
}
impl Fake {
    fn new(context: &Context) -> Self {
        Self {
            context: context.clone(),
            commands: Vec::new(),
            fail: "",
            dirty: false,
            head: REVISION.into(),
            image: IMAGE.into(),
            driver_id: DRIVER.into(),
            platform: "linux/arm64".into(),
            user: "10001:10001".into(),
            label: REVISION.into(),
            source_env: vec![format!("SOURCE_REVISION={REVISION}")],
            inspect_id: IMAGE.into(),
            journal_seen: false,
            ready_failure: false,
            container_name: String::new(),
            container_alive: false,
            started: false,
            foreign: false,
            guest_exit: 0,
            mutate_source: false,
            rename_on_cleanup: false,
        }
    }
    fn args(&self, prefix: &[&str]) -> Vec<Vec<String>> {
        self.commands
            .iter()
            .filter_map(|command| {
                let args: Vec<_> = command
                    .arguments
                    .iter()
                    .map(|arg| arg.to_str().unwrap().to_owned())
                    .collect();
                args.iter()
                    .map(String::as_str)
                    .take(prefix.len())
                    .eq(prefix.iter().copied())
                    .then_some(args)
            })
            .collect()
    }
}
impl Executor for Fake {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        let args: Vec<String> = command
            .arguments
            .iter()
            .map(|a| a.to_str().unwrap().into())
            .collect();
        let at = |flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .map(|i| args[i + 1].clone())
        };
        let phase = if command.program == Program::Git {
            args[0].as_str()
        } else if args.starts_with(&["buildx".into(), "create".into()]) {
            "create"
        } else if args.starts_with(&["buildx".into(), "rm".into()]) {
            "cleanup"
        } else if args.starts_with(&["buildx".into(), "build".into()]) {
            if at("--platform").as_deref() == Some("linux/arm64") {
                "arm"
            } else {
                "build"
            }
        } else if args[0] == "container" {
            match args[1].as_str() {
                "create" => "verify-create",
                "start" => "verify",
                "inspect" => "container-inspect",
                "ls" => "container-ls",
                "rm" => "container-rm",
                _ => panic!("Unexpected container command"),
            }
        } else if args
            .get(3)
            .is_some_and(|value| value == "{{.Os}}/{{.Architecture}}")
        {
            "platform"
        } else {
            "inspect"
        };
        let phase = phase.to_owned();
        if phase == "create" {
            let name = at("--name").unwrap();
            let token = name.strip_prefix("simplestchat-ci-").unwrap();
            assert_eq!(token.len(), 32);
            let journal: Value = serde_json::from_slice(
                &fs::read(
                    self.context
                        .temporary
                        .join(format!("production-builder-{token}/ownership.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                journal,
                json!({"builder":name,"revision":REVISION,"state":"creation-planned"})
            );
            self.journal_seen = true;
        }
        if phase == "build" && self.mutate_source {
            self.dirty = true;
        }
        if phase == "arm" || phase == "build" {
            let id = if phase == "arm" {
                &self.driver_id
            } else {
                &self.image
            };
            fs::write(at("--iidfile").unwrap(), format!("{id}\n")).unwrap();
            if at("--cache-to").is_some() {
                let next = self.context.temporary.join("buildx-cache-next");
                fs::create_dir(&next).unwrap();
                fs::write(next.join("index.json"), "new").unwrap();
            }
        }
        if phase == "verify-create" {
            self.container_name = at("--name").unwrap();
            self.container_alive = true;
            let token = self
                .container_name
                .strip_prefix("simplestchat-ci-verify-")
                .unwrap();
            let journal: Value = serde_json::from_slice(
                &fs::read(
                    self.context
                        .temporary
                        .join(format!("production-verification-{token}/ownership.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                journal,
                json!({"container":self.container_name,"revision":REVISION,
                "state":"creation-planned"})
            );
        }
        if phase == "verify" {
            self.started = true;
        }
        if phase == "container-rm" {
            self.container_alive = false;
        }
        let cleanup = command.cleanup;
        self.commands.push(command);
        if self.fail == phase {
            return Err(tooling::Failure::new(42, "Fixed fixture refusal."));
        }
        Ok(match phase.as_str() {
            "rev-parse" => format!("{}\n", self.head).into_bytes(),
            "status" => {
                if self.dirty {
                    b"?? private-file\n".to_vec()
                } else {
                    Vec::new()
                }
            }
            "verify-create"=>format!("{CONTAINER}\n").into_bytes(),
            "container-ls"=>if self.container_alive {format!("{CONTAINER}\n").into_bytes()}
                else {Vec::new()},
            "container-inspect"=>serde_json::to_vec(&json!({"Id":CONTAINER,
                "Name":if cleanup && self.rename_on_cleanup {"/renamed".into()}
                    else {format!("/{}",self.container_name)},"Image":if self.foreign {DRIVER}else{IMAGE},
                "Config":{"Labels":{plan::OWNER_LABEL:self.container_name}},
                "State":{"Running":false,"Status":if self.started {"exited"} else {"created"},
                    "ExitCode":self.guest_exit}})).unwrap(),
            "platform" => format!("{}\n", self.platform).into_bytes(),
            "inspect" => serde_json::to_vec(&json!({"Id":self.inspect_id,"Config":{
                "User":self.user,"Labels":{"org.opencontainers.image.revision":self.label},
                "Env":self.source_env}}))
            .unwrap(),
            _ => Vec::new(),
        })
    }
    async fn ready(&mut self) -> Result<()> {
        if self.ready_failure {
            Err(tooling::Failure::new(143, "Fixed fixture interruption."))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn all_hosted_and_local_architectures_preserve_driver_platform_and_cleanup_order() {
    for (act, machine, derived) in [
        (false, "x86_64", false),
        (false, "aarch64", false),
        (true, "x86_64", false),
        (true, "aarch64", true),
    ] {
        let mut fixture = Fixture::new();
        fixture
            .context
            .environment
            .insert("ACT".into(), act.to_string().into());
        fixture
            .context
            .environment
            .insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
        fixture.context = Context::new(
            &fixture.root,
            fixture.context.environment.clone(),
            "Linux",
            machine,
            false,
        )
        .unwrap();
        let mut fake = Fake::new(&fixture.context);
        execute::build(&fixture.context, &mut fake).await.unwrap();
        assert!(fake.journal_seen);
        let creates = fake.args(&["buildx", "create"]);
        assert_eq!(creates.len(), 1);
        assert!(
            creates[0]
                .windows(2)
                .any(|a| a == ["--driver", "docker-container"])
        );
        assert_eq!(creates[0].iter().any(|a| a == "--driver-opt"), derived);
        if derived {
            assert!(creates[0].contains(&format!("image={DRIVER}")));
            let arm = &fake.args(&["buildx", "build"])[0];
            assert!(arm.windows(2).any(|a| a == ["--builder", "default"]));
            assert!(arm.windows(2).any(|a| a == ["--platform", "linux/arm64"]));
            assert!(arm.contains(&"--load".into()));
            let dockerfile =
                fs::read_to_string(arm.last().unwrap().to_owned() + "/Dockerfile").unwrap();
            assert_eq!(dockerfile, plan::ARM_DOCKERFILE);
            assert!(dockerfile.contains("@sha256:3ad6bb9bc8c78c0069d03247adb9a59b3"));
        }
        let removed = fake.args(&["buildx", "rm"]);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].last(), creates[0].get(3));
        assert_eq!(
            fixture.output(),
            format!("EXISTING=value\nPRODUCTION_IMAGE={IMAGE}\n")
        );
        assert!(
            fake.commands
                .iter()
                .find(|command| command.arguments.get(1) == Some(&"rm".into()))
                .unwrap()
                .cleanup
        );
    }
}

#[test]
fn ordinary_local_untrusted_unsupported_and_ambiguous_inputs_are_refused() {
    let fixture = Fixture::new();
    for (key, value) in [
        ("GITHUB_ACTIONS", ""),
        ("RUNNER_ENVIRONMENT", "self-hosted"),
        ("ACT", "true"),
        ("GITHUB_SHA", "private\ninput"),
        ("PRODUCTION_IMAGE", "--option"),
        ("RUNNER_TEMP", "/tmp/csv,option"),
    ] {
        let mut environment = fixture.context.environment.clone();
        environment.insert(key.into(), value.into());
        assert!(Context::new(&fixture.root, environment, "Linux", "x86_64", false).is_err());
    }
    assert!(
        Context::new(
            &fixture.root,
            fixture.context.environment.clone(),
            "Darwin",
            "arm64",
            false
        )
        .is_err()
    );
    assert!(
        Context::new(
            &fixture.root,
            fixture.context.environment.clone(),
            "Linux",
            "x86_64",
            true
        )
        .is_err()
    );
}

#[tokio::test]
async fn cache_hits_misses_and_failed_exports_preserve_original_layer_policy() {
    for (hit, restored, status) in [
        (true, true, 0),
        (false, true, 0),
        (false, false, 0),
        (false, true, 42),
    ] {
        let mut fixture = Fixture::new();
        fixture.context.cache_hit = hit;
        if restored {
            fixture.cache();
        }
        let mut fake = Fake::new(&fixture.context);
        if status != 0 {
            fake.fail = "build";
        }
        let result = execute::build(&fixture.context, &mut fake).await;
        assert_eq!(result.err().map_or(0, |error| error.code), status);
        let builds = fake.args(&["buildx", "build"]);
        assert_eq!(builds.len(), 1);
        let tokens = &builds[0];
        assert_eq!(tokens.contains(&"--cache-from".into()), restored);
        assert_eq!(tokens.contains(&"--cache-to".into()), !hit);
        if restored {
            assert!(tokens.contains(&format!(
                "type=local,src={}",
                fixture.context.temporary.join("buildx-cache").display()
            )));
        }
        let cache = fixture.context.temporary.join("buildx-cache");
        assert_eq!(
            fs::read_to_string(cache.join("index.json")).unwrap(),
            if hit || status != 0 { "old" } else { "new" }
        );
        assert_eq!(cache.join("old-layer").exists(), hit || status != 0);
        assert_eq!(
            fixture.context.temporary.join("buildx-cache-next").exists(),
            status != 0
        );
        assert_eq!(
            fixture.output(),
            if status == 0 {
                format!("EXISTING=value\nPRODUCTION_IMAGE={IMAGE}\n")
            } else {
                "EXISTING=value\n".into()
            }
        );
        let build = fake
            .commands
            .iter()
            .find(|command| command.arguments.get(1) == Some(&"build".into()))
            .unwrap();
        assert_eq!(build.timeout, Duration::from_secs(3000));
        assert_eq!(build.grace, Duration::from_secs(120));
        for expected in [
            "--pull",
            "--load",
            "--provenance=false",
            "--sbom=false",
            "SOURCE_REVISION=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "org.opencontainers.image.revision=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(tokens.contains(&expected.into()));
        }
        assert!(
            tokens
                .windows(2)
                .any(|a| a == ["--platform", "linux/amd64"])
        );
    }
}

#[tokio::test]
async fn git_guard_runs_before_any_engine_or_builder_intent() {
    for dirty in [false, true] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture.context);
        if dirty {
            fake.dirty = true;
        } else {
            fake.head = "d".repeat(40);
        }
        assert!(execute::build(&fixture.context, &mut fake).await.is_err());
        assert!(
            fake.commands
                .iter()
                .all(|command| command.program == Program::Git)
        );
        assert_eq!(fs::read_dir(&fixture.context.temporary).unwrap().count(), 0);
        assert_eq!(fixture.output(), "EXISTING=value\n");
    }
}

#[tokio::test]
async fn every_build_failure_retires_only_its_journaled_builder_and_never_publishes() {
    for phase in ["create", "build", "inspect", "cleanup"] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture.context);
        fake.fail = phase;
        let error = execute::build(&fixture.context, &mut fake)
            .await
            .unwrap_err();
        assert_eq!(error.code, 42);
        assert_eq!(fixture.output(), "EXISTING=value\n");
        assert_eq!(fake.args(&["buildx", "rm"]).len(), 1);
        assert!(
            fake.args(&["buildx", "rm"])[0]
                .last()
                .unwrap()
                .starts_with("simplestchat-ci-")
        );
    }
}

#[tokio::test]
async fn invalid_arm_build_id_platform_and_failed_bootstrap_cannot_create_builder() {
    for mode in ["arm", "platform", "id"] {
        let mut fixture = Fixture::new();
        fixture.context.local_arm = true;
        let mut fake = Fake::new(&fixture.context);
        if mode == "arm" {
            fake.fail = "arm";
        } else if mode == "platform" {
            fake.platform = "linux/amd64".into();
        } else {
            fake.driver_id = "floating:tag".into();
        }
        assert!(execute::build(&fixture.context, &mut fake).await.is_err());
        assert!(fake.args(&["buildx", "create"]).is_empty());
        assert!(fake.args(&["buildx", "rm"]).is_empty());
        assert_eq!(fixture.output(), "EXISTING=value\n");
    }
    let arm = plan::arm(std::path::Path::new("/tmp/owned"));
    assert_eq!(arm.timeout, Duration::from_secs(180));
    assert_eq!(arm.grace, Duration::from_secs(10));
}

#[tokio::test]
async fn revision_immutable_id_and_duplicate_source_environment_are_verified_before_publish() {
    for mode in ["iid", "id", "label", "source", "duplicate"] {
        let fixture = Fixture::new();
        let mut fake = Fake::new(&fixture.context);
        match mode {
            "iid" => fake.image = "floating:tag".into(),
            "id" => fake.inspect_id = DRIVER.into(),
            "label" => fake.label = "d".repeat(40),
            "source" => fake.source_env.clear(),
            "duplicate" => fake.source_env.push(format!("SOURCE_REVISION={REVISION}")),
            _ => unreachable!(),
        }
        assert!(execute::build(&fixture.context, &mut fake).await.is_err());
        assert_eq!(fixture.output(), "EXISTING=value\n");
        assert_eq!(fake.args(&["buildx", "rm"]).len(), 1);
    }
}

#[tokio::test]
async fn cancellation_after_builder_retirement_cannot_publish_or_promote_cache() {
    let fixture = Fixture::new();
    fixture.cache();
    let mut fake = Fake::new(&fixture.context);
    fake.ready_failure = true;
    assert_eq!(
        execute::build(&fixture.context, &mut fake)
            .await
            .unwrap_err()
            .code,
        143
    );
    assert_eq!(fixture.output(), "EXISTING=value\n");
    assert_eq!(
        fs::read_to_string(fixture.context.temporary.join("buildx-cache/index.json")).unwrap(),
        "old"
    );
    assert_eq!(fake.args(&["buildx", "rm"]).len(), 1);
}

#[tokio::test]
async fn verification_passes_complete_literal_migration_inventory_to_isolated_guest_predicate() {
    let mut fixture = Fixture::new();
    fixture.context.image = IMAGE.into();
    let mut fake = Fake::new(&fixture.context);
    execute::verify(&fixture.context, &mut fake).await.unwrap();
    let runs = fake.args(&["container", "create"]);
    assert_eq!(runs.len(), 1);
    let args = &runs[0];
    for pair in [
        ["--network", "none"],
        ["--cap-drop", "ALL"],
        ["--security-opt", "no-new-privileges"],
        ["--entrypoint", "/bin/sh"],
    ] {
        assert!(args.windows(2).any(|a| a == pair));
    }
    assert!(args.contains(&"--read-only".into()));
    assert!(args.contains(&plan::VERIFY.into()));
    assert_eq!(
        &args[args.len() - 2..],
        ["001 first.sql", "002 $(literal).sql"]
    );
    for assertion in [
        "id -u",
        "id -g",
        "stat -c %u /app/simplestChat",
        "stat -c %u /app/web/dist/index.html",
        "stat -c %u /app/migrations",
        "test -x /app/simplestChat",
        "test -s /app/web/dist/index.html",
        "test ! -e /app/load_test",
        "test \"${actual_migrations}\" -eq \"$#\"",
        "test -s \"/app/migrations/${migration}\"",
    ] {
        assert!(plan::VERIFY.contains(assertion));
    }
    assert_eq!(fixture.output(), "EXISTING=value\n");
}

#[tokio::test]
async fn bad_user_or_missing_migrations_cannot_start_guest_and_guest_failures_propagate() {
    for mode in ["user", "migrations", "guest"] {
        let mut fixture = Fixture::new();
        fixture.context.image = IMAGE.into();
        let mut fake = Fake::new(&fixture.context);
        match mode {
            "user" => fake.user = "0:0".into(),
            "migrations" => {
                fs::remove_dir_all(fixture.root.join("migrations")).unwrap();
                fs::create_dir(fixture.root.join("migrations")).unwrap();
            }
            "guest" => fake.fail = "verify",
            _ => unreachable!(),
        }
        assert!(execute::verify(&fixture.context, &mut fake).await.is_err());
        assert_eq!(
            fake.args(&["container", "create"]).len(),
            usize::from(mode == "guest")
        );
    }
}

#[test]
fn runner_file_boundaries_refuse_links_fifos_stale_ids_and_csv_paths_without_clobber() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let path = fixture.context.temporary.join("id");
    fs::write(&path, format!("{IMAGE}\n")).unwrap();
    assert_eq!(files::image_id(&path).unwrap(), IMAGE);
    assert!(files::absent(&path).is_err());
    for text in [
        format!("{IMAGE}\nPRIVATE"),
        format!("{IMAGE}\r\n"),
        "sha256:ABC".into(),
    ] {
        fs::write(&path, text).unwrap();
        assert!(files::image_id(&path).is_err());
    }
    fs::remove_file(&path).unwrap();
    symlink(fixture.root.join("environment"), &path).unwrap();
    assert!(files::image_id(&path).is_err());
    assert!(files::new_file(&path, b"clobber").is_err());
    let mut context = fixture.context.clone();
    context
        .environment
        .insert("GITHUB_ENV".into(), path.into_os_string());
    assert!(files::EnvironmentFile::open(&context).is_err());
    let fifo = fixture.root.join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    context
        .environment
        .insert("GITHUB_ENV".into(), fifo.into_os_string());
    assert!(files::EnvironmentFile::open(&context).is_err());
    assert_eq!(fixture.output(), "EXISTING=value\n");
}

#[tokio::test]
async fn existing_output_or_unusable_environment_fails_before_engine_mutation() {
    for mode in ["iid", "cache", "missing-env"] {
        let fixture = Fixture::new();
        let mut context = fixture.context.clone();
        match mode {
            "iid" => fs::write(context.temporary.join("production.iid"), IMAGE).unwrap(),
            "cache" => fs::create_dir(context.temporary.join("buildx-cache-next")).unwrap(),
            "missing-env" => {
                context.environment.remove(OsStr::new("GITHUB_ENV"));
            }
            _ => unreachable!(),
        }
        let mut fake = Fake::new(&context);
        assert!(execute::build(&context, &mut fake).await.is_err());
        assert!(
            fake.commands
                .iter()
                .all(|command| command.program == Program::Git)
        );
        assert_eq!(fixture.output(), "EXISTING=value\n");
    }
}

#[tokio::test]
async fn verification_cleanup_recovers_lost_create_response_and_preserves_foreign_identity() {
    for phase in [
        "verify-create",
        "verify",
        "container-inspect",
        "container-rm",
        "",
    ] {
        let mut fixture = Fixture::new();
        fixture.context.image = IMAGE.into();
        let mut fake = Fake::new(&fixture.context);
        fake.fail = phase;
        let result = execute::verify(&fixture.context, &mut fake).await;
        assert_eq!(result.is_ok(), phase.is_empty());
        if phase == "container-inspect" {
            assert!(fake.args(&["container", "rm"]).is_empty());
        } else {
            let removals = fake.args(&["container", "rm"]);
            assert_eq!(removals.len(), 1);
            assert_eq!(removals[0].last().unwrap(), CONTAINER);
        }
        assert!(
            fake.commands
                .iter()
                .filter(|command| command.arguments.get(1) == Some(&"ls".into())
                    || command.arguments.get(1) == Some(&"rm".into()))
                .all(|command| command.cleanup)
        );
    }
    let mut fixture = Fixture::new();
    fixture.context.image = IMAGE.into();
    let mut fake = Fake::new(&fixture.context);
    fake.foreign = true;
    assert!(execute::verify(&fixture.context, &mut fake).await.is_err());
    assert!(fake.args(&["container", "start"]).is_empty());
    assert!(fake.args(&["container", "rm"]).is_empty());
    assert!(fake.container_alive);
}

#[tokio::test]
async fn verification_checks_stopped_guest_status_and_exact_identity_before_cleanup() {
    let mut fixture = Fixture::new();
    fixture.context.image = IMAGE.into();
    let mut fake = Fake::new(&fixture.context);
    fake.guest_exit = 19;
    assert!(execute::verify(&fixture.context, &mut fake).await.is_err());
    assert_eq!(
        fake.args(&["container", "rm"])[0].last().unwrap(),
        CONTAINER
    );
    assert!(!fake.container_alive);
}

#[tokio::test]
async fn source_mutation_during_build_is_detected_after_owned_cleanup_before_publication() {
    let fixture = Fixture::new();
    let mut fake = Fake::new(&fixture.context);
    fake.mutate_source = true;
    assert!(execute::build(&fixture.context, &mut fake).await.is_err());
    assert_eq!(fake.args(&["buildx", "rm"]).len(), 1);
    assert_eq!(fake.args(&["status"]).len(), 2);
    assert_eq!(fixture.output(), "EXISTING=value\n");
}

#[tokio::test]
async fn renamed_verification_container_is_found_by_recorded_id_and_never_removed_or_accepted() {
    let mut fixture = Fixture::new();
    fixture.context.image = IMAGE.into();
    let mut fake = Fake::new(&fixture.context);
    fake.rename_on_cleanup = true;
    assert!(execute::verify(&fixture.context, &mut fake).await.is_err());
    assert!(fake.args(&["container", "rm"]).is_empty());
    assert!(fake.args(&["container", "ls"])[0].contains(&format!("id={CONTAINER}")));
    assert!(fake.container_alive);
}

#[test]
fn help_and_invalid_subcommands_do_not_construct_runtime_or_engine_context() {
    let root = std::path::Path::new("/nonexistent/read-only-help");
    assert_eq!(
        super::run(root, &["--help".into()]).unwrap(),
        std::process::ExitCode::SUCCESS
    );
    for args in [
        vec![],
        vec!["build".into(), "extra".into()],
        vec!["prune".into()],
    ] {
        assert_eq!(
            super::run(root, &args).unwrap(),
            std::process::ExitCode::from(2)
        );
    }
}
