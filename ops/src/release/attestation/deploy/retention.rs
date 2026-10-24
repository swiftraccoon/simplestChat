//! Known-ID browser extraction and original-descriptor live backup.

use super::*;

impl State {
    fn finish_inherited_input(&mut self, input: usize, owner: &mut Owner) -> Result<()> {
        let record = self.records.files.remove(input);
        // Config inputs and the dump writer precede this temporary input.
        // Only this command's deferred logs can outlive it at higher slots.
        // Relocate those owners before checked close can record a late error.
        let mut alias = false;
        self.pending_logs.retain_mut(|(at, _)| {
            if *at == input {
                alias = true;
                false
            } else {
                if *at > input {
                    *at -= 1;
                }
                true
            }
        });
        if alias {
            owner.record(Cause::Changed);
        }
        record.finish(owner);
        if alias { Err(Cause::Changed) } else { Ok(()) }
    }

    pub(super) async fn retain_assets(
        &mut self,
        access: &Access,
        images: &[(String, String)],
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Value> {
        let mut captures = Vec::new();
        for (revision, image) in images {
            self.stage.continue_commands(self.commands);
            let identity = self
                .stage
                .installed_identity(
                    &stage_access(access),
                    image,
                    revision,
                    owner,
                    &mut StageExecutor(executor),
                )
                .await;
            self.commands = self.stage.command_count();
            identity?;
            let mut random = [0; 16];
            getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
            let name = format!(
                "scpub-release-assets-{}",
                crate::release::attestation::files::hex(&random)
            );
            self.journal("capture_browser_assets", false, owner, false)?;
            let attempt = self.attempt();
            self.atomic(
                &attempt,
                "asset-container-name.txt",
                name.as_bytes(),
                owner,
                false,
            )?;
            self.asset_id = None;
            self.asset_remove_attempted = false;
            self.asset_remove_admitted = false;
            self.asset_remove_dispatched = false;
            self.asset_remove_effected = false;
            let capture: Result<assets::Capture> = async {
                let created = self
                    .docker(
                        access,
                        Role::AssetCreate,
                        argv(&[
                            "create",
                            "--name",
                            &name,
                            "--init",
                            "--network",
                            "none",
                            "--pull",
                            "never",
                            "--read-only",
                            "--user",
                            "10001:10001",
                            "--cap-drop",
                            "ALL",
                            "--security-opt",
                            "no-new-privileges",
                            "--memory",
                            "128m",
                            "--cpus",
                            "0.5",
                            "--pids-limit",
                            "32",
                            "--log-driver",
                            "local",
                            "--log-opt",
                            "max-size=1m",
                            "--log-opt",
                            "max-file=1",
                            "--log-opt",
                            "compress=false",
                            "--entrypoint",
                            "/usr/bin/timeout",
                            image,
                            "--signal=TERM",
                            "--kill-after=2s",
                            "10s",
                            "/bin/sh",
                            "-c",
                            "exit 0",
                        ]),
                        false,
                        owner,
                        executor,
                    )
                    .await?;
                let id = self.policy(owner, raw_text(&created))?;
                if !exact_hex(stage::python_strip(id), 64) || self.asset_id.is_none() {
                    return self.refused(owner, "Uncertain asset container creation");
                }
                let name = format!("assets-{revision}.tar");
                let record = Record::create(attempt.clone(), &name, owner, false)?;
                self.records.files.push(record);
                let at = self.records.files.len() - 1;
                let maximum = (32 * 1024 * 1024 + 1024 * 1024)
                    .min(self.free(access, owner)?.saturating_sub(GIB));
                if maximum == 0 {
                    return self.refused(owner, "Insufficient archive output reserve");
                }
                let source = format!("{}:/app/web/dist/assets", self.asset_id.as_ref().unwrap());
                let output = self
                    .command(
                        access,
                        Role::AssetCopy,
                        argv(&["cp", &source, "-"]),
                        CommandIo::bulk(at, maximum),
                        owner,
                        executor,
                    )
                    .await?;
                if output.status != Some(0) {
                    self.command_failure(owner);
                    return Err(Cause::Command);
                }
                if !output.bulk_complete {
                    return control::failed(owner, Cause::Close);
                }
                self.records.files[at].seal(owner, false)?;
                let capture = assets::Capture::read_owned(&self.records.files[at].path(), owner)
                    .map_err(|error| {
                        if self.failure.is_none() {
                            self.failure = Some(error.code().into());
                        }
                        owner.record(Cause::Policy);
                        Cause::Policy
                    })?;
                Ok(capture)
            }
            .await;
            if capture.is_err() {
                self.latch_failure(owner);
            }
            // Only a positive natural-body ID is known. The two explicitly
            // documented uncertain-create profiles retain evidence, not names.
            if let Some(id) = self.asset_id.clone() {
                self.asset_remove_attempted = true;
                let cleanup = capture.is_err();
                if cleanup {
                    owner.begin_deploy_recovery()?;
                }
                let removed = self
                    .docker(
                        access,
                        Role::AssetRemove,
                        argv(&["rm", &id]),
                        cleanup,
                        owner,
                        executor,
                    )
                    .await;
                if cleanup {
                    owner.end_deploy_recovery();
                }
                if capture.is_ok() {
                    removed?;
                }
            }
            captures.push((revision.clone(), capture?));
        }
        let root = self.stage.root().ok_or(Cause::Changed)?;
        let duplicate = control::io(owner, root.file().try_clone())?;
        let lease = self.stage.borrowed_lease()?;
        self.assets = Some(
            assets::host::HostStore::borrow(duplicate, lease, access.uid, owner).map_err(|_| {
                owner.record(Cause::Policy);
                Cause::Policy
            })?,
        );
        #[cfg(test)]
        let wall = access.wall_seconds.unwrap_or(2524608000);
        #[cfg(not(test))]
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Cause::Io)?
            .as_secs();
        let result = self
            .assets
            .as_mut()
            .unwrap()
            .publish(captures, wall, owner)
            .map_err(|_| {
                owner.record(Cause::Policy);
                Cause::Policy
            })?;
        let value = serde_json::to_value(result).map_err(|_| Cause::Io)?;
        let attempt = self.attempt();
        self.atomic(
            &attempt,
            "browser-assets.json",
            &encoded(&value)?,
            owner,
            false,
        )?;
        self.journal("browser_assets_retained", true, owner, false)?;
        Ok(value)
    }
    pub(super) async fn backup(
        &mut self,
        access: &Access,
        database: &str,
        revision: &str,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let size = self
            .docker(
                access,
                Role::Size,
                argv(&[
                    "exec",
                    "--user",
                    "999:999",
                    database,
                    "psql",
                    "-X",
                    "-q",
                    "-t",
                    "-A",
                    "-h",
                    "/run/simplestchat-postgres",
                    "-U",
                    "postgres",
                    "-d",
                    "simplestchat",
                    "-c",
                    "SELECT pg_database_size(current_database())",
                ]),
                false,
                owner,
                executor,
            )
            .await?;
        let size = self.policy(owner, raw_text(&size))?;
        let size = stage::python_strip(size);
        self.policy(
            owner,
            ensure(
                !size.is_empty()
                    && size.len() <= 16
                    && size.as_bytes()[0] != b'0'
                    && size.bytes().all(|v| v.is_ascii_digit()),
                "Invalid database size for backup reserve",
            ),
        )?;
        let size: u64 = size.parse().map_err(|_| Cause::Policy)?;
        if self.free(access, owner)? <= GIB + 2 * size {
            return self.refused(owner, "Insufficient backup headroom");
        }
        self.journal("live_backup", false, owner, false)?;
        let attempt = self.attempt();
        self.records.files.push(Record::create(
            attempt.clone(),
            "database-before.dump.partial",
            owner,
            false,
        )?);
        let at = self.records.files.len() - 1;
        let maximum = (4 * GIB).min(self.free(access, owner)?.saturating_sub(GIB));
        if maximum == 0 {
            return self.refused(owner, "Insufficient archive output reserve");
        }
        let output = self
            .command(
                access,
                Role::Dump,
                argv(&[
                    "exec",
                    "--user",
                    "999:999",
                    database,
                    "timeout",
                    "--signal=TERM",
                    "--kill-after=2s",
                    "45s",
                    "pg_dump",
                    "--host",
                    "/run/simplestchat-postgres",
                    "--username",
                    "postgres",
                    "--dbname",
                    "simplestchat",
                    "--format",
                    "custom",
                ]),
                CommandIo::bulk(at, maximum),
                owner,
                executor,
            )
            .await?;
        if output.status != Some(0) {
            self.command_failure(owner);
            return Err(Cause::Command);
        }
        if !output.bulk_complete {
            return control::failed(owner, Cause::Close);
        }
        if self.records.files[at].len() == 0 {
            return self.refused(owner, "Empty live database backup");
        }
        // The consumer receives a separately admitted read-only capability to
        // the same original inode; list completion is not restore proof.
        let readonly = Record::open(
            attempt.clone(),
            "database-before.dump.partial",
            access.uid,
            maximum,
            owner,
        )?;
        if !self.records.files[at].aliases(&readonly) {
            readonly.finish(owner);
            return control::failed(owner, Cause::Changed);
        }
        self.records.files.push(readonly);
        let input = self.records.files.len() - 1;
        let listed = self
            .command(
                access,
                Role::List,
                argv(&[
                    "exec",
                    "--interactive",
                    "--user",
                    "999:999",
                    database,
                    "timeout",
                    "--signal=TERM",
                    "--kill-after=2s",
                    "15s",
                    "pg_restore",
                    "--list",
                ]),
                CommandIo::inherited(input),
                owner,
                executor,
            )
            .await;
        let input_finished = self.finish_inherited_input(input, owner);
        let listed = listed?;
        input_finished?;
        if listed.status != Some(0) {
            self.command_failure(owner);
            return Err(Cause::Command);
        }
        #[cfg(test)]
        self.fault(super::super::files::Fault::BackupFileSync, owner)?;
        self.records.files[at].seal(owner, false)?;
        #[cfg(test)]
        self.fault(super::super::files::Fault::BackupRename, owner)?;
        if attempt.exists("database-before.dump".as_ref(), owner)? {
            return self.refused(owner, "Backup already exists");
        }
        rustix::fs::renameat(
            attempt.file(),
            "database-before.dump.partial",
            attempt.file(),
            "database-before.dump",
        )
        .map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })?;
        self.records.files[at].renamed("database-before.dump", owner, false)?;
        #[cfg(test)]
        self.fault(super::super::files::Fault::BackupDirectorySync, owner)?;
        attempt.sync(owner)?;
        let sha = self.records.files[at]
            .sha256()
            .ok_or(Cause::Changed)?
            .to_owned();
        // The validated dump digest is a passive observation even when the
        // following receipt cannot be published. It is not a backup receipt.
        report["backupSha256"] = json!(sha);
        let receipt = json!({"schemaVersion":1,"dump":"database-before.dump",
            "bytes":self.records.files[at].len(),"sha256":sha,"revision":revision,
            "completedAt":chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()});
        #[cfg(test)]
        self.fault(super::super::files::Fault::BackupReceiptWrite, owner)?;
        self.atomic(
            &attempt,
            "database-before.receipt.json",
            &encoded(&receipt)?,
            owner,
            false,
        )?;
        self.journal("backed_up", true, owner, false)?;
        Ok(())
    }
}
