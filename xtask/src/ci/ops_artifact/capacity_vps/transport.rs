//! Framed finite SSH requests and the concrete paired preparation capability.

use super::{
    Result,
    execute::{Shared, lock, stage},
    failure,
    files::{Input, Pending},
    plan::Plan,
    prepare::PrepareInvocation,
    worker::Checkpoint,
};
use crate::ci::{
    command::{Command, Program},
    process::{self, Executor},
};
use simplestchat_ops::capacity_vps::protocol::{
    Action, Envelope, Header, Mode, Pin, Response, Unit,
};
use std::{
    ffi::OsString,
    sync::Arc,
    time::{Duration, Instant},
};

struct Prepared {
    command: Option<Command>,
    proof: Arc<super::tools::Invocation>,
    pending: Option<Pending>,
    frame: Option<Input>,
    header: Option<Header>,
    _permits: Vec<super::super::namespace::Permit>,
}
struct Call {
    program: Program,
    args: Vec<OsString>,
    wire: Option<(Header, Vec<u8>)>,
    body: Duration,
    grace: Duration,
    maximum: u64,
    capture_class: CaptureClass,
}
#[derive(Clone, Copy)]
enum CaptureClass {
    Ordinary,
    Cleanup,
    Archive,
}

fn account(
    state: &mut super::execute::State,
    seals: &super::tools::Captures,
    class: CaptureClass,
) -> Result<()> {
    let ordinary = match class {
        CaptureClass::Ordinary => seals.stdout.bytes.checked_add(seals.stderr.bytes),
        CaptureClass::Cleanup => Some(0),
        CaptureClass::Archive => Some(seals.stderr.bytes),
    }
    .ok_or_else(|| failure(1))?;
    state.captures = state
        .captures
        .checked_add(ordinary)
        .ok_or_else(|| failure(1))?;
    if matches!(class, CaptureClass::Cleanup) {
        state.cleanup_captures = state
            .cleanup_captures
            .checked_add(seals.stdout.bytes)
            .and_then(|n| n.checked_add(seals.stderr.bytes))
            .ok_or_else(|| failure(1))?;
    }
    if state.captures > 248 * 1024 * 1024
        || state.cleanup_captures > 8 * 1024 * 1024
        || (matches!(class, CaptureClass::Archive) && seals.stdout.bytes > super::MAX_ARCHIVE)
    {
        return Err(failure(1));
    }
    Ok(())
}
pub(super) struct Observed {
    pub header: Option<Header>,
    pub bytes: Vec<u8>,
    pub stdout: String,
    pub unavailable: bool,
}
impl Observed {
    pub fn response(&self, action: Action) -> Result<Response> {
        if self.unavailable {
            return Err(failure(1));
        }
        Response::decode(
            &self.bytes,
            self.header.as_ref().ok_or_else(|| failure(1))?,
            action,
        )
        .map_err(|_| failure(1))
    }
}
fn header(
    state: &super::execute::State,
    action: Action,
    mode: Mode,
    unit: Option<Unit>,
) -> Result<(Header, Vec<u8>)> {
    let envelope = if action == Action::Lookup {
        Envelope::lookup()
    } else {
        Envelope::request(action, state.request.as_slice(), unit).map_err(|_| failure(1))?
    };
    let request = envelope.encode().map_err(|_| failure(1))?;
    let transfer = state.transfer.as_ref().ok_or_else(|| failure(1))?;
    let header = Header {
        schema_version: 1,
        scope: "OpsCapacityBootstrap".into(),
        mode,
        run: state.run.clone(),
        selection: state.selection.clone().ok_or_else(|| failure(1))?,
        artifact_id: state.options.artifact,
        transfer: Pin {
            sha256: transfer.sha256().into(),
            bytes: transfer.bytes(),
        },
        members: state.members.clone().ok_or_else(|| failure(1))?,
        request: Pin {
            sha256: super::super::sha(&request),
            bytes: request.len() as u64,
        },
    };
    envelope.bind(&header).map_err(|_| failure(1))?;
    Ok((header, request))
}
pub(super) async fn git<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    args: Vec<OsString>,
) -> Result<Vec<u8>> {
    call(
        state,
        plan,
        executor,
        end,
        Call {
            program: Program::OpsVpsGit,
            args,
            wire: None,
            body: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            maximum: super::MAX_JSON,
            capture_class: CaptureClass::Ordinary,
        },
    )
    .await
    .map(|v| v.bytes)
}
pub(super) async fn final_git<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    args: Vec<OsString>,
) -> Result<Vec<u8>> {
    call(
        state,
        plan,
        executor,
        end,
        Call {
            program: Program::OpsVpsGit,
            args,
            wire: None,
            body: Duration::from_secs(1),
            grace: Duration::from_secs(1),
            maximum: super::MAX_JSON,
            capture_class: CaptureClass::Ordinary,
        },
    )
    .await
    .map(|value| value.bytes)
}
pub(super) async fn rpc<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    action: Action,
    mode: Mode,
    unit: Option<Unit>,
) -> Result<Observed> {
    let (header, request, args) = {
        let state = lock(state)?;
        let (header, request) = header(&state, action, mode, unit)?;
        let args = state
            .inventory
            .as_ref()
            .ok_or_else(|| failure(1))?
            .target
            .ssh(
                state
                    .sources
                    .as_ref()
                    .ok_or_else(|| failure(1))?
                    .bootstrap()?,
            );
        (header, request, args)
    };
    let maximum = if action == Action::Archive {
        super::MAX_ARCHIVE
    } else if matches!(action, Action::Status | Action::Reserve) {
        super::MAX_STATUS
    } else {
        super::MAX_JSON
    };
    let body = Duration::from_secs(if mode == Mode::Stage || action == Action::Archive {
        300
    } else if action == Action::Recover {
        600
    } else {
        60
    });
    call(
        state,
        plan,
        executor,
        end,
        Call {
            program: Program::OpsVpsSsh,
            args,
            wire: Some((header, request)),
            body,
            grace: Duration::from_secs(5),
            maximum,
            capture_class: if action == Action::Archive {
                CaptureClass::Archive
            } else if action == Action::Retire {
                CaptureClass::Cleanup
            } else {
                CaptureClass::Ordinary
            },
        },
    )
    .await
}
async fn call<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    call: Call,
) -> Result<Observed> {
    let Call {
        program,
        args,
        wire,
        body,
        grace,
        maximum,
        capture_class,
    } = call;
    executor.ready().await?;
    plan.check()?;
    let (index, output) = {
        let mut state = lock(state)?;
        state.index = state.index.checked_add(1).ok_or_else(|| failure(1))?;
        let index = state.index;
        let output = state.output.take().ok_or_else(|| failure(1))?;
        (index, output)
    };
    let pending = output.capture(index, maximum).await;
    lock(state)?.output = Some(output);
    let mut pending = pending?;
    let args_for_worker = args.clone();
    let (frame, header) = if let Some((header, request)) = wire {
        let selected = header.clone();
        let framed = stage(
            state,
            plan,
            executor,
            end,
            move |state, checkpoint| {
                let mut bytes = selected.encode().map_err(|_| failure(1))?;
                bytes.push(b'\n');
                let transfer = if selected.mode == Mode::Stage {
                    state.transfer.as_ref()
                } else {
                    None
                };
                state.output.as_mut().ok_or_else(|| failure(1))?.frame(
                    &format!("frame-{index}.bin"),
                    &bytes,
                    transfer,
                    &request,
                    checkpoint,
                )
            },
            |input, checkpoint| input.finish(checkpoint),
        )
        .await;
        match framed {
            Ok(frame) => (Some(frame), Some(header)),
            Err(error) => {
                settle_pending(pending).await;
                return Err(error);
            }
        }
    } else {
        (None, None)
    };
    let (body, latest) = match plan.dispatch(end, body, grace) {
        Ok(v) => v,
        Err(error) => {
            settle_pending(pending).await;
            let _ = finish_frame(frame, plan, executor, end).await;
            return Err(error);
        }
    };
    let built = (|| {
        let state = lock(state)?;
        let permits = state.budget.reserve_with_limit(16, 96)?;
        let input = frame.as_ref().map(Input::stamp).cloned();
        let proof = state.tools.as_ref().ok_or_else(|| failure(1))?.invocation(
            super::tools::Spec {
                program,
                args: args_for_worker,
                body,
                grace,
                maximum,
                input,
                end,
                latest,
            },
            plan,
        )?;
        let stdin = frame
            .as_ref()
            .map(|input| {
                input
                    .clone_input(&Checkpoint {
                        clock: plan.clock.clone(),
                        start: plan.start,
                        end,
                        abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    })
                    .map(Arc::new)
            })
            .transpose()?;
        Ok((proof, stdin, permits))
    })();
    let (proof, stdin, permits) = match built {
        Ok(value) => value,
        Err(error) => {
            settle_pending(pending).await;
            let _ = finish_frame(frame, plan, executor, end).await;
            return Err(error);
        }
    };
    let mut command = Command::capture(program, &[]);
    command.capture = None;
    command.arguments = args;
    command.timeout = body;
    command.grace = grace;
    command.ops_vps = Some(proof.clone());
    command.separate_logs = pending.logs.take();
    command.stdin = stdin;
    let mut prepared = Prepared {
        command: Some(command),
        proof,
        pending: Some(pending),
        frame,
        header,
        _permits: permits,
    };
    let result = executor.run(prepared.command.take().unwrap()).await;
    let owner_pending = executor.ready().await.err();
    let consumed = prepared.frame.as_ref().map(Input::consumed).transpose();
    let exit = prepared.proof.exit();
    let exit_data = exit.as_ref().ok().and_then(|v| serde_json::to_vec(v).ok());
    let saved_exit = if let Some(exit_data) = exit_data {
        stage(
            state,
            plan,
            executor,
            end,
            move |state, checkpoint| {
                state.output.as_mut().ok_or_else(|| failure(1))?.write(
                    &format!("process-{index}.exit.json"),
                    &exit_data,
                    checkpoint,
                )
            },
            |_, _| Ok(()),
        )
        .await
    } else {
        Ok(())
    };
    let seals = prepared.proof.captures();
    let settled = prepared.proof.settled();
    let pending = prepared.pending.take().unwrap();
    let stdout = pending.names[0].clone();
    let stderr = pending.names[1].clone();
    let read = match seals {
        Ok(seals) => {
            stage(
                state,
                plan,
                executor,
                end,
                move |state, checkpoint| {
                    let accounted = account(state, &seals, capture_class);
                    let bytes = state
                        .output
                        .as_mut()
                        .ok_or_else(|| failure(1))?
                        .capture_read(pending, &seals, checkpoint, maximum != super::MAX_ARCHIVE)?;
                    let error = state.output.as_ref().unwrap().read(&stderr, checkpoint)?;
                    accounted?;
                    Ok((bytes, error))
                },
                |_, _| Ok(()),
            )
            .await
        }
        Err(error) => Err(error),
    };
    let close = finish_frame(prepared.frame.take(), plan, executor, end).await;
    let unavailable = read.as_ref().is_ok_and(|(bytes, error)| {
        result.as_ref().err().is_some_and(|e| e.code == 1)
            && bytes.is_empty()
            && error == b"ops_capacity_unavailable\n"
            && exit
                .as_ref()
                .is_ok_and(|exit| exit.status == Some(1) && exit.settled)
    });
    let mut primary = if unavailable {
        None
    } else {
        result.err().map(|error| {
            let remote_code = match read.as_ref().ok().map(|(_, error)| error.as_slice()) {
                Some(b"ops_capacity_deadline\n") => Some(124),
                Some(b"ops_capacity_interrupted\n") => Some(130),
                Some(b"ops_capacity_terminated\n") => Some(143),
                _ => None,
            };
            if (error.code == 124 && exit.as_ref().map_or(true, |exit| exit.status != Some(124)))
                || owner_pending
                    .as_ref()
                    .is_some_and(|owner| owner.code == error.code)
                || (remote_code == Some(error.code)
                    && exit
                        .as_ref()
                        .is_ok_and(|exit| exit.status == Some(i32::from(error.code))))
            {
                error
            } else {
                failure(1)
            }
        })
    };
    if let Some(owner) = owner_pending {
        super::keep(&mut primary, owner);
    }
    if !unavailable
        && exit
            .as_ref()
            .is_ok_and(|exit| exit.status != Some(0) || !exit.settled)
    {
        super::keep(&mut primary, failure(1));
    }
    if exit.is_err() {
        super::keep(&mut primary, failure(1));
    }
    for checked in [consumed.map(|_| ()), settled, saved_exit, close] {
        if let Err(error) = checked {
            super::keep(&mut primary, error);
        }
    }
    let read = match read {
        Ok(value) => Some(value),
        Err(error) => {
            super::keep(&mut primary, error);
            None
        }
    };
    if let Some(error) = primary {
        return Err(error);
    }
    let (bytes, _) = read.ok_or_else(|| failure(1))?;
    executor.ready().await?;
    plan.check()?;
    Ok(Observed {
        header: prepared.header,
        bytes,
        stdout,
        unavailable,
    })
}
async fn settle_pending(mut pending: Pending) {
    let mut command = Command::capture(Program::OpsVpsGit, &[]);
    command.capture = None;
    command.separate_logs = pending.logs.take();
    let _ = process::settle_unforwarded(command).await;
}
async fn finish_frame<E: Executor>(
    frame: Option<Input>,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<()> {
    if let Some(frame) = frame {
        super::worker::work(
            plan,
            executor,
            end,
            move |checkpoint| frame.finish(&checkpoint),
            |_| Ok(()),
        )
        .await
    } else {
        Ok(())
    }
}

async fn cleanup_setup<E: Executor>(
    pending: Vec<Pending>,
    frame: Option<Input>,
    reader: Option<super::files::ReadyReader>,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) {
    for pending in pending {
        settle_pending(pending).await;
    }
    if let Some(reader) = reader {
        let _ = reader.finish();
    }
    let _ = finish_frame(frame, plan, executor, end).await;
}
pub(super) async fn preparation<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<()> {
    let (header, request, ssh, ansible) = {
        let state = lock(state)?;
        let (header, request) = header(&state, Action::Reserve, Mode::Invoke, None)?;
        let sources = state.sources.as_ref().ok_or_else(|| failure(1))?;
        let inventory = state.inventory.as_ref().ok_or_else(|| failure(1))?;
        let ssh = inventory.target.ssh(sources.bootstrap()?);
        let ansible = vec![
            "-i".into(),
            state
                .output
                .as_ref()
                .unwrap()
                .path("inventory.snapshot.json")
                .into_os_string(),
            sources
                .directory()
                .join("ops/ansible/site.yml")
                .into_os_string(),
            "--limit".into(),
            state.options.limit.clone().into(),
            "--extra-vars".into(),
            serde_json::to_string(&serde_json::json!({
                "scbench_revision": state.options.revision,
                "scbench_upgrade_packages": false,
                "scbench_reboot": false,
            }))
            .map_err(|_| failure(1))?
            .into(),
            "--tags".into(),
            "source,benchmark".into(),
            "--extra-vars".into(),
            serde_json::to_string(&serde_json::json!({
                "ansible_ssh_common_args": inventory.target.common(),
            }))
            .map_err(|_| failure(1))?
            .into(),
        ];
        (header, request, ssh, ansible)
    };
    let (index, output) = {
        let mut state = lock(state)?;
        let index = state.index.checked_add(1).ok_or_else(|| failure(1))?;
        state.index = state.index.checked_add(2).ok_or_else(|| failure(1))?;
        (index, state.output.take().ok_or_else(|| failure(1))?)
    };
    let lease_pending = output.capture(index, super::MAX_STATUS).await;
    let mut lease_pending = match lease_pending {
        Ok(pending) => pending,
        Err(error) => {
            lock(state)?.output = Some(output);
            return Err(error);
        }
    };
    let ansible_pending = output.capture(index + 1, super::MAX_JSON).await;
    lock(state)?.output = Some(output);
    let mut ansible_pending = match ansible_pending {
        Ok(pending) => pending,
        Err(error) => {
            settle_pending(lease_pending).await;
            return Err(error);
        }
    };
    let frame_header = header.clone();
    let frame = stage(
        state,
        plan,
        executor,
        end,
        move |state, checkpoint| {
            let mut bytes = frame_header.encode().map_err(|_| failure(1))?;
            bytes.push(b'\n');
            state.output.as_mut().unwrap().frame(
                &format!("frame-{index}.bin"),
                &bytes,
                None,
                &request,
                checkpoint,
            )
        },
        |input, checkpoint| input.finish(checkpoint),
    )
    .await;
    let frame = match frame {
        Ok(frame) => frame,
        Err(error) => {
            cleanup_setup(
                vec![lease_pending, ansible_pending],
                None,
                None,
                plan,
                executor,
                end,
            )
            .await;
            return Err(error);
        }
    };
    let reader = lock(state)?
        .output
        .as_ref()
        .unwrap()
        .ready_reader(&lease_pending);
    let reader = match reader {
        Ok(reader) => reader,
        Err(error) => {
            cleanup_setup(
                vec![lease_pending, ansible_pending],
                Some(frame),
                None,
                plan,
                executor,
                end,
            )
            .await;
            return Err(error);
        }
    };
    let built = (|| {
        let state = lock(state)?;
        let permits = state.budget.reserve_with_limit(32, 80)?;
        let lease_schedule =
            plan.dispatch(end, Duration::from_secs(1750), Duration::from_secs(5))?;
        let ansible_schedule =
            plan.dispatch(end, Duration::from_secs(1500), Duration::from_secs(5))?;
        let lease_proof = state.tools.as_ref().unwrap().invocation(
            super::tools::Spec {
                program: Program::OpsVpsSsh,
                args: ssh.clone(),
                body: lease_schedule.0,
                grace: Duration::from_secs(5),
                maximum: super::MAX_STATUS,
                input: Some(frame.stamp().clone()),
                end,
                latest: lease_schedule.1,
            },
            plan,
        )?;
        let ansible_proof = state.tools.as_ref().unwrap().invocation(
            super::tools::Spec {
                program: Program::OpsVpsAnsible,
                args: ansible.clone(),
                body: ansible_schedule.0,
                grace: Duration::from_secs(5),
                maximum: super::MAX_JSON,
                input: None,
                end,
                latest: ansible_schedule.1,
            },
            plan,
        )?;
        let input = Arc::new(frame.clone_input(&Checkpoint {
            clock: plan.clock.clone(),
            start: plan.start,
            end,
            abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })?);
        Ok((
            lease_proof,
            ansible_proof,
            input,
            permits,
            lease_schedule.0,
            ansible_schedule.0,
        ))
    })();
    let (lease_proof, ansible_proof, input, _permits, lease_body, ansible_body) = match built {
        Ok(value) => value,
        Err(error) => {
            cleanup_setup(
                vec![lease_pending, ansible_pending],
                Some(frame),
                Some(reader),
                plan,
                executor,
                end,
            )
            .await;
            return Err(error);
        }
    };
    let mut lease = Command::capture(Program::OpsVpsSsh, &[]);
    lease.arguments = ssh;
    lease.capture = None;
    lease.timeout = lease_body;
    lease.grace = Duration::from_secs(5);
    lease.ops_vps = Some(lease_proof.clone());
    lease.separate_logs = lease_pending.logs.take();
    lease.stdin = Some(input);
    let mut command = Command::capture(Program::OpsVpsAnsible, &[]);
    command.arguments = ansible;
    command.capture = None;
    command.timeout = ansible_body;
    command.grace = Duration::from_secs(5);
    command.ops_vps = Some(ansible_proof.clone());
    command.separate_logs = ansible_pending.logs.take();
    let prepared = PrepareInvocation::new(
        lease,
        command,
        header,
        reader,
        [lease_proof.clone(), ansible_proof.clone()],
    );
    let completion = prepared.completion();
    let result = executor.vps_prepare(prepared, end).await;
    let consumed = frame.consumed();
    let exits = serde_json::to_vec(&[lease_proof.exit().ok(), ansible_proof.exit().ok()])
        .map_err(|_| failure(1));
    let saved_exits = match exits {
        Ok(data) => {
            stage(
                state,
                plan,
                executor,
                end,
                move |state, checkpoint| {
                    state.output.as_mut().ok_or_else(|| failure(1))?.write(
                        "preparation.exit.json",
                        &data,
                        checkpoint,
                    )
                },
                |_, _| Ok(()),
            )
            .await
        }
        Err(error) => Err(error),
    };
    let seals = [lease_proof.captures(), ansible_proof.captures()];
    let read = match seals {
        [Ok(lease), Ok(ansible)] => {
            stage(
                state,
                plan,
                executor,
                end,
                move |state, checkpoint| {
                    let first = state.output.as_mut().unwrap().capture_read(
                        lease_pending,
                        &lease,
                        checkpoint,
                        true,
                    );
                    let second = state.output.as_mut().unwrap().capture_read(
                        ansible_pending,
                        &ansible,
                        checkpoint,
                        false,
                    );
                    let total = lease
                        .stdout
                        .bytes
                        .checked_add(lease.stderr.bytes)
                        .and_then(|n| n.checked_add(ansible.stdout.bytes))
                        .and_then(|n| n.checked_add(ansible.stderr.bytes))
                        .and_then(|n| state.captures.checked_add(n))
                        .ok_or_else(|| failure(1));
                    first?;
                    second?;
                    state.captures = total?;
                    if state.captures > 248 * 1024 * 1024 {
                        return Err(failure(1));
                    }
                    Ok(())
                },
                |_, _| Ok(()),
            )
            .await
        }
        _ => Err(failure(1)),
    };
    let closed = finish_frame(Some(frame), plan, executor, end).await;
    let mut primary = result.err();
    for checked in [consumed, read, saved_exits, closed] {
        if let Err(error) = checked {
            super::keep(&mut primary, error);
        }
    }
    if !completion.passed() {
        super::keep(&mut primary, failure(1));
    }
    if let Some(error) = primary {
        return Err(error);
    }
    executor.ready().await?;
    plan.check()
}
