//! Closed, best-effort diagnostics with no source names or operation authority.
//! The elapsed value is the original owner's last validated sample, which may
//! lag inside a borrowed stage. Reporting never reads or advances its clock.
//! The byte cap is not a wall-time bound on synchronous stderr/OS writes.
//!
//! Affected API guidance: C-CUSTOM-TYPE/C-GOOD-ERR keep labels closed and omit
//! body error text; C-RW-VALUE borrows the mature Write interface without taking
//! descriptor or close ownership. Reporting grants no completion authority.

use super::{fetch::Timeout, plan::Plan};
use crate::tooling::Result;
use std::io::Write;

pub(super) const MAX_BYTES: usize = 16 * 1024;
const TERMINAL_RESERVE: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Source,
    Download,
    Registry,
    Kit,
    Producer,
    Assembly,
    Finalization,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Download => "download",
            Self::Registry => "registry",
            Self::Kit => "kit",
            Self::Producer => "producer",
            Self::Assembly => "assembly",
            Self::Finalization => "finalization",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Checkpoint {
    Import,
    Resolve,
    Probe,
    NativeBuild,
    NativeCommand,
    NativeOutputs,
    RegistryCopy,
    Measure,
}
impl Checkpoint {
    fn name(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Resolve => "resolve",
            Self::Probe => "probe",
            Self::NativeBuild => "native-build",
            Self::NativeCommand => "native-command",
            Self::NativeOutputs => "native-outputs",
            Self::RegistryCopy => "registry-copy",
            Self::Measure => "measure",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Edge {
    Begin,
    Complete,
}
impl Edge {
    fn name(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Complete => "complete",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum MaterialKind {
    Catalog,
    Registry,
}
impl MaterialKind {
    fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Registry => "registry",
        }
    }
}

pub(super) struct Progress<'a> {
    owner: &'a Plan,
    sink: &'a mut dyn Write,
    attempted: usize,
    phase: Phase,
    started: bool,
    finished: bool,
    refused: Option<(Phase, u8)>,
    material_reported: bool,
    transport_reported: bool,
}
impl<'a> Progress<'a> {
    pub fn new(owner: &'a Plan, sink: &'a mut dyn Write) -> Self {
        Self {
            owner,
            sink,
            attempted: 0,
            phase: Phase::Source,
            started: false,
            finished: false,
            refused: None,
            material_reported: false,
            transport_reported: false,
        }
    }
    pub fn owner(&self) -> &'a Plan {
        self.owner
    }
    fn emit(&mut self, line: String, terminal: bool) {
        let maximum = if terminal {
            MAX_BYTES
        } else {
            MAX_BYTES - TERMINAL_RESERVE
        };
        if self.finished
            || line.len() > TERMINAL_RESERVE
            || self
                .attempted
                .checked_add(line.len())
                .is_none_or(|bytes| bytes > maximum)
        {
            return;
        }
        // Count attempted bytes before I/O. Partial writes and errors cannot
        // spend the ceiling again or influence any body/finalizer result.
        self.attempted += line.len();
        let _ = self.sink.write_all(line.as_bytes());
    }
    pub fn enter(&mut self, phase: Phase) {
        if self.finished || self.started && self.phase == phase {
            return;
        }
        self.phase = phase;
        self.started = true;
        self.emit(
            format!(
                "ops-hosted phase={} event=begin ownerObservedElapsedMs={}\n",
                phase.name(),
                self.owner.observed_elapsed().as_millis()
            ),
            false,
        );
    }
    pub fn complete(&mut self) {
        self.emit(
            format!(
                "ops-hosted phase={} event=complete ownerObservedElapsedMs={}\n",
                self.phase.name(),
                self.owner.observed_elapsed().as_millis()
            ),
            false,
        );
    }
    pub fn kit(&mut self, checkpoint: Checkpoint, edge: Edge, ordinal: usize, total: usize) {
        // Import rosters can contain 96 archives. Keep both edges of the first,
        // every eighth and last item so later probes/builds retain visibility
        // inside the same lifetime byte ceiling. Sampling grants no authority.
        if matches!(checkpoint, Checkpoint::Import)
            && ordinal != 1
            && !ordinal.is_multiple_of(8)
            && ordinal != total
        {
            return;
        }
        self.emit(
            format!(
                concat!(
                    "ops-hosted phase=kit checkpoint={} event={} ordinal={} total={} ",
                    "ownerObservedElapsedMs={}\n"
                ),
                checkpoint.name(),
                edge.name(),
                ordinal,
                total,
                self.owner.observed_elapsed().as_millis()
            ),
            false,
        );
    }
    /// Report only the first refused material, without exposing its identity or
    /// changing its result. Successful downloads spend no diagnostic bytes.
    pub fn material_refused<T>(
        &mut self,
        kind: MaterialKind,
        ordinal: usize,
        total: usize,
        result: &Result<T>,
    ) {
        let Err(error) = result else {
            return;
        };
        if self.finished || self.material_reported {
            return;
        }
        self.material_reported = true;
        self.refused(result);
        self.emit(
            format!(
                concat!(
                    "ops-hosted phase=download event=material-refused kind={} ",
                    "ordinal={} total={} status={} ownerObservedElapsedMs={}\n"
                ),
                kind.name(),
                ordinal,
                total,
                error.code,
                self.owner.observed_elapsed().as_millis()
            ),
            false,
        );
    }
    /// The fetch owner supplies this passive label only after its complete
    /// paired captures and original failed-download owner have settled.
    pub fn transport_timeout(
        &mut self,
        kind: MaterialKind,
        ordinal: usize,
        total: usize,
        timeout: Option<Timeout>,
    ) {
        let Some(timeout) = timeout else {
            return;
        };
        if self.finished || self.transport_reported || !self.material_reported {
            return;
        }
        self.transport_reported = true;
        self.emit(
            format!(
                concat!(
                    "ops-hosted phase=download event=transport-timeout kind={} ",
                    "ordinal={} total={} category={} ownerObservedElapsedMs={}\n"
                ),
                kind.name(),
                ordinal,
                total,
                timeout.name(),
                self.owner.observed_elapsed().as_millis()
            ),
            false,
        );
    }
    /// Remember a body refusal without emitting a premature terminal result.
    pub fn refused<T>(&mut self, result: &Result<T>) {
        if self.refused.is_none()
            && let Err(error) = result
        {
            self.refused = Some((self.phase, error.code));
        }
    }
    /// Call only after existing owned finalizers and completion/replay fences.
    /// The reporter receives a result by reference and can never replace it.
    pub fn finish(&mut self, result: &Result<()>) {
        if self.finished {
            return;
        }
        self.refused(result);
        self.enter(Phase::Finalization);
        let status = result.as_ref().err().map_or(0, |error| error.code);
        let refused = self
            .refused
            .filter(|(_, code)| *code == status)
            .map_or("none", |(phase, _)| phase.name());
        self.emit(
            format!(
                concat!(
                    "ops-hosted phase=finalization event=finished status={} ",
                    "refusedPhase={} ownerObservedElapsedMs={}\n"
                ),
                status,
                refused,
                self.owner.observed_elapsed().as_millis()
            ),
            true,
        );
        self.finished = true;
    }
    #[cfg(test)]
    pub fn finished(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
