//! Content selections, caller-independent identity and whole stage admission.

use super::{
    catalog::Catalog,
    context::{ArtifactKind, Context},
    fixture, plan,
};
use crate::tooling::Environment;
use serde_json::{Value, json};
use std::{ffi::OsString, sync::Arc, time::Duration};

fn environment(root: &std::path::Path) -> Environment {
    [
        ("GITHUB_ACTIONS", "true"),
        ("RUNNER_OS", "Linux"),
        ("RUNNER_ARCH", "X64"),
        ("GITHUB_REPOSITORY", "swiftraccoon/simplestChat"),
        ("GITHUB_REPOSITORY_ID", "1155881649"),
        ("GITHUB_REPOSITORY_OWNER_ID", "47705353"),
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_WORKFLOW", "CI"),
        ("RUNNER_ENVIRONMENT", "github-hosted"),
        ("GITHUB_SERVER_URL", "https://github.com"),
        ("GITHUB_API_URL", "https://api.github.com"),
        ("GITHUB_RUN_ID", "123"),
        ("GITHUB_RUN_ATTEMPT", "2"),
        (
            "GITHUB_WORKFLOW_REF",
            "swiftraccoon/simplestChat/.github/workflows/ci.yml@refs/heads/main",
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .chain([
        ("GITHUB_SHA".into(), "a".repeat(40).into()),
        ("GITHUB_WORKFLOW_SHA".into(), "a".repeat(40).into()),
        ("GITHUB_WORKSPACE".into(), root.as_os_str().to_owned()),
    ])
    .collect()
}
#[test]
fn hosted_identity_has_fixed_authority_and_safe_numeric_identity() {
    let fixture = fixture::Fixture::new();
    let original = environment(&fixture.root);
    let context = Context::capture(&original).unwrap();
    assert_eq!(
        serde_json::to_value(context.selection().unwrap().identity()).unwrap()["buildRunId"],
        123
    );
    assert_eq!(
        context.artifact_name(ArtifactKind::Signed),
        format!(
            "simplestchat-ops-{}-x86_64-unknown-linux-musl",
            "a".repeat(40)
        )
    );
    assert_eq!(
        context.artifact_name(ArtifactKind::Unsigned),
        format!(
            "simplestchat-ops-unsigned-{}-x86_64-unknown-linux-musl",
            "a".repeat(40)
        )
    );
    for (key, value) in [
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("RUNNER_ENVIRONMENT", "self-hosted"),
        ("GITHUB_REPOSITORY_ID", "1155881648"),
        ("GITHUB_REPOSITORY_OWNER_ID", "47705354"),
        ("GITHUB_REPOSITORY", "PRIVATE/other"),
        ("GITHUB_REF", "refs/heads/private"),
        ("GITHUB_WORKFLOW_SHA", "PRIVATE-other"),
        ("GITHUB_WORKFLOW_REF", "PRIVATE-other"),
        ("GITHUB_RUN_ID", "9007199254740992"),
        ("GITHUB_RUN_ID", "01"),
        ("GITHUB_RUN_ATTEMPT", "0"),
        ("GITHUB_WORKSPACE", "../PRIVATE"),
    ] {
        let mut changed = original.clone();
        changed.insert(key.into(), value.into());
        fixture::expect_error(Context::capture(&changed), 1);
    }
    let mut boundary = original;
    boundary.insert("GITHUB_RUN_ID".into(), "9007199254740991".into());
    assert!(Context::capture(&boundary).is_ok());
    assert!(
        !serde_json::to_string(&context)
            .unwrap()
            .contains(&fixture.root.display().to_string())
    );
}

#[test]
fn compiled_catalog_binds_original_content_and_closed_component_graph() {
    let catalog = Catalog::load().unwrap();
    assert_eq!(catalog.artifacts.len(), 70);
    assert_eq!(catalog.identity.archive_bytes, 409693088);
    let original = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ops/ops-build-inputs.lock.json"
    ));
    assert_eq!(catalog.identity.sha256, super::super::sha(original));
    assert_eq!(catalog.identity.bytes, original.len());
    let decoded: Value = serde_json::from_slice(original).unwrap();
    for (pointer, value) in [
        ("/profile", json!("PRIVATE")),
        ("/rustVersion", json!("1.100.0")),
        (
            "/artifacts/0/url",
            json!("https://PRIVATE.invalid/evil.tar.gz"),
        ),
        ("/artifacts/0/sha256", json!("bad")),
        ("/artifacts/0/bytes", json!(0)),
        ("/artifacts/0/component", json!("PRIVATE")),
        (
            "/artifacts/4/dependencies",
            json!(["PRIVATE-not-in-closure"]),
        ),
        (
            "/ubuntuIndices/0/url",
            json!("https://PRIVATE.invalid/Packages.xz"),
        ),
    ] {
        let mut changed = decoded.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        fixture::expect_error(Catalog::decode(&serde_json::to_vec(&changed).unwrap()), 1);
    }
    let mut duplicate = decoded;
    let first = duplicate["artifacts"][0].clone();
    duplicate["artifacts"].as_array_mut().unwrap().push(first);
    fixture::expect_error(Catalog::decode(&serde_json::to_vec(&duplicate).unwrap()), 1);
}

