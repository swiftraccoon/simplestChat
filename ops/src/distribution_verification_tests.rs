//! New conditional policy assertions. No Ops signer exists and these synthetic
//! graphs/certificates are not cryptographic fixtures or server Python parity.
use super::*;
use serde_json::{Value, json};

#[path = "distribution_verification_fixture.rs"]
mod fixture;
use fixture::{Graph, bytes};

#[test]
fn actual_unsigned_assembly_graph_retains_originals_and_fixed_payload_requirements() {
    for count in [0, 2, 16] {
        let graph = Graph::new(count);
        let metadata = graph.metadata().unwrap();
        assert_eq!(
            metadata.manifest().as_json(),
            graph.records["ops-release.json"]
        );
        assert_eq!(metadata.payloads().len(), count + 5);
        for payload in metadata.payloads() {
            let actual = &graph.records[payload.name()];
            assert_eq!(payload.bytes(), actual.len() as u64);
            assert_eq!(payload.sha256(), super::super::digest(actual));
            assert!(!format!("{payload:?}").contains("PRIVATE"));
        }
        assert_eq!(
            metadata.preparation_bytes(),
            graph.records["ops-preparation.json"].len() as u64
        );
        assert_eq!(
            metadata.preparation_sha256(),
            super::super::digest(&graph.records["ops-preparation.json"])
        );
        let pair = super::super::verify_pair(
            metadata.manifest(),
            &graph.root.0.join("binary"),
            &graph.selection,
        )
        .unwrap();
        metadata.check_pair(&pair).unwrap();
        let matched = graph
            .matched(&Graph::repository(), &graph.attempt(), &graph.verifier())
            .unwrap();
        assert_eq!(matched.evidence_count, count);
        assert_eq!(matched.subject_count, 3);
        assert_eq!(matched.observer_timestamp_count, 1);
    }
}

#[test]
fn all_selected_identities_and_safe_integer_bounds_precede_binary_access() {
    let mut graph = Graph::new(0);
    let mut value = serde_json::to_value(&graph.selection.0).unwrap();
    for (name, replacement) in [
        ("repository", json!("PRIVATE/other")),
        ("revision", json!("b".repeat(40))),
        ("packageVersion", json!("0.1.1")),
        ("buildRunId", json!(124)),
        ("runAttempt", json!(3)),
    ] {
        let mut value = value.clone();
        value[name] = replacement;
        let selected = Selection::decode(&bytes(&value)).unwrap();
        assert!(
            RetainedMetadata::decode(
                &selected,
                &graph.records["ops-release.json"],
                &graph.records["ops-evidence.json"],
                &graph.records["ops-predicate.json"],
                &graph.records["ops-preparation.json"]
            )
            .is_err(),
            "{name}"
        );
    }
    for name in ["buildRunId", "runAttempt"] {
        value[name] = json!(MAX_SAFE_INTEGER + 1);
        graph.selection = Selection::decode(&bytes(&value)).unwrap();
        assert_eq!(graph.metadata().unwrap_err(), Failure::Selection);
        value[name] = json!(1);
    }
    // The mature general Selection domain is deliberately unchanged.
    assert!(
        Selection::new(
            REPOSITORY,
            &"a".repeat(40),
            "0.1.0",
            1,
            super::super::TARGET,
            i64::MAX,
            1
        )
        .is_ok()
    );
}

#[test]
fn maximum_safe_signed_run_numbers_survive_protojson_without_rounding_authority() {
    let graph = Graph::with_run(0, MAX_SAFE_INTEGER as i64, MAX_SAFE_INTEGER as i64);
    let mut output = graph.verifier();
    for key in ["buildRunId", "runAttempt"] {
        output[0]["verificationResult"]["statement"]["predicate"]["identity"]["claims"][key] =
            json!(MAX_SAFE_INTEGER as f64);
    }
    graph
        .matched(&Graph::repository(), &graph.attempt(), &output)
        .unwrap();
    output[0]["verificationResult"]["statement"]["predicate"]["identity"]["claims"]["buildRunId"] =
        json!((MAX_SAFE_INTEGER + 1) as f64);
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
}

