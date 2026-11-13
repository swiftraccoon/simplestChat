//! Replays of the recorded exception policy and source scope.
//!
//! `recorded_policy.json` holds what `build/security_policy.py` and
//! `build/security_source_scope.py` answered for single review records, whole
//! policy files, source names and automation markers. Where the reference
//! failed with an exception of its interpreter, `named` gives the code here.

use super::{Date, Exception, parse, permitted, read};
use crate::{Result, scope, scratch::Scratch};
use serde_json::{Value, json};
use std::path::Path;

const RECORDED: &str = include_str!("recorded_policy.json");

/// Review links the reference admitted and this policy refuses: a link is
/// spelled plainly, to an ASCII host or an IPv6 address.
const PLAIN_LINKS_ONLY: [&str; 10] = [
    "a review link with a leading space",
    "a review link with a trailing space",
    "a review link with a space in its host",
    "a review link with a tab inside its host",
    "a review link with a line feed inside its path",
    "a review link to a host outside ASCII",
    "a review link to a scoped IPv6 address",
    "a review link to a future address form",
    "a review link with a delete character",
    "a review link with a NUL in its path",
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn today(recording: &Value) -> Date {
    Date::parse(text(&recording["today"])).expect("the recorded day")
}

fn named<'a>(code: &'a str, name: &str) -> &'a str {
    match code {
        "ValueError" if name.starts_with("an expiry") => "invalid_exception_expiry",
        "ValueError" => "invalid_exception_review_link",
        "JSONDecodeError" => "invalid_tool_json",
        code => code,
    }
}

fn code<T>(judged: &Result<T>) -> Option<&'static str> {
    judged.as_ref().err().map(|fault| fault.0)
}

fn fields(review: &Exception) -> Value {
    json!([
        review.scanner,
        review.fingerprint,
        review.scope,
        review.owner,
        review.rationale,
        review.reachability,
        review.expires.to_string(),
        review.review,
    ])
}

#[test]
fn every_recorded_review_is_judged_alike() {
    let recording = recorded();
    let today = today(&recording);
    let (mut held, mut refused, mut stricter) = (0, 0, 0);
    for case in recording["records"].as_array().expect("records") {
        let name = text(&case["name"]);
        let judged = parse(&case["record"], today);
        let result = &case["result"];
        if PLAIN_LINKS_ONLY.contains(&name) {
            assert!(result.get("fields").is_some(), "{name}");
            assert_eq!(
                code(&judged),
                Some("invalid_exception_review_link"),
                "{name}"
            );
            stricter += 1;
        } else if let Some(expected) = result.get("code") {
            assert_eq!(code(&judged), Some(named(text(expected), name)), "{name}");
            refused += 1;
        } else {
            let review = judged.unwrap_or_else(|fault| panic!("{name}: {fault}"));
            assert_eq!(fields(&review), result["fields"], "{name}");
            held += 1;
        }
    }
    assert_eq!((held, refused, stricter), (83, 207, 10));
}

#[test]
fn every_recorded_policy_is_read_alike() {
    let recording = recorded();
    let today = today(&recording);
    let scratch = Scratch::new("policies");
    let path = scratch.0.join("exceptions.json");
    let base = &recording["records"][0]["record"];
    let (mut held, mut refused) = (0, 0);
    for case in recording["policies"].as_array().expect("policies") {
        let name = text(&case["name"]);
        let mut document = match case.get("count").and_then(Value::as_u64) {
            Some(count) => {
                let reviews: Vec<Value> = (0..count)
                    .map(|index| {
                        let mut review = base.clone();
                        review["fingerprint"] = json!(format!("RUSTSEC-2024-{index:04}"));
                        review
                    })
                    .collect();
                json!({"schemaVersion": 1, "exceptions": reviews}).to_string()
            }
            None => text(&case["text"]).to_owned(),
        };
        let pad = case.get("pad").and_then(Value::as_u64).unwrap_or(0);
        document.push_str(&" ".repeat(usize::try_from(pad).expect("pad")));
        std::fs::write(&path, &document).expect("policy");
        let vendor = case["includeVendor"] == true;
        let judged = read(&path, today, vendor);
        let result = &case["result"];
        if let Some(expected) = result.get("code") {
            assert_eq!(code(&judged), Some(named(text(expected), name)), "{name}");
            refused += 1;
            continue;
        }
        let reviews = judged.unwrap_or_else(|fault| panic!("{name}: {fault}"));
        match result.get("count") {
            Some(count) => assert_eq!(json!(reviews.len()), *count, "{name}"),
            None => {
                let listed: Vec<Value> = reviews.iter().map(fields).collect();
                assert_eq!(json!(listed), result["reviews"], "{name}");
            }
        }
        held += 1;
    }
    assert_eq!((held, refused), (13, 27));
}

