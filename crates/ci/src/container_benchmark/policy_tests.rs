use super::*;
use serde_json::json;

fn selection(clients: u32, rooms: u32) -> SummarySelection {
    SummarySelection {
        clients,
        rooms,
        duration: 60,
        ramp_up: 5,
        warmup: 10,
        run_id: "a".repeat(32),
        server_image: format!("sha256:{}", "b".repeat(64)),
        generator_image: format!("sha256:{}", "c".repeat(64)),
    }
}
fn summary(selected: &SummarySelection) -> Value {
    json!({"schemaVersion":2,"totalErrors":0,"failedConnections":0,"failedConsumers":0,
        "validatedConsumers":expected_consumers(selected).unwrap(),"skippedShortLivedConsumers":0,
        "run":{"completed":true,"passed":true,"configuration":{
            "numClients":selected.clients,"numRooms":selected.rooms,
            "durationSecs":selected.duration,
            "rampUpSecs":selected.ramp_up,"warmupSecs":selected.warmup,"runLabel":selected.run_id},
            "provenance":{"serverRevision":selected.server_image,
                "generatorRevision":selected.generator_image}},
        "attemptCoverage":{"version":1,"scope":"stable-publishers","available":true,
            "attempts":selected.clients,"passedAttempts":selected.clients,"failedAttempts":0,
            "missingCoverageAttempts":0,"skippedShortTailAttempts":0,
            "requestedChurners":0,"validatedChurners":0}})
}
fn admit(value: &Value, selected: &SummarySelection) -> bool {
    admit_summary(&serde_json::to_vec(value).unwrap(), selected).is_ok()
}
fn metrics(snapshot: &str) -> Vec<u8> {
    format!(
        "simplestchat_rooms_active 0\nsimplestchat_participants_active 0\n\
        simplestchat_connections_active 0\nsimplestchat_participants_snapshot_complete {snapshot}\n"
    )
    .into_bytes()
}

#[test]
fn minimal_selected_summary_has_no_dependency_on_unselected_media_or_latency_fields() {
    for (clients, rooms, consumers) in [(4, 1, 24), (10, 1, 80), (10, 4, 32), (30, 4, 240)] {
        let selected = selection(clients, rooms);
        let value =
            admit_summary(&serde_json::to_vec(&summary(&selected)).unwrap(), &selected).unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            json!({"schemaVersion":2,
            "completed":true,"passed":true,"totalErrors":0,"failedConnections":0,
            "failedConsumers":0,"validatedConsumers":consumers,"skippedShortLivedConsumers":0,
            "attempts":clients,"passedAttempts":clients})
        );
    }
    let isolated = selection(2, 2);
    assert!(!admit(&summary(&isolated), &isolated));
}

#[test]
fn every_selected_requirement_is_present_typed_and_agrees_with_the_requested_workload() {
    let selected = selection(10, 1);
    let original = summary(&selected);
    let paths = [
        "/schemaVersion",
        "/totalErrors",
        "/failedConnections",
        "/failedConsumers",
        "/validatedConsumers",
        "/skippedShortLivedConsumers",
        "/run/completed",
        "/run/passed",
        "/run/configuration/numClients",
        "/run/configuration/numRooms",
        "/run/configuration/durationSecs",
        "/run/configuration/rampUpSecs",
        "/run/configuration/warmupSecs",
        "/run/configuration/runLabel",
        "/run/provenance/serverRevision",
        "/run/provenance/generatorRevision",
        "/attemptCoverage/version",
        "/attemptCoverage/scope",
        "/attemptCoverage/available",
        "/attemptCoverage/attempts",
        "/attemptCoverage/passedAttempts",
        "/attemptCoverage/failedAttempts",
        "/attemptCoverage/missingCoverageAttempts",
        "/attemptCoverage/skippedShortTailAttempts",
        "/attemptCoverage/requestedChurners",
        "/attemptCoverage/validatedChurners",
    ];
    for pointer in paths {
        for replacement in [Value::Null, json!("PRIVATE"), json!([]), json!({})] {
            let mut value = original.clone();
            *value.pointer_mut(pointer).unwrap() = replacement;
            assert!(!admit(&value, &selected), "{pointer}");
        }
        let mut value = original.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        value
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(!admit(&value, &selected), "missing {pointer}");
    }
    for (pointer, value) in [
        ("/schemaVersion", json!(1)),
        ("/run/passed", json!(false)),
        ("/totalErrors", json!(1)),
        ("/failedConnections", json!(1)),
        ("/failedConsumers", json!(1)),
        ("/validatedConsumers", json!(79)),
        ("/validatedConsumers", json!(81)),
        ("/skippedShortLivedConsumers", json!(1)),
        ("/run/configuration/numClients", json!(9)),
        ("/run/configuration/numRooms", json!(2)),
        ("/run/configuration/durationSecs", json!(30)),
        ("/run/configuration/rampUpSecs", json!(10)),
        ("/run/configuration/warmupSecs", json!(11)),
        ("/attemptCoverage/attempts", json!(0)),
        ("/attemptCoverage/passedAttempts", json!(9)),
        ("/attemptCoverage/failedAttempts", json!(1)),
        ("/attemptCoverage/missingCoverageAttempts", json!(1)),
        ("/attemptCoverage/skippedShortTailAttempts", json!(1)),
        ("/attemptCoverage/requestedChurners", json!(1)),
        ("/attemptCoverage/validatedChurners", json!(1)),
    ] {
        let mut candidate = original.clone();
        *candidate.pointer_mut(pointer).unwrap() = value;
        assert!(!admit(&candidate, &selected), "{pointer}");
    }
}

