//! The single fixed request, READY/GO and closed result wire; no interpreter.

use super::{Error, Result, control::Control, sources};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, fs::File, io, os::fd::AsFd, time::Duration};

pub(super) const MAX_RECORD: usize = 131072;
pub(super) const CADDY_IMAGE: &str = concat!(
    "ghcr.io/swiftraccoon/simplestchat-caddy:",
    "2.11.7-73c81fdbfe625d3df3989ba807064c81fbe2c071",
    "@sha256:23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0"
);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pin {
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    pub schema_version: u64,
    pub scope: String,
    pub run_id: String,
    pub revision: String,
    pub scenario: String,
    pub setup_seconds: u64,
    pub body_seconds: u64,
    pub end_seconds: u64,
    pub binary: Pin,
    pub manifest: Pin,
    pub archive: Pin,
    pub source_bundle: Pin,
    pub source_tree: String,
    pub source_pins: BTreeMap<String, String>,
    pub helper_pins: BTreeMap<String, String>,
    pub postgres_image: String,
    pub caddy_image: String,
    pub engine_version: String,
}
pub(super) fn digest(value: &str, count: usize) -> bool {
    value.len() == count
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn decode(bytes: &[u8]) -> Result<Value> {
    if bytes.is_empty() || bytes.len() > MAX_RECORD {
        return Err(Error::Bounds);
    }
    // The existing strict decoder rejects decoded duplicate keys, nonfinite values,
    // trailing records and excessive nesting before serde constructs typed fields.
    crate::json::decode(bytes).map_err(|_| Error::Input)?;
    serde_json::from_slice(bytes).map_err(|_| Error::Input)
}
pub(super) fn exact(value: &Value, names: &[&str]) -> Result<()> {
    let object = value.as_object().ok_or(Error::Input)?;
    if object.len() != names.len() || names.iter().any(|name| !object.contains_key(*name)) {
        return Err(Error::Input);
    }
    Ok(())
}
impl Request {
    pub fn is_deploy(&self) -> bool {
        self.scope == "NativeHostDeployRequest"
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::decode_for(bytes, false)
    }
    pub fn decode_host(bytes: &[u8]) -> Result<Self> {
        Self::decode_for(bytes, true)
    }
    fn decode_for(bytes: &[u8], host: bool) -> Result<Self> {
        let value = decode(bytes)?;
        let request: Self = serde_json::from_value(value).map_err(|_| Error::Input)?;
        let image = |value: &str| {
            value.rsplit_once("@sha256:").is_some_and(|(prefix, hash)| {
                !prefix.is_empty()
                    && prefix.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"./:-".contains(&b)
                    })
                    && digest(hash, 64)
            })
        };
        let version = |value: &str| {
            let parts = value.split('.').collect::<Vec<_>>();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        };
        if request.schema_version != 1
            || request.scope
                != if host {
                    "PreparedHostMaintenanceRequest"
                } else {
                    "MaintainLauncherPrerequisiteRequest"
                }
            || !(if host {
                &[
                    "host-success",
                    "host-launcher-refusal",
                    "host-migration-upgrade",
                    "host-proxy-upgrade",
                ][..]
            } else {
                &[
                    "positive",
                    "dirty-source",
                    "cancel-owned-launcher",
                    "timeout-stopped-launcher",
                ][..]
            })
            .contains(&request.scenario.as_str())
            || !digest(&request.run_id, 32)
            || !digest(&request.revision, 40)
            || !digest(&request.source_tree, 40)
            || !(1..=if host { 1560 } else { 510 }).contains(&request.setup_seconds)
            || !(if host { 2040..=3660 } else { 1170..=1680 }).contains(&request.body_seconds)
            || !(if host { 2040..=3780 } else { 1170..=1800 }).contains(&request.end_seconds)
            || request.end_seconds <= request.body_seconds
            || request.end_seconds - request.body_seconds > 120
            || !image(&request.postgres_image)
            || request.caddy_image != CADDY_IMAGE
            || !version(&request.engine_version)
        {
            return Err(Error::Input);
        }
        for (pin, maximum) in [
            (&request.binary, 256 * 1024 * 1024),
            (&request.manifest, 131072),
            (&request.archive, 2 * 1024 * 1024 * 1024),
            (&request.source_bundle, 320 * 1024 * 1024),
        ] {
            if !digest(&pin.sha256, 64) || pin.bytes == 0 || pin.bytes > maximum {
                return Err(Error::Input);
            }
        }
        if request.source_pins.keys().cloned().collect::<Vec<_>>() != sources::guest_names()
            || request.helper_pins.keys().cloned().collect::<Vec<_>>() != sources::helper_names()
            || request
                .source_pins
                .values()
                .chain(request.helper_pins.values())
                .any(|v| !digest(v, 64))
        {
            return Err(Error::Input);
        }
        Ok(request)
    }
    pub fn is_host(&self) -> bool {
        self.scope == "PreparedHostMaintenanceRequest"
    }
    pub fn host_success(&self) -> bool {
        self.is_host()
            && matches!(
                self.scenario.as_str(),
                "host-success" | "host-migration-upgrade" | "host-proxy-upgrade"
            )
    }
    pub fn migration_upgrade(&self) -> bool {
        self.is_host() && self.scenario == "host-migration-upgrade"
    }
    pub fn proxy_upgrade(&self) -> bool {
        self.is_host() && self.scenario == "host-proxy-upgrade"
    }
    /// The new host profile first establishes the same positive cold baseline.
    pub fn native_scenario(&self) -> &str {
        if self.is_host() {
            "positive"
        } else {
            &self.scenario
        }
    }
    pub fn trial(&self) -> &'static str {
        match self.scenario.as_str() {
            "cancel-owned-launcher" => "cancel-owned-launcher",
            "timeout-stopped-launcher" => "timeout-stopped-launcher",
            _ => "normal",
        }
    }
}

