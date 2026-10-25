//! Passive, closed first-refusal facts for one mandatory application rollback.

use super::super::control::{Cause, Result};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Stage {
    RecoveryAdmission,
    Stop,
    SelectionComposeRead,
    SelectionComposeWrite,
    SelectionEnvRead,
    SelectionEnvWrite,
    SelectionImagesRead,
    SelectionImagesWrite,
    AssetsSelect,
    Up,
    ApplicationInspect,
    ApplicationImage,
    LoopbackReadiness,
    PublicReadiness,
    DatabaseContinuity,
    ProxyContinuity,
    JournalFinalize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Boundary {
    Operation,
    OriginalsBefore,
    RoleCeiling,
    StdoutAdmission,
    StderrAdmission,
    Executor,
    Settlement,
    ExitStatus,
    StderrSeal,
    StdoutSeal,
    OriginalsAfter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum FailureCause {
    Input,
    Policy,
    Archive,
    Changed,
    Io,
    Close,
    Deadline,
    Interrupt,
    Terminate,
    Zip,
    Bounds,
    Command,
}
impl From<Cause> for FailureCause {
    fn from(cause: Cause) -> Self {
        match cause {
            Cause::Input => Self::Input,
            Cause::Policy => Self::Policy,
            Cause::Archive => Self::Archive,
            Cause::Changed => Self::Changed,
            Cause::Io => Self::Io,
            Cause::Close => Self::Close,
            Cause::Deadline => Self::Deadline,
            Cause::Interrupt => Self::Interrupt,
            Cause::Terminate => Self::Terminate,
            Cause::Zip => Self::Zip,
            Cause::Bounds => Self::Bounds,
            Cause::Command => Self::Command,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Failure {
    stage: Stage,
    boundary: Boundary,
    cause: FailureCause,
    log_ordinal: Option<usize>,
}

#[derive(Default)]
pub(super) struct Trace {
    active: Option<Stage>,
    boundary: Option<Boundary>,
    log_ordinal: Option<usize>,
    first: Option<Failure>,
}
impl Trace {
    pub fn start(&mut self, stage: Stage) {
        self.active = Some(stage);
        self.operation();
    }
    pub fn stage(&mut self, stage: Stage) {
        if self.active.is_some() {
            self.start(stage);
        }
    }
    pub fn pause(&mut self) {
        self.active = None;
        self.operation();
    }
    pub fn operation(&mut self) {
        self.boundary = Some(Boundary::Operation);
        self.log_ordinal = None;
    }
    pub fn boundary(&mut self, boundary: Boundary) {
        self.boundary = Some(boundary);
    }
    pub fn logs_admitted(&mut self, ordinal: usize) {
        self.log_ordinal = Some(ordinal);
    }
    pub fn refuse(&mut self, cause: Cause) {
        if self.first.is_none()
            && let Some(stage) = self.active
        {
            self.first = Some(Failure {
                stage,
                boundary: self.boundary.unwrap_or(Boundary::Operation),
                cause: cause.into(),
                log_ordinal: self.log_ordinal,
            });
        }
    }
    pub fn observe<T>(&mut self, result: Result<T>) -> Result<T> {
        result.inspect_err(|cause| self.refuse(*cause))
    }
    pub fn finish(&mut self) -> Option<Failure> {
        self.pause();
        self.first.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ignored_diagnostics_and_success_do_not_mint_failure() {
        let mut trace = Trace::default();
        assert_eq!(
            trace.observe::<()>(Err(Cause::Command)),
            Err(Cause::Command)
        );
        trace.start(Stage::Stop);
        assert_eq!(trace.observe(Ok(())), Ok(()));
        assert_eq!(trace.finish(), None);
    }

    #[test]
    fn first_refusal_survives_later_evidence_and_stage_failures() {
        let mut trace = Trace::default();
        trace.start(Stage::Stop);
        trace.boundary(Boundary::Executor);
        trace.logs_admitted(105);
        assert_eq!(
            trace.observe::<()>(Err(Cause::Command)),
            Err(Cause::Command)
        );
        trace.boundary(Boundary::StderrSeal);
        assert_eq!(trace.observe::<()>(Err(Cause::Close)), Err(Cause::Close));
        trace.stage(Stage::Up);
        trace.refuse(Cause::Deadline);
        assert_eq!(
            serde_json::to_value(trace.finish()).unwrap(),
            json!({"stage":"stop","boundary":"executor","cause":"command","logOrdinal":105})
        );
    }

    #[test]
    fn before_dispatch_has_no_previous_log_ordinal() {
        let mut trace = Trace::default();
        trace.start(Stage::Stop);
        trace.logs_admitted(104);
        trace.operation();
        trace.boundary(Boundary::OriginalsBefore);
        trace.refuse(Cause::Changed);
        assert_eq!(
            serde_json::to_value(trace.finish()).unwrap(),
            json!({"stage":"stop","boundary":"originals-before",
                "cause":"changed","logOrdinal":null})
        );
    }
}
