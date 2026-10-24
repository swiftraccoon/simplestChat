//! Actual failed bulk writer witnesses cannot consume M or revoke known-ID cleanup.

use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{FileExt, MetadataExt, PermissionsExt},
};

struct Daemon {
    late: bool,
    roles: Vec<Role>,
    reads_before_refusal: u64,
    exact: bool,
}

const LIST_DUMP: &[u8] = b"PGDMP-retained-list-input";

struct BackupDaemon {
    list_status: i32,
    pair_close: bool,
    roles: Vec<Role>,
    dump_inode: Option<(u64, u64)>,
    input_matches: bool,
    closes_before_input: usize,
}
impl Executor for BackupDaemon {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        self.roles.push(request.role);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut status = 0;
        let mut error = None;
        let mut bulk_bytes = 0;
        let mut bulk_sha256 = super::super::super::files::hex(&Sha256::digest(b""));
        match request.role {
            Role::Size => stdout = b"1\n".to_vec(),
            Role::Dump => {
                if let Some(sink) = request.output {
                    let written = sink.file.write_all_at(LIST_DUMP, 0);
                    self.dump_inode = sink.file.metadata().ok().map(|m| (m.dev(), m.ino()));
                    if written.is_ok() && self.dump_inode.is_some() {
                        bulk_bytes = LIST_DUMP.len() as u64;
                        bulk_sha256 = super::super::super::files::hex(&Sha256::digest(LIST_DUMP));
                    } else {
                        error = Some(Cause::Io);
                    }
                } else {
                    error = Some(Cause::Input);
                }
            }
            Role::List => {
                let mut bytes = vec![0; LIST_DUMP.len()];
                self.input_matches = request.input.is_some_and(|input| {
                    input.read_exact_at(&mut bytes, 0).is_ok()
                        && bytes == LIST_DUMP
                        && input.metadata().ok().map(|m| (m.dev(), m.ino())) == self.dump_inode
                });
                if !self.input_matches {
                    error = Some(Cause::Changed);
                }
                self.closes_before_input = super::super::files::checked_closes();
                if self.pair_close {
                    super::super::files::fail_close(self.closes_before_input + 1);
                }
                stdout = b"LIST_STDOUT_PREFIX\n".to_vec();
                if self.list_status != 0 {
                    stderr = b"LIST_FAILED\n".to_vec();
                }
                status = self.list_status;
            }
            _ => error = Some(Cause::Input),
        }
        if let Some(cause) = error {
            owner.record(cause);
        }
        CommandOutput {
            stdout,
            stderr,
            status: Some(status),
            error,
            settled: true,
            stdout_complete: true,
            bulk_bytes,
            bulk_sha256,
            bulk_complete: error.is_none(),
        }
    }
}

