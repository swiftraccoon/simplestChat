//! Exactly one run owner and one PID1 exclusive reaper. No runtime waiter races it.
use super::{
    Cause, FRAME, LAUNCH, LAUNCHER, OPS, Options, Owner, REAP, Result, TERM, TIMEOUT, Trial,
    UNSHARE, Window, fail,
    identity::{self, Proc},
    process::{Capture, Channel, Tool},
    protocol::{self, Clock, Facts, Failure, Frame, Kind, Phase},
};
use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid, waitpid};
use rustix::process::{Pid, Signal};
use serde::Serialize;
use std::{
    io,
    os::fd::{AsFd, OwnedFd},
    os::unix::process::{CommandExt, ExitStatusExt},
    process::{Child, Command, ExitCode, ExitStatus, Stdio},
    time::{Duration, Instant},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    schema_version: u8,
    experiment: &'static str,
    trial: Trial,
    passed: bool,
    phase: Phase,
    failure: Option<Failure>,
    facts: Facts,
}

/// Observed normal transport facts, including facts from failed attempts.
/// Callers must validate status and closure flags before claiming success. This
/// passive record owns no process, signal registration, descriptor or clock.
#[derive(Serialize)]
pub(in crate::release::attestation) struct NormalLaunch {
    facts: Facts,
}

#[derive(Clone, Copy)]
enum InitRole {
    Fixed,
    Inherited,
}

/// Run the fixed normal launcher using the caller's original owner and its
/// reserved post-launch ceiling. No report or outer owner is created here.
pub(in crate::release::attestation) fn launch_normal(
    owner: &mut Owner,
    ceiling: Instant,
    observed: &mut Option<NormalLaunch>,
) -> Result<()> {
    owner.check(Window::Body)?;
    if observed.is_some() {
        return fail(owner, Cause::Input);
    }
    let end = owner
        .now()
        .checked_add(Duration::from_secs(super::WHOLE))
        .ok_or(Cause::Input)?
        .min(ceiling);
    let body = end
        .checked_sub(Duration::from_secs(super::FINAL))
        .ok_or(Cause::Input)?;
    let clock = Clock::inherited(owner, body, end)?;
    clock.require_work(owner)?;
    owner.begin_maintain_launcher(body, end)?;
    let mut phase = Phase::Admission;
    let mut facts = Facts::default();
    let result = outer(
        owner,
        clock,
        Trial::Normal,
        InitRole::Inherited,
        &mut phase,
        &mut facts,
    );
    if let Err(first) = result {
        owner.record(first);
    }
    let fence = owner.final_work();
    if let Err(first) = fence {
        owner.record(first);
    }
    owner.end_maintain_launcher();
    *observed = Some(NormalLaunch { facts });
    result?;
    fence?;
    owner.check(Window::Body)?;
    Ok(())
}

pub(super) fn entry(options: Options, origin: u64) -> ExitCode {
    let (clock, trial, is_inner) = match &options {
        Options::Run(trial) => (Clock::new(origin), *trial, false),
        Options::Init { clock, trial, .. } => (Ok(*clock), *trial, true),
        Options::NormalInit { clock, .. } => (Ok(*clock), Trial::Normal, true),
    };
    let Ok(clock) = clock else {
        return ExitCode::FAILURE;
    };
    let admitted = match &options {
        Options::NormalInit { .. } => clock.inherited_owner(),
        _ => clock.owner(),
    };
    let mut owner = match admitted {
        Ok(owner) => owner,
        Err(cause) => return ExitCode::from(super::status(Some(cause))),
    };
    let old = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let mut phase = Phase::Admission;
    let mut facts = Facts::default();
    let result = match options {
        Options::Run(_) => outer(
            &mut owner,
            clock,
            trial,
            InitRole::Fixed,
            &mut phase,
            &mut facts,
        ),
        Options::Init { nonce, .. } => {
            inner(&mut owner, clock, trial, &nonce, &mut phase, &mut facts)
        }
        Options::NormalInit { nonce, .. } => inner(
            &mut owner,
            clock,
            Trial::Normal,
            &nonce,
            &mut phase,
            &mut facts,
        ),
    };
    if let Err(first) = result {
        owner.record(first);
    }
    let private = nix::sys::stat::Mode::from_bits_truncate(0o077);
    if nix::sys::stat::umask(old) != private {
        owner.record(Cause::Changed);
    }
    if !is_inner {
        let report = Report {
            schema_version: 1,
            experiment: "maintain-launcher-prerequisite",
            trial,
            passed: owner.error().is_none(),
            phase,
            failure: owner.error().map(Into::into),
            facts,
        };
        if let Err(cause) = emit(&report, &mut owner) {
            owner.record(cause);
        }
    }
    let before = owner.error();
    let finished = owner.finish();
    ExitCode::from(super::status(before.or(finished.err())))
}
fn emit(report: &Report, owner: &mut Owner) -> Result<()> {
    let mut bytes = serde_json::to_vec(report).map_err(|_| Cause::Input)?;
    bytes.push(b'\n');
    if bytes.len() > super::MAX_FRAME {
        return fail(owner, Cause::Bounds);
    }
    let output = io::stdout();
    let mut offset = 0;
    while offset < bytes.len() {
        owner.final_work()?;
        match rustix::io::write(output.as_fd(), &bytes[offset..]) {
            Ok(0) => return fail(owner, Cause::Io),
            Ok(n) => offset += n,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return fail(owner, Cause::Io),
        }
    }
    owner.final_work()
}
fn command(path: &str) -> Command {
    let mut command = Command::new(path);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}
struct Leader {
    child: Option<Child>,
    pid: Pid,
    status: Option<i32>,
    consumed: bool,
}
impl Leader {
    fn new(child: Child) -> Self {
        let pid = Pid::from_child(&child);
        Self {
            child: Some(child),
            pid,
            status: None,
            consumed: false,
        }
    }
    fn terminal(&self, owner: &mut Owner) -> Result<bool> {
        crate::owned_process::exited_now(self.pid).map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })
    }
    fn consume(&mut self, owner: &mut Owner) -> Result<()> {
        // Disarm the SDK owner before any later fallible record/validation.
        let mut child = self.child.take().ok_or(Cause::Policy)?;
        match child.wait() {
            Ok(value) => {
                self.consumed = true;
                self.status = Some(raw(value).inspect_err(|&cause| owner.record(cause))?);
                Ok(())
            }
            Err(_) => fail(owner, Cause::Io),
        }
    }
    fn signal(&self, signal: Signal, owner: &mut Owner) -> Result<()> {
        if self.child.is_none() || !crate::owned_process::reserved(self.pid) {
            return fail(owner, Cause::Policy);
        }
        match rustix::process::kill_process_group(self.pid, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(_) => fail(owner, Cause::Io),
        }
    }
}
fn raw(value: ExitStatus) -> Result<i32> {
    if let Some(code) = value.code() {
        return Ok(code);
    }
    value
        .signal()
        .filter(|signal| *signal > 0)
        .map(|signal| -signal)
        .ok_or(Cause::Io)
}

