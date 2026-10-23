//! Pure producer-policy regressions.

use super::{fixture::*, *};

fn wire() -> serde_json::Value {
    serde_json::json!({"schemaVersion":1,"selection":{"repository":"owner/repo",
        "revision":"a".repeat(40),"packageVersion":"0.1.0","protocolVersion":1,
        "target":TARGET,"buildRunId":1,"runAttempt":1},"source":{"path":"/materials/source",
        "sha256":"b".repeat(64)},"builderKit":{"path":"/materials/kit","sha256":"c".repeat(64)},
        "output":"/controlled/fresh-run"})
}
fn decode(value: &serde_json::Value) -> Result<request::Request> {
    let bytes = serde_json::to_vec(value).unwrap();
    request::Request::decode(&bytes, &sha(&bytes))
}
#[test]
fn request_original_bytes_and_strict_relationships_are_admitted() {
    let value = wire();
    let request = decode(&value).unwrap();
    assert_eq!(
        request.selection.identity(),
        simplestchat_ops::distribution::manifest::Selection::decode(
            &serde_json::to_vec(&value["selection"]).unwrap()
        )
        .unwrap()
        .identity()
    );
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    assert!(request::Request::decode(&bytes, &request.sha256).is_err());
    assert!(!format!("{request:?}").contains("materials"));
}
#[test]
fn request_refuses_scope_types_numbers_unknowns_and_aliases() {
    for (field, value) in [
        ("schemaVersion", serde_json::json!(1.0)),
        ("selection", serde_json::json!({})),
        ("PRIVATE-KEY", serde_json::json!("PRIVATE-VALUE")),
        ("output", serde_json::json!("/materials/source/run")),
        ("output", serde_json::json!("/materials/../escape")),
        ("output", serde_json::json!("/controlled:injected/run")),
    ] {
        let mut request = wire();
        request[field] = value;
        assert!(decode(&request).is_err(), "{field}");
    }
    for field in ["buildRunId", "runAttempt", "protocolVersion"] {
        for invalid in [
            serde_json::json!(true),
            serde_json::json!(1.0),
            serde_json::json!(null),
        ] {
            let mut request = wire();
            request["selection"][field] = invalid;
            assert!(decode(&request).is_err());
        }
    }
    let mut request = wire();
    request["builderKit"]["path"] = "/materials/source/kit".into();
    assert!(decode(&request).is_err());
}
#[test]
fn duplicate_decoded_keys_and_oversized_requests_are_refused() {
    let original = serde_json::to_string(&wire()).unwrap();
    let duplicate = original.replacen("{", "{\"schemaVersion\":1,", 1);
    assert!(request::Request::decode(duplicate.as_bytes(), &sha(duplicate.as_bytes())).is_err());
    let mut oversized = original.into_bytes();
    oversized.resize(64 * 1024 + 1, b' ');
    assert!(request::Request::decode(&oversized, &sha(&oversized)).is_err());
}

#[test]
fn only_output_paths_need_the_path_list_colon_refusal() {
    let mut value = wire();
    value["source"]["path"] = "/materials/source:opaque".into();
    assert!(decode(&value).is_ok());
    value["output"] = "/controlled/fresh:run".into();
    assert!(decode(&value).is_err());
}
#[test]
fn watchdog_reserves_cleanup_and_never_admits_zero_body() {
    let clock = FakeClock::new();
    let deadline = Deadline::new(&clock);
    assert_eq!(
        deadline.body(Duration::from_secs(2000)).unwrap(),
        Duration::from_secs(1054)
    );
    clock.0.set(clock.now() + Duration::from_secs(1055));
    assert_eq!(deadline.body(Duration::from_secs(1)).unwrap_err().code, 124);
    clock.0.set(clock.now() + Duration::from_secs(25));
    assert!(deadline.check(false).is_err());
    assert!(deadline.check(true).is_ok());
    clock.0.set(clock.now() + RESERVE);
    assert!(deadline.check(true).is_err());
}
#[test]
fn embedded_stages_preserve_the_outer_end_and_original_settlement_reserve() {
    let clock = FakeClock::new();
    let end = clock.now() + Duration::from_secs(300);
    let first = Deadline::bounded(&clock, end);
    assert_eq!(
        first.body(Duration::from_secs(2000)).unwrap(),
        Duration::from_secs(154)
    );
    clock.0.set(clock.now() + Duration::from_secs(50));
    let next = Deadline::bounded(&clock, end);
    assert_eq!(next.end(), end);
    assert_eq!(
        next.body(Duration::from_secs(2000)).unwrap(),
        Duration::from_secs(104)
    );
    clock.0.set(end - RESERVE);
    assert_eq!(next.check(false).unwrap_err().code, 124);
    assert!(next.check(true).is_ok());
    clock.0.set(end);
    assert_eq!(next.check(true).unwrap_err().code, 124);

    let capped = Deadline::bounded(&clock, clock.now() + Duration::from_secs(3600));
    assert_eq!(capped.end(), clock.now() + TOTAL);
    let short = Deadline::bounded(&clock, clock.now() + Duration::from_secs(119));
    assert_eq!(short.check(false).unwrap_err().code, 124);
}
#[test]
fn dispatch_cutoff_keeps_the_chosen_body_and_original_end_after_setup() {
    let clock = FakeClock::new();
    let start = clock.now();
    let end = start + Duration::from_secs(300);
    let deadline = Deadline::bounded(&clock, end);
    let body = deadline.body(Duration::from_secs(30)).unwrap();
    let latest = deadline.dispatch_by(body).unwrap();
    assert_eq!(latest, start + Duration::from_secs(125));
    clock.0.set(start + Duration::from_secs(50));
    let next_stage = Deadline::bounded(&clock, end);
    assert_eq!(next_stage.dispatch_by(body).unwrap(), latest);
    clock.0.set(latest);
    assert_eq!(deadline.dispatch_by(body).unwrap(), latest);
    clock.0.set(latest + Duration::from_nanos(1));
    assert_eq!(deadline.dispatch_by(body).unwrap_err().code, 124);
    assert_eq!(next_stage.dispatch_by(body).unwrap_err().code, 124);
    assert!(deadline.check(false).is_ok());
    assert_eq!(deadline.dispatch_by(Duration::ZERO).unwrap_err().code, 124);
    assert_eq!(deadline.dispatch_by(Duration::MAX).unwrap_err().code, 124);
}