#[test]
fn schedule_keeps_all_future_phases_and_settlement_without_extending_entry() {
    let clock = Arc::new(fixture::Clock::new());
    let plan = plan::Plan::new(clock.clone());
    let original_end = plan.end();
    fixture::expect_error(plan.begin(1), 1);
    for index in 0..5 {
        let end = plan.begin(index).unwrap();
        let (body, latest) = plan.body(end, Duration::from_secs(120)).unwrap();
        assert!(body <= Duration::from_secs(120));
        assert!(latest + body + plan::SETTLEMENT <= end);
        clock.advance(Duration::from_secs(1));
        plan.complete(index, end).unwrap();
    }
    plan.finalizing().unwrap();
    assert_eq!(plan.end(), original_end);
    let clock = Arc::new(fixture::Clock::new());
    let plan = plan::Plan::new(clock.clone());
    clock.advance(Duration::from_secs(301));
    fixture::expect_error(plan.begin(0), 124);
    let clock = Arc::new(fixture::Clock::new());
    let plan = plan::Plan::new(clock.clone());
    let end = plan.begin(0).unwrap();
    clock.advance(Duration::from_secs(300));
    fixture::expect_error(plan.complete(0, end), 124);
}

#[test]
fn kit_profile_keeps_exact_initial_setup_headroom_without_moving_entry() {
    let reserved = plan::PHASES.iter().copied().sum::<Duration>() + plan::FINAL;
    assert_eq!(reserved, Duration::from_secs(3540));
    assert_eq!(plan::ENTRY, Duration::from_secs(3600));
    assert_eq!(plan::FINAL, Duration::from_secs(300));
    let slack = plan::ENTRY - reserved;
    assert_eq!(slack, Duration::from_secs(60));
    for over in [Duration::ZERO, Duration::from_nanos(1)] {
        let clock = Arc::new(fixture::Clock::new());
        let plan = plan::Plan::new(clock.clone());
        let original_end = plan.end();
        clock.advance(slack + over);
        if over.is_zero() {
            assert!(plan.begin(0).is_ok());
        } else {
            fixture::expect_error(plan.begin(0), 124);
        }
        assert_eq!(plan.end(), original_end);
    }
}

struct MaterialClock(Arc<fixture::Clock>);
impl super::super::Clock for MaterialClock {
    fn now(&self) -> std::time::Instant {
        plan::Clock::now(self.0.as_ref())
    }
}

