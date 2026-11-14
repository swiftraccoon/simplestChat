//! Replays of the recorded licence expressions.
//!
//! `recorded_spdx.json` holds what the evaluator of
//! `build/security_image_policy.py` answered for each expression against
//! a list of allowed licences: allowed, refused, or the code of a fault in
//! its form. `STRICTER` names what it allowed and is refused here.

use super::{Allowed, DEEPEST, LONGEST, permits};
use serde_json::{Value, json};

const RECORDED: &str = include_str!("recorded_spdx.json");

/// A name is compared whatever the case of its ASCII letters. A licence of
/// the list that becomes the name only when other letters are folded, a
/// sharp s into two letters or the Kelvin sign into a k, is another name.
const STRICTER: [(&str, &str); 2] = [("MASS", "Ma\u{df}"), ("KIT", "\u{212a}IT")];

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

#[test]
fn every_recorded_expression_is_judged_alike() {
    let recording: Value = serde_json::from_str(RECORDED).expect("the recording is JSON");
    let (mut allowed, mut refused, mut faulty, mut stricter) = (0, 0, 0, 0);
    for case in recording["cases"].as_array().expect("cases") {
        let expression = text(&case["expression"]);
        let list = if case["allowed"].is_null() {
            &recording["allowed"]
        } else {
            &case["allowed"]
        };
        let list: Vec<&str> = list.as_array().expect("a list").iter().map(text).collect();
        let judged = permits(expression, &Allowed::new(list.iter().copied()));
        let reference = &case["result"];
        let strict = STRICTER
            .iter()
            .any(|(named, listed)| *named == expression && list == [*listed]);
        if strict {
            assert_eq!(reference["allowed"], json!(true), "{expression:?}");
            assert_eq!(judged, Ok(false), "{expression:?}");
            stricter += 1;
        } else if let Some(code) = reference.get("code") {
            let fault = judged.err().map(|fault| fault.0);
            assert_eq!(fault, Some(text(code)), "{expression:?}");
            faulty += 1;
        } else {
            let permitted = reference["allowed"].as_bool().expect("a verdict");
            assert_eq!(judged, Ok(permitted), "{expression:?} of {list:?}");
            if permitted {
                allowed += 1;
            } else {
                refused += 1;
            }
        }
    }
    assert_eq!((allowed, refused, faulty, stricter), (63, 48, 93, 2));
}

#[test]
fn the_bounds_are_the_recorded_ones() {
    let recording: Value = serde_json::from_str(RECORDED).expect("the recording is JSON");
    assert_eq!(json!(LONGEST), recording["limits"]["MAX_EXPRESSION"]);
    assert_eq!(json!(DEEPEST), recording["limits"]["MAX_LICENSE_DEPTH"]);
}
