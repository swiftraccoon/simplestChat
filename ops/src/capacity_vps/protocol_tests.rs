//! Wire tests exercise refusal before a decoded result can become policy input.

use super::*;
use serde_json::json;

fn header() -> Header {
    let pin = || Pin {
        sha256: "c".repeat(64),
        bytes: 1,
    };
    Header {
        schema_version: 1,
        scope: "OpsCapacityBootstrap".into(),
        mode: Mode::Stage,
        run: "b".repeat(32),
        selection: Source {
            repository: "owner/repo".into(),
            revision: "a".repeat(40),
            package_version: "0.1.0".into(),
            protocol_version: 1,
            target: "x86_64-unknown-linux-musl".into(),
            build_run_id: 17,
            run_attempt: 2,
        },
        artifact_id: 29,
        transfer: pin(),
        members: Members {
            manifest: pin(),
            selection: pin(),
            binary: pin(),
        },
        request: pin(),
    }
}

fn request() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "revision":"a".repeat(40), "run":"b".repeat(32), "workload":"meetings",
        "label":"fixture", "firstSize":2, "steps":1, "meetingSize":2, "speakers":1,
        "runtimeSeconds":120, "quick":true, "audioOnly":false, "chatIntervalMs":0,
        "serverCpus":null,"generatorCpus":null,"appCpus":null,"monthlyPrice":null
    }))
    .unwrap()
}

fn state() -> UnitState {
    UnitState {
        load_state: "loaded".into(),
        active_state: "inactive".into(),
        main_pid: "0".into(),
        sub_state: Some("dead".into()),
        result: Some("success".into()),
        exec_main_code: Some("exited".into()),
        exec_main_status: Some("0".into()),
        unit: Some(header().capacity_unit()),
        retained: None,
    }
}

fn clean() -> Residuals {
    Residuals {
        containers_before: vec![],
        containers_after: vec![],
        residuals: vec![],
        clean: true,
    }
}

fn missing_collection() -> Collect {
    Collect {
        passed: false,
        unit: state(),
        outcome_json:
            "{\"passed\":false,\"finalized\":false,\"failureClass\":\"worker_outcome_missing\"}"
                .into(),
        cleanup: clean(),
        evidence: header().evidence_path(),
        revision: header().selection.revision,
        request_json: String::from_utf8(request()).unwrap(),
    }
}

fn wire(action: Action, result: Value) -> Vec<u8> {
    let h = header();
    let mut bytes = serde_json::to_vec(&json!({
        "schemaVersion":1,"scope":"OpsCapacityResponse","action":action,"run":h.run,
        "revision":h.selection.revision,"artifactId":h.artifact_id,
        "binarySha256":h.members.binary.sha256,"result":result
    }))
    .unwrap();
    bytes.push(b'\n');
    bytes
}

fn check(action: Action, result: Value) -> bool {
    Response::decode(&wire(action, result), &header(), action).is_ok()
}

