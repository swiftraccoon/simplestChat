//! Prometheus sample text. Floats keep the spelling the earlier collector
//! wrote, so retained textfiles and new ones read alike.

/// One sample value: byte and snapshot counts are whole, the rest are floats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Sample {
    Whole(i128),
    Float(f64),
}

pub(super) use crate::json::python_float as float;

/// Append one sample under the fixed metric prefix, labelled only with an
/// internally selected service name.
pub(super) fn gauge(lines: &mut Vec<String>, name: &str, value: Sample, service: Option<&str>) {
    let label = service.map_or_else(String::new, |service| format!("{{service=\"{service}\"}}"));
    let value = match value {
        Sample::Whole(value) => value.to_string(),
        Sample::Float(value) => float(value),
    };
    lines.push(format!("simplestchat_ops_{name}{label} {value}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every recorded sample line, whole and fractional, small and large.
    #[test]
    fn samples_are_spelled_as_the_recorded_collector_spelled_them() {
        let recorded = super::super::recorded::reference();
        let cases = recorded["gauge"].as_array().unwrap();
        assert_eq!(cases.len(), 13);
        for case in cases {
            let value = if case["integer"] == true {
                Sample::Whole(i128::from(case["value"].as_u64().unwrap()))
            } else {
                Sample::Float(case["value"].as_f64().unwrap())
            };
            let mut lines = Vec::new();
            gauge(
                &mut lines,
                case["name"].as_str().unwrap(),
                value,
                case["service"].as_str(),
            );
            assert_eq!(lines, [case["line"].as_str().unwrap()]);
        }
    }
}