#[test]
fn raw_decimal_identity_preserves_jq_precision_zero_and_numeric_boolean_distinctions() {
    let selected = selection(10, 1);
    let original = serde_json::to_string(&summary(&selected)).unwrap();
    for literal in ["2.0", "2e0", "0.2e1", "2000e-3"] {
        let candidate = original.replace(
            "\"schemaVersion\":2",
            &format!("\"schemaVersion\":{literal}"),
        );
        assert!(
            admit_summary(candidate.as_bytes(), &selected).is_ok(),
            "{literal}"
        );
    }
    for literal in [
        "2.00000000000000000001",
        "1.99999999999999999999",
        "2e999",
        "true",
    ] {
        let candidate = original.replace(
            "\"schemaVersion\":2",
            &format!("\"schemaVersion\":{literal}"),
        );
        assert!(
            admit_summary(candidate.as_bytes(), &selected).is_err(),
            "{literal}"
        );
    }
    for literal in ["-0", "0.0", "0e99999999999999999999999999999"] {
        let candidate =
            original.replace("\"totalErrors\":0", &format!("\"totalErrors\":{literal}"));
        assert!(
            admit_summary(candidate.as_bytes(), &selected).is_ok(),
            "{literal}"
        );
    }
    for literal in ["1e-999", "-1e-999", "false"] {
        let candidate =
            original.replace("\"totalErrors\":0", &format!("\"totalErrors\":{literal}"));
        assert!(
            admit_summary(candidate.as_bytes(), &selected).is_err(),
            "{literal}"
        );
    }
    for expected in 0..=300 {
        assert!(number(&expected.to_string(), expected));
        assert!(number(&format!("{expected}000e-3"), expected));
    }
}

#[test]
fn repeated_selected_keys_last_win_and_unknown_private_numbers_never_enter_public_output() {
    let selected = selection(10, 1);
    let original = serde_json::to_string(&summary(&selected)).unwrap();
    let good = original.replacen('{', "{\"schemaVersion\":0,\"PRIVATE\":1e999,", 1);
    let projection = admit_summary(good.as_bytes(), &selected).unwrap();
    assert!(!format!("{projection:?}").contains("PRIVATE"));
    assert!(
        !serde_json::to_string(&projection)
            .unwrap()
            .contains(&selected.run_id)
    );
    let bad = format!("{},\"schemaVersion\":0}}", original.trim_end_matches('}'));
    assert!(admit_summary(bad.as_bytes(), &selected).is_err());
    let nested = original.replace("\"numClients\":10", "\"numClients\":9,\"numClients\":10");
    assert!(admit_summary(nested.as_bytes(), &selected).is_ok());
    let unknown = original.replacen('{', "{\"PRIVATE\":{\"token\":\"secret\",\"n\":1e999},", 1);
    assert!(admit_summary(unknown.as_bytes(), &selected).is_ok());
}

#[test]
fn bounded_single_json_object_profile_and_selection_are_explicit() {
    let selected = selection(10, 1);
    for bytes in [
        b"".as_slice(),
        b"[]",
        b"null",
        b"{} {}",
        b"{\"private\":NaN}",
        &[0xff],
    ] {
        assert!(admit_summary(bytes, &selected).is_err());
    }
    let original = serde_json::to_vec(&summary(&selected)).unwrap();
    let mut padded = original.clone();
    padded.resize(SUMMARY_BYTES, b' ');
    assert!(admit_summary(&padded, &selected).is_ok());
    padded.push(b' ');
    assert!(admit_summary(&padded, &selected).is_err());
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend(original);
    assert!(admit_summary(&bom, &selected).is_ok());
    for (clients, rooms) in [(1, 1), (31, 4), (10, 0), (10, 5), (11, 1)] {
        assert!(expected_consumers(&selection(clients, rooms)).is_err());
    }
    let mut invalid = selection(10, 1);
    invalid.run_id = "PRIVATE".into();
    assert!(admit_summary(b"{}", &invalid).is_err());
}