#[test]
fn header_and_envelope_keep_distinct_integer_and_presence_domains() {
    let h = header();
    assert_eq!(Header::decode(&h.encode().unwrap()).unwrap(), h);
    for name in ["artifactId", "schemaVersion"] {
        for bad in [json!(true), json!(1.0), json!(null), json!(0)] {
            let mut value = serde_json::to_value(&h).unwrap();
            value[name] = bad;
            assert!(Header::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    let original = request();
    let e = Envelope::request(Action::Status, &original, Some(Unit::Capacity)).unwrap();
    assert_eq!(
        Envelope::decode(&e.encode().unwrap())
            .unwrap()
            .original_request(),
        Some(original.as_slice())
    );
    for value in [
        json!({
            "schemaVersion":1,"scope":"OpsCapacityOperation","action":"lookup","requestJson":null
        }),
        json!({"schemaVersion":1,"scope":"OpsCapacityOperation","action":"lookup","unit":null}),
    ] {
        assert!(Envelope::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    assert!(Envelope::request(Action::Collect, &original, Some(Unit::Capacity)).is_err());
    assert!(Envelope::request(Action::Status, &original, None).is_err());
}

#[test]
fn private_workload_spelling_and_extended_numbers_stay_inside_strings() {
    let mut original = request();
    original.pop();
    original.extend_from_slice(
        b",\"unknown\":[NaN,Infinity,-Infinity,9007199254740993,-0,1.00,1e2] }\n",
    );
    let envelope = Envelope::request(Action::Collect, &original, None).unwrap();
    let encoded = envelope.encode().unwrap();
    let decoded = Envelope::decode(&encoded).unwrap();
    assert_eq!(decoded.original_request(), Some(original.as_slice()));
    decoded.bind(&header()).unwrap();
    assert_ne!(sha(&encoded), sha(&original));
    let lookup = ResultData::Lookup(Lookup {
        request_json: String::from_utf8(original.clone()).unwrap(),
        request_sha256: sha(&original),
        unit: state(),
    });
    let response = Response::decode(
        &Response::encode(&header(), &lookup).unwrap(),
        &header(),
        Action::Lookup,
    )
    .unwrap();
    response.bind_workload(&header(), &original).unwrap();
    assert!(response.bind_workload(&header(), &request()).is_err());
}

#[test]
fn normal_response_framing_owns_exact_one_line_but_not_key_spacing() {
    let bytes = wire(Action::Reserve, json!({"privateHost":true,"ready":true}));
    assert!(Response::decode(&bytes, &header(), Action::Reserve).is_ok());
    let body = std::str::from_utf8(&bytes).unwrap().trim_end_matches('\n');
    let spaced = format!("{}\n", body.replace(':', ": \t").replace(',', ", "));
    assert!(Response::decode(spaced.as_bytes(), &header(), Action::Reserve).is_ok());
    let invalid = [
        body.as_bytes().to_vec(),
        format!(" {body}\n").into_bytes(),
        format!("{body} \n").into_bytes(),
        format!("{body}\r\n").into_bytes(),
        format!("{body}\n\n").into_bytes(),
        format!("{body}{{}}\n").into_bytes(),
        format!("{}\n{}\n", &body[..1], &body[1..]).into_bytes(),
        format!("{}\r{}\n", &body[..1], &body[1..]).into_bytes(),
        vec![],
    ];
    for value in invalid {
        assert!(Response::decode(&value, &header(), Action::Reserve).is_err());
    }
}

#[test]
fn every_wrapper_identity_is_bound_and_unknown_duplicate_controls_fail() {
    let bytes = wire(Action::Reserve, json!({"privateHost":true,"ready":true}));
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    for (field, value) in [
        ("schemaVersion", json!(2)),
        ("scope", json!("other")),
        ("action", json!("preflight")),
        ("run", json!("d".repeat(32))),
        ("revision", json!("d".repeat(40))),
        ("artifactId", json!(30)),
        ("binarySha256", json!("d".repeat(64))),
        ("authenticated", json!(true)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        let mut changed = serde_json::to_vec(&changed).unwrap();
        changed.push(b'\n');
        assert!(Response::decode(&changed, &header(), Action::Reserve).is_err());
    }
    let duplicate = std::str::from_utf8(&bytes)
        .unwrap()
        .replace("\"ready\":true", "\"ready\":false,\"ready\":true");
    assert!(Response::decode(duplicate.as_bytes(), &header(), Action::Reserve).is_err());
}

#[test]
fn result_semantics_refuse_false_ready_foreign_units_paths_and_container_ids() {
    for result in [
        json!({"privateHost":false,"ready":true}),
        json!({"privateHost":true,"ready":false}),
    ] {
        assert!(!check(Action::Reserve, result));
    }
    assert!(!check(
        Action::Preflight,
        json!({"privateHost":true,"containersBefore":["not-an-id"]})
    ));
    assert!(check(
        Action::Preflight,
        json!({"privateHost":true,"containersBefore":["d".repeat(64)]})
    ));
    assert!(!check(Action::Build, json!({"unit":"arbitrary.service"})));
    assert!(check(Action::Build, json!({"unit":BUILD_UNIT})));
    let h = header();
    let start = json!({"unit":h.capacity_unit(),"evidence":h.evidence_path(),"run":h.run});
    assert!(check(Action::Start, start.clone()));
    for field in ["unit", "evidence", "run"] {
        let mut bad = start.clone();
        bad[field] = json!("other");
        assert!(!check(Action::Start, bad));
    }
}

#[test]
fn unit_empty_observations_remain_negative_and_null_is_not_omitted() {
    let mut value = serde_json::to_value(state()).unwrap();
    for name in ["Result", "ExecMainCode", "ExecMainStatus", "SubState"] {
        value[name] = json!("");
    }
    let unit: UnitState = serde_json::from_value(value.clone()).unwrap();
    assert!(unit.terminal());
    assert!(!unit.successful());
    assert!(check(Action::Status, value.clone()));
    for field in [
        "SubState",
        "Result",
        "ExecMainCode",
        "ExecMainStatus",
        "Unit",
        "Retained",
    ] {
        let mut bad = value.clone();
        bad[field] = Value::Null;
        assert!(!check(Action::Status, bad));
    }
    let mut absent = value.clone();
    absent.as_object_mut().unwrap().remove("Result");
    assert!(check(Action::Status, absent));
    for (name, bad) in [
        ("LoadState", json!("loading")),
        ("Retained", json!(false)),
        ("Unit", json!("foreign.service")),
        ("MainPID", json!("1\n2")),
    ] {
        let mut changed = value.clone();
        changed[name] = bad;
        assert!(!check(Action::Status, changed));
    }
    value["Retained"] = json!(true);
    assert!(check(Action::Status, value.clone()));
    value["Unit"] = json!(BUILD_UNIT);
    assert!(!check(Action::Status, value));
}

#[test]
fn lookup_and_retirement_bind_workload_bytes_rather_than_envelope_pin() {
    let original = request();
    let lookup = json!({
        "requestJson":String::from_utf8(original.clone()).unwrap(),
        "requestSha256":sha(&original),"unit":state()
    });
    assert!(check(Action::Lookup, lookup.clone()));
    for field in ["requestSha256", "requestJson"] {
        let mut bad = lookup.clone();
        bad[field] = json!("d".repeat(64));
        assert!(!check(Action::Lookup, bad));
    }
    let retirement = ResultData::Retire(Retirement {
        retired: true,
        lifecycle: RetirementPhase::Terminal,
        request_sha256: sha(&original),
    });
    let response = Response::decode(
        &Response::encode(&header(), &retirement).unwrap(),
        &header(),
        Action::Retire,
    )
    .unwrap();
    response.bind_workload(&header(), &original).unwrap();
    let mut same_semantics = original.clone();
    same_semantics.push(b' ');
    assert!(response.bind_workload(&header(), &same_semantics).is_err());
    assert!(!check(
        Action::Retire,
        json!({"retired":false,"lifecycle":"terminal","requestSha256":sha(&original)})
    ));
}

#[test]
fn missing_outcome_stays_collectable_but_cannot_be_promoted_by_clean_residuals() {
    let collection = missing_collection();
    let response = Response::decode(
        &Response::encode(&header(), &ResultData::Collect(collection.clone())).unwrap(),
        &header(),
        Action::Collect,
    )
    .unwrap();
    response.bind_workload(&header(), &request()).unwrap();
    let mut promoted = collection.clone();
    promoted.passed = true;
    assert!(Response::encode(&header(), &ResultData::Collect(promoted)).is_err());
    let mut foreign = collection;
    foreign.evidence = "/tmp/arbitrary".into();
    assert!(Response::encode(&header(), &ResultData::Collect(foreign)).is_err());
}

#[test]
fn collected_positive_requires_all_verdict_peers_and_saved_outcome_identity() {
    let mut collection = missing_collection();
    collection.passed = true;
    let outcome = json!({
        "schemaVersion":1,"revision":header().selection.revision,"passed":true,
        "finalized":true,"exitStatus":0,"cleanup":clean(),
        "passingSteps":[{"valid":true,"passed":true,"workload":"meetings"}],"failedSteps":[]
    });
    collection.outcome_json = serde_json::to_string(&outcome).unwrap();
    assert!(Response::encode(&header(), &ResultData::Collect(collection.clone())).is_ok());
    for (field, bad) in [
        ("schemaVersion", json!(2)),
        ("revision", json!("d".repeat(40))),
        ("finalized", json!(false)),
        ("exitStatus", json!(1)),
        ("failedSteps", json!([{}])),
        ("passingSteps", json!([])),
        ("passingSteps", json!([{"valid":true,"passed":true}])),
        (
            "passingSteps",
            json!([{"valid":true,"passed":true,"workload":"webinar"}]),
        ),
    ] {
        let mut changed = outcome.clone();
        changed[field] = bad;
        let mut bad = collection.clone();
        bad.outcome_json = serde_json::to_string(&changed).unwrap();
        assert!(Response::encode(&header(), &ResultData::Collect(bad)).is_err());
    }
    let mut malformed_cleanup = outcome.clone();
    malformed_cleanup["cleanup"] = json!({"clean":true});
    let mut bad = collection.clone();
    bad.outcome_json = serde_json::to_string(&malformed_cleanup).unwrap();
    assert!(Response::encode(&header(), &ResultData::Collect(bad)).is_err());
    let mut failed_unit = collection.clone();
    failed_unit.unit.result = Some("exit-code".into());
    assert!(Response::encode(&header(), &ResultData::Collect(failed_unit.clone())).is_err());
    // Failure remains collectable evidence after a successful worker verdict.
    failed_unit.passed = false;
    assert!(Response::encode(&header(), &ResultData::Collect(failed_unit)).is_ok());
    let mut not_found = state();
    not_found.load_state = "not-found".into();
    assert!(not_found.terminal());
    assert!(!not_found.successful());
}

#[test]
fn private_outcome_extended_values_survive_but_control_nonfinite_is_refused() {
    let mut collection = missing_collection();
    collection.outcome_json = format!(
        "{{\"schemaVersion\":1,\"revision\":\"{}\",\"passed\":false,\"finalized\":false,\
         \"unknown\":[NaN,Infinity,-Infinity,9007199254740993,-0,1.00,1e2]}}",
        header().selection.revision
    );
    let original = collection.outcome_json.clone();
    let bytes = Response::encode(&header(), &ResultData::Collect(collection)).unwrap();
    let response = Response::decode(&bytes, &header(), Action::Collect).unwrap();
    let ResultData::Collect(decoded) = response.result else {
        panic!("wrong result")
    };
    assert_eq!(decoded.outcome_json, original);
    let mut bad = missing_collection();
    // A missing saved-outcome identity cannot be disguised as a generic negative.
    bad.outcome_json = "{\"passed\":false,\"finalized\":false}".into();
    assert!(Response::encode(&header(), &ResultData::Collect(bad)).is_err());
    let invalid = String::from_utf8(wire(
        Action::Reserve,
        json!({"privateHost":true,"ready":true}),
    ))
    .unwrap()
    .replace("\"ready\":true", "\"ready\":NaN");
    assert!(Response::decode(invalid.as_bytes(), &header(), Action::Reserve).is_err());
}

#[test]
fn collection_baseline_disagreement_stays_negative_evidence() {
    let mut collection = missing_collection();
    let saved_cleanup = Residuals {
        containers_before: vec!["d".repeat(64)],
        ..clean()
    };
    let mut outcome = json!({
        "schemaVersion":1,"revision":header().selection.revision,"passed":false,
        "finalized":false,"cleanup":saved_cleanup
    });
    collection.outcome_json = serde_json::to_string(&outcome).unwrap();
    assert!(Response::encode(&header(), &ResultData::Collect(collection.clone())).is_ok());
    outcome["passed"] = json!(true);
    outcome["finalized"] = json!(true);
    outcome["exitStatus"] = json!(0);
    outcome["passingSteps"] = json!([{"valid":true,"passed":true,"workload":"meetings"}]);
    outcome["failedSteps"] = json!([]);
    collection.outcome_json = serde_json::to_string(&outcome).unwrap();
    assert!(Response::encode(&header(), &ResultData::Collect(collection.clone())).is_ok());
    collection.passed = true;
    assert!(Response::encode(&header(), &ResultData::Collect(collection.clone())).is_err());
    collection.cleanup = saved_cleanup;
    assert!(Response::encode(&header(), &ResultData::Collect(collection)).is_ok());
}

fn residual() -> Value {
    json!({"id":"d".repeat(64),"confirmedOwned":false,"inspection":{
        "Id":"d".repeat(64),"Name":"unknown","Image":null,"RequestedImage":null,
        "State":{
            "Status":null,"Running":null,"ExitCode":null,"OOMKilled":null,
            "StartedAt":null,"FinishedAt":null
        }
    }})
}

#[test]
fn residual_nullable_fields_are_required_and_private_docker_extras_refused() {
    let value = residual();
    assert!(serde_json::from_value::<Residual>(value.clone()).is_ok());
    for field in ["Image", "RequestedImage"] {
        let mut bad = value.clone();
        bad["inspection"].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Residual>(bad).is_err());
    }
    for field in [
        "Status",
        "Running",
        "ExitCode",
        "OOMKilled",
        "StartedAt",
        "FinishedAt",
    ] {
        let mut bad = value.clone();
        bad["inspection"]["State"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(serde_json::from_value::<Residual>(bad).is_err());
    }
    let mut bad = value.clone();
    bad["inspection"]["Env"] = json!(["private"]);
    assert!(serde_json::from_value::<Residual>(bad).is_err());
    let mut bad = value;
    bad["inspection"]["State"]["ExitCode"] = json!(2147483648_i64);
    assert!(serde_json::from_value::<Residual>(bad).is_err());
}

#[test]
fn residual_clean_and_same_id_claims_cannot_hide_discovered_container() {
    let mut collection = serde_json::to_value(missing_collection()).unwrap();
    collection["cleanup"] = json!({
        "containersBefore":[],"containersAfter":["d".repeat(64)],"residuals":[],"clean":true
    });
    assert!(!check(Action::Collect, collection.clone()));
    collection["cleanup"]["residuals"] = json!([residual()]);
    assert!(!check(Action::Collect, collection.clone()));
    collection["cleanup"]["clean"] = json!(false);
    assert!(check(Action::Collect, collection.clone()));
    collection["cleanup"]["residuals"][0]["inspection"]["Id"] = json!("e".repeat(64));
    assert!(!check(Action::Collect, collection));
}

#[test]
fn successful_recovery_requires_snapshots_without_inventing_cleanup_authority() {
    let value =
        json!({"run":header().run,"removed":[],"finalized":false,"before":clean(),"after":clean()});
    assert!(check(Action::Recover, value.clone()));
    for name in ["before", "after"] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(name);
        assert!(!check(Action::Recover, missing));
        let mut null = value.clone();
        null[name] = Value::Null;
        assert!(!check(Action::Recover, null));
    }
    let mut bad = value.clone();
    bad["removed"] = json!(["d".repeat(64)]);
    assert!(!check(Action::Recover, bad));
    let mut unknown = value;
    unknown["before"] = json!({
        "containersBefore":[],"containersAfter":["d".repeat(64)],
        "residuals":[residual()],"clean":false
    });
    unknown["after"] = unknown["before"].clone();
    assert!(check(Action::Recover, unknown.clone()));
    unknown["finalized"] = json!(true);
    assert!(!check(Action::Recover, unknown.clone()));
    // Reclassifying an unknown residual as an old baseline cannot prove cleanup.
    unknown["after"] = json!({
        "containersBefore":["d".repeat(64)],"containersAfter":["d".repeat(64)],
        "residuals":[],"clean":true
    });
    assert!(!check(Action::Recover, unknown.clone()));
    unknown["finalized"] = json!(false);
    assert!(!check(Action::Recover, unknown));
}

#[test]
fn bounded_writer_refuses_growth_before_crossing_the_limit() {
    let mut output = BoundedWriter {
        bytes: Vec::new(),
        maximum: 4,
        exhausted: false,
    };
    output.write_all(b"1234").unwrap();
    let capacity = output.bytes.capacity();
    assert!(output.write_all(b"5").is_err());
    assert_eq!(output.bytes, b"1234");
    assert_eq!(output.bytes.capacity(), capacity);
    assert!(output.exhausted);
    assert_eq!(bounded(&"a", 3).unwrap(), b"\"a\"");
    assert!(matches!(bounded(&"a", 2), Err(Failure::Bounds)));
}

#[test]
fn escaped_private_strings_count_against_whole_response_before_output() {
    let mut collection = missing_collection();
    // Valid extended private evidence below its own 8MiB cap, but escaping its
    // quoted unknown value exceeds the separate outer wire cap.
    collection.outcome_json = format!(
        "{{\"schemaVersion\":1,\"revision\":\"{}\",\"passed\":false,\"finalized\":false,\
         \"unknown\":\"{}\"}}",
        header().selection.revision,
        "\\\"".repeat(MAX_RESPONSE / 4)
    );
    assert!(collection.outcome_json.len() < MAX_RESPONSE);
    assert!(matches!(
        Response::encode(&header(), &ResultData::Collect(collection)),
        Err(Failure::Bounds)
    ));
    let oversized = vec![b' '; MAX_STATUS + 1];
    assert!(Response::decode(&oversized, &header(), Action::Status).is_err());
}