#[test]
fn every_recorded_name_and_marker_is_scoped_alike() {
    let recording = recorded();
    let names = recording["names"].as_array().expect("names");
    for case in names {
        let name = text(&case["name"]);
        assert_eq!(json!(scope::vendor_path(name)), case["vendor"], "{name:?}");
        assert_eq!(
            json!(scope::selected(name, false)),
            case["selected"],
            "{name:?}"
        );
        assert_eq!(
            json!(scope::selected(name, true)),
            case["selectedWithVendor"],
            "{name:?}"
        );
    }
    let markers = recording["markers"].as_array().expect("markers");
    let mut refused = 0;
    for case in markers {
        let present: Vec<&str> = case["present"]
            .as_array()
            .expect("present")
            .iter()
            .map(text)
            .collect();
        let automated = scope::automated(|name| present.contains(&name));
        let judged = scope::require_local_vendor(case["includeVendor"] == true, automated);
        assert_eq!(code(&judged), case["result"]["code"].as_str(), "{case}");
        refused += usize::from(judged.is_err());
    }
    assert_eq!((names.len(), markers.len(), refused), (19, 14, 4));
}

#[test]
fn a_review_waives_exactly_its_own_finding() {
    let recording = recorded();
    let review = parse(&recording["records"][0]["record"], today(&recording)).expect("a review");
    let reviews = [review.clone()];
    let (scanner, fingerprint, scope) = (&review.scanner, &review.fingerprint, &review.scope);
    assert!(permitted(&reviews, scanner, fingerprint, scope));
    assert!(!permitted(&reviews, "cargo-deny", fingerprint, scope));
    assert!(!permitted(&reviews, scanner, "RUSTSEC-2024-0385", scope));
    assert!(!permitted(&reviews, scanner, fingerprint, "instant@0.1.14"));
    assert!(!permitted(&[], scanner, fingerprint, scope));
}

#[test]
fn a_day_number_is_the_calendar_day_the_reference_names() {
    let recording = recorded();
    let days = recording["days"].as_array().expect("days");
    for case in days {
        let day = Date::of_epoch_day(case[0].as_u64().expect("a day number"));
        assert_eq!(
            day.map(|day| day.to_string()),
            case[1].as_str().map(str::to_owned)
        );
    }
    assert_eq!(days.len(), 22);
    let seconds = recording["seconds"].as_array().expect("seconds");
    for case in seconds {
        let day = Date::of_epoch_second(case[0].as_u64().expect("a second"));
        assert_eq!(
            day.map(|day| day.to_string()),
            case[1].as_str().map(str::to_owned)
        );
    }
    assert_eq!(seconds.len(), 14);
    // The day after the last one is no day, and today is a day of this era.
    assert_eq!(Date::of_epoch_day(2_932_897), None);
    assert!(Date::today() > Date::new(2026, 1, 1).expect("a day"));
    assert_eq!(Date::MIN.to_string(), "0001-01-01");
}

#[test]
fn the_committed_policy_is_admitted_whole() {
    // Read as of the first day, so that only the records' shape is judged:
    // whether one has expired is the gate's question on the day it runs.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../security/exceptions.json");
    let all = read(&path, Date::MIN, true).expect("the committed policy");
    let selected = read(&path, Date::MIN, false).expect("the committed policy");
    assert!(!selected.is_empty() && selected.len() <= all.len());
    assert!(selected.iter().all(|review| {
        !(scope::vendor_path(&review.scope)
            && ["codeql", "gitleaks", "semgrep", "zizmor"].contains(&review.scanner.as_str()))
    }));
}
