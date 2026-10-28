//! Closed frames and the same absolute guest monotonic interval in both roles.
use super::{Cause, FINAL, MAX_FRAME, Result, Trial, WHOLE};
#[cfg(target_os = "linux")]
use super::{FRAME, KNOWN, Owner, Window, fail};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

const SECOND: u64 = 1_000_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Clock {
    pub origin: u64,
    pub body: u64,
    pub end: u64,
}
impl Clock {
    pub fn parse(origin: &str, body: &str, end: &str) -> Result<Self> {
        let value = Self::fields(origin, body, end)?;
        value.validate()?;
        Ok(value)
    }
    fn fields(origin: &str, body: &str, end: &str) -> Result<Self> {
        let number = |value: &str| {
            if value.is_empty()
                || value.len() > 20
                || value.starts_with('0')
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Cause::Input);
            }
            value.parse::<u64>().map_err(|_| Cause::Input)
        };
        Ok(Self {
            origin: number(origin)?,
            body: number(body)?,
            end: number(end)?,
        })
    }
    pub fn new(origin: u64) -> Result<Self> {
        let end = origin.checked_add(WHOLE * SECOND).ok_or(Cause::Input)?;
        Ok(Self {
            origin,
            body: end - FINAL * SECOND,
            end,
        })
    }
    fn validate(self) -> Result<()> {
        if self.origin == 0
            || self.end.checked_sub(self.origin) != Some(WHOLE * SECOND)
            || self.end.checked_sub(self.body) != Some(FINAL * SECOND)
        {
            return Err(Cause::Input);
        }
        Ok(())
    }
    pub fn parse_inherited(origin: &str, body: &str, end: &str) -> Result<Self> {
        let value = Self::fields(origin, body, end)?;
        value.validate_inherited()?;
        Ok(value)
    }
    fn validate_inherited(self) -> Result<()> {
        if self.origin == 0
            || self.body <= self.origin
            || self.end.checked_sub(self.body) != Some(FINAL * SECOND)
            || self
                .end
                .checked_sub(self.origin)
                .is_none_or(|whole| whole > 1800 * SECOND)
        {
            return Err(Cause::Input);
        }
        Ok(())
    }
    /// Convert the parent's original interval conservatively. Sampling kernel
    /// time first places every exported ceiling no later than its Instant bound.
    #[cfg(target_os = "linux")]
    pub fn inherited(owner: &mut Owner, body: Instant, end: Instant) -> Result<Self> {
        owner.check(Window::Body)?;
        let current = now()?;
        let instant = Instant::now();
        let nanos = |value: Duration| u64::try_from(value.as_nanos()).map_err(|_| Cause::Input);
        let elapsed = nanos(
            instant
                .checked_duration_since(owner.start())
                .ok_or(Cause::Input)?,
        )?;
        let value = Self {
            origin: current.checked_sub(elapsed).ok_or(Cause::Input)?,
            body: current
                .checked_add(nanos(
                    body.checked_duration_since(instant)
                        .ok_or(Cause::Deadline)?,
                )?)
                .ok_or(Cause::Input)?,
            end: current
                .checked_add(nanos(
                    end.checked_duration_since(instant).ok_or(Cause::Deadline)?,
                )?)
                .ok_or(Cause::Input)?,
        };
        value.validate_inherited()?;
        owner.check(Window::Body)?;
        Ok(value)
    }
    #[cfg(target_os = "linux")]
    pub fn owner(self) -> Result<Owner> {
        self.validate()?;
        self.role_owner()
    }
    #[cfg(target_os = "linux")]
    pub fn inherited_owner(self) -> Result<Owner> {
        self.validate_inherited()?;
        self.role_owner()
    }
    #[cfg(target_os = "linux")]
    fn role_owner(self) -> Result<Owner> {
        // Sampling Instant before CLOCK_MONOTONIC makes the converted ceiling
        // conservative. This backdates one owner; spawn and GO never restart it.
        let instant = Instant::now();
        let current = now()?;
        let elapsed = current.checked_sub(self.origin).ok_or(Cause::Input)?;
        if current >= self.body {
            return Err(Cause::Deadline);
        }
        let start = instant
            .checked_sub(Duration::from_nanos(elapsed))
            .ok_or(Cause::Input)?;
        Owner::new(
            start,
            Duration::from_nanos(self.end.checked_sub(self.origin).ok_or(Cause::Input)?),
            Duration::from_nanos(self.end.checked_sub(self.body).ok_or(Cause::Input)?),
        )
    }
    #[cfg(target_os = "linux")]
    pub fn require_work(self, owner: &mut Owner) -> Result<()> {
        owner.check(Window::Body)?;
        if now()?
            .checked_add(KNOWN * SECOND)
            .is_none_or(|v| v >= self.body)
        {
            return fail(owner, Cause::Deadline);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub fn phase(self, owner: &mut Owner) -> Result<Instant> {
        owner.check(Window::Body)?;
        Ok((owner.now() + Duration::from_secs(FRAME)).min(owner.body_end()))
    }
}
#[cfg(target_os = "linux")]
pub(super) fn now() -> Result<u64> {
    let value = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let seconds = u64::try_from(value.tv_sec).map_err(|_| Cause::Io)?;
    let nanos = u64::try_from(value.tv_nsec).map_err(|_| Cause::Io)?;
    if nanos >= SECOND {
        return Err(Cause::Io);
    }
    seconds
        .checked_mul(SECOND)
        .and_then(|n| n.checked_add(nanos))
        .ok_or(Cause::Io)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Kind {
    Ready,
    Go,
    Stopped,
    Settled,
    Result,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Phase {
    Admission,
    Sources,
    Ready,
    Go,
    Launcher,
    Client,
    Stopped,
    Settle,
    Result,
    Complete,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Failure {
    Input,
    Policy,
    Changed,
    Io,
    Close,
    Deadline,
    Interrupt,
    Terminate,
    Bounds,
    Command,
}
impl From<Cause> for Failure {
    fn from(value: Cause) -> Self {
        match value {
            Cause::Input => Self::Input,
            Cause::Policy | Cause::Archive | Cause::Zip => Self::Policy,
            Cause::Changed => Self::Changed,
            Cause::Io => Self::Io,
            Cause::Close => Self::Close,
            Cause::Deadline => Self::Deadline,
            Cause::Interrupt => Self::Interrupt,
            Cause::Terminate => Self::Terminate,
            Cause::Bounds => Self::Bounds,
            Cause::Command => Self::Command,
        }
    }
}
impl From<Failure> for Cause {
    fn from(value: Failure) -> Self {
        match value {
            Failure::Input => Self::Input,
            Failure::Policy => Self::Policy,
            Failure::Changed => Self::Changed,
            Failure::Io => Self::Io,
            Failure::Close => Self::Close,
            Failure::Deadline => Self::Deadline,
            Failure::Interrupt => Self::Interrupt,
            Failure::Terminate => Self::Terminate,
            Failure::Bounds => Self::Bounds,
            Failure::Command => Self::Command,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Facts {
    pub namespace_mapped: bool,
    pub pidfd_mapped: bool,
    pub credentials_matched: bool,
    pub private_go: bool,
    pub deadlines_inherited: bool,
    pub launcher_started: bool,
    pub nested_client_observed: bool,
    pub launcher_stopped: bool,
    pub term_seen: bool,
    pub work_cap_expired: bool,
    pub leader_reaped: bool,
    pub adopted_reaped: u32,
    pub echild: bool,
    pub adopted_raw_statuses: Vec<i32>,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub stdout_closed: bool,
    pub stderr_closed: bool,
    pub capabilities_closed: bool,
    pub tools_unchanged: bool,
    pub wrapper_reaped: bool,
    pub wrapper_natural_exit: bool,
    #[serde(deserialize_with = "required_option")]
    pub wrapper_raw_status: Option<i32>,
    #[serde(deserialize_with = "required_option")]
    pub launcher_raw_status: Option<i32>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_sha256: Option<String>,
    pub stderr_sha256: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub launcher_work_elapsed_ns: Option<u64>,
    pub ops_sha256: Option<String>,
    pub unshare_sha256: Option<String>,
    pub launcher_sha256: Option<String>,
    pub timeout_sha256: Option<String>,
    pub init_control_flags_restored: bool,
    pub run_control_flags_restored: bool,
    pub chld_registration_closed: bool,
    pub wrapper_stdout_eof: bool,
    pub wrapper_stderr_eof: bool,
    pub wrapper_stdout_closed: bool,
    pub wrapper_stderr_closed: bool,
}
impl Facts {
    fn validate(&self, failure: Option<Failure>) -> Result<()> {
        for value in [
            &self.stdout_sha256,
            &self.stderr_sha256,
            &self.ops_sha256,
            &self.unshare_sha256,
            &self.launcher_sha256,
            &self.timeout_sha256,
        ]
        .into_iter()
        .flatten()
        {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(Cause::Policy);
            }
        }
        if self
            .launcher_work_elapsed_ns
            .is_some_and(|value| value > WHOLE * SECOND)
        {
            return Err(Cause::Policy);
        }
        let status = |value: i32| (-127..=255).contains(&value);
        if self.wrapper_raw_status.is_some_and(|v| !status(v))
            || self.launcher_raw_status.is_some_and(|v| !status(v))
            || self.adopted_raw_statuses.iter().any(|v| !status(*v))
        {
            return Err(Cause::Policy);
        }
        let retained = usize::try_from(self.adopted_reaped.min(256)).map_err(|_| Cause::Bounds)?;
        if self.adopted_raw_statuses.len() != retained {
            return Err(Cause::Policy);
        }
        if failure.is_none()
            && (self.adopted_reaped > 256
                || self.stdout_bytes > super::MAX_STREAM
                || self.stderr_bytes > super::MAX_STREAM)
        {
            return Err(Cause::Policy);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Frame {
    pub schema_version: u8,
    pub kind: Kind,
    pub nonce: String,
    pub trial: Trial,
    pub clock: Clock,
    #[serde(deserialize_with = "required_option")]
    pub facts: Option<Facts>,
    pub phase: Phase,
    #[serde(deserialize_with = "required_option")]
    pub failure: Option<Failure>,
    #[serde(deserialize_with = "required_option")]
    pub phase_deadline_ns: Option<u64>,
}
impl Frame {
    pub fn new(kind: Kind, nonce: &str, trial: Trial, clock: Clock) -> Self {
        Self {
            schema_version: 1,
            kind,
            nonce: nonce.to_owned(),
            trial,
            clock,
            facts: None,
            phase: Phase::Admission,
            failure: None,
            phase_deadline_ns: None,
        }
    }
    pub fn decode(
        bytes: &[u8],
        kind: Kind,
        nonce: &str,
        trial: Trial,
        clock: Clock,
    ) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_FRAME {
            return Err(Cause::Bounds);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Cause::Input)?;
        if value.schema_version != 1
            || value.kind != kind
            || value.nonce != nonce
            || value.trial != trial
            || value.clock != clock
            || ((kind == Kind::Result) != value.facts.is_some())
            || (kind != Kind::Result && value.failure.is_some())
            || (matches!(kind, Kind::Settled | Kind::Result) != value.phase_deadline_ns.is_some())
            || value
                .phase_deadline_ns
                .is_some_and(|end| end <= clock.origin || end > clock.end)
        {
            return Err(Cause::Policy);
        }
        if let Some(facts) = &value.facts {
            facts.validate(value.failure)?;
        }
        Ok(value)
    }
    pub fn bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self).map_err(|_| Cause::Input)?;
        if bytes.len() > MAX_FRAME {
            return Err(Cause::Bounds);
        }
        Ok(bytes)
    }
}
pub(super) fn nonce(value: &str) -> Result<()> {
    if value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(Cause::Input)
    }
}
#[cfg(target_os = "linux")]
pub(super) fn fresh(owner: &mut Owner) -> Result<String> {
    owner.check(Window::Body)?;
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return fail(owner, Cause::Io);
    }
    owner.check(Window::Body)?;
    Ok(super::process::hex(&bytes))
}

fn required_option<'de, D, T>(decoder: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(decoder)
}
