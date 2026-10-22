//! Complete fake engine/clock traces; no candidate or engine is executed.

#[path = "fixture.rs"]
pub(super) mod fixture;
#[path = "oracle_tests.rs"]
mod oracle;
use super::*;
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::Failure,
};
use fixture::{ENDPOINT, Fixture};
use std::time::Duration;

#[tokio::test]
async fn complete_owner_binds_export_roundtrip_two_artifacts_and_all_27_retirement_calls() {
    let fixture = Fixture::new();
    let files = fixture.files();
    let mut fake = fixture.fake();
    fake.cleanup_ticks = true;
    execute::run(
        &fixture.context,
        ENDPOINT,
        &mut fake,
        files,
        fixture.tools(),
    )
    .await
    .unwrap();
    let cleanup = fake
        .calls
        .iter()
        .filter(|call| call.cleanup)
        .collect::<Vec<_>>();
    assert_eq!(cleanup.len(), 27);
    assert_eq!(fixture.context.time.elapsed(), Duration::from_secs(702));
    assert!(
        cleanup
            .iter()
            .all(|call| call.timeout <= Duration::from_secs(5)
                && call.grace == Duration::from_secs(1))
    );
    let target = fixture.root.join("temporary/loadtest-image");
    let names = std::fs::read_dir(&target)
        .unwrap()
        .map(|v| v.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert_eq!(
        std::fs::read(target.join("image-id.txt")).unwrap(),
        format!("{}\n", fixture.image).as_bytes()
    );
    assert_eq!(
        std::fs::read(target.join(fixture.context.archive_name())).unwrap(),
        [b"inert-zstd:".as_slice(), &fixture.raw].concat()
    );
    assert!(!String::from_utf8_lossy(&fake.replayed).contains("PRIVATE"));
    assert!(
        fake.calls
            .iter()
            .any(|call| call.program == Program::Docker
                && call.arguments.iter().any(|v| v == "save"))
    );
    assert_eq!(
        fake.calls
            .iter()
            .filter(|call| call.program == Program::Zstd)
            .count(),
        2
    );
    assert!(fake.build_limit.unwrap() <= Duration::from_secs(1060));
}
#[tokio::test]
async fn known_build_status_survives_every_cleanup_attempt_and_forbids_publication() {
    for code in [1, 42, 124, 130, 143] {
        let fixture = Fixture::new();
        let files = fixture.files();
        let mut fake = fixture.fake();
        fake.fault = "build";
        fake.primary = code;
        let error = execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            files,
            fixture.tools(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(fake.calls.iter().filter(|c| c.cleanup).count(), 27);
        assert!(!fixture.root.join("temporary/loadtest-image").exists());
    }
}
#[tokio::test]
async fn foreign_bindings_lost_creation_roundtrip_and_final_fences_cannot_pass() {
    for fault in ["marker", "foreign-name", "volume-lost", "roundtrip", "zstd"] {
        let fixture = Fixture::new();
        let files = fixture.files();
        let mut fake = fixture.fake();
        fake.fault = fault;
        assert!(
            execute::run(
                &fixture.context,
                ENDPOINT,
                &mut fake,
                files,
                fixture.tools()
            )
            .await
            .is_err(),
            "{fault}"
        );
        assert!(!fixture.root.join("temporary/loadtest-image").exists());
        if matches!(fault, "marker" | "foreign-name") {
            assert!(
                !fake
                    .calls
                    .iter()
                    .any(|c| c.arguments.iter().any(|v| v == "rm")
                        && c.arguments.iter().any(|v| v == "container"))
            );
        }
    }
    let fixture = Fixture::new();
    let files = fixture.files();
    let mut fake = fixture.fake();
    fake.final_jump = true;
    assert_eq!(
        execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            files,
            fixture.tools()
        )
        .await
        .unwrap_err()
        .code,
        124
    );
    assert!(!fixture.root.join("temporary/loadtest-image").exists());
}
#[tokio::test]
async fn shared_virtual_clock_refuses_dispatch_and_clips_build_with_the_full_tail() {
    let fixture = Fixture::new();
    let files = fixture.files();
    let tools = fixture.tools();
    let mut fake = fixture.fake();
    let mut command = fixture.context.build(ENDPOINT, "owned", false);
    fixture.context.time.set(Duration::from_secs(999));
    assert_eq!(
        plan::available(&fixture.context, &command).unwrap(),
        Duration::from_secs(61)
    );
    fixture.context.time.set(Duration::from_secs(1060));
    assert!(plan::available(&fixture.context, &command).is_err());
    command.timeout = Duration::from_secs(1);
    let mut capture = capture::Capture {
        inner: &mut fake,
        files: &files,
        context: &fixture.context,
        tools: &tools,
    };
    assert_eq!(capture.run(command).await.unwrap_err().code, 124);
    assert!(fake.calls.is_empty());
    tools.finish().unwrap();
    files.finish().unwrap();
}
#[tokio::test]
async fn hash_worker_is_joined_and_closed_after_known_cancellation_and_keeps_prior_error() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let fixture = Fixture::new();
    let mut fake = fixture.fake();
    let entered = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    struct Pending<'a> {
        fake: &'a mut fixture::Fake,
        entered: Arc<AtomicBool>,
    }
    impl Executor for Pending<'_> {
        async fn run(&mut self, _: Command) -> crate::tooling::Result<Vec<u8>> {
            unreachable!()
        }
        async fn ready(&mut self) -> crate::tooling::Result<()> {
            if self.entered.load(Ordering::SeqCst) {
                Err(Failure::new(143, "fixed cancel"))
            } else {
                self.fake.ready().await
            }
        }
    }
    let mut pending = Pending {
        fake: &mut fake,
        entered: entered.clone(),
    };
    let done = finished.clone();
    let input = std::fs::File::create(fixture.root.join("worker-owned-file")).unwrap();
    let closed = Arc::new(AtomicBool::new(false));
    let closed_by_worker = closed.clone();
    let result = hash::work(&fixture.context, &mut pending, move |abort, _| {
        entered.store(true, Ordering::SeqCst);
        while !abort.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        files::close(input)?;
        closed_by_worker.store(true, Ordering::SeqCst);
        done.store(true, Ordering::SeqCst);
        Err::<(), _>(plan::failure())
    })
    .await;
    assert_eq!(result.unwrap_err().code, 143);
    assert!(finished.load(Ordering::SeqCst));
    assert!(closed.load(Ordering::SeqCst));
    let mut fake = fixture.fake();
    let result = hash::work(&fixture.context, &mut fake, |_, _| {
        Err::<(), _>(Failure::new(42, "known worker error"))
    })
    .await;
    assert_eq!(result.unwrap_err().code, 42);
}

