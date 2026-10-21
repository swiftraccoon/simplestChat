//! The shared local-run identity and exact successful matrix receipt.

use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct Identity {
    pub(crate) run_id: String,
    pub(crate) revision: String,
    pub(crate) base: String,
}

impl Identity {
    pub(crate) fn matches(&self, value: &Value) -> bool {
        value["runId"] == self.run_id
            && value["revision"] == self.revision
            && value["base"] == self.base
    }

    pub(crate) fn matrix_receipt(&self, identifier: &str) -> Value {
        json!({"schema": 1, "status": "passed", "check": identifier,
            "runId": self.run_id, "revision": self.revision, "base": self.base})
    }

    /// Python's original dict equality accepts `true` and `1.0` as schema 1.
    /// Keep that acceptance while requiring every receipt field exactly.
    pub(crate) fn matches_matrix(&self, value: &Value, identifier: &str) -> bool {
        value.as_object().is_some_and(|object| object.len() == 6)
            && successful(value)
            && value["check"] == identifier
            && self.matches(value)
    }
}

pub(crate) fn successful(value: &Value) -> bool {
    (value["schema"] == true || value["schema"].as_f64() == Some(1.0))
        && value["status"] == "passed"
}
