//! Pure Linux loopback profile planning and bounded iproute2 JSON observations.
//!
//! Shapes/signatures do not authenticate kernel ownership. The caller must own
//! an exclusive disposable interface, remember its clean baseline and attempted
//! prefix, and refuse changed signatures before deleting handle `1:`.
//! The supported schema follows iproute2 v6.1.0 `tc/q_netem.c` and `tc/f_u32.c`:
//! <https://github.com/iproute2/iproute2/blob/v6.1.0/tc/q_netem.c>.
//! Loss is a fraction, times are seconds and rates are bytes/second. Six-digit
//! JSON float printing and kernel quantization permit only the tolerances below.

use anyhow::{Result, bail, ensure};
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use serde::{Deserializer, Serialize};
use serde_json::{Map, Number, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;

const INVALID_PROFILE: &str = "invalid impairment profile";
const INVALID_STATE: &str = "invalid impairment state";
const MAX_OUTPUT: usize = 65536;
const MAX_VALUES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Direction {
    Downlink,
    Both,
}

/// Validated shell-free parameters. Defaults match `build/impair.sh`; the
/// generator controller supplies its different 5%/50ms/10ms/both defaults.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Profile {
    pub port: u16,
    pub workers: u8,
    pub loss: f64,
    pub delay_ms: f64,
    pub jitter_ms: f64,
    pub rate_kbit: f64,
    pub direction: Direction,
}

fn decimal(value: &str, maximum: f64) -> Result<f64> {
    ensure!(
        !value.is_empty()
            && value.len() <= 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            && value.split('.').count() <= 2
            && value.split('.').all(|part| !part.is_empty())
            && value
                .split_once('.')
                .is_none_or(|(_, part)| part.len() <= 6),
        INVALID_PROFILE
    );
    let number: f64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!(INVALID_PROFILE))?;
    ensure!(number.is_finite() && number <= maximum, INVALID_PROFILE);
    Ok(number)
}

impl Profile {
    /// Read only the fixed IMPAIR vocabulary from an explicitly captured map.
    /// Decimal values have at most six fractional digits; delay/jitter <=60s,
    /// rate <=10Gbit/s, loss <=100%. Jitter needs nonzero delay. Nonzero times
    /// below 1us and rates below 1byte/s are unsupported, rather than rounded
    /// away into an unverifiable requested profile.
    pub fn parse(environment: &BTreeMap<OsString, OsString>) -> Result<Self> {
        let read = |name: &str, default: &str| -> Result<String> {
            environment.get(&OsString::from(name)).map_or_else(
                || Ok(default.to_owned()),
                |value| {
                    value
                        .to_str()
                        .map(|value| if value.is_empty() { default } else { value }.to_owned())
                        .ok_or_else(|| anyhow::anyhow!(INVALID_PROFILE))
                },
            )
        };
        let integer = |name, default, maximum: u32| -> Result<u32> {
            let value = read(name, default)?;
            ensure!(
                !value.is_empty() && value.len() <= 5 && value.bytes().all(|b| b.is_ascii_digit()),
                INVALID_PROFILE
            );
            let number: u32 = value
                .parse()
                .map_err(|_| anyhow::anyhow!(INVALID_PROFILE))?;
            ensure!((1..=maximum).contains(&number), INVALID_PROFILE);
            Ok(number)
        };
        let port = integer("IMPAIR_UDP_PORT", "41100", 65535)? as u16;
        let workers = integer("IMPAIR_WORKERS", "1", 4)? as u8;
        let span = u32::from(workers).next_power_of_two();
        ensure!(
            u32::from(port).is_multiple_of(span) && u32::from(port) + span <= 65536,
            INVALID_PROFILE
        );
        let direction = match read("IMPAIR_DIRECTION", "downlink")?.as_str() {
            "downlink" => Direction::Downlink,
            "both" => Direction::Both,
            _ => bail!(INVALID_PROFILE),
        };
        let loss = decimal(&read("IMPAIR_LOSS", "0")?, 100.0)?;
        let delay_ms = decimal(&read("IMPAIR_DELAY_MS", "0")?, 60000.0)?;
        let jitter_ms = decimal(&read("IMPAIR_JITTER_MS", "0")?, 60000.0)?;
        let rate_kbit = decimal(&read("IMPAIR_RATE_KBIT", "0")?, 10000000.0)?;
        ensure!(
            (jitter_ms == 0.0 || delay_ms != 0.0)
                && [delay_ms, jitter_ms]
                    .iter()
                    .all(|v| *v == 0.0 || *v >= 0.001)
                && (rate_kbit == 0.0 || rate_kbit >= 0.008),
            INVALID_PROFILE
        );
        Ok(Self {
            port,
            workers,
            loss,
            delay_ms,
            jitter_ms,
            rate_kbit,
            direction,
        })
    }