#[test]
fn failed_list_retires_original_input_then_finishes_its_relocated_log_owners() {
    for pair_close in [false, true] {
        let data = effect_tests::Data::new();
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(1800),
            Duration::from_secs(60),
        )
        .unwrap();
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                owner.record(Cause::Io);
                let finished = owner.finish();
                drop(data);
                panic!("owned runtime unavailable after cleanup: {finished:?}");
            }
        };
        super::super::super::files::start_probe(None);
        super::super::files::start_probe();
        let mut state = State::new(&data.access);
        let admitted = state
            .stage
            .setup(&stage_access(&data.access), &"a".repeat(40), &mut owner)
            .and_then(|_| {
                state.config = Some(Dir::open(&data.access.config, &mut owner)?);
                state
                    .input("app.env", 1024, &data.access, &mut owner)
                    .map(|_| ())
            });
        let inputs_before = state.inputs.clone();
        let mut daemon = BackupDaemon {
            list_status: 7,
            pair_close,
            roles: Vec::new(),
            dump_inode: None,
            input_matches: false,
            closes_before_input: 0,
        };
        let mut report = json!({"phase":"preflight","passed":false});
        let result = runtime.block_on(async {
            admitted?;
            state
                .backup(
                    &data.access,
                    &"2".repeat(64),
                    &"a".repeat(40),
                    &mut report,
                    &mut owner,
                    &mut daemon,
                )
                .await
        });
        let input_closed = super::super::files::checked_closes() == daemon.closes_before_input + 1;
        let inputs_after = state.inputs.clone();
        let pending_names = state
            .pending_logs
            .iter()
            .map(|(at, _)| {
                state
                    .records
                    .files
                    .get(*at)
                    .map(|file| file.path().file_name().map(|name| name.to_os_string()))
            })
            .collect::<Vec<_>>();
        let writer = state.records.files.iter().find(|file| {
            file.path()
                .file_name()
                .is_some_and(|name| name == "database-before.dump.partial")
        });
        let writer_matches = writer
            .and_then(|file| file.file().metadata().ok())
            .is_some_and(|m| Some((m.dev(), m.ino())) == daemon.dump_inode);
        let attempt = state.stage.attempt().map(|dir| dir.path().to_path_buf());
        state
            .pending_logs
            .sort_unstable_by_key(|(at, _)| std::cmp::Reverse(*at));
        let log_results = state
            .pending_logs
            .drain(..)
            .map(|(at, bytes)| state.records.finish_log(at, &bytes, &mut owner, true))
            .collect::<Vec<_>>();
        let evidence = attempt.map(|path| {
            (
                fs::read(path.join("003.stdout")),
                fs::read(path.join("003.stderr")),
                fs::read(path.join("database-before.dump.partial")),
            )
        });
        let diagnostic = state.failure.clone();
        state.stage.unlock(&mut owner);
        state.records.finish(&mut owner);
        if let Some(directory) = state.config.take() {
            directory.finish(&mut owner);
        }
        state.stage.finish(&mut owner);
        let shared = super::super::super::files::take_probe();
        let records = super::super::files::take_probe();
        drop(runtime);
        let primary = owner.error();
        let finished = owner.finish();
        drop(data);
        // All facts are captured and owners finished; disarm the scoped fault
        // before this test thread can execute another filesystem fixture.
        super::super::files::start_probe();
        assert_eq!(result, Err(Cause::Command));
        assert!(report.get("backupSha256").is_none());
        assert_eq!(primary, Some(Cause::Command));
        assert_eq!(finished, Err(Cause::Command));
        assert_eq!(
            diagnostic.as_deref(),
            Some("Command 003 failed; inspect private output")
        );
        assert_eq!(daemon.roles, [Role::Size, Role::Dump, Role::List]);
        assert!(daemon.input_matches && input_closed && writer_matches);
        assert_eq!(inputs_before, inputs_after);
        assert_eq!(
            pending_names,
            [
                Some(Some(OsString::from("003.stderr"))),
                Some(Some(OsString::from("003.stdout")))
            ]
        );
        assert_eq!(log_results, [Ok(()), Ok(())]);
        let (stdout, stderr, dump) = evidence.unwrap();
        assert_eq!(stdout.unwrap(), b"LIST_STDOUT_PREFIX\n");
        assert_eq!(stderr.unwrap(), b"LIST_FAILED\n");
        assert_eq!(dump.unwrap(), LIST_DUMP);
        assert!(shared.0 > 0 && records.0 > 0);
        assert_eq!((shared.1, records.1), (0, 0));
    }
}

