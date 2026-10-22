use super::*;

#[test]
fn malformed_blocks_remain_failed_observations_and_cannot_cover_a_window() {
    let mut data = Data::default();
    for kind in [Kind::Server, Kind::Generator, Kind::Memory] {
        record(&mut data, kind, 0.0, "malformed", 100.0, 4096.0).unwrap();
        record(&mut data, kind, 3000.0, "malformed", 100.0, 4096.0).unwrap();
    }
    assert!(data.server.iter().all(|sample| sample.server.is_none()));
    assert!(
        data.generator
            .iter()
            .all(|sample| sample.generator.is_none())
    );
    assert!(data.memory.iter().all(|sample| sample["rssKiB"].is_null()));
    let (server, generator) = data.resource_samples();
    assert!(
        crate::bench::resources::summarize(
            &server,
            crate::bench::resources::Role::Server,
            0.0,
            3000.0
        )
        .is_err()
    );
    assert!(
        crate::bench::resources::summarize(
            &generator,
            crate::bench::resources::Role::Generator,
            0.0,
            3000.0
        )
        .is_err()
    );
}

#[test]
fn sampler_record_bound_retains_all_prior_failed_evidence() {
    let mut data = Data::default();
    for index in 0..MAX_SAMPLES {
        record(
            &mut data,
            Kind::Server,
            index as f64,
            "malformed",
            100.0,
            4096.0,
        )
        .unwrap();
    }
    assert!(record(&mut data, Kind::Server, 99999.0, "malformed", 100.0, 4096.0).is_err());
    assert_eq!(data.server.len(), MAX_SAMPLES);
}
