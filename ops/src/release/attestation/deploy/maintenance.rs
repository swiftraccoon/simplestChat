//! Maintenance orchestration over the fixed host adapters. No app rollback.

use super::*;
use crate::release::attestation::maintain::{Launcher, Options as MaintainOptions};
use sha2::Digest;
use std::os::unix::fs::MetadataExt;

// Two readiness loops include their last sleep/request/settlement. Six final
// daemon queries include app/database ps+inspect, app config hash and ledger.
const POST_LAUNCH: Duration = Duration::from_secs(91 + 6 * 40);
// The native work/three frames/TERM/reap owe925; its own explicit local tail
// owes60 more, still inside the parent body rather than the parent's reserve.
const NATIVE: Duration = Duration::from_secs(925 + 60);
const PROXY_STOP: Duration = Duration::from_secs(30 + 10);
const APP_STOP: Duration = Duration::from_secs(45 + 10);
const FIRST_STOP: Duration = Duration::from_secs(
    APP_STOP.as_secs() + PROXY_STOP.as_secs() + NATIVE.as_secs() + POST_LAUNCH.as_secs(),
);

/// Passive original-owner observations. Dispatch means entering the bounded
/// executor, not proof that Docker accepted or completed the requested stop.
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Evidence {
    backup_published_elapsed_ns: Option<u64>,
    first_stop_dispatched_elapsed_ns: Option<u64>,
    first_unlock_elapsed_ns: Option<u64>,
    second_lock_acquired_elapsed_ns: Option<u64>,
}
fn elapsed(owner: &mut Owner) -> Result<u64> {
    owner.check(Window::Body)?;
    owner
        .now()
        .checked_duration_since(owner.start())
        .and_then(|time| time.as_nanos().try_into().ok())
        .ok_or(Cause::Deadline)
}
impl Evidence {
    pub(super) fn backup(&mut self, owner: &mut Owner) -> Result<()> {
        if self.backup_published_elapsed_ns.is_some() {
            return control::failed(owner, Cause::Changed);
        }
        self.backup_published_elapsed_ns = Some(elapsed(owner)?);
        Ok(())
    }
    pub(super) fn stop_dispatch(&mut self, owner: &mut Owner) -> Result<()> {
        if self.backup_published_elapsed_ns.is_none()
            || self.first_stop_dispatched_elapsed_ns.is_some()
        {
            return control::failed(owner, Cause::Changed);
        }
        self.first_stop_dispatched_elapsed_ns = Some(elapsed(owner)?);
        Ok(())
    }
}

fn before(owner: &mut Owner, debt: Duration) -> Result<Instant> {
    let end = owner.body_end().checked_sub(debt).ok_or(Cause::Deadline)?;
    if owner.now() >= end {
        return control::failed(owner, Cause::Deadline);
    }
    owner.check(Window::Body)?;
    Ok(end)
}

impl State {
    pub(super) fn maintenance_source_checkout(
        &mut self,
        access: &Access,
        revision: &str,
        owner: &mut Owner,
    ) -> Result<()> {
        #[cfg(test)]
        let base = access.root.parent().ok_or(Cause::Input)?.join("sources");
        #[cfg(not(test))]
        let base = std::path::PathBuf::from("/srv/simplestchat-bench/sources");
        let source = base.join(revision);
        for path in [source.clone(), source.join(".git")] {
            let directory = Dir::open(&path, owner)?;
            self.maintenance_directories.push(directory.clone());
            let stamp = control::io(owner, directory.file().metadata())?;
            if stamp.uid() != access.uid || stamp.mode() & 0o022 != 0 {
                return self.refused(owner, "Uncontrolled maintenance source checkout");
            }
        }
        self.maintenance_source = Some(source);
        self.maintenance_revision = Some(revision.into());
        Ok(())
    }