#[test]
fn validated_dump_digest_precedes_receipt_and_survives_failure_with_all_owners_closed() {
    use super::super::files::Fault;
    for (fault, renamed, digest_known) in [
        (Fault::BackupFileSync, false, false),
        (Fault::BackupRename, false, false),
        (Fault::BackupDirectorySync, true, false),
        (Fault::BackupReceiptWrite, true, true),
    ] {
        for pair_close in [false, true] {
            let data = effect_tests::Data::new();
            let mut owner = Owner::new(
                Instant::now(),
                Duration::from_secs(1800),
                Duration::from_secs(60),
            )
            .unwrap();
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => {
                    owner.record(Cause::Io);
                    let finished = owner.finish();
                    drop(data);
                    panic!("owned runtime unavailable after cleanup: {finished:?}");
                }
            };
            super::super::super::files::start_probe(None);
            super::super::files::start_probe();
            let mut state = State::new(&data.access);
            state.fault = Some(fault);
            let admitted = state
                .stage
                .setup(&stage_access(&data.access), &"a".repeat(40), &mut owner)
                .map(|_| ());
            let mut daemon = BackupDaemon {
                list_status: 0,
                pair_close: false,
                roles: Vec::new(),
                dump_inode: None,
                input_matches: false,
                closes_before_input: 0,
            };
            let mut report = json!({"phase":"preflight","passed":false});
            let result = runtime.block_on(async {
                admitted?;
                state
                    .backup(
                        &data.access,
                        &"2".repeat(64),
                        &"a".repeat(40),
                        &mut report,
                        &mut owner,
                        &mut daemon,
                    )
                    .await
            });
            state.latch_failure(&owner);
            let diagnostic = state.failure.clone();
            let before_closes = report.clone();
            let attempt = state.stage.attempt().map(|dir| dir.path().to_path_buf());
            let evidence = attempt.map(|path| {
                let name = if renamed {
                    "database-before.dump"
                } else {
                    "database-before.dump.partial"
                };
                (
                    fs::read(path.join(name)),
                    fs::symlink_metadata(path.join("database-before.receipt.json"))
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                )
            });
            let journal = fs::read(data.access.root.join("release-state.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            if pair_close {
                super::super::files::fail_close(super::super::files::checked_closes() + 1);
            }
            state.stage.unlock(&mut owner);
            state.records.finish(&mut owner);
            state.stage.finish(&mut owner);
            let shared = super::super::super::files::take_probe();
            let records = super::super::files::take_probe();
            drop(runtime);
            let primary = owner.error();
            let finished = owner.finish();
            drop(data);
            super::super::files::start_probe();
            assert_eq!(result, Err(Cause::Io), "{fault:?}/{pair_close}");
            assert_eq!(primary, Some(Cause::Io));
            assert_eq!(finished, Err(Cause::Io));
            assert_eq!(diagnostic.as_deref(), Some("OSError"));
            assert_eq!(report, before_closes);
            let expected_sha =
                Value::String(super::super::super::files::hex(&Sha256::digest(LIST_DUMP)));
            assert_eq!(
                report.get("backupSha256"),
                digest_known.then_some(&expected_sha)
            );
            assert_eq!(report["passed"], false);
            assert_eq!(report["phase"], "preflight");
            assert!(daemon.input_matches);
            assert_eq!(daemon.roles, [Role::Size, Role::Dump, Role::List]);
            let (dump, no_receipt) = evidence.unwrap();
            assert_eq!(dump.unwrap(), LIST_DUMP);
            assert!(no_receipt);
            let journal = journal.unwrap();
            assert_eq!(journal["phase"], "live_backup");
            assert_eq!(journal["finalized"], false);
            assert!(shared.0 > 0 && records.0 > 0);
            assert_eq!((shared.1, records.1), (0, 0));
        }
    }
}

impl Executor for Daemon {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        self.roles.push(request.role);
        let id = "c".repeat(64);
        let mut stdout = Vec::new();
        let mut bulk_bytes = 0;
        let mut bulk_sha256 = super::super::super::files::hex(&Sha256::digest(b""));
        let error = match request.role {
            Role::AssetCreate => {
                stdout = format!("{id}\n").into_bytes();
                None
            }
            Role::AssetCopy => {
                let mut observation = Some(Cause::Close);
                if let Some(sink) = request.output {
                    let bytes = b"retained original prefix";
                    match sink.file.write_at(bytes, 0) {
                        Ok(count) if count == bytes.len() => {
                            bulk_bytes = count as u64;
                            bulk_sha256 = super::super::super::files::hex(&Sha256::digest(bytes));
                        }
                        _ => observation = Some(Cause::Io),
                    }
                    if self.late {
                        owner.observe_at(owner.body_end());
                    } else if sink
                        .file
                        .set_permissions(fs::Permissions::from_mode(0o400))
                        .is_err()
                    {
                        observation = Some(Cause::Io);
                    }
                } else {
                    observation = Some(Cause::Input);
                }
                self.reads_before_refusal = super::super::files::digest_reads();
                observation
            }
            Role::AssetRemove => {
                self.exact &=
                    request.cleanup && request.arguments.last() == Some(&OsString::from(id));
                None
            }
            _ => Some(Cause::Input),
        };
        CommandOutput {
            stdout,
            stderr: Vec::new(),
            status: Some(0),
            error,
            settled: error.is_none(),
            stdout_complete: true,
            bulk_bytes,
            bulk_sha256,
            bulk_complete: true,
        }
    }
}