    /// Additive, literal `tc` argv only; no executable, sudo or implicit clear.
    pub fn additions(&self) -> Vec<Vec<String>> {
        let words = |text: &str| {
            text.split_ascii_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let mut root = words("qdisc add dev lo root handle 1: prio bands 3 priomap");
        root.extend(std::iter::repeat_n("0".into(), 16));
        let mut netem = words("qdisc add dev lo parent 1:3 handle 30: netem limit 200000");
        if self.loss != 0.0 {
            netem.extend(["loss".into(), format!("{}%", numeric(self.loss))]);
        }
        if self.delay_ms != 0.0 {
            netem.extend(["delay".into(), format!("{}ms", numeric(self.delay_ms))]);
            if self.jitter_ms != 0.0 {
                netem.push(format!("{}ms", numeric(self.jitter_ms)));
            }
        }
        if self.rate_kbit != 0.0 {
            netem.extend(["rate".into(), format!("{}kbit", numeric(self.rate_kbit))]);
        }
        let mut output = vec![root, netem];
        for (priority, direction) in [(1, "sport"), (2, "dport")] {
            if priority == 2 && self.direction != Direction::Both {
                break;
            }
            let mut filter = words("filter add dev lo parent 1: protocol ip prio");
            filter.push(priority.to_string());
            filter.extend(words("u32 match ip protocol 17 0xff match ip"));
            filter.extend([
                direction.into(),
                self.port.to_string(),
                format!(
                    "0x{:x}",
                    65536 - u32::from(self.workers).next_power_of_two()
                ),
                "flowid".into(),
                "1:3".into(),
            ]);
            output.push(filter);
        }
        output
    }
}

fn numeric(value: f64) -> String {
    ryu_js::Buffer::new().format_finite(value).to_owned()
}

struct Parser {
    filter: bool,
    selectors: usize,
    values: usize,
}
struct Seed<'a> {
    parser: &'a mut Parser,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<Value, D::Error> {
        self.parser.values += 1;
        if self.depth > 12 || self.parser.values > MAX_VALUES {
            return Err(D::Error::custom(INVALID_STATE));
        }
        input.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(INVALID_STATE)
    }
    fn visit_bool<E: Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_unit<E: Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_i64<E: Error>(self, value: i64) -> std::result::Result<Value, E> {
        self.visit_f64(value as f64)
    }
    fn visit_u64<E: Error>(self, value: u64) -> std::result::Result<Value, E> {
        self.visit_f64(value as f64)
    }
    fn visit_f64<E: Error>(self, value: f64) -> std::result::Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom(INVALID_STATE))
    }
    fn visit_str<E: Error>(self, value: &str) -> std::result::Result<Value, E> {
        if value.len() > 8192 {
            return Err(E::custom(INVALID_STATE));
        }
        Ok(Value::String(value.to_owned()))
    }
    fn visit_map<M: MapAccess<'de>>(self, mut input: M) -> std::result::Result<Value, M::Error> {
        let mut output = Map::new();
        while let Some(mut key) = input.next_key::<String>()? {
            if key.len() > 2048 || (self.parser.filter && selector_key(&key)) {
                return Err(M::Error::custom(INVALID_STATE));
            }
            if self.parser.filter && key == "match" {
                key = format!("fixtureMatch{}", self.parser.selectors);
                self.parser.selectors += 1;
            }
            if output.contains_key(&key) {
                return Err(M::Error::custom(INVALID_STATE));
            }
            let value = input.next_value_seed(Seed {
                parser: self.parser,
                depth: self.depth + 1,
            })?;
            output.insert(key, value);
        }
        Ok(Value::Object(output))
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut input: S) -> std::result::Result<Value, S::Error> {
        let mut output = Vec::new();
        while let Some(value) = input.next_element_seed(Seed {
            parser: self.parser,
            depth: self.depth + 1,
        })? {
            output.push(value);
        }
        Ok(Value::Array(output))
    }
}

fn selector_key(key: &str) -> bool {
    key.strip_prefix("fixtureMatch")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
}

fn decode(bytes: &[u8], filter: bool) -> Result<Vec<Value>> {
    ensure!(bytes.len() <= MAX_OUTPUT, INVALID_STATE);
    let mut parser = Parser {
        filter,
        selectors: 0,
        values: 0,
    };
    let mut input = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        parser: &mut parser,
        depth: 0,
    }
    .deserialize(&mut input)
    .map_err(|_| anyhow::anyhow!(INVALID_STATE))?;
    input.end().map_err(|_| anyhow::anyhow!(INVALID_STATE))?;
    let Value::Array(values) = value else {
        bail!(INVALID_STATE)
    };
    ensure!(
        values.len() <= 16 && values.iter().all(Value::is_object),
        INVALID_STATE
    );
    Ok(values)
}