#[test]
fn remaining_limited_body_keeps_setup_headroom_without_extending_the_original_end() {
    let clock = FakeClock::new();
    let start = clock.now();
    let end = start + Duration::from_secs(300);
    let deadline = Deadline::bounded(&clock, end);
    let body = deadline.body(Duration::from_secs(2000)).unwrap();
    assert_eq!(body, Duration::from_secs(154));
    let latest = deadline.dispatch_by(body).unwrap();
    assert_eq!(latest, start + DISPATCH_SETUP);
    assert_eq!(latest + body + GRACE + SETTLEMENT + RESERVE, end);
    clock.0.set(latest);
    assert_eq!(deadline.dispatch_by(body).unwrap(), latest);
    clock.0.set(latest + Duration::from_nanos(1));
    assert_eq!(deadline.dispatch_by(body).unwrap_err().code, 124);
    assert_eq!(deadline.body(Duration::ZERO).unwrap_err().code, 124);
}

#[test]
fn real_clock_remaining_limited_body_still_admits_a_later_setup_sample() {
    let clock = NativeClock;
    let deadline = Deadline::bounded(&clock, clock.now() + Duration::from_secs(300));
    let body = deadline.body(Duration::from_secs(2000)).unwrap();
    assert!(body < Duration::from_secs(2000));
    // Advance the real clock past the body() sample. Previously its calculated
    // cutoff equalled that sample, making every remaining-limited body fail.
    std::thread::sleep(Duration::from_millis(1));
    let latest = deadline.dispatch_by(body).unwrap();
    assert!(clock.now() <= latest);
    assert_eq!(latest + body + GRACE + SETTLEMENT + RESERVE, deadline.end());
}

#[tokio::test]
async fn actual_build_construction_passes_original_cutoff_to_delayed_fake_setup() {
    struct Delayed<'a> {
        clock: &'a FakeClock,
        end: Instant,
        attempted: usize,
        spawned: usize,
    }
    impl crate::ci::process::Executor for Delayed<'_> {
        async fn run(&mut self, command: crate::ci::command::Command) -> Result<Vec<u8>> {
            self.attempted += 1;
            let latest = command
                .ops_stage_dispatch
                .expect("original stage dispatch bound");
            assert_eq!(command.timeout, Duration::from_secs(30));
            assert_eq!(
                latest,
                self.end - RESERVE - command.timeout - GRACE - SETTLEMENT
            );
            self.clock.0.set(latest + Duration::from_nanos(1));
            if self.clock.now() > latest {
                return Err(failure(124));
            }
            self.spawned += 1;
            panic!("late setup cannot admit a process")
        }
        async fn ready(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let budget = namespace::Budget::default();
    let request = fixture.request(&budget).await;
    let run = namespace::OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let clock = FakeClock::new();
    let end = clock.now() + Duration::from_secs(300);
    let deadline = Deadline::bounded(&clock, end);
    let mut executor = Delayed {
        clock: &clock,
        end,
        attempted: 0,
        spawned: 0,
    };
    let result = execute::produce(&request, &run, &mut executor, &deadline).await;
    assert_eq!(result.unwrap_err().code, 124);
    assert_eq!(executor.attempted, 1);
    assert_eq!(executor.spawned, 0);
    assert!(
        std::fs::read_dir(run.path.join("target"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(!run.path.join("build.log").exists());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[test]
fn descriptor_cap_is_checked_and_partial_reservations_unwind() {
    let budget = namespace::Budget::default();
    let held = budget.reserve(128).unwrap();
    assert_eq!(budget.count(), 128);
    assert!(budget.permit().is_err());
    drop(held);
    assert_eq!(budget.count(), 0);
    assert!(budget.reserve(129).is_err());
    assert_eq!(budget.count(), 0);
}
#[test]
fn tool_versions_are_bounded_observations_not_private_output() {
    let bytes = concat!(
        "rustc 1.99.0 (fixture 2026-10-22)\nrelease: 1.99.0\n",
        "host: x86_64-unknown-linux-gnu\nPRIVATE: hidden\n"
    )
    .as_bytes();
    let result = resolution::version(bytes, false).unwrap().to_string();
    assert!(!result.contains("hidden"));
    assert_eq!(resolution::version(bytes, true).unwrap_err().code, 1);
    assert!(resolution::version(&bytes.repeat(200), false).is_err());
    assert!(resolution::version(&[bytes, b"release: 1.99.0\n"].concat(), false).is_err());
}