#[test]
fn every_small_record_and_digest_edge_is_required_before_payload_admission() {
    for name in [
        "ops-release.json",
        "ops-evidence.json",
        "ops-predicate.json",
        "ops-preparation.json",
    ] {
        for replacement in [
            b"null".as_slice(),
            b"{\"schemaVersion\":1,\"schemaVersion\":1}",
        ] {
            let mut graph = Graph::new(1);
            graph.records.insert(name.into(), replacement.to_vec());
            assert!(graph.metadata().is_err(), "{name}");
        }
        let mut graph = Graph::new(1);
        graph
            .records
            .insert(name.into(), vec![b' '; MAX_METADATA_BYTES + 1]);
        assert!(graph.metadata().is_err());
    }
    for name in [
        "ops-evidence.json",
        "ops-predicate.json",
        "ops-preparation.json",
    ] {
        let graph = Graph::new(1);
        let value = graph.value(name);
        for field in value.as_object().unwrap().keys() {
            let mut graph = Graph::new(1);
            let mut changed = graph.value(name);
            changed.as_object_mut().unwrap().remove(field);
            graph.replace(name, &changed);
            assert!(graph.metadata().is_err(), "{name}:{field}");
        }
    }
    let graph = Graph::new(1);
    for name in ["ops-predicate.json", "ops-preparation.json"] {
        let values = graph.value(name);
        for field in values["fileDigests"].as_object().unwrap().keys() {
            for key in ["sha256", "bytes"] {
                let mut graph = Graph::new(1);
                let mut changed = graph.value(name);
                changed["fileDigests"][field][key] = if key == "sha256" {
                    json!("c".repeat(64))
                } else {
                    json!(1)
                };
                graph.replace(name, &changed);
                assert!(graph.metadata().is_err(), "{name}:{field}:{key}");
            }
        }
    }
}

#[test]
fn evidence_index_is_sorted_exact_contiguous_and_bound_to_manifest_claims() {
    let graph = Graph::new(2);
    let baseline = graph.value("ops-evidence.json");
    let mut cases = Vec::new();
    let mut value = baseline.clone();
    value["entries"].as_array_mut().unwrap().reverse();
    cases.push(value);
    for field in ["logicalName", "payload", "sha256", "bytes"] {
        let mut value = baseline.clone();
        value["entries"][0].as_object_mut().unwrap().remove(field);
        cases.push(value);
    }
    for (field, replacement) in [
        ("logicalName", json!("PRIVATE/foreign")),
        ("payload", json!("../escape")),
        ("payload", json!("ops-evidence-99.bin")),
        ("sha256", json!("C".repeat(64))),
        ("bytes", json!(0)),
        ("bytes", json!(assembly::MAX_EVIDENCE_BYTES + 1)),
        ("bytes", json!(true)),
        ("bytes", json!(1.0)),
    ] {
        let mut value = baseline.clone();
        value["entries"][0][field] = replacement;
        cases.push(value);
    }
    let mut value = baseline;
    let duplicate = value["entries"][0].clone();
    value["entries"][1] = duplicate;
    cases.push(value);
    for value in cases {
        let mut graph = Graph::new(2);
        graph.replace("ops-evidence.json", &value);
        assert!(graph.metadata().is_err());
    }
}

#[test]
fn declared_identity_is_not_allowed_to_replace_actual_pair_inspection() {
    let mut graph = Graph::new(0);
    // Keep the small-record graph internally consistent while lying about one
    // measured fact. Pure decoding succeeds; the real unchanged inspector wins.
    let mut predicate = graph.value("ops-predicate.json");
    let before = predicate["identity"]["executable"]["bindNow"]
        .as_bool()
        .unwrap();
    predicate["identity"]["executable"]["bindNow"] = json!(!before);
    graph.replace("ops-predicate.json", &predicate);
    let mut preparation = graph.value("ops-preparation.json");
    preparation["fileDigests"]["ops-predicate.json"] = json!({
        "sha256":super::super::digest(&graph.records["ops-predicate.json"]),
        "bytes":graph.records["ops-predicate.json"].len()});
    graph.replace("ops-preparation.json", &preparation);
    let metadata = graph.metadata().unwrap();
    let pair = super::super::verify_pair(
        metadata.manifest(),
        &graph.root.0.join("binary"),
        &graph.selection,
    )
    .unwrap();
    assert_eq!(metadata.check_pair(&pair), Err(Failure::Pair));
}

