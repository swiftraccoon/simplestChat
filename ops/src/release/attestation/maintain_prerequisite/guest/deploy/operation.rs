//! The complete prepared/continue and two-native-call transaction.

use super::*;
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

pub(in super::super) fn read_record(
    ctx: &mut Context,
    path: &Path,
    mode: u32,
    maximum: u64,
) -> Result<Value> {
    let mut original = Original::open(path, maximum, 0, mode, &mut ctx.control)?;
    let body = original
        .data(maximum, &mut ctx.control)
        .and_then(|bytes| decode(&bytes));
    if let Err(error) = body {
        ctx.control.record(error);
    }
    let closed = original.finish(&mut ctx.control);
    body.and_then(|value| closed.map(|()| value))
}
fn retained(ctx: &mut Context, path: &Path, maximum: u64) -> Result<(usize, Vec<u8>)> {
    let index = ctx.retain(path, maximum, 0, 0o600)?;
    ctx.originals[index].require_parent(0, 0o700, &mut ctx.control)?;
    Ok((index, ctx.originals[index].data(maximum, &mut ctx.control)?))
}
fn names(path: &Path) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(|_| Error::Io)? {
        let name = entry
            .map_err(|_| Error::Io)?
            .file_name()
            .into_string()
            .map_err(|_| Error::Refused)?;
        require(result.len() < 4096 && result.insert(name))?;
    }
    Ok(result)
}
pub(super) fn selection(ctx: &mut Context) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut selected = BTreeMap::new();
    for name in ["compose.public.yml", "app.env", "images.json"] {
        let mut original = Original::open(
            &Path::new(CONFIG).join(name),
            1024 * 1024,
            0,
            0o600,
            &mut ctx.control,
        )?;
        let body = original.data(1024 * 1024, &mut ctx.control);
        if let Err(error) = body {
            ctx.control.record(error);
        }
        let closed = original.finish(&mut ctx.control);
        selected.insert(name.into(), body.and_then(|value| closed.map(|()| value))?);
    }
    Ok(selected)
}
pub(super) fn attempt(before: &BTreeSet<String>) -> Result<PathBuf> {
    let fresh = names(&Path::new(PUBLIC).join("results"))?
        .difference(before)
        .cloned()
        .collect::<Vec<_>>();
    require(fresh.len() == 1)?;
    let name = &fresh[0];
    require(
        name.strip_prefix("release.")
            .is_some_and(|suffix| hex(suffix, 32)),
    )?;
    let path = Path::new(PUBLIC).join("results").join(name);
    let meta = fs::symlink_metadata(&path).map_err(|_| Error::Io)?;
    require(meta.is_dir() && meta.uid() == 0 && meta.gid() == 0 && meta.mode() & 0o7777 == 0o700)?;
    Ok(path)
}
fn stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
fn migration_digest(migrations: &BTreeMap<String, String>) -> Result<String> {
    let entries = migrations
        .iter()
        .map(|(version, checksum)| {
            Ok(format!(
                "{}: {}",
                serde_json::to_string(version).map_err(|_| Error::Input)?,
                serde_json::to_string(checksum).map_err(|_| Error::Input)?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(hash(format!("{{{}}}", entries.join(", ")).as_bytes()))
}
impl State {
    async fn probe(&mut self, ctx: &mut Context) -> Result<Value> {
        let migrations = &self
            .payloads
            .as_ref()
            .ok_or(Error::Refused)?
            .current
            .manifest
            .fields()
            .migrations;
        self.fixture
            .deploy_probe(
                ctx,
                self.current_image.as_deref().ok_or(Error::Refused)?,
                migrations,
            )
            .await
    }
    async fn backup(
        &mut self,
        ctx: &mut Context,
        attempt: &Path,
        report: &Value,
        target: &str,
    ) -> Result<Value> {
        use std::os::unix::fs::FileExt;
        let index = ctx.retain(
            &attempt.join("database-before.dump"),
            4 * 1024 * 1024 * 1024,
            0,
            0o600,
        )?;
        ctx.originals[index].require_parent(0, 0o700, &mut ctx.control)?;
        let (_, receipt) = retained(ctx, &attempt.join("database-before.receipt.json"), 131072)?;
        let receipt = decode(&receipt)?;
        let mut header = [0u8; 5];
        require(
            ctx.originals[index]
                .file()?
                .read_at(&mut header, 0)
                .map_err(|_| Error::Io)?
                == 5,
        )?;
        validation::backup(
            &receipt,
            ctx.originals[index].bytes,
            &ctx.originals[index].sha256,
            &header,
            report["backupSha256"].as_str().ok_or(Error::Refused)?,
            target,
        )?;
        let pg = self.baseline.as_ref().ok_or(Error::Refused)?.pg.clone();
        let args: Vec<String> = [
            "docker",
            "--host",
            "unix:///var/run/docker.sock",
            "exec",
            "--interactive",
            "--user",
            "999:999",
            &pg,
            "timeout",
            "--signal=TERM",
            "--kill-after=2s",
            "15s",
            "pg_restore",
            "--list",
        ]
        .map(str::to_owned)
        .into();
        ctx.check()?;
        let listed = ctx
            .commands
            .execute_input(
                &mut ctx.control,
                "deploy-backup-list",
                &args,
                ctx.originals[index].file()?,
                Duration::from_secs(20),
            )
            .await?;
        require(
            listed.status == 0
                && listed
                    .stdout
                    .windows(b"TABLE DATA".len())
                    .any(|bytes| bytes == b"TABLE DATA")
                && listed
                    .stdout
                    .windows(b"release_fixture_marker".len())
                    .any(|bytes| bytes == b"release_fixture_marker"),
        )?;
        ctx.originals[index].check(&mut ctx.control)?;
        match fs::symlink_metadata(attempt.join("database-before.dump.partial")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(Error::Refused),
        }
        Ok(
            json!({"bytes":ctx.originals[index].bytes,"sha256":ctx.originals[index].sha256,
            "listed":true,"restoreRehearsed":false}),
        )
    }
    async fn continuity(&mut self, ctx: &mut Context) -> Result<Value> {
        let payloads = self.payloads.as_ref().ok_or(Error::Refused)?;
        self.fixture
            .deploy_continuity(
                ctx,
                self.baseline.as_ref().ok_or(Error::Refused)?,
                &self.request.revision,
                self.current_image.as_deref().ok_or(Error::Refused)?,
                &payloads.current.manifest.fields().migrations,
            )
            .await
    }
    async fn invoke(&mut self, ctx: &mut Context, negative: bool) -> Result<Value> {
        let started = Instant::now();
        require(
            ctx.control.body_end().saturating_duration_since(started) >= Duration::from_secs(1840),
        )?;
        self.trial = Some(if negative { "negative" } else { "positive" });
        self.stdout = native_report::Capture::not_observed();
        self.outcome = native_report::Capture::not_observed();
        let before = names(&Path::new(PUBLIC).join("results"))?;
        let selected = selection(ctx)?;
        let since = stamp();
        let target = if negative {
            self.request.previous_revision.clone()
        } else {
            self.request.revision.clone()
        };
        let mut injector = if negative {
            Some(injection::Injector::new(
                ctx,
                self.baseline.as_ref().ok_or(Error::Refused)?,
                self.first_image.as_deref().ok_or(Error::Refused)?,
                self.current_image.as_deref().ok_or(Error::Refused)?,
                self.fixture.deploy_app()?,
                &before,
                &selected,
            )?)
        } else {
            None
        };
        let spawned = (|| {
            ctx.check_full()?;
            self.fixture.native_boundary(ctx)?;
            let args = if let Some(injector) = &mut injector {
                injector.arguments(ctx, &target)?
            } else {
                vec![
                    "ops".into(),
                    "release".into(),
                    "deploy".into(),
                    "--revision".into(),
                    target.clone(),
                    "--quiet-seconds".into(),
                    "0".into(),
                    "--timeout-seconds".into(),
                    "1800".into(),
                    "--finalization-seconds".into(),
                    "60".into(),
                ]
            };
            if negative {
                ctx.commands.spawn_deploy_negative(
                    &mut ctx.control,
                    &args,
                    Duration::from_secs(1810),
                )
            } else {
                ctx.commands.spawn(
                    &mut ctx.control,
                    "deploy-positive",
                    &args,
                    Duration::from_secs(1810),
                )
            }
        })();
        let mut running = match spawned {
            Ok(child) => child,
            Err(error) => {
                ctx.control.record(error);
                if let Some(injector) = &mut injector {
                    let _ = injector.finish(ctx);
                }
                return Err(error);
            }
        };
        if let Some(injector) = &mut injector {
            injector.leader(running.pid());
        }
        let body = async {
            running.set_limits(2 * 1024 * 1024, 2 * 1024 * 1024)?;
            loop {
                ctx.control.check()?;
                ctx.wire.no_extra(&mut ctx.control)?;
                ctx.commands.monitor(&mut ctx.control)?;
                if let Some(injector) = &mut injector {
                    injector.poll(ctx, &mut self.fixture).await?;
                }
                let chunk = running.pump(&mut ctx.control)?;
                if chunk.stdout_eof && chunk.stderr_eof && !running.running()? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        .await;
        if let Err(error) = body {
            ctx.control.record(error);
        }
        let finished = running.finish(&mut ctx.control).await;
        let raw_exit = running.status();
        let raw_stdout = running.snapshot().0.to_vec();
        // Passive observation precedes every expected-exit or ordinary-report gate.
        self.stdout = native_report::capture(&raw_stdout, 2 * 1024 * 1024).with_exit(raw_exit);
        let observed_attempt = attempt(&before);
        let observed_outcome = observed_attempt
            .as_ref()
            .map_err(|error| *error)
            .and_then(|path| {
                retained(ctx, &path.join("outcome.json"), 131072).map(|(_, data)| data)
            });
        if let Ok(bytes) = &observed_outcome {
            self.outcome = native_report::capture(bytes, 131072).with_exit(raw_exit);
        }
        let until = stamp();
        let stopped = injector.as_ref().is_some_and(|injector| injector.stopped());
        let injected = injector
            .as_ref()
            .and_then(|injector| injector.identifier().map(str::to_owned));
        let injection_closed = injector
            .as_mut()
            .map_or(Ok(()), |injector| injector.finish(ctx));
        body?;
        let output = finished?;
        injection_closed?;
        self.mark("observation");
        let report = decode(&raw_stdout)?;
        let admitted = validation::native_report(&report, &target, negative)?;
        require(output.status == if negative { 1 } else { 0 })?;
        let attempt = observed_attempt?;
        require(admitted == attempt)?;
        let outcome = decode(&observed_outcome?)?;
        validation::outcome(&report, &outcome)?;
        let journal = read_record(
            ctx,
            &Path::new(PUBLIC).join("release-state.json"),
            0o600,
            131072,
        )?;
        require(
            journal
                == json!({"schemaVersion":1,"attempt":attempt.to_str().ok_or(Error::Input)?,
            "finalized":true,"phase":if negative {"rolled_back"} else {"complete"}}),
        )?;
        let payloads = self.payloads.as_ref().ok_or(Error::Refused)?;
        let payload = if negative {
            &payloads.previous
        } else {
            &payloads.current
        };
        let staged = read_record(
            ctx,
            &Path::new(PUBLIC)
                .join("releases")
                .join(&target)
                .join("staged.json"),
            0o600,
            131072,
        )?;
        super::super::protocol::exact(
            &staged,
            &[
                "schemaVersion",
                "revision",
                "serverImage",
                "manifestSha256",
                "stagedAt",
            ],
        )?;
        require(
            staged["schemaVersion"].as_u64() == Some(1)
                && staged["revision"] == target
                && staged["manifestSha256"] == ctx.originals[payload.manifest_index].sha256,
        )?;
        let image = effects::Fixture::deploy_image(ctx, payload).await?;
        require(staged["serverImage"] == image)?;
        if !negative {
            self.current_image = Some(image.clone());
        }
        let backup = self.backup(ctx, &attempt, &report, &target).await?;
        for (name, bytes) in &selected {
            let (_, observed) =
                retained(ctx, &attempt.join(format!("before-{name}")), 1024 * 1024)?;
            require(&observed == bytes)?;
        }
        let events = effects::Fixture::deploy_docker(
            ctx,
            "deploy-trial-history",
            &[
                "events".into(),
                "--since".into(),
                since,
                "--until".into(),
                until,
                "--format".into(),
                "{{json .}}".into(),
            ],
            15,
        )
        .await?;
        require(events.len() <= 2 * 1024 * 1024)?;
        let text = std::str::from_utf8(&events).map_err(|_| Error::Refused)?;
        let rows = text
            .lines()
            .map(|line| decode(line.as_bytes()))
            .collect::<Result<Vec<_>>>()?;
        let candidates = native_ids(ctx, &attempt)?;
        let payloads = self.payloads.as_ref().ok_or(Error::Refused)?;
        let current_images = BTreeSet::from([
            format!(
                "sha256:{}",
                payloads.current.archive.identity().config_sha256
            ),
            self.current_image.clone().ok_or(Error::Refused)?,
        ]);
        let previous_images = BTreeSet::from([
            format!(
                "sha256:{}",
                payloads.previous.archive.identity().config_sha256
            ),
            self.first_image.clone().ok_or(Error::Refused)?,
        ]);
        let baseline = self.baseline.as_ref().ok_or(Error::Refused)?;
        let binding = history::Binding {
            current_images: &current_images,
            previous_images: &previous_images,
            postgres_id: &baseline.pg,
            postgres_image: &self.fixture.pg_image,
            postgres_reference: &self.request.postgres_image,
            token: &self.request.run_id,
            label: "simplestchat.maintain-prerequisite",
        };
        let history = history::native_history(&rows, &candidates, &binding)?;
        let (_, validator) = retained(ctx, &attempt.join("validation-name.txt"), 128)?;
        let (_, assets) = retained(ctx, &attempt.join("asset-container-name.txt"), 128)?;
        validation::name_anchors(
            &rows,
            std::str::from_utf8(validator.trim_ascii()).map_err(|_| Error::Refused)?,
            std::str::from_utf8(assets.trim_ascii()).map_err(|_| Error::Refused)?,
        )?;
        if negative {
            require(stopped && selection(ctx)? == selected)?;
            history::injected_sequence(
                &rows,
                injected.as_deref().ok_or(Error::Refused)?,
                self.first_image.as_deref().ok_or(Error::Refused)?,
                &self.request.run_id,
                "simplestchat.maintain-prerequisite",
            )?;
        }
        let observed = self.continuity(ctx).await?;
        let known = effects::Fixture::deploy_docker(
            ctx,
            "deploy-native-retired",
            &[
                "ps".into(),
                "--all".into(),
                "--quiet".into(),
                "--no-trunc".into(),
            ],
            15,
        )
        .await?;
        let known = std::str::from_utf8(&known).map_err(|_| Error::Refused)?;
        for id in history["actorIds"].as_array().ok_or(Error::Refused)? {
            require(!known.lines().any(|line| Some(line) == id.as_str()))?;
        }
        ctx.check_full()?;
        Ok(
            json!({"case":if negative {"owned-candidate-stop-rollback"} else {"first-A-to-B"},
            "passed":true,"exitStatus":output.status,"rollbackAttempted":negative,
            "rollbackPassed":negative,"retainedBackup":backup,"actorHistory":history,
            "selected":observed,"injectedCandidateId":injected,"manualStopObserved":stopped,
            "dependencyProfile":if negative {"negative-inspection-response-barrier-v1"}
                else {"original-docker-cli"},"seconds":started.elapsed().as_secs_f64()}),
        )
    }
    async fn prepare(&mut self, ctx: &mut Context) -> Result<Value> {
        let start = Instant::now();
        self.mark("sources");
        sources::prepare(ctx).await?;
        ctx.commands.admit_lookup("mount", &mut ctx.control)?;
        ctx.commands.admit_lookup("nsenter", &mut ctx.control)?;
        self.mark("payload");
        self.payloads = Some(payload::prepare(ctx, &self.request)?);
        let old_path = Path::new(PUBLIC)
            .join("releases")
            .join(&self.request.previous_revision);
        self.fixture
            .prepare_payload_for(ctx, &old_path, &self.request.previous_revision)
            .await?;
        self.first_image = Some(self.fixture.image.clone());
        let absent = effects::Fixture::deploy_docker(
            ctx,
            "deploy-current-absent",
            &[
                "image".into(),
                "ls".into(),
                "--quiet".into(),
                "--filter".into(),
                format!(
                    "reference={}",
                    self.payloads
                        .as_ref()
                        .ok_or(Error::Refused)?
                        .current
                        .manifest
                        .fields()
                        .image_tag
                ),
            ],
            15,
        )
        .await?;
        require(absent.trim_ascii().is_empty())?;
        self.mark("configuration");
        super::super::rendered_files_for(
            ctx,
            self.first_image.as_deref().ok_or(Error::Refused)?,
            &self.request.previous_revision,
        )
        .await?;
        ctx.commands
            .call(
                &mut ctx.control,
                "deploy-initial-version",
                &[
                    "ops".into(),
                    "release".into(),
                    "assets".into(),
                    "select".into(),
                    "--revision".into(),
                    self.request.previous_revision.clone(),
                ],
                Duration::from_secs(30),
            )
            .await?;
        self.mark("network");
        self.fixture.setup_network(ctx).await?;
        self.mark("database");
        self.fixture.setup_database(ctx).await?;
        self.mark("tls");
        self.fixture.prepare_caddy(ctx).await?;
        self.mark("baseline");
        let baseline = self
            .fixture
            .deploy_baseline(ctx, &self.payloads.as_ref().ok_or(Error::Refused)?.previous)
            .await?;
        self.fixture.setup_complete(ctx).await?;
        // Only the three documented selection generations may be replaced by native deploy.
        for index in 0..ctx.originals.len() {
            if ["compose.public.yml", "app.env", "images.json"]
                .iter()
                .any(|name| ctx.originals[index].path == Path::new(CONFIG).join(name))
            {
                ctx.originals[index].check(&mut ctx.control)?;
                ctx.originals[index].finish(&mut ctx.control)?;
                ctx.host_retired.insert(index);
            }
        }
        self.phases
            .insert("prepare", Some(start.elapsed().as_secs_f64()));
        let dependencies = serde_json::to_vec(&baseline.dependencies).map_err(|_| Error::Input)?;
        let migration_hash = migration_digest(
            &self
                .payloads
                .as_ref()
                .ok_or(Error::Refused)?
                .previous
                .manifest
                .fields()
                .migrations,
        )?;
        self.baseline = Some(baseline);
        self.mark("prepared");
        Ok(
            json!({"schemaVersion":1,"kind":"prepared","runId":self.request.run_id,
            "revision":self.request.revision,"previousRevision":self.request.previous_revision,
            "passed":true,"binarySha256":ctx.commands.checksum("ops")?,
            "firstImage":self.first_image,"dependencySha256":hash(&dependencies),
            "migrationSha256":migration_hash}),
        )
    }
    async fn body(&mut self, ctx: &mut Context) -> Result<()> {
        let prepared = self.prepare(ctx).await?;
        ctx.check_full()?;
        ctx.wire.emit_bounded(&prepared, &mut ctx.control, 16384)?;
        let continued = decode(&ctx.wire.line_bounded(&mut ctx.control, 16384)?)?;
        super::super::protocol::exact(&continued, &["schemaVersion", "kind", "runId"])?;
        require(
            continued == json!({"schemaVersion":1,"kind":"continue","runId":self.request.run_id}),
        )?;
        ctx.wire.no_extra(&mut ctx.control)?;
        ctx.control.advance_deploy()?;
        for negative in [false, true] {
            self.mark(if negative { "negative" } else { "positive" });
            let start = Instant::now();
            let trial = self.invoke(ctx, negative).await?;
            self.phases.insert(
                if negative { "negative" } else { "positive" },
                Some(start.elapsed().as_secs_f64()),
            );
            self.trials.push(trial);
        }
        self.mark("probe");
        self.separate = Some(self.probe(ctx).await?);
        self.mark("observation");
        self.continuity(ctx).await?;
        workload_free(ctx)?;
        ctx.check_full()?;
        self.fixture.deploy_accept_complete();
        self.complete = true;
        self.mark("complete");
        Ok(())
    }
    pub(super) async fn transaction(&mut self, ctx: &mut Context) -> Value {
        if let Err(error) = self.body(ctx).await {
            ctx.control.record(error);
        }
        if ctx.control.first.is_none() {
            self.mark("cleanup");
        }
        // Deploy's frozen final30 owns all known-resource retirement, not only file closes.
        ctx.control.begin_settling();
        let body = self.fixture.finish_body(ctx).await;
        let mut cleanup = super::super::first(&mut ctx.control, body);
        let trust = effects::Fixture::deploy_retire_trust(ctx).await;
        cleanup &= super::super::first(&mut ctx.control, trust);
        let local = self.fixture.finish_local(ctx).await;
        cleanup &= super::super::first(&mut ctx.control, local);
        for (index, original) in ctx.originals.iter_mut().enumerate() {
            if ctx.host_retired.contains(&index) {
                cleanup &= original.closed();
                continue;
            }
            let closed = original.finish(&mut ctx.control);
            cleanup &= super::super::first(&mut ctx.control, closed);
        }
        let children = ctx.commands.finish(&mut ctx.control).await;
        cleanup &= super::super::first(&mut ctx.control, children);
        cleanup &= !ctx.control.cleanup_failed && !self.fixture.deploy_uncertain();
        if !cleanup && ctx.control.first.is_none() {
            ctx.control.record(Error::Close);
        }
        if ctx.control.first.is_none() {
            self.mark("complete");
        }
        self.result(ctx, cleanup)
    }
}
fn native_ids(ctx: &mut Context, attempt: &Path) -> Result<BTreeSet<String>> {
    let mut errors = BTreeSet::new();
    let mut outputs = BTreeMap::new();
    for name in names(attempt)? {
        let Some((number, stream)) = name.split_once('.') else {
            continue;
        };
        if number.len() != 3
            || !number.bytes().all(|byte| byte.is_ascii_digit())
            || !["stdout", "stderr"].contains(&stream)
        {
            continue;
        }
        let number = number.parse::<usize>().map_err(|_| Error::Refused)?;
        require((1..=192).contains(&number))?;
        let original = Original::observe(
            &attempt.join(&name),
            super::super::files::ObservedFile::NativeStream,
            &mut ctx.control,
        )?;
        ctx.originals.push(original);
        ctx.originals
            .last()
            .ok_or(Error::Close)?
            .require_parent(0, 0o700, &mut ctx.control)?;
        let bytes = ctx
            .originals
            .last()
            .ok_or(Error::Close)?
            .data(2 * 1024 * 1024, &mut ctx.control)?;
        if stream == "stderr" {
            errors.insert(number);
        } else {
            outputs.insert(number, bytes);
        }
    }
    require(!errors.is_empty() && errors.iter().copied().eq(1..=errors.len()))?;
    require(outputs.keys().all(|number| errors.contains(number)))?;
    let mut ids = BTreeSet::new();
    for bytes in outputs.values() {
        if let Ok(value) = std::str::from_utf8(bytes.trim_ascii())
            && hex(value, 64)
        {
            ids.insert(value.to_owned());
        }
    }
    Ok(ids)
}
fn workload_free(ctx: &mut Context) -> Result<()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open("/run/simplestchat-bench/workload.lock")
        .map_err(|_| Error::Io)?;
    let result = (|| {
        ctx.control.check()?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| Error::Refused)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock).map_err(|_| Error::Close)?;
        ctx.control.check()
    })();
    if let Err(error) = result {
        ctx.control.record(error);
    }
    result.and(super::super::files::close(file, &mut ctx.control))
}