    pub(super) async fn maintenance_source_file(
        &mut self,
        access: &Access,
        name: &str,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Vec<u8>> {
        if name != "Caddyfile" {
            return control::failed(owner, Cause::Input);
        }
        if self.maintenance_git.is_none() {
            #[cfg(test)]
            let path = access.root.parent().ok_or(Cause::Input)?.join("git");
            #[cfg(not(test))]
            let path = std::path::PathBuf::from("/usr/bin/git");
            let parent = Dir::open(path.parent().ok_or(Cause::Input)?, owner)?;
            self.maintenance_directories.push(parent.clone());
            let tool = Record::tool(parent, "git", access.uid, owner)?;
            self.records.files.push(tool);
            self.maintenance_git = Some(path);
        }
        let source = self.maintenance_source.as_ref().ok_or(Cause::Input)?;
        let source = source.to_str().ok_or(Cause::Input)?;
        let selected = format!(
            "{}:Caddyfile",
            self.maintenance_revision.as_ref().ok_or(Cause::Input)?
        );
        self.docker(
            access,
            Role::Source,
            argv(&["--no-replace-objects", "-C", source, "show", &selected]),
            false,
            owner,
            executor,
        )
        .await
    }

    async fn maintenance_select(
        &mut self,
        options: &MaintainOptions,
        access: &Access,
        selection: &PreparedSelection,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        self.observe_rooms(access, report, owner, executor).await?;
        self.originals(access, owner, false)?;
        before(owner, FIRST_STOP)?;
        report["phase"] = json!("maintenance_stop");
        report["interruptionStartedAt"] = json!(timestamp());
        let app_end = before(owner, PROXY_STOP + NATIVE + POST_LAUNCH)?;
        owner.begin_deploy_candidate(app_end)?;
        self.candidate_end = Some(app_end);
        let stopped = self
            .compose(
                access,
                argv(&["stop", "--timeout", "30", "simplestchat"]),
                None,
                false,
                owner,
                executor,
            )
            .await;
        owner.end_deploy_candidate();
        self.candidate_end = None;
        stopped?;
        owner.check(Window::Body)?;

        let selection_end = before(owner, NATIVE + POST_LAUNCH)?;
        owner.begin_deploy_candidate(selection_end)?;
        self.candidate_end = Some(selection_end);
        let selected: Result<()> = async {
            let arguments =
                self.compose_args(access, argv(&["stop", "--timeout", "3", "caddy"]), None);
            self.docker(access, Role::ProxyStop, arguments, false, owner, executor)
                .await?;
            let config = self.config().clone();
            self.atomic(
                &config,
                "compose.public.yml",
                &selection.compose,
                owner,
                false,
            )?;
            self.atomic(&config, "app.env", &selection.environment, owner, false)?;
            let mut installed = selection.old.clone();
            let image = self
                .stage
                .selected_image()
                .ok_or(Cause::Changed)?
                .to_owned();
            let fields = self.policy(owner, object_mut(&mut installed))?;
            fields.insert("revision".into(), Json::String(options.revision.clone()));
            fields.insert("serverImage".into(), Json::String(image));
            self.atomic(&config, "images.json", &encoded(&installed)?, owner, false)?;
            self.assets
                .as_mut()
                .ok_or(Cause::Changed)?
                .select(&options.revision, owner, false)
                .map_err(|_| Cause::Policy)?;
            if options.browser_assets_upgrade {
                let caddy = self.bytes("Caddyfile.candidate", 1024 * 1024, owner)?;
                self.records.publish_caddy(&config, &caddy, owner)?;
                let digest = super::super::super::files::hex(&sha2::Sha256::digest(&caddy));
                self.atomic(
                    &config,
                    "browser-assets-installed.json",
                    &encoded(&json!({"schemaVersion":1,"caddyfileSha256":digest}))?,
                    owner,
                    false,
                )?;
                report["browserAssetProxyUpgraded"] = json!(true);
            }
            if let Some(input) = self.maintenance_candidate_input {
                self.records.files[input].unname(owner)?;
                // Retained parent authority is verified by unname. Persist the
                // consumption before publishing the finalized selected journal.
                for directory in &self.maintenance_directories {
                    directory.sync(owner)?;
                }
            }
            self.journal("maintenance_selected", true, owner, false)?;
            self.originals(access, owner, false)?;
            if let Some(assets) = self.assets.take() {
                assets.finish(owner).map_err(|_| Cause::Close)?;
            }
            self.stage.prepare_maintenance_unlock(owner)?;
            self.stage.unlock(owner);
            // unlock records errors; continuation must explicitly fence them.
            owner.check(Window::Body)?;
            self.maintenance_evidence
                .as_mut()
                .ok_or(Cause::Changed)?
                .first_unlock_elapsed_ns = Some(elapsed(owner)?);
            Ok(())
        }
        .await;
        owner.end_deploy_candidate();
        self.candidate_end = None;
        selected?;
        owner.check(Window::Body)
    }

    async fn maintenance_finish(
        &mut self,
        access: &Access,
        selection: &PreparedSelection,
        manifest: &Manifest,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        report["phase"] = json!("verify");
        self.stage
            .reacquire_maintenance(&stage_access(access), owner)?;
        self.maintenance_evidence
            .as_mut()
            .ok_or(Cause::Changed)?
            .second_lock_acquired_elapsed_ns = Some(elapsed(owner)?);
        self.originals(access, owner, false)?;
        self.ready(access, None, 30, false, owner, executor).await?;
        self.ready(access, Some(&selection.origin), 30, false, owner, executor)
            .await?;
        let app = self
            .container(access, "simplestchat", false, owner, executor)
            .await?;
        let image = self
            .stage
            .selected_image()
            .ok_or(Cause::Changed)?
            .to_owned();
        let actual_image = self.policy(owner, text(&app, "image"))?.to_owned();
        self.policy(
            owner,
            ensure(actual_image == image, "Replacement is not the staged image"),
        )?;
        let hash = self
            .compose(
                access,
                argv(&["config", "--hash", "simplestchat"]),
                None,
                false,
                owner,
                executor,
            )
            .await?;
        let hash = self.policy(owner, raw_text(&hash))?;
        let fields = hash
            .split(stage::python_space)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        let expected_hash = self.policy(owner, text(&app, "configHash"))?;
        self.policy(
            owner,
            ensure(
                fields == ["simplestchat", expected_hash],
                "The launcher did not start the app from the installed configuration",
            ),
        )?;
        let database = self
            .container(access, "postgres", false, owner, executor)
            .await?;
        let database_id = self
            .policy(owner, text(&selection.database, "id"))?
            .to_owned();
        let actual_database = self.policy(owner, text(&database, "id"))?.to_owned();
        self.policy(
            owner,
            ensure(
                actual_database == database_id,
                "The database container was replaced during maintenance",
            ),
        )?;
        let ledger_bytes = self
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
        let applied = self.policy(owner, ledger(&ledger_bytes))?;
        self.policy(
            owner,
            ensure(
                applied == manifest.fields().migrations,
                "Ledger differs from the candidate after migration",
            ),
        )?;
        report["interruptionFinishedAt"] = json!(timestamp());
        report["phase"] = json!("complete");
        self.journal("complete", true, owner, false)
    }
}

pub(in crate::release::attestation) async fn execute(
    options: &MaintainOptions,
    access: &Access,
    owner: &mut Owner,
    executor: &mut impl Executor,
    launcher: &mut impl Launcher,
) -> Outcome {
    let mut state = State::new(access);
    state.maintenance_evidence = Some(Evidence::default());
    let mut report = json!({"action":"maintain","revision":options.revision,
        "startedAt":timestamp(),"passed":false,"phase":"stage"});
    let operation: Result<()> = async {
        let manifest = state
            .stage
            .setup(&stage_access(access), &options.revision, owner)?;
        state.config = Some(Dir::open(&access.config, owner)?);
        let parent = Dir::open(access.curl.parent().ok_or(Cause::Input)?, owner)?;
        state.curl_directory = Some(parent.clone());
        state.records.files.push(Record::tool(
            parent,
            access
                .curl
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or(Cause::Input)?,
            access.uid,
            owner,
        )?);
        if options.browser_assets_upgrade {
            state.input("Caddyfile", 1024 * 1024, access, owner)?;
            if state
                .config()
                .exists("browser-assets-installed.json".as_ref(), owner)?
            {
                state.input("browser-assets-installed.json", 512, access, owner)?;
            }
        }
        state
            .stage
            .stage(
                &stage_access(access),
                &manifest,
                owner,
                &mut StageExecutor(executor),
            )
            .await?;
        state.commands = state.stage.command_count();
        report["phase"] = json!("preflight");
        let common = Options {
            revision: options.revision.clone(),
            quiet_seconds: 0,
            whole: options.whole,
            reserve: options.reserve,
        };
        let selection = state
            .selection(
                replacement::SelectionRequest {
                    options: &common,
                    maintenance: Some(options),
                },
                access,
                &manifest,
                &mut report,
                owner,
                executor,
            )
            .await?;
        if options.browser_assets_upgrade {
            let data = state.bytes("Caddyfile", 1024 * 1024, owner)?;
            let attempt = state.attempt();
            state.atomic(&attempt, "before-Caddyfile", &data, owner, false)?;
        }
        state
            .maintenance_select(options, access, &selection, &mut report, owner, executor)
            .await?;
        report["phase"] = json!("launcher");
        let ceiling = before(owner, POST_LAUNCH)?;
        let entered = owner.now();
        let mut observed = None;
        let launched = launcher.run(owner, ceiling, &mut observed);
        report["launcherPassed"] = json!(launched.is_ok());
        report["nativeLauncher"] = observed.unwrap_or(Value::Null);
        launched?;
        owner.check(Window::Body)?;
        report["launcherSeconds"] = json!(
            (owner.now().saturating_duration_since(entered).as_secs_f64() * 10.0).round() / 10.0
        );
        state
            .maintenance_finish(access, &selection, &manifest, &mut report, owner, executor)
            .await
    }
    .await;
    match serde_json::to_value(&state.maintenance_evidence) {
        Ok(value) => report["maintenanceEvidence"] = value,
        Err(_) => owner.record(Cause::Io),
    }
    finish_operation(state, report, operation, access, owner, executor).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stop_debt_includes_both_stops_native_local_tail_and_final_queries() {
        assert_eq!(FIRST_STOP, APP_STOP + PROXY_STOP + NATIVE + POST_LAUNCH);
        assert_eq!(FIRST_STOP.as_secs(), 1411);
        let start = Instant::now();
        let mut owner =
            Owner::new(start, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
        owner.observe_at(start + Duration::from_secs(329));
        assert_eq!(before(&mut owner, FIRST_STOP).err(), Some(Cause::Deadline));
        assert_eq!(owner.end(), start + Duration::from_secs(1800));
        assert_eq!(owner.body_end(), start + Duration::from_secs(1740));
        let _ = owner.finish();
    }
}
