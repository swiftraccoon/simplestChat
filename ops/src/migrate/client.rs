//! A scripted stand-in for the database client, shared by the replays: it
//! answers each group of statements as a recording says and keeps what it
//! was asked.

use crate::host::dialogue::{Dialogue, Heard, PIECE};
use crate::host::failure::{Failure, Result};
use crate::host::tests::Fixed;
use crate::host::{Clock, hexadecimal};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::time::Duration;

pub(super) fn number(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

/// The command line and the environment one client was started with.
pub(super) type Started = (Vec<String>, Vec<(String, String)>);

/// What scripted clients were started with and told.
#[derive(Default)]
pub(super) struct Seen {
    pub started: Vec<Started>,
    pub asked: Vec<String>,
    pub farewell: Option<String>,
    pub markers: BTreeSet<String>,
}

/// A recorded text: literal, or one run repeated.
pub(super) fn expanded(part: &Value) -> String {
    match part {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().map(expanded).collect(),
        part if part["repeat"].is_string() => {
            let count = usize::try_from(part["count"].as_u64().unwrap()).unwrap();
            part["repeat"].as_str().unwrap().repeat(count)
        }
        part => part["text"].as_str().unwrap().to_owned(),
    }
}

/// The database client of one recorded scenario.
pub(super) struct Client<'a> {
    scenario: &'a Value,
    clock: &'a Fixed,
    seen: &'a RefCell<Seen>,
    waiting: Vec<u8>,
    pieces: usize,
    answered: usize,
    /// Whether the output ends once what is waiting has been read.
    closing: bool,
    exited: bool,
}

impl<'a> Client<'a> {
    /// The client of `scenario`, whose `answers` it gives in order.
    pub(super) fn new(scenario: &'a Value, clock: &'a Fixed, seen: &'a RefCell<Seen>) -> Self {
        Self {
            scenario,
            clock,
            seen,
            waiting: Vec::new(),
            pieces: PIECE,
            answered: 0,
            closing: false,
            exited: scenario["exitsAtOnce"] == true,
        }
    }
}

impl Dialogue for Client<'_> {
    fn send(&mut self, payload: &[u8], within: Duration) -> Result<()> {
        assert!(!within.is_zero() && within <= Duration::from_secs(40));
        if self.exited {
            return Err(Failure::Command);
        }
        let text = std::str::from_utf8(payload).unwrap();
        let (_, marker) = text
            .trim_end_matches('\n')
            .rsplit_once("\n\\echo ")
            .unwrap();
        let random = marker.strip_prefix("SNAPSHOT_END_").unwrap();
        assert!(hexadecimal(random, 48), "{marker}");
        let mut seen = self.seen.borrow_mut();
        seen.markers.insert(marker.to_owned());
        seen.asked.push(text.replace(marker, "<MARKER>"));
        let answers = self.scenario["answers"].as_array().unwrap();
        let fallback = serde_json::json!({"parts": []});
        let answer = answers.get(self.answered).unwrap_or(&fallback);
        self.answered += 1;
        let mut said = expanded(&answer["parts"]);
        match answer["end"].as_str().unwrap_or("marker") {
            "marker" => said += &format!("{marker}\n"),
            "trailing" => said += &format!("{marker}\nlate\n"),
            "silent" => {}
            "close" => (self.closing, self.exited) = (true, true),
            "orphan" => self.exited = true,
            other => panic!("unexpected recorded ending {other}"),
        }
        if answer["leaves"] == true {
            (self.closing, self.exited) = (true, true);
        }
        self.pieces = answer.get("pieces").map_or(PIECE, number);
        self.waiting.extend_from_slice(said.as_bytes());
        Ok(())
    }

    fn hear(&mut self, within: Duration) -> Result<Heard> {
        assert!(!within.is_zero() && within <= Duration::from_secs(1));
        if self.waiting.is_empty() {
            if self.closing {
                return Ok(Heard::Closed);
            }
            // Nothing comes: the wait takes all the time it was given.
            self.clock.sleep(within);
            return Ok(Heard::Quiet);
        }
        let size = self.pieces.min(PIECE).min(self.waiting.len());
        Ok(Heard::Bytes(self.waiting.drain(..size).collect()))
    }

    fn alive(&mut self) -> bool {
        !self.exited
    }

    fn end(self: Box<Self>, farewell: &[u8]) {
        if !self.exited {
            let farewell = String::from_utf8(farewell.to_vec()).unwrap();
            self.seen.borrow_mut().farewell = Some(farewell);
        }
    }
}