#[tokio::test]
async fn actual_postbuild_dispatch_trace_fits_the_1482_second_tail() {
    let fixture = Fixture::new();
    let mut fake = fixture.fake();
    execute::run(
        &fixture.context,
        ENDPOINT,
        &mut fake,
        fixture.files(),
        fixture.tools(),
    )
    .await
    .unwrap();
    let build = fake
        .calls
        .iter()
        .position(|c| c.arguments.iter().any(|v| v == "--target"))
        .unwrap();
    let tail = &fake.calls[build + 1..];
    assert_eq!(tail.len(), 37);
    let slots = tail
        .iter()
        .map(|c| c.timeout + c.grace + plan::SETTLEMENT)
        .sum::<Duration>();
    assert_eq!(slots, Duration::from_secs(1307));
    let total = slots + Duration::from_secs(120 + 25 + 30);
    assert_eq!(total, Duration::from_secs(1482));
    assert!(total < plan::TAIL);
    assert_eq!(tail.iter().filter(|c| c.cleanup).count(), 27);
    assert_eq!(
        tail.iter().filter(|c| c.program == Program::Zstd).count(),
        2
    );
}
#[tokio::test]
async fn cancellation_lost_builder_response_and_changed_references_never_publish() {
    for (fault, code) in [
        ("cancel-build", 130),
        ("cancel-build", 143),
        ("builder-lost", 42),
        ("bootstrap-empty", 42),
        ("volume-owner", 1),
        ("replace-after-rm", 1),
        ("builder-err", 1),
    ] {
        let fixture = Fixture::new();
        let mut fake = fixture.fake();
        fake.fault = fault;
        fake.primary = code;
        let error = execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            fixture.files(),
            fixture.tools(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, code, "{fault}");
        assert!(!fixture.root.join("temporary/loadtest-image").exists());
        if matches!(fault, "bootstrap-empty" | "replace-after-rm") {
            assert!(
                !fake
                    .calls
                    .iter()
                    .any(|c| c.arguments.windows(2).any(|v| v == ["buildx", "rm"]))
            );
        }
        if fault == "volume-owner" {
            assert!(
                !fake
                    .calls
                    .iter()
                    .any(|c| c.arguments.windows(2).any(|v| v == ["volume", "rm"]))
            );
        }
    }
    let fixture = Fixture::new();
    let mut fake = fixture.fake();
    fake.reference_volume = true;
    assert!(
        execute::run(
            &fixture.context,
            ENDPOINT,
            &mut fake,
            fixture.files(),
            fixture.tools()
        )
        .await
        .is_err()
    );
    assert!(
        !fake
            .calls
            .iter()
            .any(|c| c.arguments.windows(2).any(|v| v == ["volume", "rm"]))
    );
}
#[test]
fn joined_worker_result_precedes_only_later_observed_cancellation() {
    for code in [1, 42, 124] {
        let result = hash::conclude(
            Ok(()),
            Err::<(), _>(Failure::new(code, "joined worker failure")),
            Err(Failure::new(143, "later cancellation")),
            Err(plan::expired()),
        );
        assert_eq!(result.unwrap_err().code, code);
    }
    assert_eq!(
        hash::conclude(
            Err(Failure::new(130, "earlier owner")),
            Err::<(), _>(Failure::new(42, "worker")),
            Err(Failure::new(143, "later")),
            Ok(())
        )
        .unwrap_err()
        .code,
        130
    );
    assert_eq!(
        hash::conclude(
            Ok(()),
            Ok(()),
            Err(Failure::new(143, "later")),
            Err(plan::expired())
        )
        .unwrap_err()
        .code,
        143
    );
    assert_eq!(
        hash::conclude(Ok(()), Ok(()), Ok(()), Err(plan::expired()))
            .unwrap_err()
            .code,
        124
    );
}
#[test]
fn frozen_execution_environment_removes_git_and_transport_overrides_only_for_this_owner() {
    let fixture = Fixture::new();
    let files = fixture.files();
    let mut environment = fixture.context.environment.clone();
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_CONFIG_COUNT",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "BUILDX_CONFIG",
    ] {
        environment.insert(key.into(), "private redirected value".into());
    }
    environment.insert("CALLER_RUNTIME".into(), "retained".into());
    let selected = files.environment(&environment);
    assert!(
        !selected
            .keys()
            .any(|key| key.as_encoded_bytes().starts_with(b"GIT_"))
    );
    assert!(!selected.contains_key(std::ffi::OsStr::new("DOCKER_HOST")));
    assert_eq!(
        selected
            .get(std::ffi::OsStr::new("CALLER_RUNTIME"))
            .unwrap(),
        "retained"
    );
    assert_eq!(
        selected.get(std::ffi::OsStr::new("DOCKER_CONFIG")).unwrap(),
        files.path.join("docker").as_os_str()
    );
    files.finish().unwrap();
}

#[test]
fn missing_selected_tools_keep_127_without_exposing_private_paths() {
    for missing in ["git", "docker", "zstd"] {
        let fixture = Fixture::new();
        std::fs::remove_file(fixture.root.join("bin").join(missing)).unwrap();
        let error = match tools::Tools::select(&fixture.root, &fixture.context.environment) {
            Ok(_) => panic!("missing tool accepted"),
            Err(error) => error,
        };
        assert_eq!(error.code, 127);
        assert!(!error.message.contains(fixture.root.to_str().unwrap()));
        assert!(!error.message.contains("Node"));
    }
}

#[test]
fn cache_csv_path_rejects_comma_without_rejecting_literal_spaces_or_shell_text() {
    let fixture = Fixture::new();
    let mut environment = fixture.context.environment.clone();
    environment.insert(
        "RUNNER_TEMP".into(),
        fixture.root.join("cache,mode=foreign").into_os_string(),
    );
    let result = plan::Context::new(
        &fixture.root,
        environment,
        "Linux",
        std::time::Instant::now(),
    );
    assert!(result.is_err());
    assert!(
        fixture
            .context
            .temporary
            .to_string_lossy()
            .contains("literal $(x)")
    );
}
