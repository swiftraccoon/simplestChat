//! Retirement is a proved lifecycle transition, never a caller assertion.

use super::{
    Failure, Result, files,
    host::{CURRENT, Session, Stage},
    protocol::{self, Outcome, ResultData, Unit, Workload},
};
use serde_json::{Value, json};

impl Session<'_> {
    fn cgroup_empty(&mut self) -> Result<()> {
        let unit = self.header.capacity_unit();
        let bytes = self.command(
            "/usr/bin/systemctl",
            &[
                "show".into(),
                unit.clone(),
                "--property=ControlGroup,Slice".into(),
            ],
            30,
            true,
            false,
        )?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Unit)?;
        let expected = format!("/system.slice/{unit}");
        let mut group = None;
        let mut slice = None;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("ControlGroup=") {
                group = Some(value);
            }
            if let Some(value) = line.strip_prefix("Slice=") {
                slice = Some(value);
                if !matches!(value, "" | "system.slice") {
                    return Err(Failure::Unit);
                }
            }
        }
        let group = group.ok_or(Failure::Unit)?;
        if slice.is_none() || (!group.is_empty() && group != expected) {
            return Err(Failure::Unit);
        }
        let path = format!("/sys/fs/cgroup{expected}/cgroup.procs");
        if !self.fs.exists(&path)? {
            return Ok(());
        }
        // This is changing trusted kernel data, not an immutable regular input;
        // st_size is normally zero even when tasks are present.
        let bytes = self.fs.kernel_record(&path, 65536, self.owner)?;
        if bytes.iter().any(|v| !v.is_ascii_whitespace()) {
            Err(Failure::Unit)
        } else {
            Ok(())
        }
    }
    pub fn retire(&mut self, workload: &Workload, stage: &Stage) -> Result<ResultData> {
        let lock = self.lock()?;
        let body = (|| {
            let directory = self.header.evidence_path();
            let lifecycle = if !self.fs.exists(&directory)? {
                let state = self.raw_capacity_unit()?;
                if state.load_state != "not-found" || state.main_pid != "0" {
                    return Err(Failure::Retirement);
                }
                // Exclusive absence of the run evidence plus not-found unit
                // precedes every possible start handoff in this protocol.
                if self.fs.exists(CURRENT)? {
                    let journal = self.json(CURRENT)?;
                    if journal.get("run").and_then(Value::as_str) == Some(self.header.run.as_str())
                    {
                        return Err(Failure::Retirement);
                    }
                }
                protocol::RetirementPhase::Prestart
            } else {
                self.require_saved(workload)?;
                let state = self.unit(Unit::Capacity, false)?;
                if !state.terminal() || state.main_pid != "0" {
                    return Err(Failure::Retirement);
                }
                self.cgroup_empty()?;
                self.fs.open(
                    &format!("{directory}/collection.json"),
                    protocol::MAX_RESPONSE as u64,
                    Some(0o600),
                )?;
                let journal = self.json(CURRENT)?;
                if journal.get("schemaVersion").and_then(Value::as_u64) != Some(1)
                    || journal.get("operation").and_then(Value::as_str) != Some("capacity")
                    || journal.get("run").and_then(Value::as_str) != Some(self.header.run.as_str())
                    || journal.get("finalized") != Some(&json!(true))
                {
                    return Err(Failure::Retirement);
                }
                let before = self.before(true)?;
                let fresh = self.residuals(before.clone(), false)?;
                if !fresh.clean {
                    return Err(Failure::Retirement);
                }
                let mut cleanup_proven = false;
                let path = format!("{directory}/outcome.json");
                if self.fs.exists(&path)? {
                    let bytes = self.read(&path, protocol::MAX_RESPONSE)?;
                    let outcome = Outcome::decode(&bytes).map_err(|_| Failure::Collection)?;
                    outcome
                        .validate_workload(workload)
                        .map_err(|_| Failure::Collection)?;
                    if let Some(bytes) = outcome.cleanup_json() {
                        let cleanup: protocol::Residuals =
                            serde_json::from_slice(&bytes).map_err(|_| Failure::Ownership)?;
                        cleanup.validate()?;
                        cleanup_proven = outcome.finalized()
                            && cleanup.clean
                            && cleanup.containers_before == before;
                    }
                }
                let path = format!("{directory}/recovery.json");
                if !cleanup_proven && self.fs.exists(&path)? {
                    let receipt: protocol::Recovery = serde_json::from_value(self.json(&path)?)
                        .map_err(|_| Failure::Ownership)?;
                    protocol::Response::encode(self.header, &ResultData::Recover(receipt.clone()))?;
                    cleanup_proven = receipt.run == self.header.run
                        && receipt.finalized
                        && receipt
                            .before
                            .as_ref()
                            .is_some_and(|v| v.containers_before == before)
                        && receipt
                            .after
                            .as_ref()
                            .is_some_and(|v| v.clean && v.containers_before == before);
                }
                if !cleanup_proven {
                    return Err(Failure::Retirement);
                }
                protocol::RetirementPhase::Terminal
            };
            stage.retire(self.header, self.fs, self.owner)?;
            Ok(ResultData::Retire(protocol::Retirement {
                retired: true,
                lifecycle,
                request_sha256: files::sha(workload.as_json()),
            }))
        })();
        let unlocked = self.unlock(lock);
        let result = body?;
        unlocked?;
        Ok(result)
    }
}