#[test]
fn failed_bulk_hash_stays_in_body_and_known_id_retirement_ignores_failed_writer() {
    for late in [false, true] {
        let data = effect_tests::Data::new();
        let started = Instant::now();
        let mut owner =
            Owner::new(started, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
        owner.elapsed = Some(Duration::ZERO);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        super::super::super::files::start_probe(None);
        super::super::files::start_probe();
        let mut state = State::new(&data.access);
        let admitted = state
            .stage
            .setup(&stage_access(&data.access), &"a".repeat(40), &mut owner);
        let mut daemon = Daemon {
            late,
            roles: Vec::new(),
            reads_before_refusal: 0,
            exact: true,
        };
        let mut removal_admitted = false;
        let mut reads_after_refusal = 0;
        let body: Result<()> = runtime.block_on(async {
            admitted?;
            state
                .docker(
                    &data.access,
                    Role::AssetCreate,
                    argv(&["create"]),
                    false,
                    &mut owner,
                    &mut daemon,
                )
                .await?;
            let writer = Record::create(state.attempt(), "assets.tar", &mut owner, false)?;
            state.records.files.push(writer);
            let index = state.records.files.len() - 1;
            let copied = state
                .command(
                    &data.access,
                    Role::AssetCopy,
                    argv(&["cp"]),
                    CommandIo::bulk(index, 1024),
                    &mut owner,
                    &mut daemon,
                )
                .await;
            reads_after_refusal = super::super::files::digest_reads();
            state.asset_remove_attempted = true;
            let admission = owner.begin_deploy_recovery();
            removal_admitted = admission.is_ok();
            if admission.is_ok() {
                let _ = state
                    .docker(
                        &data.access,
                        Role::AssetRemove,
                        argv(&["rm", &"c".repeat(64)]),
                        true,
                        &mut owner,
                        &mut daemon,
                    )
                    .await;
                owner.end_deploy_recovery();
            }
            copied.map(|_| ())
        });
        let facts = (
            state.asset_remove_attempted,
            state.asset_remove_admitted,
            state.asset_remove_dispatched,
            state.asset_remove_effected,
            state.asset_id.is_none(),
        );
        state
            .pending_logs
            .sort_unstable_by_key(|(at, _)| std::cmp::Reverse(*at));
        for (at, bytes) in state.pending_logs.drain(..) {
            let _ = state.records.finish_log(at, &bytes, &mut owner, true);
        }
        state.stage.unlock(&mut owner);
        state.records.finish(&mut owner);
        state.stage.finish(&mut owner);
        let shared = super::super::super::files::take_probe();
        let records = super::super::files::take_probe();
        drop(runtime);
        let primary = owner.error();
        let finished = owner.finish();
        drop(data);
        assert_eq!(body, Err(Cause::Close));
        assert_eq!(primary, Some(Cause::Close));
        assert_eq!(finished, Err(Cause::Close));
        assert!(shared.0 > 0 && records.0 > 0);
        assert_eq!((shared.1, records.1), (0, 0));
        assert!(daemon.exact);
        assert_eq!(reads_after_refusal, daemon.reads_before_refusal);
        if late {
            assert!(!removal_admitted);
            assert_eq!(facts, (true, false, false, false, false));
            assert_eq!(daemon.roles, [Role::AssetCreate, Role::AssetCopy]);
        } else {
            assert!(removal_admitted);
            assert_eq!(facts, (true, true, true, true, true));
            assert_eq!(
                daemon.roles,
                [Role::AssetCreate, Role::AssetCopy, Role::AssetRemove]
            );
        }
    }
}