#[test]
fn metrics_keep_three_digit_only_counters_but_snapshot_uses_observed_decimal_strnum() {
    for value in [
        "1",
        "01",
        "1.0",
        "1e0",
        "+1",
        ".1e1",
        "1.00000000000000000001",
        "1\r",
        "\r1",
        "\u{000b}1",
        "\u{000c}1",
    ] {
        assert!(zero_counts(&metrics(value)).is_ok(), "{value:?}");
    }
    for value in [
        "0",
        "true",
        "NaN",
        "Inf",
        "0x1",
        "1e999",
        "1\u{000b}",
        "1\u{000c}",
        "1\u{0085}",
        "\u{00a0}1",
        "1 extra",
        "",
    ] {
        assert!(zero_counts(&metrics(value)).is_err(), "{value:?}");
    }
    let original = String::from_utf8(metrics("1")).unwrap();
    for value in ["1", "-1", "0.0", "0e0", "NaN", "Inf", "0\r", "0 extra", ""] {
        let value = original.replace("rooms_active 0", &format!("rooms_active {value}"));
        assert!(zero_counts(value.as_bytes()).is_err(), "{value:?}");
    }
    assert!(
        zero_counts(
            original
                .replace("rooms_active 0", "rooms_active 000")
                .as_bytes()
        )
        .is_ok()
    );
    assert!(zero_counts(format!("# unrelated\n{original}private 1e999\n").as_bytes()).is_ok());
}

#[test]
fn metrics_require_exact_four_singletons_and_nf_two_with_literal_space_prefix() {
    let original = String::from_utf8(metrics("1")).unwrap();
    for line in original.lines() {
        assert!(zero_counts(original.replace(&format!("{line}\n"), "").as_bytes()).is_err());
        assert!(zero_counts(format!("{original}{line}\n").as_bytes()).is_err());
    }
    assert!(
        zero_counts(
            original
                .replace("rooms_active ", "rooms_active\t")
                .as_bytes()
        )
        .is_err()
    );
    assert!(
        zero_counts(
            original
                .replace("rooms_active 0", "rooms_active \t0\t")
                .as_bytes()
        )
        .is_ok()
    );
    let mut padded = original.into_bytes();
    padded.resize(METRIC_BYTES, b'#');
    assert!(zero_counts(&padded).is_ok());
    padded.push(b'#');
    assert!(zero_counts(&padded).is_err());
}

#[test]
fn ownership_is_only_the_original_four_selected_string_equalities() {
    let original = json!({"id":"ID","image":"IMAGE","run":"RUN","role":"server",
        "PRIVATE":{"secret":"not emitted"}});
    assert!(ownership(&original, "ID", "IMAGE", "RUN", "server"));
    for field in ["id", "image", "run", "role"] {
        for value in [Value::Null, json!("foreign"), json!(false), json!(1)] {
            let mut value = value;
            let mut candidate = original.clone();
            std::mem::swap(&mut candidate[field], &mut value);
            assert!(!ownership(&candidate, "ID", "IMAGE", "RUN", "server"));
        }
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!ownership(&missing, "ID", "IMAGE", "RUN", "server"));
    }
}

#[test]
fn terminal_guest_exit_needs_all_five_observations_and_numeric_zero_not_false() {
    let original =
        json!({"Running":false,"ExitCode":0,"OOMKilled":false,"Error":"","Restarting":false});
    assert!(exit_passed(&original));
    for field in ["Running", "ExitCode", "OOMKilled", "Error", "Restarting"] {
        let mut candidate = original.clone();
        candidate.as_object_mut().unwrap().remove(field);
        assert!(!exit_passed(&candidate));
    }
    for (field, value) in [
        ("Running", json!(true)),
        ("ExitCode", json!(1)),
        ("ExitCode", json!(137)),
        ("ExitCode", json!(143)),
        ("ExitCode", json!(false)),
        ("OOMKilled", json!(true)),
        ("Error", json!("PRIVATE")),
        ("Restarting", json!(true)),
    ] {
        let mut candidate = original.clone();
        candidate[field] = value;
        assert!(!exit_passed(&candidate));
    }
    let mut float = original;
    float["ExitCode"] = json!(0.0);
    assert!(exit_passed(&float));
}
