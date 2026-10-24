//! The original replacement try boundary and one known-owned recovery attempt.

use super::*;

impl State {
    pub(super) async fn preflight(
        &mut self,
        options: &Options,
        access: &Access,
        manifest: &Manifest,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<PreparedSelection> {
        for name in SELECTION {
            self.input(name, 1024 * 1024, access, owner)?;
        }
        let selected = self.bytes("images.json", 1024 * 1024, owner)?;
        let old = self.policy(owner, decode_text(&selected))?;
        let old_image = self.policy(owner, text(&old, "serverImage"))?.to_owned();
        let old_revision = self.policy(owner, text(&old, "revision"))?.to_owned();
        self.policy(
            owner,
            ensure(
                image_id(&old_image) && exact_hex(&old_revision, 40),
                "Invalid deployed identity",
            ),
        )?;
        self.stage.continue_commands(self.commands);
        let checked = self
            .stage
            .installed_identity(
                &stage_access(access),
                &old_image,
                &old_revision,
                owner,
                &mut StageExecutor(executor),
            )
            .await;
        self.commands = self.stage.command_count();
        checked?;
        let new_image = self
            .stage
            .selected_image()
            .ok_or(Cause::Changed)?
            .to_owned();
        self.stage.continue_commands(self.commands);
        let checked = self
            .stage
            .installed_identity(
                &stage_access(access),
                &new_image,
                &options.revision,
                owner,
                &mut StageExecutor(executor),
            )
            .await;
        self.commands = self.stage.command_count();
        checked?;
        self.policy(
            owner,
            ensure(old_image != new_image, "This image is already selected"),
        )?;
        let app = self
            .container(access, "simplestchat", false, owner, executor)
            .await?;
        let database = self
            .container(access, "postgres", false, owner, executor)
            .await?;
        let proxy = self
            .container(access, "caddy", false, owner, executor)
            .await?;
        for (service, container) in [
            ("simplestchat", &app),
            ("postgres", &database),
            ("caddy", &proxy),
        ] {
            let output = self
                .compose(
                    access,
                    argv(&["config", "--hash", service]),
                    None,
                    false,
                    owner,
                    executor,
                )
                .await?;
            let output = self.policy(owner, raw_text(&output))?;
            let fields = output
                .split(stage::python_space)
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>();
            let expected = self.policy(owner, text(container, "configHash"))?;
            self.policy(
                owner,
                ensure(
                    fields == [service, expected],
                    "Running configuration differs from disk; use reviewed maintenance",
                ),
            )?;
        }
        let running_image = self.policy(owner, text(&app, "image"))?;
        self.policy(
            owner,
            ensure(
                running_image == old_image,
                "Running app differs from selected image",
            ),
        )?;
        let healthy = database
            .get("state")
            .and_then(|state| state.get("Health"))
            .and_then(|health| health.get("Status"))
            .and_then(Json::string)
            == Some("healthy");
        self.policy(owner, ensure(healthy, "Database must be healthy"))?;
        let migrate = self
            .compose(
                access,
                argv(&[
                    "--profile",
                    "maintenance",
                    "ps",
                    "--all",
                    "--quiet",
                    "migrate",
                ]),
                None,
                false,
                owner,
                executor,
            )
            .await?;
        self.policy(
            owner,
            ensure(
                stage::python_byte_strip(&migrate).is_empty(),
                "Inspect retained migration container first",
            ),
        )?;
        self.ready(access, None, 3, false, owner, executor).await?;
        let database_id = self.policy(owner, text(&database, "id"))?.to_owned();
        let applied = self
            .docker(
                access,
                Role::Query,
                argv(&[
                    "exec",
                    "--user",
                    "999:999",
                    &database_id,
                    "psql",
                    "--no-psqlrc",
                    "--set",
                    "ON_ERROR_STOP=on",
                    "--host",
                    "/run/simplestchat-postgres",
                    "--username",
                    "postgres",
                    "--dbname",
                    "simplestchat",
                    "--tuples-only",
                    "--no-align",
                    "--field-separator",
                    " ",
                    "--command",
                    LEDGER,
                ]),
                false,
                owner,
                executor,
            )
            .await?;
        let applied = self.policy(owner, ledger(&applied))?;
        report["backupMigrations"] = json!(applied);
        self.policy(
            owner,
            ensure(
                applied == manifest.fields().migrations,
                "Schema changes require the explicit maintenance deployment",
            ),
        )?;
        self.stage.continue_commands(self.commands);
        let packaged = self
            .stage
            .migrations(
                &stage_access(access),
                &new_image,
                owner,
                &mut StageExecutor(executor),
            )
            .await;
        self.commands = self.stage.command_count();
        self.policy(
            owner,
            ensure(
                packaged? == manifest.fields().migrations,
                "Candidate migration mismatch",
            ),
        )?;
        let (preview, environment) = self
            .candidate(access, &old, &new_image, owner, executor)
            .await?;
        let images = if old_revision == options.revision {
            vec![(options.revision.clone(), new_image)]
        } else {
            vec![
                (old_revision, old_image),
                (options.revision.clone(), new_image),
            ]
        };
        report["browserAssets"] = self.retain_assets(access, &images, owner, executor).await?;
        let config = self
            .compose(
                access,
                argv(&["config", "--format", "json"]),
                None,
                false,
                owner,
                executor,
            )
            .await?;
        let config = self.policy(owner, decode(&config))?;
        let origin = config
            .get("services")
            .and_then(|services| services.get("simplestchat"))
            .and_then(|service| service.get("environment"))
            .and_then(|environment| environment.get("WEBAUTHN_ORIGIN"))
            .and_then(Json::string)
            .ok_or(Cause::Policy)?
            .to_owned();
        self.policy(
            owner,
            ensure(
                origin.strip_prefix("https://").is_some_and(|name| {
                    !name.is_empty()
                        && name.bytes().all(|v| {
                            v.is_ascii_lowercase() || v.is_ascii_digit() || b".-".contains(&v)
                        })
                }),
                "Unexpected public origin",
            ),
        )?;
        self.ready(access, Some(&origin), 3, false, owner, executor)
            .await?;
        self.quiet(access, options.quiet_seconds, report, owner, executor)
            .await?;
        self.backup(
            access,
            &database_id,
            &options.revision,
            report,
            owner,
            executor,
        )
        .await?;
        #[cfg(test)]
        self.fault(super::super::files::Fault::BeforeCopy, owner)?;
        for name in SELECTION {
            let data = self.bytes(name, 1024 * 1024, owner)?;
            let record = Record::create(self.attempt(), &format!("before-{name}"), owner, false)?;
            self.records.files.push(record);
            self.records
                .files
                .last_mut()
                .unwrap()
                .write(&data, owner, false)?;
        }
        #[cfg(test)]
        self.fault(super::super::files::Fault::ReplaceJournalWrite, owner)?;
        #[cfg(test)]
        if self.fault == Some(super::super::files::Fault::InterruptBeforeStop) {
            self.fault = None;
            owner.record(Cause::Interrupt);
            return Err(Cause::Interrupt);
        }
        self.journal("replace_application", false, owner, false)?;
        Ok(PreparedSelection {
            old,
            database,
            proxy,
            origin,
            compose: preview,
            environment,
        })
    }
    pub(super) async fn replace(
        &mut self,
        options: &Options,
        access: &Access,
        selection: &PreparedSelection,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let PreparedSelection {
            old,
            database,
            proxy,
            origin,
            compose: preview,
            environment,
        } = selection;
        let end = budget::candidate_end(owner)?;
        owner.begin_deploy_candidate(end)?;
        self.candidate_end = Some(end);
        let mut replaced = false;
        let body: Result<()> = async {
            report["phase"] = json!("replace_application");
            self.observe_rooms(access, report, owner, executor).await?;
            report["interruptionStartedAt"] = json!(timestamp());
            budget::first_stop_fits(owner, end)?;
            replaced = true;
            self.compose(
                access,
                argv(&["stop", "--timeout", "30", "simplestchat"]),
                None,
                false,
                owner,
                executor,
            )
            .await?;
            #[cfg(test)]
            if self.fault == Some(super::super::files::Fault::InterruptAfterStop) {
                self.fault = None;
                owner.record(Cause::Interrupt);
                return Err(Cause::Interrupt);
            }
            #[cfg(test)]
            self.fault(super::super::files::Fault::SelectComposeWrite, owner)?;
            self.atomic(
                &self.config().clone(),
                "compose.public.yml",
                preview,
                owner,
                false,
            )?;
            #[cfg(test)]
            self.fault(super::super::files::Fault::SelectEnvWrite, owner)?;
            self.atomic(&self.config().clone(), "app.env", environment, owner, false)?;
            let mut selected = old.clone();
            let map = self.policy(owner, object_mut(&mut selected))?;
            map.insert("revision".into(), Json::String(options.revision.clone()));
            map.insert(
                "serverImage".into(),
                Json::String(self.stage.selected_image().ok_or(Cause::Changed)?.into()),
            );
            #[cfg(test)]
            self.fault(super::super::files::Fault::SelectImagesWrite, owner)?;
            self.atomic(
                &self.config().clone(),
                "images.json",
                &encoded(&selected)?,
                owner,
                false,
            )?;
            #[cfg(test)]
            self.fault(super::super::files::Fault::SelectAssets, owner)?;
            self.assets
                .as_mut()
                .ok_or(Cause::Changed)?
                .select(&options.revision, owner, false)
                .map_err(|_| {
                    owner.record(Cause::Policy);
                    Cause::Policy
                })?;
            self.compose(
                access,
                argv(&[
                    "up",
                    "--detach",
                    "--no-build",
                    "--pull",
                    "never",
                    "--no-deps",
                    "simplestchat",
                ]),
                None,
                false,
                owner,
                executor,
            )
            .await?;
            let app = self
                .container(access, "simplestchat", false, owner, executor)
                .await?;
            self.policy(
                owner,
                ensure(
                    app.get("image").and_then(Json::string) == self.stage.selected_image(),
                    "Replacement is not the staged image",
                ),
            )?;
            self.ready(access, None, 30, false, owner, executor).await?;
            self.ready(access, Some(origin), 30, false, owner, executor)
                .await?;
            self.continuity(access, database, proxy, false, owner, executor)
                .await?;
            report["interruptionFinishedAt"] = json!(timestamp());
            report["phase"] = json!("complete");
            #[cfg(test)]
            self.fault(super::super::files::Fault::CompleteJournalWrite, owner)?;
            self.journal("complete", true, owner, false)
        }
        .await;
        let fence = owner.check(Window::Body);
        owner.end_deploy_candidate();
        self.candidate_end = None;
        if let Err(cause) = body {
            owner.record(cause);
            self.latch_failure(owner);
            report["rollbackAttempted"] = json!(replaced);
            if replaced {
                let recovery = owner.begin_deploy_recovery();
                let rollback = if recovery.is_ok() {
                    self.rollback(access, selection, owner, executor).await
                } else {
                    recovery
                };
                if recovery.is_ok() {
                    owner.end_deploy_recovery();
                }
                report["rollbackPassed"] = json!(rollback.is_ok());
            }
            Err(cause)
        } else {
            // A late phase/output/close fence after the completed body remains
            // negative, but is outside the reference's application rollback try.
            fence
        }
    }
    async fn continuity(
        &mut self,
        access: &Access,
        database: &Json,
        proxy: &Json,
        recovery: bool,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let after_db = self
            .container(access, "postgres", recovery, owner, executor)
            .await?;
        let first = self.policy(owner, stable(&after_db))?;
        let previous = self.policy(owner, stable(database))?;
        if !first.semantic_eq(&previous) {
            return self.refused(
                owner,
                if recovery {
                    "Dependency continuity changed"
                } else {
                    "A retained dependency restarted during release"
                },
            );
        }
        let after_proxy = self
            .container(access, "caddy", recovery, owner, executor)
            .await?;
        let first = self.policy(owner, stable(&after_proxy))?;
        let previous = self.policy(owner, stable(proxy))?;
        self.policy(
            owner,
            ensure(
                first.semantic_eq(&previous),
                if recovery {
                    "Dependency continuity changed"
                } else {
                    "A retained dependency restarted during release"
                },
            ),
        )
    }
    async fn rollback(
        &mut self,
        access: &Access,
        selection: &PreparedSelection,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let PreparedSelection {
            old,
            database,
            proxy,
            origin,
            ..
        } = selection;
        let _: Result<()> = async {
            let failed = self
                .compose(
                    access,
                    argv(&["ps", "--all", "--quiet", "simplestchat"]),
                    None,
                    true,
                    owner,
                    executor,
                )
                .await?;
            let failed = std::str::from_utf8(&failed).map_err(|_| Cause::Policy)?;
            let failed = stage::python_strip(failed);
            if exact_hex(failed, 64) {
                self.docker(
                    access,
                    Role::Inspect,
                    argv(&[
                        "inspect",
                        "--format",
                        "{\"image\":{{json .Image}},\"state\":{{json .State}}}",
                        failed,
                    ]),
                    true,
                    owner,
                    executor,
                )
                .await?;
            }
            self.compose(
                access,
                argv(&["logs", "--no-color", "--tail", "100", "simplestchat"]),
                None,
                true,
                owner,
                executor,
            )
            .await?;
            Ok(())
        }
        .await;
        self.compose(
            access,
            argv(&["stop", "--timeout", "30", "simplestchat"]),
            None,
            true,
            owner,
            executor,
        )
        .await?;
        for name in SELECTION {
            let before = self
                .records
                .files
                .iter()
                .find(|file| {
                    file.path()
                        .file_name()
                        .is_some_and(|v| v == format!("before-{name}").as_str())
                })
                .ok_or(Cause::Changed)?;
            let data = before.bytes(1024 * 1024, owner, true)?;
            #[cfg(test)]
            if name == "compose.public.yml" {
                self.fault(super::super::files::Fault::RollbackComposeWrite, owner)?;
            }
            self.atomic(&self.config().clone(), name, &data, owner, true)?;
        }
        let revision = self.policy(owner, text(old, "revision"))?;
        #[cfg(test)]
        self.fault(super::super::files::Fault::RollbackAssets, owner)?;
        self.assets
            .as_mut()
            .ok_or(Cause::Changed)?
            .select(revision, owner, true)
            .map_err(|_| {
                owner.record(Cause::Policy);
                Cause::Policy
            })?;
        self.compose(
            access,
            argv(&[
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "--no-deps",
                "simplestchat",
            ]),
            None,
            true,
            owner,
            executor,
        )
        .await?;
        let app = self
            .container(access, "simplestchat", true, owner, executor)
            .await?;
        self.policy(
            owner,
            ensure(
                app.get("image").is_some_and(|image| {
                    old.get("serverImage")
                        .is_some_and(|expected| expected.semantic_eq(image))
                }),
                "Rollback image mismatch",
            ),
        )?;
        self.ready(access, None, 30, true, owner, executor).await?;
        self.ready(access, Some(origin), 30, true, owner, executor)
            .await?;
        self.continuity(access, database, proxy, true, owner, executor)
            .await?;
        self.journal("rolled_back", true, owner, true)
    }
}