fn captures(leader: &mut Leader, owner: &mut Owner) -> Result<(Capture, Capture)> {
    let child = leader.child.as_mut().ok_or(Cause::Policy)?;
    let out = child.stdout.take().ok_or(Cause::Io).map(OwnedFd::from);
    if let Err(first) = out {
        owner.record(first);
    }
    let err = child.stderr.take().ok_or(Cause::Io).map(OwnedFd::from);
    if let Err(first) = err {
        owner.record(first);
    }
    let (out, err) = match (out, err) {
        (Ok(out), Ok(err)) => (out, err),
        (Ok(out), Err(first)) => {
            let _ = identity::close(owner, out);
            return Err(first);
        }
        (Err(first), Ok(err)) => {
            let _ = identity::close(owner, err);
            return Err(first);
        }
        (Err(first), Err(_)) => return Err(first),
    };
    let output = Capture::new(out, owner);
    let error = Capture::new(err, owner);
    match (output, error) {
        (Ok(out), Ok(err)) => Ok((out, err)),
        (Ok(mut out), Err(first)) => {
            owner.record(first);
            out.finish(owner);
            Err(first)
        }
        (Err(first), Ok(mut err)) => {
            owner.record(first);
            err.finish(owner);
            Err(first)
        }
        (Err(first), Err(_)) => Err(first),
    }
}

fn own_pidfd(owner: &mut Owner) -> Result<OwnedFd> {
    owner.check(Window::Body)?;
    rustix::process::pidfd_open(
        rustix::process::getpid(),
        rustix::process::PidfdFlags::NONBLOCK,
    )
    .map_err(|_| Cause::Io)
}
fn checked_frame(
    packet: &super::process::Packet,
    kind: Kind,
    nonce: &str,
    trial: Trial,
    clock: Clock,
) -> Result<Frame> {
    Frame::decode(&packet.bytes, kind, nonce, trial, clock)
}
fn same_credential(left: Option<rustix::net::UCred>, right: Option<rustix::net::UCred>) -> bool {
    left.is_some() && left == right
}

fn outer(
    owner: &mut Owner,
    clock: Clock,
    trial: Trial,
    init_role: InitRole,
    phase: &mut Phase,
    facts: &mut Facts,
) -> Result<()> {
    let mut proc = None;
    let mut ops = None;
    let mut unshare = None;
    let mut parent = None;
    let mut child_socket = None;
    let mut leader = None;
    let mut output = None;
    let mut error = None;
    let mut caps = Vec::new();
    let mut own = None;
    let mut result_frame = None;
    let mut natural_end = None;
    let body = (|| {
        if rustix::process::geteuid() != rustix::process::Uid::ROOT {
            return fail(owner, Cause::Policy);
        }
        clock.require_work(owner)?;
        *phase = Phase::Sources;
        proc = Some(Proc::open(owner)?);
        if !proc.as_ref().ok_or(Cause::Input)?.outer() {
            return fail(owner, Cause::Policy);
        }
        ops = Some(Tool::open(OPS, 32 * 1024 * 1024, 0o755, owner)?);
        super::process::self_image(ops.as_ref().ok_or(Cause::Input)?, owner)?;
        unshare = Some(Tool::open(UNSHARE, 8 * 1024 * 1024, 0o755, owner)?);
        let pair = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|_| Cause::Io)?;
        parent = Some(pair.0);
        child_socket = Some(pair.1);
        let nonce = protocol::fresh(owner)?;
        let channel = Channel::new(parent.as_ref().ok_or(Cause::Input)?.as_fd(), true, owner)?;
        let run_body = (|| {
            let input = child_socket
                .as_ref()
                .ok_or(Cause::Input)?
                .as_fd()
                .try_clone_to_owned()
                .map_err(|_| Cause::Io)?;
            let mut cmd = command(
                &unshare
                    .as_mut()
                    .ok_or(Cause::Input)?
                    .outer_alias(proc.as_ref().ok_or(Cause::Input)?, owner)?,
            );
            cmd.args([
                "--pid",
                "--fork",
                "--kill-child=KILL",
                &ops.as_mut()
                    .ok_or(Cause::Input)?
                    .outer_alias(proc.as_ref().ok_or(Cause::Input)?, owner)?,
                "release",
                "maintain-prerequisite",
            ]);
            match init_role {
                InitRole::Fixed => {
                    cmd.args(["init", "--trial", trial_name(trial)]);
                }
                InitRole::Inherited => {
                    cmd.arg("normal-init");
                }
            }
            cmd.args([
                "--nonce",
                &nonce,
                "--origin-monotonic-ns",
                &clock.origin.to_string(),
                "--body-deadline-monotonic-ns",
                &clock.body.to_string(),
                "--deadline-monotonic-ns",
                &clock.end.to_string(),
            ])
            .stdin(Stdio::from(input));
            owner.check(Window::Body)?;
            leader = Some(Leader::new(cmd.spawn().map_err(|_| Cause::Io)?));
            if let Some(fd) = child_socket.take()
                && !identity::close(owner, fd)
            {
                return Err(Cause::Close);
            }
            let peers = captures(leader.as_mut().ok_or(Cause::Input)?, owner)?;
            output = Some(peers.0);
            error = Some(peers.1);
            *phase = Phase::Ready;
            let end = clock.phase(owner)?;
            let mut ready = channel.receive(owner, end, false)?;
            let admitted = (|| {
                let _ = checked_frame(&ready, Kind::Ready, &nonce, trial, clock)?;
                proc.as_ref().ok_or(Cause::Input)?.sender(
                    &ready.rights,
                    ready.credential.ok_or(Cause::Policy)?,
                    owner,
                )
            })();
            if let Err(first) = admitted {
                owner.record(first);
                let _ = ready.finish(owner);
                return Err(first);
            }
            let credential = ready.credential;
            caps.append(&mut ready.rights);
            let _ = ready.finish(owner);
            facts.namespace_mapped = true;
            facts.pidfd_mapped = true;
            facts.credentials_matched = true;
            own = Some(own_pidfd(owner)?);
            *phase = Phase::Go;
            clock.require_work(owner)?;
            identity::live(&caps[1], owner)?;
            let go = Frame::new(Kind::Go, &nonce, trial, clock);
            let go_end = clock.phase(owner)?;
            channel.send(
                &go,
                &[
                    proc.as_ref().ok_or(Cause::Input)?.self_namespace().as_fd(),
                    own.as_ref().ok_or(Cause::Input)?.as_fd(),
                ],
                owner,
                go_end,
                false,
            )?;
            facts.deadlines_inherited = true;
            let peer = Peer {
                credential,
                nonce: &nonce,
                clock,
                trial,
            };
            let mut slots = WaitSlots {
                leader: &mut leader,
                output: &mut output,
                error: &mut error,
                result: &mut result_frame,
                natural_end: &mut natural_end,
            };
            wait_outer(&channel, &peer, owner, phase, &mut slots, &caps)?;
            Ok(())
        })();
        if let Err(first) = run_body {
            owner.record(first);
        }
        facts.run_control_flags_restored = channel.finish(owner);
        run_body
    })();
    if let Err(first) = body {
        owner.record(first);
    }
    facts.wrapper_natural_exit =
        settle_wrapper(&mut leader, &mut output, &mut error, owner, natural_end);
    if let Some(mut value) = result_frame {
        facts.private_go = value.private_go;
        facts.init_control_flags_restored = value.init_control_flags_restored;
        facts.chld_registration_closed = value.chld_registration_closed;
        facts.launcher_started = value.launcher_started;
        facts.nested_client_observed = value.nested_client_observed;
        facts.launcher_stopped = value.launcher_stopped;
        facts.term_seen = value.term_seen;
        facts.work_cap_expired = value.work_cap_expired;
        facts.leader_reaped = value.leader_reaped;
        facts.adopted_reaped = value.adopted_reaped;
        facts.adopted_raw_statuses = value.adopted_raw_statuses.clone();
        facts.echild = value.echild;
        facts.stdout_eof = value.stdout_eof;
        facts.stderr_eof = value.stderr_eof;
        facts.stdout_closed = value.stdout_closed;
        facts.stderr_closed = value.stderr_closed;
        facts.stdout_bytes = value.stdout_bytes;
        facts.stderr_bytes = value.stderr_bytes;
        facts.stdout_sha256 = value.stdout_sha256.take();
        facts.stderr_sha256 = value.stderr_sha256.take();
        facts.launcher_raw_status = value.launcher_raw_status;
        facts.launcher_work_elapsed_ns = value.launcher_work_elapsed_ns;
        facts.launcher_sha256 = value.launcher_sha256.take();
        facts.timeout_sha256 = value.timeout_sha256.take();
        facts.capabilities_closed = value.capabilities_closed;
        facts.tools_unchanged = value.tools_unchanged;
    }
    if let Some(value) = leader {
        facts.wrapper_raw_status = value.status;
        facts.wrapper_reaped = value.consumed;
    }
    if let Some(value) = output {
        facts.wrapper_stdout_eof = value.eof;
        facts.wrapper_stdout_closed = value.closed;
    }
    if let Some(value) = error {
        facts.wrapper_stderr_eof = value.eof;
        facts.wrapper_stderr_closed = value.closed;
    }
    facts.capabilities_closed &= identity::close_all(owner, caps);
    for slot in [&mut own, &mut child_socket, &mut parent] {
        if let Some(fd) = slot.take() {
            facts.capabilities_closed &= identity::close(owner, fd);
        }
    }
    if let Some(value) = proc {
        facts.capabilities_closed &= value.finish(owner);
    }
    let mut unchanged = true;
    if let Some(mut value) = unshare {
        facts.unshare_sha256 = Some(value.hash.clone());
        unchanged &= value.check_final(owner).is_ok();
        unchanged &= value.finish(owner);
    }
    if let Some(mut value) = ops {
        facts.ops_sha256 = Some(value.hash.clone());
        unchanged &= value.check_final(owner).is_ok();
        unchanged &= value.finish(owner);
    }
    facts.tools_unchanged &= unchanged;
    body?;
    validate_finished(facts, trial, owner)?;
    *phase = Phase::Complete;
    Ok(())
}
fn trial_name(value: Trial) -> &'static str {
    match value {
        Trial::Normal => "normal",
        Trial::CancelOwnedLauncher => "cancel-owned-launcher",
        Trial::TimeoutStoppedLauncher => "timeout-stopped-launcher",
    }
}

