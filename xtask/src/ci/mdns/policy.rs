//! Bounded observation projection. Raw host strings never enter a report/error.
use super::failure;
use crate::tooling::Result;
use serde_json::{Map, Value, json};
use std::net::Ipv4Addr;
const SAFE: i64 = 9_007_199_254_740_991;
pub(super) const LIMIT: u64 = 512 * 1024;
pub(super) const PREDICATE: &str = concat!(
    "((process CONTAINS[c] \"WebKit\" AND (eventMessage CONTAINS[c] \"NetworkMDNSRegister\" ",
    "OR eventMessage CONTAINS[c] \"DNSServiceRegisterRecord\")) OR (process == \"sandboxd\" ",
    "AND eventMessage CONTAINS[c] \"WebKit\" AND (eventMessage CONTAINS[c] \"mDNS\" ",
    "OR eventMessage CONTAINS[c] \"network\")))"
);
pub(super) fn text(bytes: &[u8]) -> Result<&str> {
    if bytes.len() as u64 > LIMIT {
        return Err(failure(1));
    }
    std::str::from_utf8(bytes).map_err(|_| failure(1))
}
pub(super) fn interfaces(address: &str, output: &[u8]) -> Result<Value> {
    let address = address.parse::<Ipv4Addr>().map_err(|_| failure(1))?;
    let mut current = None;
    let mut names = std::collections::BTreeSet::new();
    let (mut total, mut external, mut selected) = (0u32, 0u32, false);
    for line in text(output)?.lines() {
        if line.len() > 4096 {
            return Err(failure(1));
        }
        if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
            let (name, rest) = line.split_once(": flags=").ok_or_else(|| failure(1))?;
            if name.is_empty()
                || name.len() > 64
                || names.len() >= 4096
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || !names.insert(name)
            {
                return Err(failure(1));
            }
            let (number, flags) = rest.split_once('<').ok_or_else(|| failure(1))?;
            let (flags, tail) = flags.split_once('>').ok_or_else(|| failure(1))?;
            if number.is_empty()
                || !number.bytes().all(|b| b.is_ascii_hexdigit())
                || flags.len() > 1024
                || tail.bytes().any(|b| b < 32 || b == 127)
                || (!flags.is_empty()
                    && !flags.split(',').all(|f| {
                        !f.is_empty()
                            && f.bytes()
                                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    }))
            {
                return Err(failure(1));
            }
            let flags: Vec<_> = flags.split(',').collect();
            current = Some((
                flags.contains(&"LOOPBACK"),
                flags.contains(&"UP") && flags.contains(&"RUNNING"),
            ));
        } else if let Some(rest) = line.trim_start().strip_prefix("inet ") {
            let (internal, eligible) = current.ok_or_else(|| failure(1))?;
            let value = rest
                .split_ascii_whitespace()
                .next()
                .ok_or_else(|| failure(1))?
                .parse::<Ipv4Addr>()
                .map_err(|_| failure(1))?;
            if !eligible {
                continue;
            }
            if total >= 4096 {
                return Err(failure(1));
            }
            total += 1;
            if !internal {
                external += 1;
                selected |= value == address;
            }
        }
    }
    if !selected {
        return Err(failure(1));
    }
    Ok(
        json!({"ipv4Addresses":total,"nonLoopbackIpv4Addresses":external,
        "selectedAddressAssigned":true}),
    )
}
pub(super) fn logs(output: &[u8]) -> Result<Value> {
    let patterns = [
        "registerMDNSNameCallback with error (-?[0-9]+)",
        "DNSServiceCreateConnection error (-?[0-9]+)",
        "DNSServiceRegisterRecord error (-?[0-9]+)",
    ];
    let expressions = patterns.map(|p| regex::Regex::new(p).expect("fixed numeric policy"));
    let mut counts = [Map::new(), Map::new(), Map::new()];
    let (mut callbacks, mut denials) = (0u32, 0u32);
    let webkit = regex::Regex::new("(?i)WebKit").unwrap();
    let network = regex::Regex::new("(?i)mDNS|network").unwrap();
    let deny = regex::Regex::new("(?i)deny|denied").unwrap();
    for (index, line) in text(output)?.split('\n').enumerate() {
        if index > 32768 {
            return Err(failure(1));
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(message) = value.get("eventMessage").and_then(Value::as_str) else {
            continue;
        };
        for (index, expression) in expressions.iter().enumerate() {
            if let Some(capture) = expression.captures(message) {
                if index == 0 {
                    callbacks += 1;
                }
                if let Ok(code) = capture[1].parse::<i64>() {
                    if !(-SAFE..=SAFE).contains(&code) {
                        continue;
                    }
                    let key = code.to_string();
                    if counts[index].len() >= 4096 && !counts[index].contains_key(&key) {
                        return Err(failure(1));
                    }
                    let count = counts[index].get(&key).and_then(Value::as_u64).unwrap_or(0) + 1;
                    counts[index].insert(key, count.into());
                }
            }
        }
        if webkit.is_match(message) && network.is_match(message) && deny.is_match(message) {
            denials += 1;
        }
    }
    let [callback, create, register] = counts;
    Ok(
        json!({"registrationCallbacks":callbacks,"callbackErrors":callback,
        "createConnectionErrors":create,"registerRecordErrors":register,"sandboxDenials":denials}),
    )
}
pub(super) struct Observation {
    pub stdout: Vec<u8>,
    pub exit: Option<u8>,
    pub timed_out: bool,
}
impl Observation {
    pub fn passed(&self) -> bool {
        self.exit == Some(0) && !self.timed_out
    }
}
pub(super) fn probe(result: &Observation) -> Value {
    let invalid = || {
        json!({"passed":false,"stage":"invalid_report",
        "exitCode":result.exit,"timedOut":result.timed_out})
    };
    let Ok(value) = serde_json::from_slice::<Value>(&result.stdout) else {
        return invalid();
    };
    let stage = value.get("stage").and_then(Value::as_str).filter(|s| {
        [
            "arguments",
            "create_connection",
            "set_dispatch_queue",
            "register_record",
            "callback",
            "callback_timeout",
        ]
        .contains(s)
    });
    let error = value
        .get("error")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= SAFE as f64)
        .map(|n| n as i64);
    let (Some(stage), Some(error), Some(callback), Some(timeout), Some(deallocated)) = (
        stage,
        error,
        value.get("callbackObserved").and_then(Value::as_bool),
        value.get("timedOut").and_then(Value::as_bool),
        value.get("deallocated").and_then(Value::as_bool),
    ) else {
        return invalid();
    };
    json!({"passed":result.passed() && stage == "callback" && error == 0 && callback
        && deallocated && !timeout,"stage":stage,"error":error,"callbackObserved":callback,
        "timedOut":timeout || result.timed_out,"deallocated":deallocated,"exitCode":result.exit})
}
pub(super) fn system(
    plist: &Observation,
    launch: &Observation,
    logs_output: &Observation,
) -> Result<Value> {
    let known = [
        "com.apple.mDNSResponder",
        "com.apple.mDNSResponder.reloaded",
    ]
    .contains(&text(&plist.stdout)?.trim());
    let pattern = regex::Regex::new(r"(?m)^\s*state = (running|waiting|exited|not running)\s*$")
        .expect("fixed responder policy");
    let launch_text = text(&launch.stdout)?;
    let state = pattern
        .captures(launch_text)
        .and_then(|v| v.get(1))
        .map_or("unavailable", |v| v.as_str());
    let mut logs = logs(&logs_output.stdout)?;
    logs["readable"] = (logs_output.exit == Some(0)).into();
    logs["exitCode"] = json!(logs_output.exit);
    logs["timedOut"] = logs_output.timed_out.into();
    Ok(
        json!({"responder":{"readable":known && launch.exit==Some(0),"exitCode":launch.exit,
        "timedOut":launch.timed_out,"state":state},"logs":logs}),
    )
}
