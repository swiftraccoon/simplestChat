//! Complete controller traces and finite clock/ownership failures on inert fixtures.

use super::{fixture::*, *};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn exact_four_inspections_surround_five_requests_and_all_owners_close() {
    let f = Fixture::new();
    let clock = Clock::new();
    let time = plan::Time::new(clock.clone());
    let http = Http::new(clock.clone());
    let calls = http.calls.clone();
    let (tools, scratch, ca) = f.owners();
    let mut executor = Fake::new(&f, clock);
    execute::complete(&f.config, tools, scratch, ca, http, &time, &mut executor)
        .await
        .unwrap();
    assert_eq!(executor.calls, 4);
    assert_eq!(*calls.lock().unwrap(), policy::ENDPOINTS.map(|e| e.path));
    assert_eq!(std::fs::read_dir(&f.root).unwrap().count(), 2);
}
#[tokio::test]
async fn each_inspection_or_request_failure_blocks_later_work_and_retains_status() {
    for index in 0..4 {
        for code in [1, 42, 124, 130, 143] {
            let f = Fixture::new();
            let clock = Clock::new();
            let time = plan::Time::new(clock.clone());
            let http = Http::new(clock.clone());
            let calls = http.calls.clone();
            let (t, s, c) = f.owners();
            let mut executor = Fake::new(&f, clock);
            executor.fail_at = Some((index, code));
            assert_eq!(
                execute::complete(&f.config, t, s, c, http, &time, &mut executor)
                    .await
                    .unwrap_err()
                    .code,
                code
            );
            assert_eq!(executor.calls, index + 1);
            assert_eq!(calls.lock().unwrap().len(), if index < 2 { 0 } else { 5 });
        }
    }
    for index in 0..5 {
        let f = Fixture::new();
        let clock = Clock::new();
        let time = plan::Time::new(clock.clone());
        let mut http = Http::new(clock.clone());
        http.fail_at = Some(index);
        let calls = http.calls.clone();
        let (t, s, c) = f.owners();
        let mut executor = Fake::new(&f, clock);
        assert_eq!(
            execute::complete(&f.config, t, s, c, http, &time, &mut executor)
                .await
                .unwrap_err()
                .code,
            1
        );
        assert_eq!(executor.calls, 2);
        assert_eq!(calls.lock().unwrap().len(), index + 1);
    }
}
#[tokio::test]
async fn late_ready_response_and_unproven_group_never_become_success() {
    for late in [true, false] {
        let f = Fixture::new();
        let clock = Clock::new();
        let time = plan::Time::new(clock.clone());
        let mut http = Http::new(clock.clone());
        if late {
            http.late_at = Some(0);
        }
        let (t, s, c) = f.owners();
        let mut executor = Fake::new(&f, clock);
        executor.clean = late;
        assert_eq!(
            execute::complete(&f.config, t, s, c, http, &time, &mut executor)
                .await
                .unwrap_err()
                .code,
            if late { 124 } else { 1 }
        );
        assert_eq!(executor.calls, if late { 2 } else { 4 });
    }
}
#[test]
fn full_phase_tail_and_monotonic_fences_refuse_before_more_work() {
    let clock = Clock::new();
    let time = plan::Time::new(clock.clone());
    assert_eq!(
        time.docker(0).unwrap(),
        clock.start + Duration::from_secs(16)
    );
    clock.set(16);
    time.docker(0).unwrap();
    clock.set(17);
    assert_eq!(time.docker(0).unwrap_err().code, 124);
    clock.set(16);
    assert_eq!(time.now().unwrap_err().code, 1);
    let clock = Clock::new();
    let time = plan::Time::new(clock.clone());
    clock.set(68);
    time.http().unwrap();
    clock.set(69);
    assert_eq!(time.http().unwrap_err().code, 124);
    clock.set(180);
    assert_eq!(time.now().unwrap_err().code, 124);
}
#[tokio::test]
async fn late_setup_dispatches_nothing_but_finishes_both_capture_peers() {
    let f = Fixture::new();
    let clock = Clock::new();
    let time = plan::Time::new(clock.clone());
    let http = Http::new(clock.clone());
    let (t, s, c) = f.owners();
    let mut executor = Fake::new(&f, clock);
    executor.late_setup = true;
    assert_eq!(
        execute::complete(&f.config, t, s, c, http, &time, &mut executor)
            .await
            .unwrap_err()
            .code,
        124
    );
    assert_eq!(executor.calls, 0);
    assert_eq!(std::fs::read_dir(&f.root).unwrap().count(), 2);
}

#[tokio::test]
async fn held_http_cancellation_finishes_the_same_owners_and_never_runs_post_inspections() {
    use std::sync::atomic::Ordering;
    for code in [130, 143] {
        let f = Fixture::new();
        let clock = Clock::new();
        let time = plan::Time::new(clock.clone());
        let mut http = Http::new(clock.clone());
        http.pending = true;
        let calls = http.calls.clone();
        let (t, s, c) = f.owners();
        let mut executor = Fake::new(&f, clock);
        let interrupt = executor.interrupt.clone();
        let cancel = async {
            while calls.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
            interrupt.store(code, Ordering::SeqCst);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                execute::complete(&f.config, t, s, c, http, &time, &mut executor),
                cancel
            )
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().code, code);
        assert_eq!(executor.calls, 2);
        assert_eq!(std::fs::read_dir(&f.root).unwrap().count(), 2);
    }
}
#[test]
fn fixture_arguments_are_closed_and_never_contain_dynamic_diagnostics() {
    let f = Fixture::new();
    let mut args = vec![
        "--fixture-token".into(),
        f.config.token.clone(),
        "--caddy-id".into(),
        f.config.caddy.clone(),
        "--app-id".into(),
        f.config.app.clone(),
        "--ca".into(),
        f.config.ca.to_str().unwrap().into(),
        "--ca-sha256".into(),
        f.config.sha256.clone(),
    ];
    config::Config::parse(&args).unwrap();
    args[0] = "--origin".into();
    assert_eq!(
        config::Config::parse(&args).err().unwrap().message,
        "fixture_arguments"
    );
    args[0] = "--fixture-token".into();
    args[3] = args[5].clone();
    assert_eq!(
        config::Config::parse(&args).err().unwrap().message,
        "fixture_distinct_services"
    );
}
#[test]
fn recursive_duplicate_nonfinite_and_selected_exact_types_stay_refused() {
    for bytes in [
        br#"{"a":{"b":1,"b":2}}"#.as_slice(),
        br#"{"a":NaN}"#,
        br#"{"a":1e999}"#,
    ] {
        assert!(json::object(bytes).is_err());
    }
    for version in ["true", "1.0", "1e0", "null", "\"1\""] {
        let mut response = response(1);
        response.body = String::from_utf8(response.body)
            .unwrap()
            .replace("\"version\":1", &format!("\"version\":{version}"))
            .into_bytes();
        assert_eq!(
            policy::validate(1, response.status, &response.headers, &response.body)
                .unwrap_err()
                .message,
            "http_public_capabilities"
        );
    }
    let _: Arc<dyn plan::Clock> = Clock::new();
}