struct Peer<'a> {
    credential: Option<rustix::net::UCred>,
    nonce: &'a str,
    clock: Clock,
    trial: Trial,
}
struct WaitSlots<'a> {
    leader: &'a mut Option<Leader>,
    output: &'a mut Option<Capture>,
    error: &'a mut Option<Capture>,
    result: &'a mut Option<Facts>,
    natural_end: &'a mut Option<std::time::Instant>,
}
fn wait_outer(
    channel: &Channel<'_>,
    peer: &Peer<'_>,
    owner: &mut Owner,
    phase: &mut Phase,
    slots: &mut WaitSlots<'_>,
    caps: &[OwnedFd],
) -> Result<()> {
    *phase = Phase::Launcher;
    let mut stopped = false;
    let mut settled = None;
    let mut result_deadline_ns = None;
    let mut forwarded = false;
    let mut terminal_seen = false;
    loop {
        let observed = if settled.is_some() {
            owner.final_work()
        } else {
            owner.check(Window::Body)
        };
        if let Err(first) = observed {
            owner.record(first);
        }
        if owner.error().is_some() && !forwarded {
            let signal = if owner.error() == Some(Cause::Interrupt) {
                Signal::INT
            } else {
                Signal::TERM
            };
            if let Some(pidfd) = caps.get(1)
                && rustix::process::pidfd_send_signal(pidfd, signal).is_err()
            {
                owner.record(Cause::Io);
            }
            forwarded = true;
        }
        let window = if settled.is_some() || owner.error().is_some() {
            Window::Final
        } else {
            Window::Body
        };
        if let Some(value) = slots.output.as_mut()
            && let Err(first) = value.pump(owner, window)
        {
            owner.record(first);
        }
        if let Some(value) = slots.error.as_mut()
            && let Err(first) = value.pump(owner, window)
        {
            owner.record(first);
        }
        loop {
            let cleanup = owner.error().is_some() || settled.is_some();
            let receive_end = settled.unwrap_or_else(|| owner.end());
            let Some(packet) = channel.attempt(owner, receive_end, cleanup)? else {
                break;
            };
            let result = (|| {
                if !same_credential(packet.credential, peer.credential) || !packet.rights.is_empty()
                {
                    return fail(owner, Cause::Policy);
                }
                let parsed: Frame =
                    serde_json::from_slice(&packet.bytes).map_err(|_| Cause::Input)?;
                let frame =
                    checked_frame(&packet, parsed.kind, peer.nonce, peer.trial, peer.clock)?;
                match frame.kind {
                    Kind::Stopped
                        if !stopped && settled.is_none() && peer.trial != Trial::Normal =>
                    {
                        stopped = true;
                        *phase = Phase::Stopped;
                        if peer.trial == Trial::CancelOwnedLauncher {
                            signal_hook::low_level::raise(signal_hook::consts::SIGTERM)
                                .map_err(|_| Cause::Io)?;
                            let _ = owner.check(Window::Body);
                            if owner.error() != Some(Cause::Terminate) {
                                return fail(owner, Cause::Policy);
                            }
                        }
                    }
                    Kind::Settled if settled.is_none() => {
                        owner.cleanup_before(owner.body_end())?;
                        let end_ns = frame.phase_deadline_ns.ok_or(Cause::Policy)?;
                        let current = protocol::now()?;
                        if end_ns <= current
                            || end_ns > current.saturating_add(FRAME * 1_000_000_000)
                        {
                            return fail(owner, Cause::Deadline);
                        }
                        result_deadline_ns = Some(end_ns);
                        settled = Some(
                            (owner.now() + Duration::from_nanos(end_ns - current)).min(owner.end()),
                        );
                        *phase = Phase::Result;
                    }
                    Kind::Result if settled.is_some() => {
                        owner.cleanup_before(settled.ok_or(Cause::Policy)?)?;
                        if frame.phase_deadline_ns != result_deadline_ns
                            || protocol::now()? >= result_deadline_ns.ok_or(Cause::Policy)?
                        {
                            return fail(owner, Cause::Deadline);
                        }
                        let facts = frame.facts.ok_or(Cause::Policy)?;
                        if peer.trial != Trial::Normal && !stopped {
                            return fail(owner, Cause::Policy);
                        }
                        if let Some(failure) = frame.failure {
                            owner.record(failure.into());
                        }
                        *slots.natural_end = settled;
                        *slots.result = Some(facts);
                        return Ok(());
                    }
                    _ => return fail(owner, Cause::Policy),
                }
                Ok(())
            })();
            if let Err(first) = result {
                owner.record(first);
            }
            let closed = packet.finish(owner);
            result?;
            if !closed {
                return Err(Cause::Close);
            }
            if slots.result.is_some() {
                return Ok(());
            }
        }
        if settled.is_some_and(|end| owner.now() >= end) {
            return fail(owner, Cause::Deadline);
        }
        if owner.now() >= owner.end() {
            return fail(owner, Cause::Deadline);
        }
        if terminal_seen {
            // Only a genuinely empty queue observed after reserved terminal
            // status can prove that the child omitted its closed result.
            return fail(owner, Cause::Command);
        }
        if slots.leader.as_ref().ok_or(Cause::Input)?.terminal(owner)? {
            // Result can be queued between the previous EAGAIN and this
            // observation. Drain once more before judging it absent.
            terminal_seen = true;
            continue;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn settle_wrapper(
    leader: &mut Option<Leader>,
    output: &mut Option<Capture>,
    error: &mut Option<Capture>,
    owner: &mut Owner,
    natural_end: Option<std::time::Instant>,
) -> bool {
    let Some(child) = leader.as_mut() else {
        return false;
    };
    let natural = natural_end.is_some_and(|end| natural_join(child, output, error, owner, end));
    if child.child.is_some() {
        let end = owner.end();
        let grace = (owner.now() + Duration::from_secs(TERM)).min(end);
        if !child.terminal(owner).unwrap_or(false) {
            let _ = child.signal(Signal::TERM, owner);
        }
        while owner.now() < grace && !child.terminal(owner).unwrap_or(false) {
            if let Some(value) = output.as_mut() {
                let _ = value.pump(owner, Window::Final);
            }
            if let Some(value) = error.as_mut() {
                let _ = value.pump(owner, Window::Final);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Finish exact reserved group before the only consuming wrapper wait.
        let _ = child.signal(Signal::KILL, owner);
        while !child.terminal(owner).unwrap_or(true) {
            if owner.now() >= end {
                owner.record(Cause::Deadline);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = child.consume(owner);
    }
    let end = (owner.now() + Duration::from_secs(REAP)).min(owner.end());
    loop {
        if let Some(value) = output.as_mut()
            && let Err(first) = value.pump(owner, Window::Final)
        {
            owner.record(first);
        }
        if let Some(value) = error.as_mut()
            && let Err(first) = value.pump(owner, Window::Final)
        {
            owner.record(first);
        }
        if output.as_ref().is_none_or(|v| v.eof) && error.as_ref().is_none_or(|v| v.eof) {
            break;
        }
        if owner.now() >= end {
            owner.record(Cause::Deadline);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if let Some(value) = output.as_mut() {
        value.finish(owner);
    }
    if let Some(value) = error.as_mut() {
        value.finish(owner);
    }
    natural
}
fn phase_live(owner: &mut Owner, end: std::time::Instant) -> Result<()> {
    owner
        .cleanup_before(end)
        .inspect_err(|&cause| owner.record(cause))
}
fn natural_join(
    child: &Leader,
    output: &mut Option<Capture>,
    error: &mut Option<Capture>,
    owner: &mut Owner,
    end: std::time::Instant,
) -> bool {
    loop {
        if let Err(first) = phase_live(owner, end) {
            owner.record(first);
            return false;
        }
        for slot in [&mut *output, &mut *error] {
            if let Some(value) = slot.as_mut()
                && let Err(first) = value.pump(owner, Window::Final)
            {
                owner.record(first);
                return false;
            }
            #[cfg(test)]
            if slot.as_ref().is_some_and(|value| value.eof)
                && let Some(elapsed) = NATURAL_AFTER_EOF.with(std::cell::Cell::take)
            {
                owner.elapsed = Some(elapsed);
            }
            if let Err(first) = phase_live(owner, end) {
                owner.record(first);
                return false;
            }
        }
        let terminal = match child.terminal(owner) {
            Ok(value) => value,
            Err(_) => return false,
        };
        #[cfg(test)]
        if terminal && let Some(elapsed) = NATURAL_AFTER_TERMINAL.with(std::cell::Cell::take) {
            owner.elapsed = Some(elapsed);
        }
        if let Err(first) = phase_live(owner, end) {
            owner.record(first);
            return false;
        }
        if terminal
            && output.as_ref().is_some_and(|value| value.eof)
            && error.as_ref().is_some_and(|value| value.eof)
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[cfg(test)]
thread_local! {
    static NATURAL_AFTER_EOF: std::cell::Cell<Option<Duration>> =
        const {std::cell::Cell::new(None)};
    static NATURAL_AFTER_TERMINAL: std::cell::Cell<Option<Duration>> =
        const {std::cell::Cell::new(None)};
}
fn validate_finished(facts: &Facts, trial: Trial, owner: &mut Owner) -> Result<()> {
    if !facts.namespace_mapped
        || !facts.pidfd_mapped
        || !facts.credentials_matched
        || !facts.private_go
        || !facts.deadlines_inherited
        || !facts.launcher_started
        || !facts.leader_reaped
        || !facts.echild
        || !facts.stdout_eof
        || !facts.stderr_eof
        || !facts.stdout_closed
        || !facts.stderr_closed
        || !facts.capabilities_closed
        || !facts.tools_unchanged
        || !facts.init_control_flags_restored
        || !facts.run_control_flags_restored
        || !facts.chld_registration_closed
        || !facts.wrapper_natural_exit
        || !facts.wrapper_reaped
        || !facts.wrapper_stdout_eof
        || !facts.wrapper_stderr_eof
        || !facts.wrapper_stdout_closed
        || !facts.wrapper_stderr_closed
        || [
            &facts.ops_sha256,
            &facts.unshare_sha256,
            &facts.launcher_sha256,
            &facts.timeout_sha256,
            &facts.stdout_sha256,
            &facts.stderr_sha256,
        ]
        .into_iter()
        .any(Option::is_none)
    {
        return fail(owner, Cause::Policy);
    }
    match trial {
        Trial::Normal
            if facts.launcher_raw_status == Some(0) && facts.wrapper_raw_status == Some(0) =>
        {
            Ok(())
        }
        Trial::CancelOwnedLauncher
            if facts.term_seen && facts.nested_client_observed && facts.launcher_stopped =>
        {
            fail(owner, Cause::Terminate)
        }
        Trial::TimeoutStoppedLauncher
            if facts.work_cap_expired
                && facts.nested_client_observed
                && facts.launcher_stopped
                && facts
                    .launcher_work_elapsed_ns
                    .is_some_and(|v| v >= LAUNCHER * 1_000_000_000) =>
        {
            fail(owner, Cause::Deadline)
        }
        _ => fail(owner, Cause::Command),
    }
}

struct Chld {
    registration: Option<signal_hook::SigId>,
}
impl Chld {
    fn new(owner: &mut Owner) -> Result<Self> {
        owner.check(Window::Body)?;
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let id = signal_hook::flag::register(signal_hook::consts::SIGCHLD, flag)
            .map_err(|_| Cause::Io)?;
        Ok(Self {
            registration: Some(id),
        })
    }
    fn finish(mut self, owner: &mut Owner) -> bool {
        let good = self
            .registration
            .take()
            .is_none_or(signal_hook::low_level::unregister);
        if !good {
            owner.record(Cause::Close);
        }
        good
    }
}
fn inner(
    owner: &mut Owner,
    clock: Clock,
    trial: Trial,
    nonce: &str,
    phase: &mut Phase,
    facts: &mut Facts,
) -> Result<()> {
    let input = io::stdin();
    let channel = Channel::new(input.as_fd(), false, owner)?;
    let mut proc = None;
    let mut own = None;
    let mut parent_caps = Vec::new();
    let mut launcher = None;
    let mut timeout = None;
    let mut chld = None;
    let mut leader = None;
    let mut launched = None;
    let mut output = None;
    let mut error = None;
    let body = (|| {
        if rustix::process::geteuid() != rustix::process::Uid::ROOT
            || rustix::process::getpid() != Pid::INIT
        {
            return fail(owner, Cause::Policy);
        }
        proc = Some(Proc::open(owner)?);
        proc.as_ref().ok_or(Cause::Input)?.guard_realm()?;
        own = Some(own_pidfd(owner)?);
        *phase = Phase::Ready;
        let ready = Frame::new(Kind::Ready, nonce, trial, clock);
        let ready_end = clock.phase(owner)?;
        channel.send(
            &ready,
            &[
                proc.as_ref().ok_or(Cause::Input)?.self_namespace().as_fd(),
                own.as_ref().ok_or(Cause::Input)?.as_fd(),
            ],
            owner,
            ready_end,
            false,
        )?;
        *phase = Phase::Go;
        let go_end = clock.phase(owner)?;
        let mut packet = channel.receive(owner, go_end, false)?;
        let admitted = (|| {
            let _ = checked_frame(&packet, Kind::Go, nonce, trial, clock)?;
            if packet.credential.is_some() {
                return fail(owner, Cause::Policy);
            }
            proc.as_ref()
                .ok_or(Cause::Input)?
                .ancestor(&packet.rights, owner)
        })();
        if let Err(first) = admitted {
            owner.record(first);
            let _ = packet.finish(owner);
            return Err(first);
        }
        parent_caps.append(&mut packet.rights);
        let _ = packet.finish(owner);
        clock.require_work(owner)?;
        facts.private_go = true;
        facts.deadlines_inherited = true;
        *phase = Phase::Sources;
        launcher = Some(Tool::open(LAUNCH, 1024 * 1024, 0o755, owner)?);
        timeout = Some(Tool::open(TIMEOUT, 8 * 1024 * 1024, 0o755, owner)?);
        chld = Some(Chld::new(owner)?);
        clock.require_work(owner)?;
        proc.as_ref().ok_or(Cause::Input)?.guard_realm()?;
        *phase = Phase::Launcher;
        let started = protocol::now()?;
        launched = Some(started);
        leader = Some(Leader::new(command(LAUNCH).spawn().map_err(|_| Cause::Io)?));
        facts.launcher_started = true;
        let peers = captures(leader.as_mut().ok_or(Cause::Input)?, owner)?;
        output = Some(peers.0);
        error = Some(peers.1);
        if trial != Trial::Normal {
            *phase = Phase::Client;
            let stopped_end = stop_client(
                proc.as_ref().ok_or(Cause::Input)?,
                timeout.as_ref().ok_or(Cause::Input)?.node,
                leader.as_ref().ok_or(Cause::Input)?,
                owner,
            )?;
            facts.nested_client_observed = true;
            facts.launcher_stopped = true;
            *phase = Phase::Stopped;
            channel.send(
                &Frame::new(Kind::Stopped, nonce, trial, clock),
                &[],
                owner,
                stopped_end,
                false,
            )?;
        }
        *phase = Phase::Launcher;
        reap_launcher(
            proc.as_ref().ok_or(Cause::Input)?,
            Work {
                started,
                parent: &parent_caps[1],
            },
            owner,
            &mut leader,
            &mut output,
            &mut error,
            facts,
        )?;
        Ok(())
    })();
    if let Err(first) = body {
        owner.record(first);
    }
    // Even setup/copy/parse refusal attempts every original peer. If a launcher
    // was admitted, only its own verified PID1 realm may be retired.
    if leader.as_ref().is_some_and(|v| v.child.is_some())
        && let (Some(held), Some(started), Some(parent)) =
            (proc.as_ref(), launched, parent_caps.get(1))
    {
        let _ = reap_launcher(
            held,
            Work { started, parent },
            owner,
            &mut leader,
            &mut output,
            &mut error,
            facts,
        );
    }
    if let Some(value) = leader.as_ref() {
        facts.launcher_raw_status = value.status;
        facts.leader_reaped = value.consumed;
    }
    let result_end = (owner.now() + Duration::from_secs(FRAME)).min(owner.end());
    let result_deadline_ns = protocol::now()
        .ok()
        .and_then(|now| now.checked_add(FRAME * 1_000_000_000))
        .map(|end| end.min(clock.end));
    if owner.cleanup_before(owner.body_end()).is_ok() && facts.echild {
        let mut frame = Frame::new(Kind::Settled, nonce, trial, clock);
        frame.phase_deadline_ns = result_deadline_ns;
        if let Err(first) = channel.send(&frame, &[], owner, result_end, true) {
            owner.record(first);
        }
    }
    if let Some(value) = output.as_mut() {
        let _ = value.pump(owner, Window::Final);
        value.finish(owner);
        facts.stdout_eof = value.eof;
        facts.stdout_closed = value.closed;
        facts.stdout_bytes = value.bytes;
        facts.stdout_sha256 = value.digest();
    }
    if let Some(value) = error.as_mut() {
        let _ = value.pump(owner, Window::Final);
        value.finish(owner);
        facts.stderr_eof = value.eof;
        facts.stderr_closed = value.closed;
        facts.stderr_bytes = value.bytes;
        facts.stderr_sha256 = value.digest();
    }
    let mut checked = true;
    if let Some(mut value) = launcher {
        facts.launcher_sha256 = Some(value.hash.clone());
        checked &= value.check_final(owner).is_ok();
        checked &= value.finish(owner);
    }
    if let Some(mut value) = timeout {
        facts.timeout_sha256 = Some(value.hash.clone());
        checked &= value.check_final(owner).is_ok();
        checked &= value.finish(owner);
    }
    facts.tools_unchanged = checked;
    if let Some(value) = chld {
        facts.chld_registration_closed = value.finish(owner);
        checked &= facts.chld_registration_closed;
    }
    facts.capabilities_closed = identity::close_all(owner, parent_caps);
    if let Some(fd) = own.take() {
        facts.capabilities_closed &= identity::close(owner, fd);
    }
    if let Some(value) = proc {
        facts.capabilities_closed &= value.finish(owner);
    }
    if !checked
        || !facts.capabilities_closed
        || !facts.stdout_eof
        || !facts.stderr_eof
        || !facts.stdout_closed
        || !facts.stderr_closed
    {
        owner.record(Cause::Close);
    }
    facts.init_control_flags_restored = channel.restore(owner);
    let mut final_frame = Frame::new(Kind::Result, nonce, trial, clock);
    final_frame.phase_deadline_ns = result_deadline_ns;
    final_frame.facts = Some(facts.clone());
    final_frame.phase = *phase;
    final_frame.failure = owner.error().map(Into::into);
    if let Err(first) = channel.send(&final_frame, &[], owner, result_end, true) {
        owner.record(first);
    }
    body
}
fn realm(proc: &Proc, signal: Signal, owner: &mut Owner) -> Result<()> {
    proc.guard_realm()
        .inspect_err(|&cause| owner.record(cause))?;
    // Pid::INIT is kill(-1) here: it is legal ONLY behind the actual private
    // PID1/namespace/original-proc guard above, never in the outer role.
    match rustix::process::kill_process_group(Pid::INIT, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(_) => fail(owner, Cause::Io),
    }
}
fn stop_client(
    proc: &Proc,
    timeout: (u64, u64),
    leader: &Leader,
    owner: &mut Owner,
) -> Result<std::time::Instant> {
    let end = (owner.now() + Duration::from_secs(FRAME)).min(owner.body_end());
    let client = loop {
        owner.check(Window::Body)?;
        if leader.terminal(owner)? {
            return fail(owner, Cause::Policy);
        }
        if let Some(client) = proc.nested_client(timeout, owner)? {
            if let Err(first) = phase_live(owner, end) {
                owner.record(first);
                let _ = client.finish(owner);
                return Err(first);
            }
            break client;
        }
        if owner.now() >= end {
            return fail(owner, Cause::Policy);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let result = (|| {
        phase_live(owner, end)?;
        realm(proc, Signal::STOP, owner)?;
        let pid = nix::unistd::Pid::from_raw(leader.pid.as_raw_pid());
        loop {
            owner.check(Window::Body)?;
            match waitid(
                Id::Pid(pid),
                WaitPidFlag::WSTOPPED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
            ) {
                Ok(WaitStatus::Stopped(actual, nix::sys::signal::Signal::SIGSTOP))
                    if actual == pid =>
                {
                    phase_live(owner, end)?;
                    client.stopped(proc, owner)?;
                    return phase_live(owner, end);
                }
                Ok(WaitStatus::StillAlive) => {}
                Err(nix::errno::Errno::EINTR) => {}
                _ => return fail(owner, Cause::Policy),
            }
            if owner.now() >= end {
                return fail(owner, Cause::Policy);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    if let Err(first) = result {
        owner.record(first);
    }
    let closed = client.finish(owner);
    result?;
    if !closed {
        return Err(Cause::Close);
    }
    phase_live(owner, end)?;
    Ok(end)
}

fn available(
    proc: &Proc,
    leader: &mut Leader,
    owner: &mut Owner,
    facts: &mut Facts,
) -> Result<bool> {
    loop {
        proc.guard_realm()
            .inspect_err(|&cause| owner.record(cause))?;
        match waitid(
            Id::All,
            WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
        ) {
            Ok(WaitStatus::StillAlive) => return Ok(false),
            Err(nix::errno::Errno::EINTR) => {}
            Err(nix::errno::Errno::ECHILD) => {
                if leader.child.is_some() {
                    return fail(owner, Cause::Policy);
                }
                return Ok(true);
            }
            Ok(observation @ (WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _))) => {
                let pid = observation.pid().ok_or(Cause::Io)?;
                if pid.as_raw() == leader.pid.as_raw_pid() {
                    leader.consume(owner)?;
                    facts.leader_reaped = true;
                    facts.launcher_raw_status = leader.status;
                    if leader.status != Some(0) {
                        owner.record(Cause::Command);
                    }
                } else {
                    let status = loop {
                        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                            Err(nix::errno::Errno::EINTR) => {}
                            value => break value,
                        }
                    }
                    .map_err(|_| Cause::Io)?;
                    let raw = match status {
                        WaitStatus::Exited(actual, code) if actual == pid => code,
                        WaitStatus::Signaled(actual, signal, _) if actual == pid => {
                            -(signal as i32)
                        }
                        _ => return fail(owner, Cause::Io),
                    };
                    facts.adopted_reaped =
                        facts.adopted_reaped.checked_add(1).ok_or(Cause::Bounds)?;
                    if facts.adopted_raw_statuses.len() < 256 {
                        facts.adopted_raw_statuses.push(raw);
                    } else {
                        owner.record(Cause::Bounds);
                    }
                }
            }
            _ => return fail(owner, Cause::Io),
        }
    }
}
struct Work<'a> {
    started: u64,
    parent: &'a OwnedFd,
}
fn reap_launcher(
    proc: &Proc,
    work: Work<'_>,
    owner: &mut Owner,
    leader: &mut Option<Leader>,
    output: &mut Option<Capture>,
    error: &mut Option<Capture>,
    facts: &mut Facts,
) -> Result<()> {
    let work_end = work
        .started
        .checked_add(LAUNCHER * 1_000_000_000)
        .ok_or(Cause::Deadline)?;
    let mut retiring = None;
    let mut killed = false;
    loop {
        let observed = owner.check(Window::Body);
        if let Err(first) = observed {
            owner.record(first);
        }
        facts.term_seen |= owner.error() == Some(Cause::Terminate);
        if owner.error().is_none()
            && let Err(first) = identity::live(work.parent, owner)
        {
            owner.record(first);
        }
        let current = protocol::now()?;
        if current >= work_end {
            facts.work_cap_expired = true;
            owner.record(Cause::Deadline);
        }
        if current >= work_end
            || owner.error().is_some()
            || leader.as_ref().is_some_and(|v| v.status.is_some())
        {
            facts
                .launcher_work_elapsed_ns
                .get_or_insert(current.saturating_sub(work.started));
        }
        let window = if owner.error().is_some() {
            Window::Final
        } else {
            Window::Body
        };
        if let Some(value) = output.as_mut()
            && let Err(first) = value.pump(owner, window)
        {
            owner.record(first);
        }
        if let Some(value) = error.as_mut()
            && let Err(first) = value.pump(owner, window)
        {
            owner.record(first);
        }
        let done = available(proc, leader.as_mut().ok_or(Cause::Input)?, owner, facts);
        if let Err(first) = done {
            owner.record(first);
        }
        if matches!(done, Ok(true)) {
            facts.echild = true;
            if facts.launcher_work_elapsed_ns.is_none() {
                facts.launcher_work_elapsed_ns =
                    Some(protocol::now()?.saturating_sub(work.started));
            }
            return owner.cleanup_before(owner.body_end());
        }
        if retiring.is_none()
            && (owner.error().is_some() || leader.as_ref().is_some_and(|v| v.status.is_some()))
        {
            let _ = realm(proc, Signal::TERM, owner);
            let _ = realm(proc, Signal::CONT, owner);
            retiring = Some((owner.now() + Duration::from_secs(TERM)).min(owner.body_end()));
        }
        if retiring.is_some_and(|end| owner.now() >= end) && !killed {
            let _ = realm(proc, Signal::KILL, owner);
            killed = true;
        }
        if killed {
            let _ = realm(proc, Signal::KILL, owner);
        }
        // Never detach an admitted child at an elapsed boundary. Late physical
        // settlement stays negative and the outer owned VM remains the backstop.
        if owner.now() >= owner.end() {
            owner.record(Cause::Deadline);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod join_tests {
    use super::*;
    #[test]
    fn original_stop_phase_refuses_a_late_successful_close() {
        for preparation in [Duration::ZERO, Duration::from_secs(10)] {
            let started = std::time::Instant::now().checked_sub(preparation).unwrap();
            let mut owner = Owner::new(
                started,
                Duration::from_secs(super::super::WHOLE),
                Duration::from_secs(super::super::FINAL),
            )
            .unwrap();
            let end = owner.now() + Duration::from_secs(1);
            let original = rustix::fs::open(
                "/dev/null",
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            );
            let admitted = original.is_ok();
            let closed = match original {
                Ok(fd) => identity::close(&mut owner, fd),
                Err(_) => {
                    owner.record(Cause::Io);
                    false
                }
            };
            owner.elapsed = Some(end.duration_since(started) + Duration::from_nanos(1));
            let result = phase_live(&mut owner, end);
            let primary = owner.error();
            let _ = owner.finish();
            assert!(admitted && closed);
            assert_eq!(result, Err(Cause::Deadline));
            assert_eq!(primary, Some(Cause::Deadline));
        }
    }
    #[test]
    fn original_result_end_fences_initial_terminal_and_eof_observations() {
        for preparation in [Duration::ZERO, Duration::from_secs(10)] {
            for edge in [0, 1, 2, 3] {
                let started = std::time::Instant::now().checked_sub(preparation).unwrap();
                let mut owner = Owner::new(
                    started,
                    Duration::from_secs(super::super::WHOLE),
                    Duration::from_secs(super::super::FINAL),
                )
                .unwrap();
                let mut leader = None;
                let mut output = None;
                let mut error = None;
                let prepared = (|| {
                    owner.check(Window::Body)?;
                    leader = Some(Leader::new(
                        command("/usr/bin/true").spawn().map_err(|_| Cause::Io)?,
                    ));
                    let captures = captures(leader.as_mut().ok_or(Cause::Io)?, &mut owner)?;
                    output = Some(captures.0);
                    error = Some(captures.1);
                    Ok::<(), Cause>(())
                })();
                let mut accepted = false;
                if prepared.is_ok() {
                    let end = owner.now() + Duration::from_secs(FRAME);
                    let late = end.duration_since(started) + Duration::from_nanos(1);
                    match edge {
                        0 => owner.elapsed = Some(late),
                        1 => NATURAL_AFTER_TERMINAL.with(|v| v.set(Some(late))),
                        2 => NATURAL_AFTER_EOF.with(|v| v.set(Some(late))),
                        _ => {}
                    }
                    accepted = natural_join(
                        leader.as_ref().unwrap(),
                        &mut output,
                        &mut error,
                        &mut owner,
                        end,
                    );
                } else if let Err(first) = prepared {
                    owner.record(first);
                }
                let primary = owner.error();
                let _ = settle_wrapper(&mut leader, &mut output, &mut error, &mut owner, None);
                NATURAL_AFTER_TERMINAL.with(|v| v.set(None));
                NATURAL_AFTER_EOF.with(|v| v.set(None));
                let reaped = leader.as_ref().is_some_and(|v| v.consumed);
                let peers = output.as_ref().is_some_and(|v| v.closed)
                    && error.as_ref().is_some_and(|v| v.closed);
                let _ = owner.finish();
                assert!(prepared.is_ok());
                assert_eq!(accepted, edge == 3);
                assert_eq!(
                    primary,
                    if edge == 3 {
                        None
                    } else {
                        Some(Cause::Deadline)
                    }
                );
                assert!(reaped && peers);
            }
        }
    }
}

#[cfg(test)]
mod bytecode_tests {
    use super::command;
    use crate::owned_process::{Group, exited, finish_before};
    use serde_json::json;
    use std::{
        fs,
        io::{Read, Seek},
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        path::PathBuf,
        process::Stdio,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn actual_frozen_seed_import_preserves_retained_parent_without_bytecode() {
        let end = Instant::now() + Duration::from_secs(30);
        let body_end = end - Duration::from_secs(3);
        let path = std::env::temp_dir().join(format!(
            "maintain-seed-bytecode-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Fixture(path);
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        let bench = fixture.0.join("bench");
        fs::create_dir(&bench).unwrap();
        fs::set_permissions(&bench, fs::Permissions::from_mode(0o700)).unwrap();
        let sources = [
            "seed-public-room.py",
            "seed_public_room.py",
            "release_json.py",
        ]
        .map(|name| {
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("ansible/files")
                .join(name);
            let target = bench.join(name);
            fs::copy(&source, &target).unwrap();
            (source.clone(), target, fs::read(source).unwrap())
        });
        let logs = ["stdout", "stderr"].map(|name| {
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(fixture.0.join(name))
                .unwrap()
        });
        let before = fs::symlink_metadata(&bench).unwrap();
        let python = std::env::var_os("PYTHON").unwrap_or_else(|| "/usr/bin/python3".into());
        assert!(Instant::now() < body_end);
        let child = command(python.to_str().expect("PYTHON is an interpreter path"))
            .arg(bench.join("seed-public-room.py"))
            .arg("--help")
            .current_dir(&bench)
            .stdout(Stdio::from(logs[0].try_clone().unwrap()))
            .stderr(Stdio::from(logs[1].try_clone().unwrap()))
            .spawn()
            .unwrap();
        let mut group = Group::new(child);
        let natural = tokio::time::timeout_at(body_end.into(), exited(group.pid())).await;
        let retired = finish_before(group.pid(), end).await;
        let reaped = group.reap().await;
        let captured = logs.map(|mut file| {
            let mut bytes = Vec::new();
            let read = file
                .rewind()
                .and_then(|()| (&mut file).take(4097).read_to_end(&mut bytes));
            let closed = nix::unistd::close(file);
            (read, closed, bytes)
        });
        let after = fs::symlink_metadata(&bench).unwrap();
        let cache_absent = !bench.join("__pycache__").exists();
        let sources_unchanged = sources.iter().all(|(source, target, bytes)| {
            fs::read(source).unwrap() == *bytes && fs::read(target).unwrap() == *bytes
        });
        let removed = fs::remove_dir_all(&fixture.0);
        assert!(matches!(natural, Ok(Ok(()))) && retired);
        assert_eq!(reaped.unwrap().code(), Some(1));
        let [stdout, stderr] = captured.map(|(read, closed, bytes)| {
            read.unwrap();
            closed.unwrap();
            assert!(bytes.len() <= 4096);
            bytes
        });
        removed.unwrap();
        assert!(stdout.len() + stderr.len() <= 4096 && stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&stdout).unwrap(),
            json!({
                "schemaVersion":1,"passed":false,"changed":false,"ownerCreated":false,
                "roomCreated":false,"ownerFileCreated":false,"existingSettingsPreserved":false,
                "refreshSessionRevoked":null,"failure":"arguments_not_supported",
                "ownerCredentialFile":"/etc/simplestchat-public/owner.json"
            })
        );
        assert!(cache_absent && sources_unchanged);
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.uid(),
                before.gid(),
                before.nlink()
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.uid(),
                after.gid(),
                after.nlink()
            )
        );
        assert!(Instant::now() < end);
    }
}

#[cfg(test)]
mod result_queue_tests {
    use super::*;
    use std::time::Instant;

    #[derive(Clone, Copy)]
    enum Case {
        Complete,
        Failed,
        Missing,
        Nonce,
        Credential,
        Rights,
        Ordering,
        Late,
        ChangedEnd,
    }
    struct Observed {
        result: Result<()>,
        first: Option<Cause>,
        facts: Option<Facts>,
        natural: bool,
        reaped: bool,
        pipes_closed: bool,
        sockets_closed: bool,
        flags_restored: bool,
        deadline_retained: bool,
        close_count: usize,
        all_closes_passed: bool,
        finished: Result<()>,
    }
    fn queued(case: Case) -> Observed {
        identity::CLOSED.with(|events| events.borrow_mut().clear());
        let start = Instant::now();
        let mut owner = Owner::new(start, Duration::from_secs(30), Duration::from_secs(3)).unwrap();
        let original_end = owner.end();
        let mut sockets = Vec::new();
        let mut leader = None;
        let mut output = None;
        let mut error = None;
        let mut facts = None;
        let mut natural_end = None;
        let mut flags_restored = false;
        let mut original_result_end = None;
        let result = (|| {
            let pair = rustix::net::socketpair(
                rustix::net::AddressFamily::UNIX,
                rustix::net::SocketType::SEQPACKET,
                rustix::net::SocketFlags::CLOEXEC,
                None,
            )
            .map_err(|_| Cause::Io)?;
            sockets.extend([pair.0, pair.1]);
            let receiver = Channel::new(sockets[0].as_fd(), true, &mut owner)?;
            let sender = match Channel::new(sockets[1].as_fd(), false, &mut owner) {
                Ok(value) => value,
                Err(first) => {
                    let _ = receiver.finish(&mut owner);
                    return Err(first);
                }
            };
            let received = (|| {
                let clock = Clock::new(protocol::now()?)?;
                let nonce = "a".repeat(32);
                let end_ns = if matches!(case, Case::Late) {
                    clock.origin + 1
                } else {
                    protocol::now()? + 3_000_000_000
                };
                original_result_end = Some(Instant::now() + Duration::from_secs(4));
                let mut settled = Frame::new(Kind::Settled, &nonce, Trial::Normal, clock);
                settled.phase_deadline_ns = Some(end_ns);
                let mut terminal = Frame::new(Kind::Result, &nonce, Trial::Normal, clock);
                terminal.phase_deadline_ns = Some(if matches!(case, Case::ChangedEnd) {
                    end_ns - 1
                } else {
                    end_ns
                });
                terminal.phase = Phase::Complete;
                terminal.facts = Some(Facts {
                    private_go: true,
                    launcher_started: true,
                    echild: true,
                    launcher_raw_status: Some(if matches!(case, Case::Failed) { 1 } else { 0 }),
                    ..Facts::default()
                });
                if matches!(case, Case::Failed) {
                    terminal.failure = Some(Failure::Command);
                }
                if matches!(case, Case::Nonce) {
                    terminal.nonce = "b".repeat(32);
                }
                let send_end = owner.now() + Duration::from_secs(3);
                if !matches!(case, Case::Ordering) {
                    sender.send(&settled, &[], &mut owner, send_end, false)?;
                }
                if !matches!(case, Case::Missing) {
                    let rights = if matches!(case, Case::Rights) {
                        vec![sockets[1].as_fd()]
                    } else {
                        Vec::new()
                    };
                    sender.send(&terminal, &rights, &mut owner, send_end, false)?;
                }
                // These real datagrams are queued before an inert wrapper is
                // spawned and observed naturally terminal, without consuming it.
                // The harness sender is local; this proves no PID namespace.
                leader = Some(Leader::new(
                    command("/usr/bin/true").spawn().map_err(|_| Cause::Io)?,
                ));
                let peers = captures(leader.as_mut().ok_or(Cause::Io)?, &mut owner)?;
                output = Some(peers.0);
                error = Some(peers.1);
                while !leader.as_ref().ok_or(Cause::Io)?.terminal(&mut owner)? {
                    owner.check(Window::Body)?;
                    std::thread::sleep(Duration::from_millis(1));
                }
                let mut credential =
                    rustix::net::sockopt::socket_peercred(&sockets[0]).map_err(|_| Cause::Io)?;
                if matches!(case, Case::Credential) {
                    credential.gid =
                        rustix::process::Gid::from_raw(credential.gid.as_raw().wrapping_add(1));
                }
                let peer = Peer {
                    credential: Some(credential),
                    nonce: &nonce,
                    clock,
                    trial: Trial::Normal,
                };
                let mut slots = WaitSlots {
                    leader: &mut leader,
                    output: &mut output,
                    error: &mut error,
                    result: &mut facts,
                    natural_end: &mut natural_end,
                };
                wait_outer(
                    &receiver,
                    &peer,
                    &mut owner,
                    &mut Phase::Ready,
                    &mut slots,
                    &[],
                )
            })();
            flags_restored = sender.finish(&mut owner);
            flags_restored &= receiver.finish(&mut owner);
            received
        })();
        if let Err(first) = result {
            owner.record(first);
        }
        let natural = settle_wrapper(
            &mut leader,
            &mut output,
            &mut error,
            &mut owner,
            natural_end,
        );
        let first = owner.error();
        let reaped = leader
            .as_ref()
            .is_some_and(|child| child.consumed && child.status == Some(0));
        let pipes_closed = [&output, &error].iter().all(|slot| {
            slot.as_ref()
                .is_some_and(|capture| capture.eof && capture.closed)
        });
        let sockets_closed = identity::close_all(&mut owner, sockets);
        let deadline_retained = owner.end() == original_end
            && natural_end.is_none_or(|end| {
                original_result_end.is_some_and(|original| end < original && end <= original_end)
            });
        let closes = identity::CLOSED.with(|events| std::mem::take(&mut *events.borrow_mut()));
        let close_count = closes.len();
        let all_closes_passed = closes.into_iter().all(|closed| closed);
        let finished = owner.finish();
        Observed {
            result,
            first,
            facts,
            natural,
            reaped,
            pipes_closed,
            sockets_closed,
            flags_restored,
            deadline_retained,
            close_count,
            all_closes_passed,
            finished,
        }
    }

    #[test]
    fn queued_settled_and_result_survive_natural_wrapper_exit() {
        for case in [Case::Complete, Case::Failed] {
            let observed = queued(case);
            let expected = if matches!(case, Case::Failed) {
                Some(Cause::Command)
            } else {
                None
            };
            assert_eq!(observed.result, Ok(()));
            assert_eq!(observed.first, expected);
            let facts = observed.facts.unwrap();
            assert!(facts.private_go && facts.launcher_started && facts.echild);
            assert_eq!(
                facts.launcher_raw_status,
                Some(if expected.is_some() { 1 } else { 0 })
            );
            assert!(observed.natural && observed.reaped && observed.pipes_closed);
            assert!(
                observed.sockets_closed && observed.flags_restored && observed.deadline_retained
            );
            assert!(observed.all_closes_passed);
            assert_eq!(observed.close_count, 4);
            assert_eq!(observed.finished, expected.map_or(Ok(()), Err));
        }
    }

    #[test]
    fn queued_result_refusals_still_close_every_original() {
        for (case, expected) in [
            (Case::Missing, Cause::Command),
            (Case::Nonce, Cause::Policy),
            (Case::Credential, Cause::Policy),
            (Case::Rights, Cause::Policy),
            (Case::Ordering, Cause::Policy),
            (Case::Late, Cause::Deadline),
            (Case::ChangedEnd, Cause::Deadline),
        ] {
            let observed = queued(case);
            assert_eq!(observed.result, Err(expected));
            assert_eq!(observed.first, Some(expected));
            assert!(observed.facts.is_none() && observed.reaped && observed.pipes_closed);
            assert!(
                observed.sockets_closed && observed.flags_restored && observed.deadline_retained
            );
            assert!(observed.all_closes_passed);
            assert_eq!(
                observed.close_count,
                if matches!(case, Case::Rights) { 5 } else { 4 }
            );
            assert_eq!(observed.finished, Err(expected));
        }
    }
}
