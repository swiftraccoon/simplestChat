//! Durable handoff intent and the existing private report shape. Retained
//! extended-number payloads are projected through the established capacity codec.

use super::{Result, failure};
use serde_json::{Value, json};
use simplestchat_ops::capacity_vps::protocol::{Outcome, Workload, project_controller_report};

pub(super) struct Report {
    value: Value,
    request: Vec<u8>,
    outcome: Option<Vec<u8>>,
}
impl Report {
    pub fn new(
        revision: &str,
        run: &str,
        host: &str,
        evidence: &std::path::Path,
        request: Vec<u8>,
    ) -> Self {
        Self {
            value: json!({"schemaVersion":1,"passed":false,"revision":revision,"run":run,
            "inventoryHost":host,"evidence":evidence,"remoteOutcome":"not_started"}),
            request,
            outcome: None,
        }
    }
    pub fn set(&mut self, name: &str, value: Value) {
        self.value[name] = value;
    }
    pub fn adopt(&mut self, bytes: Vec<u8>) {
        self.request = bytes;
    }
    pub fn collected(&mut self, result: Value, outcome: Vec<u8>) {
        self.value["result"] = result;
        self.outcome = Some(outcome);
    }
    pub fn fail(&mut self, code: u8) {
        self.value["passed"] = false.into();
        self.value["failureClass"] = match code {
            124 => "deadline",
            130 => "interrupted",
            143 => "terminated",
            _ => "unproven",
        }
        .into();
    }
    pub fn passed(&self) -> bool {
        self.value["passed"] == true
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        let request = Workload::decode(&self.request).map_err(|_| failure(1))?;
        let outcome = self
            .outcome
            .as_deref()
            .map(Outcome::decode)
            .transpose()
            .map_err(|_| failure(1))?;
        let base = serde_json::to_vec(&self.value).map_err(|_| failure(1))?;
        let mut bytes =
            project_controller_report(&base, &request, outcome.as_ref()).map_err(|_| failure(1))?;
        if bytes.len() as u64 >= super::MAX_JSON {
            return Err(failure(1));
        }
        bytes.push(b'\n');
        Ok(bytes)
    }
}
