//! Canonical service handoff and retained collection/recovery policy.

use super::{
    Failure, Result, files,
    host::{CURRENT, Identity, LOCK, Session, Stage},
    protocol::{self, Action, Collect, Outcome, ResultData, Unit, Workload},
};
use serde_json::{Value, json};
use simplestchat_load_test::capacity::vps;

impl Session<'_> {
    pub fn build(&mut self, workload: &Workload) -> Result<ResultData> {
        let lock = self.lock()?;
        let body = (|| {
            self.preflight(None)?;
            let source = self.source(workload)?;
            let launcher = "/usr/local/libexec/simplestchat-bench/build-images";
            let id = self.fs.open(launcher, 1024 * 1024, None)?;
            let bytes = self.fs.read(id, 1024 * 1024, self.owner)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Source)?;
            if !text.contains(&format!("revision='{}'", workload.revision()))
                || !text.contains(&format!("source_root='{}'", source.display()))
            {
                return Err(Failure::Source);
            }
            let id = self.fs.open(
                "/etc/systemd/system/simplestchat-image-build.service",
                1024 * 1024,
                None,
            )?;
            let installed = self.fs.read(id, 1024 * 1024, self.owner)?;
            let id = self.fs.open(
                source
                    .join("ops/ansible/templates/image-build.service.j2")
                    .to_str()
                    .ok_or(Failure::Source)?,
                1024 * 1024,
                None,
            )?;
            if installed != self.fs.read(id, 1024 * 1024, self.owner)? {
                return Err(Failure::Source);
            }
            Ok(())
        })();
        let unlocked = self.unlock(lock);
        body?;
        unlocked?;
        self.command(
            "/usr/bin/systemctl",
            &["start".into(), protocol::BUILD_UNIT.into()],
            60,
            false,
            false,
        )?;
        Ok(ResultData::Build(protocol::Build {
            unit: protocol::BUILD_UNIT.into(),
        }))
    }
    pub fn start(&mut self, workload: &Workload, stage: &Stage) -> Result<ResultData> {
        let lock = self.lock()?;
        let directory = self.header.evidence_path();
        let body = (|| {
            self.preflight(None)?;
            self.source(workload)?;
            self.images(workload)?;
            self.fs.mkdir(&directory)?;
            self.fs.create(
                &format!("{directory}/request.json"),
                workload.as_json(),
                self.owner,
            )?;
            let identity = Identity::new(self.header, workload);
            let bytes = serde_json::to_vec(&identity).map_err(|_| Failure::Identity)?;
            if bytes.len() > 64 * 1024 {
                return Err(Failure::Bounds);
            }
            self.fs.create(
                &format!("{directory}/ops-identity.json"),
                &bytes,
                self.owner,
            )?;
            let metadata = stage.original_metadata(self.fs, self.owner)?;
            self.fs.create(
                &format!("{directory}/ops-release.json"),
                &metadata[0],
                self.owner,
            )?;
            self.fs.create(
                &format!("{directory}/ops-selection.json"),
                &metadata[1],
                self.owner,
            )?;
            let unit = self.header.capacity_unit();
            let args = vec![
                "--quiet".into(),
                "--unit".into(),
                unit,
                "--property=Type=exec".into(),
                "--property=RemainAfterExit=yes".into(),
                format!("--property=RuntimeMaxSec={}", workload.runtime_seconds()),
                "--property=TimeoutStopSec=600".into(),
                "--property=KillMode=mixed".into(),
                "--property=Restart=no".into(),
                "--property=UMask=0077".into(),
                "--property=LimitCORE=0".into(),
                "--property=Slice=system.slice".into(),
                "--setenv=PATH=/usr/sbin:/usr/bin:/sbin:/bin".into(),
                "--setenv=LC_ALL=C".into(),
                "--setenv=HOME=/root".into(),
                "/usr/bin/flock".into(),
                "--nonblock".into(),
                "--no-fork".into(),
                LOCK.into(),
                stage.binary.clone(),
                "capacity-vps".into(),
                "worker".into(),
                "--run".into(),
                self.header.run.clone(),
                "--request-sha256".into(),
                files::sha(workload.as_json()),
            ];
            self.save(
                &format!("{directory}/unit-command.json"),
                &json!({
                    "argv": std::iter::once("/usr/bin/systemd-run".to_owned())
                        .chain(args.iter().cloned())
                        .collect::<Vec<_>>()
                }),
            )?;
            stage.check(self.fs, self.owner)?;
            Ok(args)
        })();
        let unlocked = self.unlock(lock);
        let args = body?;
        unlocked?;
        // A lost/failed start response leaves the exclusive request and original
        // identity intact. No route retries this handoff or creates another run.
        self.command("/usr/bin/systemd-run", &args, 60, false, false)?;
        Ok(ResultData::Start(protocol::Start {
            unit: self.header.capacity_unit(),
            evidence: directory,
            run: self.header.run.clone(),
        }))
    }
    pub fn residuals(&mut self, before: Vec<String>, cleanup: bool) -> Result<protocol::Residuals> {
        let after = self.containers(false, None, cleanup)?;
        let ownership = format!("{}/workload/ownership.json", self.header.evidence_path());
        let mut snapshot = vps::Snapshot {
            before,
            after,
            ..vps::Snapshot::default()
        };
        if self.fs.exists(&ownership)? {
            let bytes = self.read(&ownership, protocol::MAX_RESPONSE)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::Ownership)?;
            let run = value
                .get("runId")
                .and_then(Value::as_str)
                .ok_or(Failure::Ownership)?;
            if !protocol::hex(run, 16) {
                return Err(Failure::Ownership);
            }
            snapshot.owned = self.containers(
                false,
                Some(&format!("simplestchat.capacity.run={run}")),
                cleanup,
            )?;
            snapshot.journal = Some(bytes);
        }
        let selected = snapshot
            .after
            .iter()
            .filter(|id| !snapshot.before.contains(id))
            .chain(snapshot.owned.iter())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for id in selected {
            let inspected = self.docker(&["inspect".into(), id.clone()], 30, false, cleanup)?;
            snapshot.inspections.insert(id, inspected);
        }
        let value = vps::residuals(&snapshot).map_err(|_| Failure::Ownership)?;
        serde_json::from_slice(&value.to_json()).map_err(|_| Failure::Ownership)
    }
    pub(super) fn before(&mut self, required: bool) -> Result<Vec<String>> {
        let path = format!("{}/containers-before.json", self.header.evidence_path());
        if !self.fs.exists(&path)? {
            return if required {
                Err(Failure::Journal)
            } else {
                Ok(Vec::new())
            };
        }
        let value = self.json(&path)?;
        let values = value
            .get("ids")
            .and_then(Value::as_array)
            .ok_or(Failure::Journal)?;
        values
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|v| protocol::hex(v, 64))
                    .map(str::to_owned)
                    .ok_or(Failure::Journal)
            })
            .collect()
    }
    pub fn collect(&mut self, workload: &Workload) -> Result<ResultData> {
        self.require_saved(workload)?;
        let directory = self.header.evidence_path();
        let mut state = self.unit(Unit::Capacity, false)?;
        if !state.terminal() || state.main_pid != "0" {
            return Err(Failure::Unit);
        }
        state.unit = Some(self.header.capacity_unit());
        self.save(&format!("{directory}/unit-result.json"), &state)?;
        let journal = self.command(
            "/usr/bin/journalctl",
            &[
                "--unit".into(),
                self.header.capacity_unit(),
                "--no-pager".into(),
                "--lines=2000".into(),
                "--output=short-iso".into(),
            ],
            30,
            false,
            false,
        )?;
        self.fs
            .replace(&format!("{directory}/unit.log"), &journal, self.owner)?;
        let lock = self.lock()?;
        let cleanup = (|| {
            let before = self.before(false)?;
            self.residuals(before, false)
        })();
        let unlocked = self.unlock(lock);
        let cleanup = cleanup?;
        unlocked?;
        self.save(&format!("{directory}/collection-cleanup.json"), &cleanup)?;
        let path = format!("{directory}/outcome.json");
        let original = if self.fs.exists(&path)? {
            self.read(&path, protocol::MAX_RESPONSE)?
        } else {
            br#"{"passed":false,"finalized":false,"failureClass":"worker_outcome_missing"}"#
                .to_vec()
        };
        let outcome = Outcome::decode(&original).map_err(|_| Failure::Collection)?;
        outcome
            .validate_workload(workload)
            .map_err(|_| Failure::Collection)?;
        if outcome.revision().is_some_and(|v| v != workload.revision()) {
            return Err(Failure::Identity);
        }
        let result = Collect {
            passed: outcome.passed() && outcome.finalized() && cleanup.clean && state.successful(),
            unit: state.clone(),
            outcome_json: String::from_utf8(original).map_err(|_| Failure::Collection)?,
            cleanup,
            evidence: directory.clone(),
            revision: workload.revision().into(),
            request_json: std::str::from_utf8(workload.as_json())
                .map_err(|_| Failure::Request)?
                .into(),
        };
        // Validate the closed collection before persisting a private positive
        // record, as well as again at the final response publication fence.
        protocol::Response::encode(self.header, &ResultData::Collect(result.clone()))?;
        let control = serde_json::to_vec(&result).map_err(|_| Failure::Collection)?;
        let private = vps::project_collection(&control, workload, &outcome)
            .map_err(|_| Failure::Collection)?;
        self.fs.replace(
            &format!("{directory}/collection.json"),
            &private,
            self.owner,
        )?;
        if state.active_state == "active" && state.retained != Some(true) {
            self.command(
                "/usr/bin/systemctl",
                &["stop".into(), self.header.capacity_unit()],
                30,
                false,
                false,
            )?;
        }
        Ok(ResultData::Collect(result))
    }
    pub fn recover(&mut self, workload: &Workload) -> Result<ResultData> {
        self.require_saved(workload)?;
        if self
            .fs
            .exists("/etc/simplestchat-public/compose.public.yml")?
        {
            return Err(Failure::HostBusy);
        }
        let state = self.unit(Unit::Capacity, false)?;
        if !state.terminal() || state.main_pid != "0" {
            return Err(Failure::Unit);
        }
        let lock = self.lock()?;
        let mut receipt = protocol::Recovery {
            run: self.header.run.clone(),
            removed: Vec::new(),
            finalized: false,
            before: None,
            after: None,
        };
        let body = (|| {
            let before = self.before(true)?;
            let initial = self.residuals(before.clone(), false)?;
            receipt.before = Some(initial.clone());
            if initial.residuals.len() > 6 {
                return Err(Failure::Bounds);
            }
            let mut candidates = initial.residuals.clone();
            candidates.sort_by_key(|v| !v.inspection.name.starts_with("capacity-gen-"));
            for candidate in candidates {
                if !candidate.confirmed_owned {
                    continue;
                }
                let current = self.residuals(before.clone(), false)?;
                if !current
                    .residuals
                    .iter()
                    .any(|v| v.id == candidate.id && v.confirmed_owned)
                {
                    return Err(Failure::Ownership);
                }
                self.docker(
                    &[
                        "stop".into(),
                        "--time".into(),
                        "20".into(),
                        candidate.id.clone(),
                    ],
                    30,
                    false,
                    false,
                )?;
                // Recheck after stop too; do not carry a destructive capability
                // forward from a stale pre-stop snapshot.
                let current = self.residuals(before.clone(), false)?;
                if !current
                    .residuals
                    .iter()
                    .any(|v| v.id == candidate.id && v.confirmed_owned)
                {
                    return Err(Failure::Ownership);
                }
                self.docker(
                    &["rm".into(), "--volumes".into(), candidate.id.clone()],
                    30,
                    false,
                    false,
                )?;
                receipt.removed.push(candidate.id);
            }
            let after = self.residuals(before, false)?;
            let mut same_journal = false;
            if after.clean && self.fs.exists(CURRENT)? {
                let mut journal = self.json(CURRENT)?;
                if journal.get("run").and_then(Value::as_str) != Some(self.header.run.as_str()) {
                    return Err(Failure::Journal);
                }
                let object = journal.as_object_mut().ok_or(Failure::Journal)?;
                object.insert("finalized".into(), json!(true));
                object.insert("passed".into(), json!(false));
                object.insert("recovered".into(), json!(true));
                self.save(CURRENT, &journal)?;
                same_journal = true;
            }
            receipt.finalized = after.clean && same_journal;
            receipt.after = Some(after);
            Ok(())
        })();
        // Partial removals are durable even after a later failure. The primary
        // operation status wins over every independent save/unlock close error.
        if let Err(error) = &body {
            self.owner.latch(*error);
            self.owner.begin_cleanup();
        }
        let saved = self.save(
            &format!("{}/recovery.json", self.header.evidence_path()),
            &receipt,
        );
        let unlocked = self.unlock(lock);
        body?;
        saved?;
        unlocked?;
        Ok(ResultData::Recover(receipt))
    }
    pub fn lookup(&mut self) -> Result<ResultData> {
        let (_, workload) = self.saved()?;
        let state = self.unit(Unit::Capacity, false)?;
        Ok(ResultData::Lookup(protocol::Lookup {
            request_json: std::str::from_utf8(workload.as_json())
                .map_err(|_| Failure::Request)?
                .into(),
            request_sha256: files::sha(workload.as_json()),
            unit: state,
        }))
    }
    pub fn dispatch(
        &mut self,
        action: Action,
        workload: Option<&Workload>,
        stage: &Stage,
    ) -> Result<ResultData> {
        match action {
            Action::Lookup => self.lookup(),
            Action::Preflight => {
                let lock = self.lock()?;
                let value = self.preflight(None);
                let unlocked = self.unlock(lock);
                let value = value?;
                unlocked?;
                Ok(ResultData::Preflight(value))
            }
            Action::Build => self.build(workload.ok_or(Failure::Request)?),
            Action::Start => self.start(workload.ok_or(Failure::Request)?, stage),
            Action::Collect => self.collect(workload.ok_or(Failure::Request)?),
            Action::Recover => self.recover(workload.ok_or(Failure::Request)?),
            _ => Err(Failure::Arguments),
        }
    }
}
