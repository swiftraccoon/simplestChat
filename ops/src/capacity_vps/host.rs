//! Fixed private-host policy; every command selector originates here.

use super::{
    Failure, Result,
    control::Owner,
    files::{self, FileId, Fs},
    process::{self, Runner},
    protocol::{self, Header, Unit, UnitState, Workload},
};
use crate::distribution::manifest::{ArtifactManifest, Selection, verify_pair};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::Metadata,
    path::{Path, PathBuf},
};

pub(super) const ROOT: &str = "/srv/simplestchat-bench";
pub(super) const STATE: &str = "/run/simplestchat-bench";
pub(super) const CURRENT: &str = "/run/simplestchat-bench/current.json";
pub(super) const LOCK: &str = "/run/simplestchat-bench/workload.lock";

pub(super) struct Stage {
    pub binary: String,
    pub directory: usize,
    before: Metadata,
    files: [FileId; 3],
    executed: FileId,
}
impl Stage {
    /// Inert action fixtures exercise real file custody, not image authentication.
    #[cfg(test)]
    pub fn fixture(header: &Header, fs: &mut Fs, owner: &mut Owner) -> Result<Self> {
        let path = format!("{ROOT}/artifacts/ops-runtime.{}", header.run);
        let directory = fs.directory(&path, true)?;
        let before = fs.dir_stamp(directory)?;
        let mut files = Vec::new();
        for (name, pin, mode) in [
            ("ops-release.json", &header.members.manifest, 0o600),
            ("ops-selection.json", &header.members.selection, 0o600),
            ("simplestchat-ops", &header.members.binary, 0o755),
        ] {
            let id = fs.open(&format!("{path}/{name}"), pin.bytes, Some(mode))?;
            if fs.hash(id, owner)? != pin.sha256 {
                return Err(Failure::Identity);
            }
            files.push(id);
        }
        let executed = fs.alias(files[2])?;
        let value = Self {
            binary: format!("{path}/simplestchat-ops"),
            directory,
            before,
            files: files.try_into().map_err(|_| Failure::Unproven)?,
            executed,
        };
        value.check(fs, owner)?;
        Ok(value)
    }
    pub fn open(header: &Header, fs: &mut Fs, owner: &mut Owner) -> Result<Self> {
        fs.directory(&format!("{ROOT}/artifacts"), true)?;
        let path = format!("{ROOT}/artifacts/ops-runtime.{}", header.run);
        let directory = fs.directory(&path, true)?;
        let before = fs.dir_stamp(directory)?;
        let mut ids = Vec::new();
        for (name, pin, mode) in [
            ("ops-release.json", &header.members.manifest, 0o600),
            ("ops-selection.json", &header.members.selection, 0o600),
            ("simplestchat-ops", &header.members.binary, 0o755),
        ] {
            let id = fs.open(&format!("{path}/{name}"), pin.bytes, Some(mode))?;
            if fs.stamp(id).len() != pin.bytes || fs.hash(id, owner)? != pin.sha256 {
                return Err(Failure::Identity);
            }
            ids.push(id);
        }
        let executed = fs.executed()?;
        if !files::same(fs.stamp(ids[2]), fs.stamp(executed))
            || fs.hash(executed, owner)? != header.members.binary.sha256
        {
            return Err(Failure::Identity);
        }
        let manifest = ArtifactManifest::decode(&fs.read(ids[0], 64 * 1024, owner)?)
            .map_err(|_| Failure::Identity)?;
        let selection_bytes = fs.read(ids[1], 16 * 1024, owner)?;
        let selection = Selection::decode(&selection_bytes).map_err(|_| Failure::Identity)?;
        let expected = serde_json::to_value(&header.selection).map_err(|_| Failure::Identity)?;
        if serde_json::to_value(selection.identity()).map_err(|_| Failure::Identity)? != expected {
            return Err(Failure::Identity);
        }
        let binary = format!("{path}/simplestchat-ops");
        verify_pair(&manifest, Path::new(&binary), &selection).map_err(|_| Failure::Identity)?;
        let value = Self {
            binary,
            directory,
            before,
            files: ids.try_into().map_err(|_| Failure::Unproven)?,
            executed,
        };
        value.check(fs, owner)?;
        Ok(value)
    }
    pub fn check(&self, fs: &mut Fs, owner: &mut Owner) -> Result<()> {
        owner.check(false)?;
        let names = fs.names(self.directory, 3, owner)?;
        if names != ["ops-release.json", "ops-selection.json", "simplestchat-ops"]
            || !files::same(&self.before, &fs.dir_stamp(self.directory)?)
        {
            return Err(Failure::Changed);
        }
        for id in self.files {
            fs.check(id)?;
        }
        fs.check(self.executed)?;
        if !files::same(fs.stamp(self.files[2]), fs.stamp(self.executed)) {
            return Err(Failure::Changed);
        }
        Ok(())
    }
    pub fn original_metadata(&self, fs: &mut Fs, owner: &mut Owner) -> Result<[Vec<u8>; 2]> {
        Ok([
            fs.read(self.files[0], 64 * 1024, owner)?,
            fs.read(self.files[1], 16 * 1024, owner)?,
        ])
    }
    pub fn retire(&self, header: &Header, fs: &mut Fs, owner: &mut Owner) -> Result<()> {
        self.check(fs, owner)?;
        let mut primary = None;
        let pins = [
            &header.members.manifest,
            &header.members.selection,
            &header.members.binary,
        ];
        let names = ["ops-release.json", "ops-selection.json", "simplestchat-ops"];
        for (index, id) in self.files.into_iter().enumerate() {
            let removed = (|| {
                let expected = self
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| !fs.retired(**id))
                    .map(|(index, _)| names[index].to_owned())
                    .collect::<Vec<_>>();
                if fs.names(self.directory, 3, owner)? != expected
                    || fs.hash(id, owner)? != pins[index].sha256
                {
                    return Err(Failure::Changed);
                }
                if index == 2
                    && (!files::same(fs.stamp(id), fs.stamp(self.executed))
                        || fs.hash(self.executed, owner)? != pins[index].sha256)
                {
                    return Err(Failure::Changed);
                }
                fs.unlink(id)
            })();
            if let Err(error) = removed {
                primary.get_or_insert(error);
                owner.latch(error);
                owner.begin_cleanup();
                // A reported after-effect error can have a proven owned unlink
                // transition. An ambiguous unchanged/foreign name stops effects.
                if !fs.retired(id) {
                    break;
                }
            }
        }
        let removed = if self.files.iter().all(|id| fs.retired(*id)) {
            fs.remove_directory(self.directory, owner)
        } else {
            Err(Failure::Retirement)
        };
        if let Err(error) = removed {
            primary.get_or_insert(error);
            owner.latch(error);
            owner.begin_cleanup();
        }
        for (id, pin) in [
            (self.files[0], &header.members.manifest),
            (self.files[1], &header.members.selection),
            (self.files[2], &header.members.binary),
            (self.executed, &header.members.binary),
        ] {
            let hashed = fs.hash(id, owner).and_then(|hash| {
                if hash == pin.sha256 {
                    Ok(())
                } else {
                    Err(Failure::Changed)
                }
            });
            if let Err(error) = hashed {
                primary.get_or_insert(error);
                owner.latch(error);
                owner.begin_cleanup();
            }
        }
        primary.map_or(Ok(()), Err)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Identity {
    schema_version: u8,
    scope: String,
    run: String,
    selection: protocol::Source,
    artifact_id: u64,
    transfer: protocol::Pin,
    members: protocol::Members,
    workload: protocol::Pin,
}
impl Identity {
    pub fn new(header: &Header, workload: &Workload) -> Self {
        Self {
            schema_version: 1,
            scope: "OpsCapacityRunIdentity".into(),
            run: header.run.clone(),
            selection: header.selection.clone(),
            artifact_id: header.artifact_id,
            transfer: header.transfer.clone(),
            members: header.members.clone(),
            workload: protocol::Pin {
                sha256: files::sha(workload.as_json()),
                bytes: workload.as_json().len() as u64,
            },
        }
    }
    pub fn header(&self) -> Header {
        Header {
            schema_version: 1,
            scope: "OpsCapacityBootstrap".into(),
            mode: protocol::Mode::Invoke,
            run: self.run.clone(),
            selection: self.selection.clone(),
            artifact_id: self.artifact_id,
            transfer: self.transfer.clone(),
            members: self.members.clone(),
            request: self.workload.clone(),
        }
    }
    pub fn bind(&self, header: &Header, request: &[u8]) -> Result<()> {
        if self.schema_version != 1
            || self.scope != "OpsCapacityRunIdentity"
            || self.run != header.run
            || self.selection != header.selection
            || self.artifact_id != header.artifact_id
            || self.transfer != header.transfer
            || self.members != header.members
            || self.workload.bytes != request.len() as u64
            || self.workload.sha256 != files::sha(request)
        {
            return Err(Failure::Identity);
        }
        Ok(())
    }
}

pub(super) struct Session<'a> {
    pub header: &'a Header,
    pub fs: &'a mut Fs,
    pub owner: &'a mut Owner,
    pub runner: &'a mut Runner,
}
impl Session<'_> {
    pub fn command(
        &mut self,
        program: &str,
        args: &[String],
        seconds: u64,
        allow_failure: bool,
        cleanup: bool,
    ) -> Result<Vec<u8>> {
        self.fs.verify()?;
        let output = self.runner.run(
            process::Invocation {
                program: Path::new(program),
                arguments: args,
                environment: &process::environment(),
                seconds,
                maximum: protocol::MAX_RESPONSE,
                cleanup,
            },
            self.owner,
        )?;
        self.fs.verify()?;
        if output.status.ok_or(Failure::Unproven)? != 0 && !allow_failure {
            return Err(Failure::Command);
        }
        let mut bytes = output.stdout;
        bytes.extend(output.stderr);
        if bytes.len() > protocol::MAX_RESPONSE {
            return Err(Failure::Bounds);
        }
        Ok(bytes)
    }
    pub fn docker(
        &mut self,
        args: &[String],
        seconds: u64,
        allow_failure: bool,
        cleanup: bool,
    ) -> Result<Vec<u8>> {
        let mut full = vec!["--host".into(), "unix:///var/run/docker.sock".into()];
        full.extend_from_slice(args);
        self.command("/usr/bin/docker", &full, seconds, allow_failure, cleanup)
    }
    pub fn read(&mut self, path: &str, maximum: usize) -> Result<Vec<u8>> {
        let id = self.fs.open(path, maximum as u64, Some(0o600))?;
        self.fs.read(id, maximum, self.owner)
    }
    pub fn json(&mut self, path: &str) -> Result<Value> {
        let bytes = self.read(path, protocol::MAX_RESPONSE)?;
        crate::json::decode_limited(&bytes, protocol::MAX_RESPONSE)
            .map_err(|_| Failure::Journal)?;
        serde_json::from_slice(&bytes).map_err(|_| Failure::Journal)
    }
    pub fn save(&mut self, path: &str, value: &impl Serialize) -> Result<()> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| Failure::Journal)?;
        bytes.push(b'\n');
        if bytes.len() > protocol::MAX_RESPONSE {
            return Err(Failure::Bounds);
        }
        self.fs.replace(path, &bytes, self.owner)?;
        Ok(())
    }
    pub fn lock(&mut self) -> Result<FileId> {
        if !self.fs.exists(STATE)? {
            self.fs.mkdir(STATE)?;
        }
        self.fs.directory(STATE, true)?;
        let id = if self.fs.exists(LOCK)? {
            self.fs.open(LOCK, 0, Some(0o600))?
        } else {
            self.fs.create(LOCK, &[], self.owner)?
        };
        match rustix::fs::flock(
            self.fs.file(id),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        ) {
            Ok(()) => Ok(id),
            Err(rustix::io::Errno::WOULDBLOCK) => Err(Failure::HostBusy),
            Err(_) => Err(Failure::Io),
        }
    }
    pub fn unlock(&mut self, id: FileId) -> Result<()> {
        let unlocked = rustix::fs::flock(self.fs.file(id), rustix::fs::FlockOperation::Unlock)
            .map_err(|_| Failure::Io);
        let closed = self.fs.close(id);
        unlocked.and(closed)
    }
    pub fn unit(&mut self, selected: Unit, cleanup: bool) -> Result<UnitState> {
        let name = match selected {
            Unit::Build => protocol::BUILD_UNIT.into(),
            Unit::Capacity => self.header.capacity_unit(),
        };
        let bytes = self.command(
            "/usr/bin/systemctl",
            &[
                "show".into(),
                name,
                concat!(
                    "--property=LoadState,ActiveState,SubState,Result,",
                    "ExecMainCode,ExecMainStatus,MainPID"
                )
                .into(),
            ],
            30,
            true,
            cleanup,
        )?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Unit)?;
        let bytes = simplestchat_load_test::capacity::vps::parse_unit_state(text)
            .map_err(|_| Failure::Unit)?;
        let mut state: UnitState = serde_json::from_slice(&bytes).map_err(|_| Failure::Unit)?;
        if selected == Unit::Capacity && state.load_state == "not-found" {
            let (_, workload) = self.saved()?;
            let path = format!("{}/unit-result.json", self.header.evidence_path());
            if self.fs.exists(&path)? {
                state = serde_json::from_value(self.json(&path)?).map_err(|_| Failure::Unit)?;
                if workload.run_id() != self.header.run
                    || !state.terminal()
                    || state.unit.as_deref() != Some(self.header.capacity_unit().as_str())
                {
                    return Err(Failure::Unit);
                }
                state.retained = Some(true);
            }
        }
        state.validate(self.header, selected)?;
        Ok(state)
    }
    pub fn raw_capacity_unit(&mut self) -> Result<UnitState> {
        let bytes = self.command(
            "/usr/bin/systemctl",
            &[
                "show".into(),
                self.header.capacity_unit(),
                concat!(
                    "--property=LoadState,ActiveState,SubState,Result,",
                    "ExecMainCode,ExecMainStatus,MainPID"
                )
                .into(),
            ],
            30,
            true,
            false,
        )?;
        let bytes = simplestchat_load_test::capacity::vps::parse_unit_state(
            std::str::from_utf8(&bytes).map_err(|_| Failure::Unit)?,
        )
        .map_err(|_| Failure::Unit)?;
        let state: UnitState = serde_json::from_slice(&bytes).map_err(|_| Failure::Unit)?;
        state.validate(self.header, Unit::Capacity)?;
        Ok(state)
    }
    pub fn containers(
        &mut self,
        running: bool,
        label: Option<&str>,
        cleanup: bool,
    ) -> Result<Vec<String>> {
        let mut args = vec!["ps".into(), "--quiet".into(), "--no-trunc".into()];
        if !running {
            args.push("--all".into());
        }
        if let Some(label) = label {
            args.extend(["--filter".into(), format!("label={label}")]);
        }
        let bytes = self.docker(&args, 30, false, cleanup)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Ownership)?;
        let mut ids = text.lines().map(str::to_owned).collect::<Vec<_>>();
        if ids.iter().any(|v| !protocol::hex(v, 64)) {
            return Err(Failure::Ownership);
        }
        ids.sort();
        Ok(ids)
    }
    pub fn preflight(&mut self, allowed: Option<&str>) -> Result<protocol::Preflight> {
        if !self.fs.supported_host() {
            return Err(Failure::Platform);
        }
        // Debian's /etc/os-release is normally a symlink. Use the fixed vendor
        // record rather than granting a caller-selected symlink-follow policy.
        let os = self.fs.open("/usr/lib/os-release", 65536, None)?;
        let os = self.fs.read(os, 65536, self.owner)?;
        let os = std::str::from_utf8(&os).map_err(|_| Failure::Platform)?;
        if !os
            .lines()
            .any(|v| matches!(v, "ID=debian" | "ID=\"debian\""))
            || !os.lines().any(|v| {
                v.strip_prefix("VERSION_ID=")
                    .is_some_and(|v| v.trim_matches(['\'', '"']).split('.').next() == Some("13"))
            })
        {
            return Err(Failure::Platform);
        }
        if self
            .fs
            .exists("/etc/simplestchat-public/compose.public.yml")?
        {
            return Err(Failure::HostBusy);
        }
        if !self
            .containers(
                false,
                Some("com.docker.compose.project=simplestchat-public"),
                false,
            )?
            .is_empty()
        {
            return Err(Failure::HostBusy);
        }
        for name in ["simplestchat-benchmark.service", protocol::BUILD_UNIT] {
            let bytes = self.command(
                "/usr/bin/systemctl",
                &["show".into(), name.into(), "--property=ActiveState".into()],
                30,
                true,
                false,
            )?;
            if String::from_utf8_lossy(&bytes).lines().any(|v| {
                matches!(
                    v,
                    "ActiveState=active"
                        | "ActiveState=activating"
                        | "ActiveState=deactivating"
                        | "ActiveState=reloading"
                )
            }) {
                return Err(Failure::HostBusy);
            }
        }
        let active = self.command(
            "/usr/bin/systemctl",
            &[
                "list-units".into(),
                "--all".into(),
                "--no-legend".into(),
                "--plain".into(),
                "--state=active,activating,deactivating,reloading".into(),
                "simplestchat-capacity-*.service".into(),
            ],
            30,
            false,
            false,
        )?;
        if std::str::from_utf8(&active)
            .map_err(|_| Failure::Unit)?
            .lines()
            .any(|line| line.split_whitespace().next() != allowed)
        {
            return Err(Failure::HostBusy);
        }
        if !self.containers(true, None, false)?.is_empty() {
            return Err(Failure::HostBusy);
        }
        if self.fs.exists(CURRENT)? {
            let value = self.json(CURRENT)?;
            if value.get("schemaVersion") != Some(&json!(1))
                || value.get("finalized") != Some(&json!(true))
            {
                return Err(Failure::Journal);
            }
        }
        for name in ["sources", "artifacts", "results"] {
            self.fs.directory(&format!("{ROOT}/{name}"), true)?;
        }
        Ok(protocol::Preflight {
            containers_before: self.containers(false, None, false)?,
            private_host: true,
        })
    }
    pub fn source(&mut self, workload: &Workload) -> Result<PathBuf> {
        let source = PathBuf::from(format!("{ROOT}/sources/{}", workload.revision()));
        self.fs
            .directory(source.to_str().ok_or(Failure::Source)?, false)?;
        let base = vec!["-C".into(), source.to_string_lossy().into_owned()];
        let mut args = base.clone();
        args.extend(["rev-parse".into(), "HEAD".into()]);
        let head = self.command("/usr/bin/git", &args, 30, false, false)?;
        if std::str::from_utf8(&head)
            .map_err(|_| Failure::Source)?
            .trim()
            != workload.revision()
        {
            return Err(Failure::Source);
        }
        let mut args = base;
        args.extend([
            "status".into(),
            "--porcelain=v1".into(),
            "--untracked-files=all".into(),
        ]);
        if !self
            .command("/usr/bin/git", &args, 30, false, false)?
            .is_empty()
        {
            return Err(Failure::Source);
        }
        Ok(source)
    }
    pub fn images(&mut self, workload: &Workload) -> Result<Vec<u8>> {
        let path = format!("{ROOT}/artifacts/{}/images.json", workload.revision());
        let bytes = self.read(&path, protocol::MAX_RESPONSE)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::Image)?;
        if value.get("schemaVersion") != Some(&json!(1))
            || value.get("passed") != Some(&json!(true))
            || value.get("revision").and_then(Value::as_str) != Some(workload.revision())
        {
            return Err(Failure::Image);
        }
        for key in ["serverImage", "generatorImage"] {
            let image = value
                .get(key)
                .and_then(Value::as_str)
                .ok_or(Failure::Image)?;
            if !image
                .strip_prefix("sha256:")
                .is_some_and(|v| protocol::hex(v, 64))
            {
                return Err(Failure::Image);
            }
            let inspected = self.docker(
                &["image".into(), "inspect".into(), image.into()],
                30,
                false,
                false,
            )?;
            let data: Value = serde_json::from_slice(&inspected).map_err(|_| Failure::Image)?;
            let values = data
                .as_array()
                .filter(|v| v.len() == 1)
                .ok_or(Failure::Image)?;
            let row = &values[0];
            if row["Id"].as_str() != Some(image)
                || row["Config"]["User"] != "10001:10001"
                || row["Config"]["Labels"]["org.opencontainers.image.revision"]
                    != workload.revision()
            {
                return Err(Failure::Image);
            }
        }
        Ok(bytes)
    }
    pub fn saved(&mut self) -> Result<(Identity, Workload)> {
        let directory = self.header.evidence_path();
        self.fs.directory(&directory, true)?;
        let bytes = self.read(&format!("{directory}/request.json"), protocol::MAX_REQUEST)?;
        let identity = self.read(&format!("{directory}/ops-identity.json"), 64 * 1024)?;
        crate::json::decode_limited(&identity, 64 * 1024).map_err(|_| Failure::Identity)?;
        let identity: Identity =
            serde_json::from_slice(&identity).map_err(|_| Failure::Identity)?;
        identity.bind(self.header, &bytes)?;
        let workload = Workload::decode(&bytes).map_err(|_| Failure::Request)?;
        if workload.run_id() != self.header.run
            || workload.revision() != self.header.selection.revision
        {
            return Err(Failure::Identity);
        }
        Ok((identity, workload))
    }
    pub fn require_saved(&mut self, workload: &Workload) -> Result<()> {
        let (_, saved) = self.saved()?;
        if saved.as_json() != workload.as_json() {
            return Err(Failure::Identity);
        }
        Ok(())
    }
}
