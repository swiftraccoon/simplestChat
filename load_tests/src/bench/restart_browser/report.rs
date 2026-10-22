//! Closed, bounded Node observation projection. No URLs, text, frame bodies or
//! dependency error messages can cross the terminal receipt boundary.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const SAFE: u64 = 9_007_199_254_740_991;
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
#[derive(Debug, Serialize, Default)]
#[serde(transparent)]
struct Counts(BTreeMap<String, u64>);
impl<'de> Deserialize<'de> for Counts {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Counts;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("counts")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Counts, M::Error> {
                let mut result = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, u64>()? {
                    if result.len() >= 64
                        || key.is_empty()
                        || key.len() > 64
                        || !key.bytes().all(|v| v.is_ascii_alphabetic())
                        || value > SAFE
                        || result.insert(key, value).is_some()
                    {
                        return Err(serde::de::Error::custom("invalid restart counters"));
                    }
                }
                Ok(Counts(result))
            }
        }
        deserializer.deserialize_map(V)
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Page {
    sockets: u64,
    navigations: u64,
    received: Counts,
    sent: Counts,
    malformed_frames: u64,
}
impl Page {
    fn extends(&self, previous: &Self) -> bool {
        self.sockets >= previous.sockets
            && self.navigations >= previous.navigations
            && self.malformed_frames >= previous.malformed_frames
            && previous
                .received
                .0
                .iter()
                .all(|(key, count)| self.received.0.get(key).is_some_and(|v| v >= count))
            && previous
                .sent
                .0
                .iter()
                .all(|(key, count)| self.sent.0.get(key).is_some_and(|v| v >= count))
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    user: u64,
    display: u64,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    before: bool,
    after: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Observation {
    browser: String,
    phase: String,
    pages: Vec<Page>,
    #[serde(deserialize_with = "present")]
    recovery_pages: Option<Vec<Page>>,
    capture_calls: Vec<Capture>,
    page_errors: u64,
    blocked_external_requests: u64,
    #[serde(deserialize_with = "present")]
    browser_version: Option<String>,
    #[serde(deserialize_with = "present")]
    rejoin_milliseconds: Option<u64>,
    #[serde(deserialize_with = "present")]
    draft_preserved: Option<bool>,
    #[serde(deserialize_with = "present")]
    message_delivery: Option<Delivery>,
    #[serde(deserialize_with = "present")]
    navigation_counts_before_restart: Option<Vec<u64>>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Browser {
    browser_name: String,
    launch_mode: String,
    completed: bool,
    passed: bool,
    graceful_close_completed: bool,
    force_kill_requested: bool,
    exit_status_observed: bool,
    #[serde(deserialize_with = "present")]
    exit_code: Option<u64>,
    #[serde(deserialize_with = "present")]
    signal_code: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Receipt {
    schema_version: u8,
    run_id: String,
    completed: bool,
    phase_passed: bool,
    browser_cleanup_passed: bool,
    #[serde(deserialize_with = "present")]
    cancelled: Option<u8>,
    browser_count: u8,
    records: Vec<Browser>,
    observation: Observation,
}
impl Receipt {
    pub fn read(bytes: &[u8], run: &str, engine: &str) -> Result<Self> {
        ensure!(
            bytes.len() as u64 <= super::config::RECEIPT,
            "restart_receipt_limit"
        );
        let value: Self = serde_json::from_slice(bytes)?;
        let o = &value.observation;
        ensure!(
            value.schema_version == 1
                && value.run_id == run
                && o.browser == engine
                && value.records.len() <= 1
                && usize::from(value.browser_count) == value.records.len()
                && value.cancelled.is_none_or(|v| matches!(v, 130 | 143))
                && o.pages.len() <= 2
                && o.recovery_pages.as_ref().is_none_or(|pages| {
                    pages.len() == 2
                        && o.pages.len() == 2
                        && o.pages
                            .iter()
                            .zip(pages)
                            .all(|(after, before)| after.extends(before))
                })
                && o.capture_calls.len() <= 2
                && matches!(
                    o.phase.as_str(),
                    "setup" | "before_restart" | "restart" | "after_restart" | "complete"
                )
                && o.page_errors <= SAFE
                && o.blocked_external_requests <= SAFE
                && o.rejoin_milliseconds.is_none_or(|v| v <= SAFE)
                && o.browser_version
                    .as_ref()
                    .is_none_or(|v| v.len() <= 256 && !v.chars().any(char::is_control))
                && o.navigation_counts_before_restart
                    .as_ref()
                    .is_none_or(|v| v.len() == 2 && v.iter().all(|n| *n <= SAFE))
                && o.pages
                    .iter()
                    .chain(o.recovery_pages.iter().flatten())
                    .all(|p| {
                        p.sockets <= SAFE && p.navigations <= SAFE && p.malformed_frames <= SAFE
                    })
                && o.capture_calls
                    .iter()
                    .all(|p| p.user <= SAFE && p.display <= SAFE)
                && value.records.iter().all(|r| r.browser_name == engine
                    && r.launch_mode == "browser-server"
                    && r.exit_code.is_none_or(|n| n <= SAFE)
                    && r.signal_code
                        .as_ref()
                        .is_none_or(|v| v.strip_prefix("SIG").is_some_and(|v| !v.is_empty()
                            && v.len() <= 16
                            && v.bytes()
                                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())))),
            "restart_receipt_invalid"
        );
        Ok(value)
    }
    pub fn passed(&self) -> bool {
        let o = &self.observation;
        self.completed
            && self.phase_passed
            && self.browser_cleanup()
            && self.cancelled.is_none()
            && o.phase == "complete"
            && o.pages.len() == 2
            && o.capture_calls.len() == 2
            && o.pages.iter().all(|p| {
                p.received.0.get("roomJoined") == Some(&2)
                    && p.received.0.get("roomClosed").copied().unwrap_or(0) == 0
                    && p.malformed_frames == 0
            })
            && o.page_errors == 0
            && o.blocked_external_requests == 0
            && o.browser_version.is_some()
            && o.rejoin_milliseconds.is_some()
            && o.capture_calls
                .iter()
                .all(|p| p.user == 0 && p.display == 0)
            && o.draft_preserved == Some(true)
            && o.message_delivery
                .as_ref()
                .is_some_and(|v| v.before && v.after)
            && o.recovery_pages.as_ref().is_some_and(|pages| {
                pages.iter().all(|p| {
                    p.received.0.get("roomJoined") == Some(&2)
                        && p.received.0.get("serverRestarting") == Some(&1)
                        && p.received.0.get("roomClosed").copied().unwrap_or(0) == 0
                        && p.malformed_frames == 0
                }) && o
                    .navigation_counts_before_restart
                    .as_ref()
                    .is_some_and(|before| {
                        pages
                            .iter()
                            .map(|p| p.navigations)
                            .eq(before.iter().copied())
                    })
            })
    }
    pub fn browser_cleanup(&self) -> bool {
        self.browser_cleanup_passed
            && self.browser_count == 1
            && self.records.iter().all(|r| {
                r.completed
                    && r.passed
                    && r.graceful_close_completed
                    && !r.force_kill_requested
                    && r.exit_status_observed
                    && r.exit_code == Some(0)
                    && r.signal_code.is_none()
            })
    }
    pub fn observation(&self) -> serde_json::Value {
        serde_json::to_value(&self.observation).unwrap()
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