#[test]
fn fixed_repository_and_exact_attempt_api_authority_is_required_on_both_repositories() {
    let graph = Graph::new(0);
    let repository = Graph::repository();
    let attempt = graph.attempt();
    let output = graph.verifier();
    for pointer in ["/id", "/full_name", "/fork", "/owner/id", "/owner/login"] {
        let mut bad = repository.clone();
        *bad.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(graph.matched(&bad, &attempt, &output).is_err(), "{pointer}");
        for prefix in ["repository", "head_repository"] {
            let mut bad_attempt = attempt.clone();
            bad_attempt[prefix] = bad.clone();
            assert!(
                graph.matched(&repository, &bad_attempt, &output).is_err(),
                "{prefix}:{pointer}"
            );
        }
    }
    for field in [
        "id",
        "run_attempt",
        "head_sha",
        "head_branch",
        "name",
        "path",
        "event",
        "status",
        "conclusion",
        "repository",
        "head_repository",
    ] {
        let mut bad = attempt.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            graph.matched(&repository, &bad, &output).is_err(),
            "{field}"
        );
    }
    for (field, replacement) in [
        ("id", json!(124)),
        ("run_attempt", json!(1)),
        ("id", json!(123.0)),
        ("run_attempt", json!(true)),
        ("event", json!("pull_request")),
        ("head_branch", json!("PRIVATE-branch")),
        ("conclusion", json!("failure")),
    ] {
        let mut bad = attempt.clone();
        bad[field] = replacement;
        assert!(
            graph.matched(&repository, &bad, &output).is_err(),
            "{field}"
        );
    }
}

#[test]
fn api_workflow_paths_have_only_three_closed_aliases_without_widening_signed_identity() {
    let graph = Graph::new(0);
    for path in WORKFLOW_PATHS {
        let mut attempt = graph.attempt();
        attempt["path"] = json!(path);
        graph
            .matched(&Graph::repository(), &attempt, &graph.verifier())
            .unwrap();
    }
    for path in [
        ".github/workflows/ci.yml@other",
        ".github/workflows/ci.yml@refs/heads/other",
        ".github/workflows/ci.yml@main/extra",
        ".github/workflows/CI.yml",
        ".github/workflows/ci.yml@refs/heads/main@refs/heads/other",
    ] {
        let mut attempt = graph.attempt();
        attempt["path"] = json!(path);
        assert!(
            graph
                .matched(&Graph::repository(), &attempt, &graph.verifier())
                .is_err()
        );
    }
    let mut output = graph.verifier();
    output[0]["verificationResult"]["signature"]["certificate"]["subjectAlternativeName"] =
        json!(format!("{REPOSITORY_URI}/{WORKFLOW}@main"));
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
}

#[test]
fn every_signed_certificate_identity_field_is_required_and_has_no_deprecated_fallback() {
    let graph = Graph::new(0);
    let baseline = graph.verifier();
    let certificate = &baseline[0]["verificationResult"]["signature"]["certificate"];
    for field in certificate
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| *k != "certificateIssuer")
    {
        for replacement in [Value::Null, json!("PRIVATE-foreign"), json!(123)] {
            let mut output = baseline.clone();
            output[0]["verificationResult"]["signature"]["certificate"][field] = replacement;
            assert!(
                graph
                    .matched(&Graph::repository(), &graph.attempt(), &output)
                    .is_err(),
                "{field}"
            );
        }
    }
    let mut output = baseline.clone();
    let certificate = output[0]["verificationResult"]["signature"]["certificate"]
        .as_object_mut()
        .unwrap();
    certificate.remove("sourceRepositoryDigest");
    certificate.insert("githubWorkflowSHA".into(), json!("a".repeat(40)));
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
    for signature in [
        json!({"certificate":null}),
        json!({"publicKeyId":"PRIVATE-key-only-message-signature"}),
        Value::Null,
    ] {
        let mut output = graph.verifier();
        output[0]["verificationResult"]["signature"] = signature;
        assert_eq!(
            graph
                .matched(&Graph::repository(), &graph.attempt(), &output)
                .unwrap_err(),
            Failure::Certificate
        );
    }
    let mut output = graph.verifier();
    output[0]["verificationResult"]["statement"] = Value::Null;
    assert_eq!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .unwrap_err(),
        Failure::Verifier
    );
    // OIDC extension must be checked independently of the certificate DN.
    let mut output = baseline;
    output[0]["verificationResult"]["signature"]["certificate"]["certificateIssuer"] =
        json!(OIDC_ISSUER);
    output[0]["verificationResult"]["signature"]["certificate"]["issuer"] = json!("PRIVATE-wrong");
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
}

