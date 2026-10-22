//! Python's report formats: six-significant-digit `g`, fixed and grouped decimals.

pub(crate) fn general(value: f64) -> String {
    if !value.is_finite() {
        return repr(value);
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    if (-4..6).contains(&exponent) {
        trim(format!("{:.*}", (5 - exponent).max(0) as usize, value))
    } else {
        format!("{}e{exponent:+03}", trim(mantissa.to_owned()))
    }
}

fn trim(mut value: String) -> String {
    if value.contains('.') {
        while value.ends_with('0') {
            value.pop();
        }
        if value.ends_with('.') {
            value.pop();
        }
    }
    value
}

pub(crate) fn repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value == f64::INFINITY {
        return "inf".into();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".into();
    }
    let text = format!("{value:?}");
    if let Some((mantissa, exponent)) = text.split_once('e') {
        let exponent: i32 = exponent.parse().unwrap();
        format!("{mantissa}e{exponent:+03}")
    } else {
        text
    }
}

pub(crate) fn grouped(value: f64) -> String {
    let text = format!("{value:.0}");
    let mut result = String::new();
    let (sign, digits) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |value| ("-", value));
    result.push_str(sign);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(digit);
    }
    result
}
