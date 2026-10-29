//! Closed passive counts for witnessed cold-baseline health boundaries.
//! Counts describe retained guest observations; they confer no execution authority.

use super::{Error, Result, projection};
use serde_json::Value;

const FIELDS: [&str; 4] = [
    "openingHealthSeeded",
    "trailingHealthDeferred",
    "coldHealthObligations",
    "coldHealthCompletions",
];

fn count(value: &Value, name: &str, maximum: u64) -> Result<u64> {
    value[name]
        .as_u64()
        .filter(|count| *count <= maximum)
        .ok_or(Error::Refused)
}

pub(super) fn validate(value: &Value, positive: bool, baseline_passed: bool) -> Result<()> {
    if value.is_null() {
        return if positive || baseline_passed {
            Err(Error::Refused)
        } else {
            Ok(())
        };
    }
    projection::exact(value, &FIELDS)?;
    let opening = count(value, FIELDS[0], 1)?;
    let trailing = count(value, FIELDS[1], 1)?;
    let obligations = count(value, FIELDS[2], 2)?;
    let completions = count(value, FIELDS[3], 2)?;
    if obligations < opening.max(trailing)
        || obligations > opening + trailing
        || completions > obligations
        || positive && completions != obligations
    {
        return Err(Error::Refused);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn fixture() -> Value {
    // Synthetic maximum count widths for wire-size tests; no runtime path is proved.
    serde_json::json!({"openingHealthSeeded":1,"trailingHealthDeferred":1,
        "coldHealthObligations":2,"coldHealthCompletions":2})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evidence(opening: u64, trailing: u64, obligations: u64, completions: u64) -> Value {
        json!({"openingHealthSeeded":opening,"trailingHealthDeferred":trailing,
            "coldHealthObligations":obligations,"coldHealthCompletions":completions})
    }

    #[test]
    fn counts_admit_only_distinct_union_and_observed_completion_combinations() {
        for opening in 0..=1 {
            for trailing in 0..=1 {
                for obligations in 0..=2 {
                    for completions in 0..=2 {
                        let value = evidence(opening, trailing, obligations, completions);
                        let possible = opening.max(trailing) <= obligations
                            && obligations <= opening + trailing
                            && completions <= obligations;
                        assert_eq!(validate(&value, false, false).is_ok(), possible);
                        assert_eq!(validate(&value, false, true).is_ok(), possible);
                        assert_eq!(
                            validate(&value, true, true).is_ok(),
                            possible && completions == obligations
                        );
                    }
                }
            }
        }
        // Two boundary observations may refer to the same witnessed exec ID.
        validate(&evidence(1, 1, 1, 1), true, true).unwrap();
        validate(&evidence(1, 1, 2, 2), true, true).unwrap();
        // Explicit zero counts are valid but provide no nonzero boundary coverage.
        validate(&evidence(0, 0, 0, 0), true, true).unwrap();
    }

    #[test]
    fn absent_admission_is_null_and_unresolved_counts_never_support_success() {
        validate(&Value::Null, false, false).unwrap();
        assert!(validate(&Value::Null, false, true).is_err());
        assert!(validate(&Value::Null, true, false).is_err());
        assert!(validate(&Value::Null, true, true).is_err());
        for value in [evidence(1, 0, 1, 0), evidence(1, 1, 2, 1)] {
            validate(&value, false, false).unwrap();
            validate(&value, false, true).unwrap();
            assert!(validate(&value, true, true).is_err());
        }
    }

    #[test]
    fn fields_require_exact_bounded_integers_and_no_embedded_private_material() {
        let value = fixture();
        for name in FIELDS {
            for replacement in [
                Value::Null,
                json!(true),
                json!(-1),
                json!(0.0),
                json!(1.0),
                json!("1"),
                json!([]),
                json!({}),
                json!(u64::MAX),
            ] {
                let mut changed = value.clone();
                changed[name] = replacement;
                assert!(validate(&changed, false, false).is_err(), "{name}");
            }
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(name);
            assert!(validate(&missing, false, false).is_err(), "{name}");
        }
        for (name, maximum) in FIELDS.into_iter().zip([1, 1, 2, 2]) {
            let mut changed = value.clone();
            changed[name] = json!(maximum + 1);
            assert!(validate(&changed, false, false).is_err(), "{name}");
        }
        for (name, private) in [
            ("execId", json!("PRIVATE_EXEC")),
            ("command", json!("PRIVATE_COMMAND")),
            ("source", json!("PRIVATE_SOURCE")),
        ] {
            let mut changed = value.clone();
            changed[name] = private;
            assert!(validate(&changed, false, false).is_err(), "{name}");
        }
        for changed in [json!(true), json!(0), json!("PRIVATE"), json!([])] {
            assert!(validate(&changed, false, false).is_err());
        }
    }
}