#[test]
fn one_successful_output_must_carry_all_three_unique_exact_sha256_subjects() {
    let graph = Graph::new(0);
    let baseline = graph.verifier();
    let subject = "/0/verificationResult/statement/subject";
    for n in 0..3 {
        for field in ["name", "digest"] {
            let mut output = baseline.clone();
            output.pointer_mut(subject).unwrap()[n]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                graph
                    .matched(&Graph::repository(), &graph.attempt(), &output)
                    .is_err()
            );
        }
        for replacement in [json!("PRIVATE-other"), json!("../ops-preparation.json")] {
            let mut output = baseline.clone();
            output.pointer_mut(subject).unwrap()[n]["name"] = replacement;
            assert!(
                graph
                    .matched(&Graph::repository(), &graph.attempt(), &output)
                    .is_err()
            );
        }
        for digest in [
            json!({"sha256":"c".repeat(64)}),
            json!({"sha512":"a".repeat(128)}),
            json!({"sha256":"A".repeat(64)}),
            json!({"sha256":"a".repeat(64),"sha512":"b"}),
        ] {
            let mut output = baseline.clone();
            output.pointer_mut(subject).unwrap()[n]["digest"] = digest;
            assert!(
                graph
                    .matched(&Graph::repository(), &graph.attempt(), &output)
                    .is_err()
            );
        }
    }
    let mut output = baseline.clone();
    output
        .pointer_mut(subject)
        .unwrap()
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
    let mut output = baseline.clone();
    output
        .pointer_mut(subject)
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":"PRIVATE-fourth-subject","digest":{"sha256":"d".repeat(64)}
        }));
    assert_eq!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .unwrap_err(),
        Failure::Subjects
    );
    let mut output = baseline.clone();
    output[0]["verificationResult"]["statement"]["subject"][2] =
        baseline[0]["verificationResult"]["statement"]["subject"][0].clone();
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
    let mut output = baseline.clone();
    output.as_array_mut().unwrap().push(baseline[0].clone());
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
}

#[test]
fn real_observer_timestamps_are_typed_bounded_and_current_time_is_not_evidence() {
    let graph = Graph::new(0);
    for timestamps in [
        json!([]),
        json!(null),
        json!([{"type":"CurrentTime","uri":"",
        "timestamp":"2026-10-22T00:00:00Z"}]),
        json!([{"type":"Tlog","uri":"https://log.invalid",
        "timestamp":"PRIVATE-not-time"}]),
        json!([{"type":"TimestampAuthority","uri":"PRIVATE-path",
        "timestamp":"2026-10-22T00:00:00Z"}]),
    ] {
        let mut output = graph.verifier();
        output[0]["verificationResult"]["verifiedTimestamps"] = timestamps;
        assert!(
            graph
                .matched(&Graph::repository(), &graph.attempt(), &output)
                .is_err()
        );
    }
    let mut output = graph.verifier();
    output[0]["verificationResult"]["verifiedTimestamps"][0]["type"] = json!("Tlog");
    graph
        .matched(&Graph::repository(), &graph.attempt(), &output)
        .unwrap();
}