pub(super) struct Wire {
    input: Option<File>,
    output: Option<File>,
    input_flags: Option<rustix::fs::OFlags>,
    output_flags: Option<rustix::fs::OFlags>,
    buffer: Vec<u8>,
}
impl Wire {
    #[cfg(test)]
    pub(super) fn fixture(input: File, output: File) -> Self {
        Self {
            input: Some(input),
            output: Some(output),
            input_flags: None,
            output_flags: None,
            buffer: Vec::new(),
        }
    }
    pub fn new(control: &mut Control) -> Result<Self> {
        control.check()?;
        let mut wire = Self {
            input: None,
            output: None,
            input_flags: None,
            output_flags: None,
            buffer: Vec::new(),
        };
        let result = (|| {
            wire.input = Some(File::from(
                rustix::io::dup(std::io::stdin().as_fd()).map_err(|_| Error::Io)?,
            ));
            wire.output = Some(File::from(
                rustix::io::dup(std::io::stdout().as_fd()).map_err(|_| Error::Io)?,
            ));
            for (file, flags) in [
                (&wire.input, &mut wire.input_flags),
                (&wire.output, &mut wire.output_flags),
            ] {
                let file = file.as_ref().ok_or(Error::Close)?;
                rustix::io::fcntl_setfd(file, rustix::io::FdFlags::CLOEXEC)
                    .map_err(|_| Error::Io)?;
                let original = rustix::fs::fcntl_getfl(file).map_err(|_| Error::Io)?;
                *flags = Some(original);
                rustix::fs::fcntl_setfl(file, original | rustix::fs::OFlags::NONBLOCK)
                    .map_err(|_| Error::Io)?;
            }
            control.check()
        })();
        if let Err(error) = result {
            control.record(error);
            let _ = wire.close(control);
            return Err(error);
        }
        Ok(wire)
    }
    pub fn line(&mut self, control: &mut Control) -> Result<Vec<u8>> {
        self.line_bounded(control, MAX_RECORD)
    }
    pub fn line_bounded(&mut self, control: &mut Control, maximum: usize) -> Result<Vec<u8>> {
        if maximum == 0 || maximum > MAX_RECORD {
            return Err(Error::Input);
        }
        loop {
            control.check()?;
            if let Some(at) = self.buffer.iter().position(|b| *b == b'\n') {
                let rest = self.buffer.split_off(at + 1);
                let mut line = std::mem::replace(&mut self.buffer, rest);
                line.pop();
                if line.len() + 1 > maximum {
                    return Err(Error::Bounds);
                }
                return Ok(line);
            }
            if self.buffer.len() >= maximum {
                return Err(Error::Bounds);
            }
            let mut bytes = [0u8; 8192];
            let size = bytes.len().min(maximum + 1 - self.buffer.len());
            match rustix::io::read(self.input.as_ref().ok_or(Error::Close)?, &mut bytes[..size]) {
                Ok(0) => return Err(Error::Refused),
                Ok(count) => self.buffer.extend_from_slice(&bytes[..count]),
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => return Err(Error::Io),
            }
            control.check()?;
            if self.buffer.len() > maximum {
                return Err(Error::Bounds);
            }
        }
    }
    pub fn no_extra(&mut self, control: &mut Control) -> Result<()> {
        control.check()?;
        if !self.buffer.is_empty() {
            return Err(Error::Refused);
        }
        let mut byte = [0u8; 1];
        match rustix::io::read(self.input.as_ref().ok_or(Error::Close)?, &mut byte) {
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => control.check(),
            Ok(_) => Err(Error::Refused),
            Err(_) => Err(Error::Io),
        }
    }
    pub fn emit(&mut self, value: &Value, control: &mut Control) -> Result<()> {
        self.emit_bounded(value, control, MAX_RECORD)
    }
    pub fn emit_bounded(
        &mut self,
        value: &Value,
        control: &mut Control,
        maximum: usize,
    ) -> Result<()> {
        if maximum == 0 || maximum > MAX_RECORD {
            return Err(Error::Input);
        }
        control.check()?;
        let mut bytes = serde_json::to_vec(value).map_err(|_| Error::Input)?;
        bytes.push(b'\n');
        if bytes.len() > maximum {
            return Err(Error::Bounds);
        }
        let mut at = 0;
        while at < bytes.len() {
            control.check()?;
            match rustix::io::write(self.output.as_ref().ok_or(Error::Close)?, &bytes[at..]) {
                Ok(0) => return Err(Error::Io),
                Ok(count) => at += count,
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => return Err(Error::Io),
            }
            control.check()?;
        }
        Ok(())
    }
    pub fn close(&mut self, control: &mut Control) -> Result<()> {
        let mut first = None;
        for (file, flags) in [
            (&mut self.input, &mut self.input_flags),
            (&mut self.output, &mut self.output_flags),
        ] {
            if let Some(file) = file.take() {
                if let Some(flags) = flags.take()
                    && rustix::fs::fcntl_setfl(&file, flags).is_err()
                {
                    first.get_or_insert(Error::Close);
                }
                if nix::unistd::close(file).is_err() {
                    first.get_or_insert(Error::Close);
                }
            }
        }
        if let Some(error) = first {
            control.cleanup_failed = true;
            control.record(error);
            return Err(error);
        }
        control.check()
    }
}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Self::Io
    }
}