fn only(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.keys().all(|key| keys.contains(&key.as_str())))
}
fn number(value: &Value, expected: f64) -> bool {
    value.as_f64() == Some(expected)
}
fn near(value: &Value, expected: f64, quantization: f64) -> bool {
    // v6.1 emits %g (six significant decimal digits). This additional tolerance
    // is observational equivalence, not proof of exact hidden kernel values.
    let printing = if expected == 0.0 {
        0.0
    } else {
        0.500001 * 10_f64.powf(expected.abs().log10().floor() - 5.0)
    };
    value.as_f64().is_some_and(|actual| {
        (actual - expected).abs() <= quantization + printing
            && if expected == 0.0 {
                actual == 0.0
            } else {
                actual > 0.0
            }
    })
}

fn valid_netem(value: &Value, profile: &Profile) -> bool {
    if !only(
        value,
        &["limit", "delay", "loss-random", "rate", "ecn", "gap"],
    ) || !number(&value["limit"], 200000.0)
        || value["ecn"] != false
        || !number(&value["gap"], 0.0)
    {
        return false;
    }
    let delay = &value["delay"];
    let valid_delay = if profile.delay_ms == 0.0 {
        value.get("delay").is_none()
    } else {
        only(delay, &["delay", "jitter", "correlation"])
            && near(&delay["delay"], profile.delay_ms / 1000.0, 0.000001)
            && near(&delay["jitter"], profile.jitter_ms / 1000.0, 0.000001)
            && number(&delay["correlation"], 0.0)
    };
    let loss = &value["loss-random"];
    let valid_loss = if profile.loss == 0.0 {
        value.get("loss-random").is_none()
    } else {
        only(loss, &["loss", "correlation"])
            && near(
                &loss["loss"],
                profile.loss / 100.0,
                0.5 / f64::from(u32::MAX),
            )
            && number(&loss["correlation"], 0.0)
    };
    let rate = &value["rate"];
    let valid_rate = if profile.rate_kbit == 0.0 {
        value.get("rate").is_none()
    } else {
        only(
            rate,
            &["rate", "packetoverhead", "cellsize", "celloverhead"],
        ) && number(&rate["rate"], (profile.rate_kbit * 125.0).floor())
            && ["packetoverhead", "cellsize", "celloverhead"]
                .iter()
                .all(|name| number(&rate[*name], 0.0))
    };
    valid_delay && valid_loss && valid_rate
}

fn match_selector(selector: &Value, value: u32, mask: u32, offset: u8) -> bool {
    let hexadecimal = |key: &str, expected| {
        selector[key].as_str().is_some_and(|text| {
            !text.is_empty()
                && text.len() <= 8
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && u32::from_str_radix(text, 16).ok() == Some(expected)
        })
    };
    only(selector, &["value", "mask", "offmask", "off"])
        && hexadecimal("value", value)
        && hexadecimal("mask", mask)
        && selector["offmask"] == ""
        && number(&selector["off"], f64::from(offset))
}

fn valid_leaf(options: &Value, priority: u8, profile: &Profile) -> bool {
    let Some(options) = options.as_object() else {
        return false;
    };
    if options.keys().any(|key| {
        ![
            "fh",
            "order",
            "key_ht",
            "bkt",
            "flowid",
            "not_in_hw",
            "in_hw",
        ]
        .contains(&key.as_str())
            && !selector_key(key)
    }) {
        return false;
    }
    let selectors = options
        .iter()
        .filter(|(key, _)| selector_key(key))
        .map(|(_, value)| value);
    let selectors = selectors.collect::<Vec<_>>();
    let span = u32::from(profile.workers).next_power_of_two();
    let (value, mask) = if priority == 1 {
        (u32::from(profile.port) << 16, (65536 - span) << 16)
    } else {
        (u32::from(profile.port), 65536 - span)
    };
    selectors.len() == 2
        && selectors
            .iter()
            .any(|s| match_selector(s, 0x00110000, 0x00ff0000, 8))
        && selectors.iter().any(|s| match_selector(s, value, mask, 20))
}

fn valid_filters(filters: &[Value], netem: bool, profile: &Profile) -> Option<usize> {
    let mut leaves = [0_usize; 2];
    let mut counts = [0_usize; 2];
    for entry in filters {
        let priority = match entry["pref"].as_f64() {
            Some(1.0) => 1,
            Some(2.0) if profile.direction == Direction::Both => 2,
            _ => return None,
        };
        counts[priority - 1] += 1;
        if entry["kind"] != "u32"
            || entry["protocol"] != "ip"
            || entry.get("chain").is_some_and(|v| !number(v, 0.0))
            || entry.get("parent").is_some_and(|v| v != "1:")
            || counts[priority - 1] > 3
        {
            return None;
        }
        let Some(options) = entry.get("options") else {
            continue;
        };
        if !options.is_object() {
            return None;
        }
        if options.as_object().is_some_and(Map::is_empty) {
            continue;
        }
        if options.get("flowid").is_none() {
            if !only(options, &["fh", "ht_divisor"]) || !number(&options["ht_divisor"], 1.0) {
                return None;
            }
        } else {
            leaves[priority - 1] += 1;
            if !netem || options["flowid"] != "1:3" || !valid_leaf(options, priority as u8, profile)
            {
                return None;
            }
        }
    }
    if leaves[0] > 1 || leaves[1] > 1 || (counts[1] != 0 && leaves[0] != 1) {
        return None;
    }
    Some(leaves[0] + leaves[1])
}

