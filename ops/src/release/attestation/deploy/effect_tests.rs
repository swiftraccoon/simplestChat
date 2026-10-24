//! Actual State/lock/file ownership with lower daemon completion observations.
//! These tests do not claim Docker, signatures or checked native pipe outcomes.

use super::*;
use crate::test_support::Fixture;
use sha2::Digest;
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)));
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    written.and(closed)
}
pub(super) struct Data {
    pub(super) _fixture: Fixture,
    pub(super) access: Access,
}
impl Data {
    pub(super) fn new() -> Self {
        let fixture = Fixture::new();
        let cached = super::super::super::verification_fixture::Fixture::new();
        let public = fixture.0.join("public");
        let config = fixture.0.join("config");
        let work = fixture.0.join("work");
        let revision = "a".repeat(40);
        for path in [
            &public,
            &config,
            &work,
            &public.join("results"),
            &public.join("releases"),
            &public.join("releases").join(&revision),
        ] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        for name in ["image.tar", "release.json"] {
            let bytes =
                super::super::super::verification_fixture::read(&cached.candidate.join(name))
                    .unwrap();
            write(
                &public.join("releases").join(&revision).join(name),
                &bytes,
                0o600,
            )
            .unwrap();
        }
        write(&config.join("app.env"), b"retained input\n", 0o600).unwrap();
        let docker = fixture.0.join("docker");
        write(&docker, b"#!/bin/sh\nexit 99\n", 0o700).unwrap();
        let curl = fixture.0.join("curl");
        write(&curl, b"#!/bin/sh\nexit 99\n", 0o700).unwrap();
        drop(cached);
        Self {
            _fixture: fixture,
            access: Access {
                root: public,
                config,
                work,
                docker,
                curl,
                socket: None,
                uid: rustix::process::geteuid().as_raw(),
                free_bytes: Some(u64::MAX),
                wall_seconds: Some(2524608000),
                fault: None,
                virtual_poll: false,
            },
        }
    }
}
#[derive(Clone, Copy)]
struct Reply {
    status: i32,
    error: Option<Cause>,
    settled: bool,
    natural: bool,
    valid_id: bool,
}
struct Daemon {
    docker: PathBuf,
    replies: Vec<Reply>,
    commands: Vec<(Role, Vec<OsString>, bool)>,
    valid: bool,
}
impl Executor for Daemon {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        let at = self.commands.len();
        self.valid &= request.program == self.docker
            && request.arguments.first().is_some_and(|v| v == "--host")
            && request
                .arguments
                .get(1)
                .is_some_and(|v| v == "unix:///var/run/docker.sock")
            && request.body_end > owner.now()
            && request.body_end < request.settlement_end
            && request.settlement_end <= owner.body_end()
            && request.input.is_none()
            && request.output.is_none();
        self.commands
            .push((request.role, request.arguments, request.cleanup));
        let reply = self.replies.get(at).copied().unwrap_or(Reply {
            status: 99,
            error: Some(Cause::Input),
            settled: false,
            natural: false,
            valid_id: false,
        });
        let stdout = if request.role == Role::AssetCreate {
            if reply.valid_id {
                format!("{}\n", "b".repeat(64)).into_bytes()
            } else {
                b"unknown\n".to_vec()
            }
        } else {
            Vec::new()
        };
        CommandOutput {
            stdout,
            stderr: Vec::new(),
            status: Some(reply.status),
            error: reply.error,
            settled: reply.settled,
            stdout_complete: reply.natural,
            bulk_bytes: 0,
            bulk_sha256: crate::release::attestation::files::hex(&sha2::Sha256::digest(b"")),
            bulk_complete: false,
        }
    }
}
struct Observed {
    admitted: Result<()>,
    create: Result<Vec<u8>>,
    remove: Option<Result<Vec<u8>>>,
    adopted: Option<String>,
    remaining: Option<String>,
    primary: Option<Cause>,
    diagnostic: Option<String>,
    commands: Vec<(Role, Vec<OsString>, bool)>,
    valid: bool,
    shared: (usize, usize),
    records: (usize, usize),
    lock_released: io::Result<bool>,
    finished: Result<()>,
}
fn operation(create: Reply, remove: Option<Reply>) -> Observed {
    let data = Data::new();
    let started = Instant::now();
    let mut owner =
        Owner::new(started, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    super::super::super::files::start_probe(None);
    super::super::files::start_probe();
    let mut state = State::new(&data.access);
    let admitted = (|| {
        state
            .stage
            .setup(&stage_access(&data.access), &"a".repeat(40), &mut owner)?;
        state.config = Some(Dir::open(&data.access.config, &mut owner)?);
        state.input("app.env", 64, &data.access, &mut owner)?;
        Ok(())
    })();
    let mut replies = vec![create];
    replies.extend(remove);
    let mut daemon = Daemon {
        docker: data.access.docker.clone(),
        replies,
        commands: Vec::new(),
        valid: true,
    };
    let (created, removed, adopted) = runtime.block_on(async {
        let created = if admitted.is_ok() {
            state
                .docker(
                    &data.access,
                    Role::AssetCreate,
                    argv(&["create", "--name", "fixed-owned-fixture"]),
                    false,
                    &mut owner,
                    &mut daemon,
                )
                .await
        } else {
            Err(Cause::Input)
        };
        let adopted = state.asset_id.clone();
        state.latch_failure(&owner);
        let removed = if let (Some(id), Some(_)) = (adopted.as_ref(), remove) {
            state.asset_remove_attempted = true;
            let recovery = created.is_err();
            let admitted = if recovery {
                owner.begin_deploy_recovery()
            } else {
                Ok(())
            };
            let result = if admitted.is_ok() {
                state
                    .docker(
                        &data.access,
                        Role::AssetRemove,
                        argv(&["rm", id]),
                        recovery,
                        &mut owner,
                        &mut daemon,
                    )
                    .await
            } else {
                Err(Cause::Deadline)
            };
            if recovery {
                owner.end_deploy_recovery();
            }
            Some(result)
        } else {
            None
        };
        state.stage.continue_commands(state.commands);
        state
            .stage
            .settle_known(
                &stage_access(&data.access),
                &mut owner,
                &mut StageExecutor(&mut daemon),
            )
            .await;
        (created, removed, adopted)
    });
    // Follow the actual outer finalizer order, including deferred normal logs.
    state
        .pending_logs
        .sort_unstable_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (at, bytes) in state.pending_logs.drain(..) {
        let _ = state.records.finish_log(at, &bytes, &mut owner, true);
    }
    let remaining = state.asset_id.clone();
    let diagnostic = state.failure.clone();
    state.stage.unlock(&mut owner);
    state.records.finish(&mut owner);
    if let Some(directory) = state.config.take() {
        directory.finish(&mut owner);
    }
    state.stage.finish(&mut owner);
    let lock_released = (|| {
        let file = fs::File::options()
            .read(true)
            .write(true)
            .open(data.access.work.join("workload.lock"))?;
        let acquired =
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok();
        let closed = nix::unistd::close(file).map_err(io::Error::from);
        closed.map(|()| acquired)
    })();
    let shared = super::super::super::files::take_probe();
    let records = super::super::files::take_probe();
    drop(runtime);
    let primary = owner.error();
    let finished = owner.finish();
    Observed {
        admitted,
        create: created,
        remove: removed,
        adopted,
        remaining,
        primary,
        diagnostic,
        commands: daemon.commands,
        valid: daemon.valid,
        shared,
        records,
        lock_released,
        finished,
    }
}
fn closed(observed: &Observed) {
    assert_eq!(observed.admitted, Ok(()));
    assert!(observed.valid);
    assert!(observed.shared.0 > 0 && observed.records.0 > 0);
    assert_eq!(observed.shared.1, 0);
    assert_eq!(observed.records.1, 0);
    assert!(observed.lock_released.as_ref().is_ok_and(|value| *value));
}

#[test]
fn known_create_survives_peer_close_but_later_remove_failure_cannot_replace_it() {
    let observed = operation(
        Reply {
            status: 0,
            error: Some(Cause::Close),
            settled: false,
            natural: true,
            valid_id: true,
        },
        Some(Reply {
            status: 7,
            error: None,
            settled: true,
            natural: true,
            valid_id: true,
        }),
    );
    closed(&observed);
    assert_eq!(observed.create, Err(Cause::Close));
    assert_eq!(observed.adopted.as_deref(), Some("b".repeat(64).as_str()));
    assert_eq!(observed.remaining, observed.adopted);
    assert_eq!(observed.remove, Some(Err(Cause::Command)));
    assert_eq!(observed.primary, Some(Cause::Close));
    assert_eq!(observed.diagnostic.as_deref(), Some("native_close_refused"));
    assert_eq!(observed.finished, Err(Cause::Close));
    assert_eq!(observed.commands.len(), 2);
    assert_eq!(observed.commands[0].0, Role::AssetCreate);
    assert_eq!(observed.commands[1].0, Role::AssetRemove);
    assert!(observed.commands[1].2);
    assert_eq!(
        observed.commands[1].1.last().unwrap(),
        &OsString::from("b".repeat(64))
    );
    assert_eq!(observed.commands[1].1.len(), 4);
}

#[test]
fn known_zero_removal_clears_owed_id_before_late_peer_close() {
    let observed = operation(
        Reply {
            status: 0,
            error: None,
            settled: true,
            natural: true,
            valid_id: true,
        },
        Some(Reply {
            status: 0,
            error: Some(Cause::Close),
            settled: false,
            natural: true,
            valid_id: true,
        }),
    );
    closed(&observed);
    assert!(observed.create.is_ok());
    assert!(observed.adopted.is_some());
    assert_eq!(observed.remove, Some(Err(Cause::Close)));
    assert_eq!(observed.remaining, None);
    assert_eq!(observed.primary, Some(Cause::Close));
    assert_eq!(observed.diagnostic.as_deref(), Some("native_close_refused"));
    assert_eq!(observed.finished, Err(Cause::Close));
    assert_eq!(observed.commands.len(), 2);
    assert!(!observed.commands[1].2);
}

#[test]
fn unknown_nonzero_or_cleanup_only_cid_never_grants_a_removal() {
    for reply in [
        Reply {
            status: 0,
            error: None,
            settled: true,
            natural: true,
            valid_id: false,
        },
        Reply {
            status: 7,
            error: Some(Cause::Command),
            settled: true,
            natural: true,
            valid_id: true,
        },
        Reply {
            status: 0,
            error: Some(Cause::Deadline),
            settled: false,
            natural: false,
            valid_id: true,
        },
    ] {
        let observed = operation(reply, None);
        closed(&observed);
        assert_eq!(observed.adopted, None);
        assert_eq!(observed.remaining, None);
        assert_eq!(observed.remove, None);
        assert_eq!(observed.commands.len(), 1);
        assert_eq!(observed.commands[0].0, Role::AssetCreate);
        assert_eq!(observed.primary, reply.error);
        assert_eq!(observed.finished, reply.error.map_or(Ok(()), Err));
    }
}

#[test]
fn an_earlier_peer_close_in_the_same_nonzero_completion_keeps_its_diagnostic() {
    let observed = operation(
        Reply {
            status: 7,
            error: Some(Cause::Close),
            settled: false,
            natural: true,
            valid_id: true,
        },
        None,
    );
    closed(&observed);
    assert_eq!(observed.create, Err(Cause::Close));
    assert_eq!(observed.adopted, None);
    assert_eq!(observed.remaining, None);
    assert_eq!(observed.remove, None);
    assert_eq!(observed.primary, Some(Cause::Close));
    assert_eq!(observed.diagnostic.as_deref(), Some("native_close_refused"));
    assert_eq!(observed.finished, Err(Cause::Close));
    assert_eq!(observed.commands.len(), 1);
    assert_eq!(observed.commands[0].0, Role::AssetCreate);
}
