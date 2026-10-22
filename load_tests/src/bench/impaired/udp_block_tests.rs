//! The actual lease with virtual CLI groups and explicitly closed private files.
use super::super::tcp_tests::{Fault, Fixture};
use super::*;
#[tokio::test]
async fn complete_cli_settlement_is_reserved_before_any_privileged_dispatch() {
    for remaining in [0, 1, 25_000, 26_000] {
        let (fixture, plan) = Fixture::new(Fault::None);
        let services = fixture.services();
        let mut lease = Lease::new(&plan, &services).unwrap();
        fixture.set_time(100_000 - remaining);
        let error = lease
            .prepare(&plan, &services, 100_000.0)
            .await
            .unwrap_err();
        assert_eq!(controller::exit_code(&error), 124);
        assert_eq!(fixture.call_count(), 0);
        lease.close(&plan, &services).unwrap();
    }
}
#[tokio::test]
async fn private_snapshot_is_never_serialized_into_diagnostic_projection() {
    let (fixture, plan) = Fixture::new(Fault::None);
    let services = fixture.services();
    let mut lease = Lease::new(&plan, &services).unwrap();
    lease.prepare(&plan, &services, 100_000.0).await.unwrap();
    let value = lease.evidence();
    assert!(!value.to_string().contains("PRIVATE_SENTINEL"));
    assert_eq!(value["baselineCounts"], serde_json::json!([3, 1]));
    assert_eq!(value["baselineSha256"].as_str().unwrap().len(), 64);
    assert_eq!(value["backend"], "nf_tables");
    lease.close(&plan, &services).unwrap();
}