#[test]
fn verified_predicate_matches_full_retained_claims_with_safe_numeric_normalization() {
    let graph = Graph::new(1);
    let baseline = graph.verifier();
    let prefix = "/0/verificationResult/statement/predicate";
    for pointer in [
        "/identity/manifestSha256",
        "/identity/claims/revision",
        "/evidenceCount",
        "/fileDigests/ops-release.json/sha256",
    ] {
        let mut output = baseline.clone();
        *output.pointer_mut(&format!("{prefix}{pointer}")).unwrap() = Value::Null;
        assert!(
            graph
                .matched(&Graph::repository(), &graph.attempt(), &output)
                .is_err()
        );
    }
    let mut output = baseline.clone();
    output[0]["verificationResult"]["statement"]["predicate"]["identity"]["claims"]["buildRunId"] =
        json!(123.0);
    graph
        .matched(&Graph::repository(), &graph.attempt(), &output)
        .unwrap();
    let mut output = baseline.clone();
    output[0]["verificationResult"]["statement"]["predicate"]["schemaVersion"] = json!(true);
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
    let mut output = baseline;
    output[0]["verificationResult"]["statement"]["predicate"]["PRIVATE-extra"] = json!(1);
    assert!(
        graph
            .matched(&Graph::repository(), &graph.attempt(), &output)
            .is_err()
    );
}

#[test]
fn only_verified_statement_and_certificate_are_authority_and_public_projection_is_fixed() {
    let graph = Graph::new(1);
    let mut output = graph.verifier();
    output[0]["attestation"] = json!({"PRIVATE":"fabricated-unverified-payload"});
    let matched = graph
        .matched(&Graph::repository(), &graph.attempt(), &output)
        .unwrap();
    let projection = serde_json::to_string(&matched).unwrap();
    for value in [
        projection,
        format!("{matched:?}"),
        format!("{:?}", graph.metadata().unwrap()),
    ] {
        for private in [
            "PRIVATE",
            "inputDigests",
            "evidenceDigests",
            "fileDigests",
            "attestation",
            "certificateIssuer",
            graph.root.0.to_str().unwrap(),
        ] {
            assert!(!value.contains(private), "{private}");
        }
        assert!(!value.contains("cryptographicallyVerified"));
    }
    for field in ["statement", "signature", "verifiedTimestamps", "mediaType"] {
        let mut output = graph.verifier();
        output[0]["verificationResult"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            graph
                .matched(&Graph::repository(), &graph.attempt(), &output)
                .is_err()
        );
    }
    for failure in [
        Failure::Json,
        Failure::Selection,
        Failure::Metadata,
        Failure::Evidence,
        Failure::Identity,
        Failure::Api,
        Failure::Verifier,
        Failure::Certificate,
        Failure::Timestamp,
        Failure::Subjects,
        Failure::Predicate,
        Failure::Pair,
    ] {
        assert!(failure.to_string().starts_with("ops_verification_"));
        assert!(!format!("{failure:?}").contains("PRIVATE"));
    }
}

#[test]
fn duplicate_keys_nonfinite_numbers_unknown_shapes_and_record_caps_refuse() {
    let graph = Graph::new(0);
    let metadata = graph.metadata().unwrap();
    let repo = bytes(&Graph::repository());
    let attempt = bytes(&graph.attempt());
    let output = bytes(&graph.verifier());
    for bad in [
        b"null".as_slice(),
        b"{}",
        b"[]",
        b"[{\"verificationResult\":null}]",
        b"{\"id\":1,\"id\":1}",
        b"{\"id\":1e309}",
    ] {
        assert!(
            metadata
                .interpret_successful_verifier_output(bad, &attempt, &output)
                .is_err()
        );
        assert!(
            metadata
                .interpret_successful_verifier_output(&repo, bad, &output)
                .is_err()
        );
        assert!(
            metadata
                .interpret_successful_verifier_output(&repo, &attempt, bad)
                .is_err()
        );
    }
    assert!(
        metadata
            .interpret_successful_verifier_output(&vec![b' '; MAX_API_BYTES + 1], &attempt, &output)
            .is_err()
    );
    assert!(
        metadata
            .interpret_successful_verifier_output(
                &repo,
                &attempt,
                &vec![b' '; MAX_VERIFIER_BYTES + 1]
            )
            .is_err()
    );
}