#[test]
fn profiled_copy_and_later_reads_share_original_active_cutoff_and_final_tail() {
    let clock = Arc::new(fixture::Clock::new());
    let plan = plan::Plan::new(clock.clone());
    let original_end = plan.end();
    for index in 0..3 {
        let end = plan.begin(index).unwrap();
        plan.complete(index, end).unwrap();
    }
    let kit_start = plan.now().unwrap();
    let kit_end = plan.begin(3).unwrap();
    assert_eq!(kit_end - kit_start, Duration::from_secs(540));
    clock.advance(Duration::from_millis(120420));
    let borrowed = MaterialClock(clock.clone());
    let deadline = super::super::Deadline::bounded(&borrowed, kit_end);
    assert_eq!(deadline.end(), kit_end);
    assert_eq!(super::super::RESERVE, Duration::from_secs(120));
    // The copy duration is the complete local Darwin measurement. The two
    // thirty-second later scans are synthetic admission examples, not a claim
    // about actual full-kit reads, cold Linux or a completed hosted build.
    for elapsed in [
        Duration::from_millis(134362),
        Duration::from_secs(30),
        Duration::from_secs(30),
    ] {
        clock.advance(elapsed);
        deadline.check(false).unwrap();
    }
    let active_end = kit_end - super::super::RESERVE;
    assert_eq!(active_end - kit_start, Duration::from_secs(420));
    let before_cutoff = active_end - Duration::from_nanos(1);
    clock.advance(before_cutoff - plan.now().unwrap());
    deadline.check(false).unwrap();
    clock.advance(Duration::from_nanos(1));
    fixture::expect_error(deadline.check(false), 124);
    deadline.check(true).unwrap();
    clock.advance(Duration::from_secs(120));
    fixture::expect_error(deadline.check(true), 124);
    fixture::expect_error(plan.complete(3, kit_end), 124);
    assert_eq!(deadline.end(), kit_end);
    assert_eq!(plan.end(), original_end);
}

#[test]
fn late_kit_completion_cannot_borrow_the_producer_or_finalization_reserves() {
    for over in [Duration::ZERO, Duration::from_nanos(1)] {
        let clock = Arc::new(fixture::Clock::new());
        let plan = plan::Plan::new(clock.clone());
        let original_end = plan.end();
        clock.advance(Duration::from_secs(59));
        for (index, span) in plan::PHASES.iter().take(4).enumerate() {
            let end = plan.begin(index).unwrap();
            clock.advance(*span - Duration::from_secs(1));
            plan.complete(index, end).unwrap();
        }
        assert_eq!(plan::PHASES[4], Duration::from_secs(1200));
        let future = plan::PHASES[4] + plan::FINAL;
        let gap = original_end - plan.now().unwrap() - future;
        assert_eq!(gap, Duration::from_secs(5));
        clock.advance(gap + over);
        if over.is_zero() {
            let producer_end = plan.begin(4).unwrap();
            assert_eq!(producer_end + plan::FINAL, original_end);
            clock.advance(plan::PHASES[4] - Duration::from_secs(1));
            plan.complete(4, producer_end).unwrap();
            plan.finalizing().unwrap();
        } else {
            fixture::expect_error(plan.begin(4), 124);
        }
        assert_eq!(plan.end(), original_end);
    }
}

#[test]
fn generated_bootstrap_shim_is_relocation_derived_and_fixed_not_a_path_selector() {
    let bytes = std::str::from_utf8(super::shim::CC).unwrap();
    assert!(bytes.contains("ops_host_tools=${0%/bin/cc}"));
    assert!(bytes.contains("exec \"$ops_host_tools/sysroot/usr/bin/x86_64-linux-gnu-gcc-13\""));
    for forbidden in ["dirname", "readlink", "$(", "|", "PATH=", "eval", "sh -c"] {
        assert!(!bytes.contains(forbidden));
    }
    let fixture = fixture::Fixture::new();
    let env = super::cc_environment(&fixture.root).unwrap();
    assert_eq!(env.len(), 3);
    assert!(env.iter().all(|(_, value)| {
        value.as_encoded_bytes().starts_with(
            fixture
                .root
                .join("kit/host-tools/")
                .as_os_str()
                .as_encoded_bytes(),
        )
    }));
    assert_eq!(super::cc_sha256(), super::super::sha(super::shim::CC));
    let _: Vec<(OsString, OsString)> = env;
}
