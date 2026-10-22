//! Actual reference admission prevents foreign or timed-out evidence entering
//! post-shutdown reports; completed failing workloads retain their boundaries.

use super::{commands, options, policy_tests};
use serde_json::{Value, json};
use std::ffi::OsString;

fn options() -> Value {
    let mut arguments = policy_tests::required();
    arguments.extend(["--purpose", "diagnostic"].map(str::to_owned));
    serde_json::to_value(
        options::parse(
            &arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>(),
            &std::env::current_dir().unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn evidence_admission_matches_reference_for_completed_failures_and_foreign_inputs() {
    let options = options();
    let hash = "a".repeat(64);
    let summary = json!({"schemaVersion":2,"diagnosticFailures":0,"run":{
        "completed":true,"passed":false,"startedAt":"2026-10-21T00:00:00.000Z",
        "finishedAt":"2026-10-21T00:01:15.000Z","configuration":{
        "diagnostics":true,"departure":"abrupt","rampUpSecs":5,
        "warmupSecs":10,"durationSecs":60,"subscriptionPlan":null,"subscriptionSeed":null},
        "provenance":{"generatorBinarySha256":hash}}});
    let mut cases = Vec::new();
    let mut variants = vec![summary.clone()];
    for (pointer, values) in [
        ("/schemaVersion", vec![json!(1), Value::Null]),
        (
            "/diagnosticFailures",
            vec![
                Value::Null,
                json!(-1),
                json!(0.5),
                json!(1),
                json!(409600),
                json!(409601),
            ],
        ),
        ("/run/completed", vec![json!(false), json!(1)]),
        ("/run/passed", vec![json!(true), json!(false)]),
        (
            "/run/configuration/diagnostics",
            vec![json!(false), json!(1)],
        ),
        (
            "/run/configuration/departure",
            vec![json!("explicit-leave"), Value::Null],
        ),
        (
            "/run/configuration/subscriptionPlan",
            vec![json!("fifo"), json!("ring-v1"), json!(1)],
        ),
        (
            "/run/configuration/subscriptionSeed",
            vec![json!(0), json!("private-sentinel")],
        ),
        ("/run/configuration/rampUpSecs", vec![json!(4), json!("5")]),
        (
            "/run/configuration/warmupSecs",
            vec![json!(11), json!("10")],
        ),
        (
            "/run/configuration/durationSecs",
            vec![json!(59), json!("60")],
        ),
        (
            "/run/provenance/generatorBinarySha256",
            vec![json!("private-sentinel"), Value::Null],
        ),
    ] {
        for value in values {
            let mut variant = summary.clone();
            *variant.pointer_mut(pointer).unwrap() = value;
            variants.push(variant);
        }
    }
    for summary in variants {
        cases.push(json!({"op":"lifecycleWorkload","args":[summary,options,hash]}));
        for timed_out in [false, true] {
            cases.push(json!({"op":"admitsDiagnosticEvidence",
                "args":[summary,options,hash,timed_out]}));
        }
    }
    for detail in ["full", "capture-only"] {
        let mut settings = options.clone();
        settings["diagnosticDetail"] = json!(detail);
        for interface in [Value::Null, json!("lo"), json!("lo0"), json!("eth0")] {
            if interface.is_null() {
                settings.as_object_mut().unwrap().remove("captureInterface");
            } else {
                settings["captureInterface"] = interface;
            }
            cases.push(json!({"op":"serverDiagnosticEnvironment",
                "args":[settings,"/tmp/owned benchmark spaces"]}));
            cases.push(json!({"op":"captureArguments",
                "args":[settings,"/tmp/owned benchmark spaces"]}));
        }
    }
    policy_tests::compare(&cases);
}

#[test]
fn child_configuration_excludes_inherited_and_private_capabilities() {
    let options = serde_json::from_value(options()).unwrap();
    let inherited = [
        ("PATH", "/owned tools"),
        ("TMPDIR", "/private"),
        ("LANG", "C"),
        ("DATABASE_URL", "private-sentinel"),
        ("HTTPS_PROXY", "private-sentinel"),
        ("AWS_SECRET_ACCESS_KEY", "private-sentinel"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    let environment = commands::server_environment(
        &options,
        std::path::Path::new("/private run"),
        "secret-token",
        &inherited,
    );
    assert_eq!(
        environment[&OsString::from("METRICS_TOKEN")],
        "secret-token"
    );
    let public = serde_json::to_string(&commands::public_configuration(&environment)).unwrap();
    assert!(!public.contains("secret-token"));
    assert!(!public.contains("private-sentinel"));
    assert!(!public.contains("/owned tools"));
    assert!(public.contains("127.0.0.1"));
    assert!(public.contains("DIAGNOSTICS_PATH"));
}
