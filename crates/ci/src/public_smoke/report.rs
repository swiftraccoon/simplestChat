//! The established public projection; private wire/body observations stay local.
use super::io::{Failure, Result, fail};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) const CHECKS: [&str; 10] = [
    "health",
    "ready",
    "homepage",
    "securityHeaders",
    "staticAssets",
    "privateEndpoints",
    "guestJoins",
    "textAcknowledgedAndDelivered",
    "firstLeaveObserved",
    "webSocketCleanup",
];
pub(super) fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
pub(super) fn trim(value: &str) -> &str {
    value.trim_matches(whitespace)
}
pub(super) struct State {
    pub origin: String,
    pub room: String,
    pub run_id: String,
    pub started_at: String,
    pub checks: BTreeMap<&'static str, bool>,
    pub joined: usize,
    pub sent: usize,
    pub leaves: usize,
    pub closed: usize,
    pub completed: bool,
    pub failure: Option<(&'static str, Failure)>,
}
impl State {
    pub fn new(origin: String, room: String, run_id: String, started_at: String) -> Self {
        Self {
            origin,
            room,
            run_id,
            started_at,
            checks: CHECKS.map(|k| (k, false)).into(),
            joined: 0,
            sent: 0,
            leaves: 0,
            closed: 0,
            completed: false,
            failure: None,
        }
    }
    pub fn failed(&mut self, stage: &'static str, error: Failure) {
        self.failure.get_or_insert((stage, error));
    }
    pub fn passed(&self) -> bool {
        self.completed
            && self.failure.is_none()
            && self.joined == 2
            && self.sent == 1
            && self.leaves == 2
            && self.closed == 2
            && self.checks.values().all(|v| *v)
    }
    pub fn summary(&self, elapsed: f64) -> Value {
        json!({"schemaVersion":1,"origin":self.origin,"room":self.room,"runId":self.run_id,
        "startedAt":self.started_at,"completed":self.completed,"passed":self.passed(),
        "durationMs":(elapsed.max(0.0)*1000.0).round() as u64,"checks":self.checks,
        "guestsJoined":self.joined,"publicMessagesSent":self.sent,
        "leaveRequestsSent":self.leaves,"cleanWebSocketClosures":self.closed,
        "failure":self.failure.map(|(stage,e)|json!({"stage":stage,"code":e.code})),
        "limitations":[
            "No camera, microphone, media, browser rendering, passkeys, TURN, or capacity checks.",
            "Native WebSockets do not establish browser Origin-policy enforcement.",
            concat!("The protocol has no leave acknowledgment; only the first leave is ",
                "independently observed by the second guest.")
        ]})
    }
}
pub(super) fn assets(html: &str) -> Result<Vec<(String, &'static str)>> {
    let tags = Regex::new(r"<(?i-u:script|link)(?-u:\b)[^>]*>").unwrap();
    let script = Regex::new(r"^<(?i-u:script)(?-u:\b)").unwrap();
    let mut result = Vec::new();
    for tag in tags.find_iter(html) {
        let attr = |name: &str| {
            let space = concat!(
                r"[\u0009-\u000d\u0020\u00a0\u1680\u2000-\u200a",
                r"\u2028\u2029\u202f\u205f\u3000\ufeff]"
            );
            let pattern = format!(
                concat!(
                    r#"(?-u:\b)(?i-u:{name}){space}*={space}*(?:"([^"\r\n\u2028\u2029]*)""#,
                    r#"|'([^'\r\n\u2028\u2029]*)')"#
                ),
                name = name,
                space = space
            );
            Regex::new(&pattern)
                .unwrap()
                .captures(tag.as_str())
                .map(|c| c.get(1).or_else(|| c.get(2)).unwrap().as_str().to_owned())
        };
        let is_script = script.is_match(tag.as_str());
        if if is_script {
            attr("type").as_deref() != Some("module")
        } else {
            attr("rel").as_deref() != Some("stylesheet")
        } {
            continue;
        }
        let kind = if is_script { "js" } else { "css" };
        let path = attr(if is_script { "src" } else { "href" })
            .ok_or_else(|| fail(1, "unsafe_asset_path"))?;
        if !Regex::new(&format!(r"^/assets/[A-Za-z0-9_.-]+\.{kind}$"))
            .unwrap()
            .is_match(&path)
        {
            return Err(fail(1, "unsafe_asset_path"));
        }
        if result.iter().any(|(p, _)| *p == path) {
            return Err(fail(1, "duplicate_asset"));
        }
        result.push((path, kind));
    }
    if result.len() > 6
        || !result.iter().any(|(_, k)| *k == "js")
        || !result.iter().any(|(_, k)| *k == "css")
    {
        return Err(fail(1, "missing_or_excess_assets"));
    }
    Ok(result)
}
pub(super) fn security(headers: &BTreeMap<String, String>) -> bool {
    let get = |name: &str| headers.get(name).map_or("", String::as_str);
    let mut directives = BTreeMap::new();
    for directive in get("content-security-policy").split(';') {
        let mut words = trim(directive).split(whitespace).filter(|s| !s.is_empty());
        let name = words.next().unwrap_or("");
        if directives
            .insert(name, words.collect::<Vec<_>>().join(" "))
            .is_some()
        {
            return false;
        }
    }
    let space =
        r"[\u0009-\u000d\u0020\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000\ufeff]";
    let hsts = Regex::new(&format!(
        r"(?:^|;){space}*(?i-u:max-age)=([0-9]+)(?:{space}*;|{space}*$)"
    ))
    .unwrap();
    hsts.captures(get("strict-transport-security"))
        .is_some_and(|v| v[1].parse::<f64>().is_ok_and(|n| n >= 86400.0))
        && get("x-content-type-options") == "nosniff"
        && get("x-frame-options").eq_ignore_ascii_case("DENY")
        && get("referrer-policy") == "no-referrer"
        && get("cross-origin-opener-policy") == "same-origin"
        && get("cross-origin-resource-policy") == "same-origin"
        && get("permissions-policy")
            == "camera=(self), microphone=(self), display-capture=(self), geolocation=()"
        && ["default-src", "script-src", "connect-src", "form-action"]
            .iter()
            .all(|k| directives.get(k).is_some_and(|v| v == "'self'"))
        && ["base-uri", "object-src", "frame-ancestors"]
            .iter()
            .all(|k| directives.get(k).is_some_and(|v| v == "'none'"))
}