/// Sanitized observation. A matching shape/signature alone grants no authority
/// to mutate an interface; identical foreign replacement is indistinguishable.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct State {
    pub clean: bool,
    pub owned_shape: bool,
    pub complete: bool,
    pub signature: String,
    pub evidence: Value,
}

/// Parse <=64KiB per stream, <=4096 values and depth12. Every decoded repeated
/// `match` key becomes a distinct selector; other duplicate keys and spoofed
/// synthetic selector keys fail closed, including escaped-key aliases.
pub(super) fn inspect(qdisc: &[u8], filter: &[u8], profile: &Profile) -> Result<State> {
    let qdiscs = decode(qdisc, false)?;
    let filters = decode(filter, true)?;
    let clean = filters.is_empty()
        && (qdiscs.is_empty()
            || (qdiscs.len() == 1
                && qdiscs[0]["root"] == true
                && qdiscs[0]["kind"] == "noqueue"
                && qdiscs[0]["handle"] == "0:"));
    let roots = qdiscs
        .iter()
        .filter(|v| v["root"] == true)
        .collect::<Vec<_>>();
    let root = roots.first().copied();
    let netem = qdiscs
        .iter()
        .find(|v| v["kind"] == "netem" && v["handle"] == "30:" && v["parent"] == "1:3");
    let leaf_count = valid_filters(&filters, netem.is_some(), profile);
    let owned_shape = roots.len() == 1
        && root.is_some_and(|root| {
            let options = &root["options"];
            root["kind"] == "prio"
                && root["handle"] == "1:"
                && only(options, &["bands", "priomap", "multiqueue"])
                && number(&options["bands"], 3.0)
                && options["priomap"].as_array().is_some_and(|values| {
                    values.len() == 16 && values.iter().all(|v| number(v, 0.0))
                })
                && options.get("multiqueue").is_none_or(|value| value == false)
                && netem.is_none_or(|v| valid_netem(&v["options"], profile))
                && qdiscs
                    .iter()
                    .all(|v| std::ptr::eq(v, root) || netem.is_some_and(|n| std::ptr::eq(v, n)))
                && leaf_count.is_some()
        });
    let complete = owned_shape
        && netem.is_some()
        && leaf_count
            == Some(if profile.direction == Direction::Both {
                2
            } else {
                1
            });
    let signature = signature(&qdiscs, &filters)?;
    Ok(State {
        clean,
        owned_shape,
        complete,
        signature,
        evidence: json!({
            "state":if clean {"clean"} else if owned_shape {"fixture-shaped"} else {"foreign"},
            "qdiscs":qdiscs.len(),"filters":filters.len(),"netem":netem.is_some()
        }),
    })
}

fn canonical(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => numeric(
            value
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!(INVALID_STATE))?,
        ),
        Value::String(value) => serde_json::to_string(value)?,
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let entries = keys
                .iter()
                .map(|key| {
                    Ok(format!(
                        "{}:{}",
                        serde_json::to_string(key)?,
                        canonical(&values[*key])?
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            format!("{{{}}}", entries.join(","))
        }
    })
}

fn signature(qdiscs: &[Value], filters: &[Value]) -> Result<String> {
    let project = |values: &[Value], keys: &[&str]| -> Result<String> {
        let mut rows = values
            .iter()
            .map(|value| {
                let projection = keys
                    .iter()
                    .filter_map(|key| {
                        value
                            .get(*key)
                            .map(|value| ((*key).to_owned(), value.clone()))
                    })
                    .collect::<Map<_, _>>();
                canonical(&Value::Object(projection))
            })
            .collect::<Result<Vec<_>>>()?;
        rows.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        Ok(serde_json::to_string(&rows)?)
    };
    let q = project(qdiscs, &["kind", "handle", "root", "parent", "options"])?;
    let f = project(
        filters,
        &["kind", "parent", "protocol", "pref", "chain", "options"],
    )?;
    Ok(hex::encode(Sha256::digest(format!(
        "{{\"qdiscs\":{q},\"filters\":{f}}}"
    ))))
}

#[cfg(test)]
#[path = "netem_tests.rs"]
mod tests;
