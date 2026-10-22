//! Decode only the recorder's fixed lifecycle markers and one known native
//! clamp signature. UUIDs remain internal correlation keys, never report fields.

use super::timeline::{Instant, instant};
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

static SGR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").expect("fixed SGR grammar"));
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\S+) +DEBUG +(?:.* )?simplestchat::lifecycle: lifecycle +(.*)$")
        .expect("fixed lifecycle grammar")
});
static CLAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\S+) +ERROR +(?:.* )?mediasoup::worker: (?:\[id:([0-9a-f-]+)\] )?webrtc::GoogCcNetworkController::ClampConstraints\(\) \| start bitrate smaller than min bitrate \[starting_rate_:30000, min_data_rate_:100000\]$").expect("fixed native clamp grammar")
});
static FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^([a-z_]+)=(?:"([^"\\]*)"|([^\s"=]+))(?: +|$)"#)
        .expect("fixed marker field grammar")
});

const TRANSPORT_FIELDS: &[&str] = &["event", "participant_id", "transport_id", "transport_type"];
const STATE_FIELDS: &[&str] = &[
    "event",
    "participant_id",
    "transport_id",
    "transport_type",
    "state",
];
const CLEANUP_FIELDS: &[&str] = &["event", "participant_id", "generation"];
const PARTICIPANT_FIELDS: &[&str] = &["event", "participant_id"];

pub(super) enum Marker {
    Transport {
        event: &'static str,
        owner: String,
        id: String,
        kind: &'static str,
        state: Option<&'static str>,
    },
    Cleanup {
        event: &'static str,
        owner: String,
        generation: String,
    },
    Participant {
        event: &'static str,
        owner: String,
    },
}

pub(super) enum Event {
    Lifecycle(Marker),
    Clamp,
}

/// New TextDecoder on each line strips exactly one leading UTF-8 BOM.
pub(super) fn decode(line: &[u8]) -> Result<String, &'static str> {
    let line = std::str::from_utf8(line).map_err(|_| "invalid_log_encoding")?;
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    Ok(SGR.replace_all(line, "").into_owned())
}

/// True selects lifecycle parsing, false selects the single clamp signature.
/// Irrelevant legacy lines do not acquire the stricter marker-control policy.
pub(super) fn relevant(line: &str) -> Option<bool> {
    if line.contains("simplestchat::lifecycle") {
        Some(true)
    } else if line.contains("start bitrate smaller than min bitrate")
        || line.contains("ClampConstraints()")
    {
        Some(false)
    } else {
        None
    }
}

pub(super) fn has_control(line: &str) -> bool {
    line.chars().any(|value| matches!(value, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

pub(super) fn parse(line: &str, lifecycle: bool) -> Option<(Instant, Event)> {
    if lifecycle {
        let captures = MARKER.captures(line)?;
        let time = instant(&captures[1])?;
        Some((time, Event::Lifecycle(marker_fields(&captures[2])?)))
    } else {
        let captures = CLAMP.captures(line)?;
        let time = instant(&captures[1])?;
        if captures.get(2).is_some_and(|value| !uuid(value.as_str())) {
            return None;
        }
        Some((time, Event::Clamp))
    }
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn marker_fields(mut input: &str) -> Option<Marker> {
    let mut fields = BTreeMap::new();
    while !input.is_empty() {
        let captures = FIELD.captures(input)?;
        let key = captures[1].to_owned();
        let value = captures
            .get(2)
            .or_else(|| captures.get(3))?
            .as_str()
            .to_owned();
        if fields.insert(key, value).is_some() {
            return None;
        }
        input = &input[captures.get(0)?.end()..];
    }
    let owner = fields.get("participant_id")?.clone();
    if !uuid(&owner) {
        return None;
    }
    let event = fields.get("event")?.as_str();
    let (event, keys) = match event {
        "transport_created" => ("transport_created", TRANSPORT_FIELDS),
        "transport_closed" => ("transport_closed", TRANSPORT_FIELDS),
        "transport_ice" => ("transport_ice", STATE_FIELDS),
        "transport_dtls" => ("transport_dtls", STATE_FIELDS),
        "media_cleanup_started" => ("media_cleanup_started", CLEANUP_FIELDS),
        "media_cleanup_finished" => ("media_cleanup_finished", CLEANUP_FIELDS),
        "explicit_leave_started" => ("explicit_leave_started", PARTICIPANT_FIELDS),
        "explicit_leave_finished" => ("explicit_leave_finished", PARTICIPANT_FIELDS),
        "grace_started" => ("grace_started", PARTICIPANT_FIELDS),
        "grace_cleanup_started" => ("grace_cleanup_started", PARTICIPANT_FIELDS),
        _ => return None,
    };
    if fields.len() != keys.len() || !keys.iter().all(|key| fields.contains_key(*key)) {
        return None;
    }
    if let Some(id) = fields.get("transport_id") {
        if !uuid(id) {
            return None;
        }
        let kind = match fields.get("transport_type")?.as_str() {
            "send" => "send",
            "recv" => "recv",
            _ => return None,
        };
        let state = if event == "transport_ice" {
            Some(match fields.get("state")?.as_str() {
                "New" => "New",
                "Connected" => "Connected",
                "Completed" => "Completed",
                "Disconnected" => "Disconnected",
                _ => return None,
            })
        } else if event == "transport_dtls" {
            Some(match fields.get("state")?.as_str() {
                "New" => "New",
                "Connecting" => "Connecting",
                "Connected" => "Connected",
                "Failed" => "Failed",
                "Closed" => "Closed",
                _ => return None,
            })
        } else {
            None
        };
        Some(Marker::Transport {
            event,
            owner,
            id: id.clone(),
            kind,
            state,
        })
    } else if let Some(generation) = fields.get("generation") {
        if !uuid(generation) {
            return None;
        }
        Some(Marker::Cleanup {
            event,
            owner,
            generation: generation.clone(),
        })
    } else {
        Some(Marker::Participant { event, owner })
    }
}
