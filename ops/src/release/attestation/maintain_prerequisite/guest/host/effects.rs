//! Host-only epoch joins, with the same live collector and capture owners.
use super::super::host::{Baseline, Observed, events};
use super::*;
use sha2::{Digest, Sha256, Sha384};

async fn read(ctx: &mut Context, path: &Path, maximum: u64) -> Result<Vec<u8>> {
    ctx.originals
        .push(Original::open(path, maximum, 0, 0o600, &mut ctx.control)?);
    ctx.originals
        .last()
        .ok_or(Error::Close)?
        .data(maximum, &mut ctx.control)
}

fn pending_health(rows: &[Value], pg: &str, health: &str) -> Result<BTreeMap<String, u8>> {
    let mut pending = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|row| row["Type"] == "container" && row["Actor"]["ID"] == pg)
    {
        let action = policy::string(&row["Action"])?;
        let attrs = &row["Actor"]["Attributes"];
        if action == format!("exec_create: {health}") {
            let id = policy::string(&attrs["execID"])?;
            require(policy::digest(id) && pending.insert(id.into(), 1).is_none())?;
        } else if let Some(id) = attrs["execID"].as_str()
            && let Some(phase) = pending.get_mut(id)
        {
            if action == format!("exec_start: {health}") {
                require(*phase == 1)?;
                *phase = 2;
            } else {
                require(action == "exec_die" && *phase == 2 && attrs["exitCode"] == "0")?;
                pending.remove(id);
            }
        }
    }
    require(pending.len() <= 1)?;
    Ok(pending)
}
impl Fixture {
    pub(in super::super) fn baseline_pg_evidence(&self) -> Option<Value> {
        self.cold_pg.as_ref().map(super::cold_pg::ColdPg::evidence)
    }
    pub(in super::super) fn host_boundary(
        &mut self,
        ctx: &mut Context,
        baseline: &mut Baseline,
    ) -> Result<()> {
        self.native_boundary(ctx)?;
        let monitors = self.monitors.lock().map_err(|_| Error::Io)?;
        let collector = monitors.collector.as_ref().ok_or(Error::Refused)?;
        baseline.health_pending = pending_health(
            &collector.rows[..collector.native_start],
            &baseline.pg,
            &baseline.health,
        )?;
        let raw = &collector.rows[..collector.native_start];
        let mut audit = super::super::host::events::Auditor::new(raw);
        audit.begin_cold_pg();
        let result = self.cold_pg.as_mut().ok_or(Error::Refused)?.reconcile(
            raw,
            &baseline.health_pending,
            &mut audit,
        );
        if result.is_err() && ctx.control.first.is_none() {
            ctx.control.checkpoint = "daemon-pg-history";
        }
        super::observation::retain_pg_failure(
            true,
            None,
            ctx.control.first,
            &mut ctx.host_diagnostic,
            &result,
            audit,
        );
        result
    }
    pub(in super::super) async fn host_baseline(&mut self, ctx: &mut Context) -> Result<Baseline> {
        require(ctx.request.is_host())?;
        require(self.cold_opening.is_none() && self.cold_pg.is_none())?;
        self.check_monitors(ctx)?;
        {
            let monitors = self.monitors.lock().map_err(|_| Error::Io)?;
            let collector = monitors.collector.as_ref().ok_or(Error::Refused)?;
            self.cold_opening = Some(super::cold_pg::Opening::capture(
                &collector.rows[..collector.native_start],
                collector.rows.len(),
                self.pg.as_deref().ok_or(Error::Refused)?,
                &self.pg_health,
            )?);
            self.native_rows = collector.rows[collector.native_start..].to_vec();
        }
        let attempt = self.attempt(ctx).await?.ok_or(Error::Refused)?;
        self.daemon_observe(ctx, Some(&attempt)).await?;
        self.database_observe(ctx, Some(&attempt)).await?;
        self.tls_observe(ctx).await?;
        self.application_observe(ctx).await?;
        self.source_observe_for(ctx, false).await?;
        let pg = self.pg.clone().ok_or(Error::Refused)?;
        let image = self.image.clone();
        let (app, app_row) = self.service(ctx, "simplestchat", &image).await?;
        let image = self.caddy_image.clone();
        let (proxy, proxy_row) = self.service(ctx, "caddy", &image).await?;
        require(
            self.app.as_deref() == Some(app.as_str())
                && self.proxy.as_deref() == Some(proxy.as_str()),
        )?;
        Ok(Baseline {
            pg,
            app,
            proxy,
            image: self.image.clone(),
            caddy_image: self.caddy_image.clone(),
            network: self.network.clone().ok_or(Error::Refused)?,
            none_network: self
                .none_network
                .as_ref()
                .ok_or(Error::Refused)?
                .id()
                .into(),
            health: self.pg_health.clone(),
            health_pending: BTreeMap::new(),
            shell_pg_commands: self.expected_pg_commands(ctx)?,
            shell_attempt: None,
            app_row,
            proxy_row,
            attempts: directory_names(&Path::new(PUBLIC).join("results"))?,
        })
    }
    async fn host_service(
        &mut self,
        ctx: &mut Context,
        name: &str,
        image: &str,
        running: bool,
    ) -> Result<(String, Value)> {
        let ids = policy::ids(
            &Self::compose(
                ctx,
                &format!("host-service-{name}"),
                &["ps".into(), "--all".into(), "--quiet".into(), name.into()],
                15,
            )
            .await?,
        )?;
        require(ids.len() == 1)?;
        let id = ids.first().ok_or(Error::Refused)?.clone();
        let row = policy::one(
            &Self::docker(
                ctx,
                &format!("host-inspect-{name}"),
                &["inspect".into(), id.clone()],
                15,
            )
            .await?,
        )?;
        let labels = &row["Config"]["Labels"];
        require(
            row["Id"] == id
                && row["Image"] == image
                && labels[LABEL] == ctx.request.run_id
                && labels["com.docker.compose.project"] == PROJECT
                && labels["com.docker.compose.service"] == name
                && row["State"]["Running"] == running,
        )?;
        self.actors.insert(id.clone());
        Ok((id, row))
    }
    async fn host_ledger(&mut self, ctx: &mut Context) -> Result<()> {
        let actual = self
            .sql(
                ctx,
                "host-final-ledger",
                concat!(
                    "SELECT version,success,encode(checksum,'hex') FROM ",
                    "public._sqlx_migrations ORDER BY version"
                ),
            )
            .await?;
        let mut expected = BTreeMap::new();
        for (name, bytes) in &ctx.sources {
            if name.starts_with("migrations/") {
                let version = Path::new(name)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .and_then(|v| v.split_once('_'))
                    .ok_or(Error::Refused)?
                    .0
                    .parse::<u64>()
                    .map_err(|_| Error::Refused)?
                    .to_string();
                require(
                    expected
                        .insert(version, super::super::files::hex(&Sha384::digest(bytes)))
                        .is_none(),
                )?;
            }
        }
        require(expected.len() == 36)?;
        let mut rows = BTreeMap::new();
        for line in std::str::from_utf8(&actual)
            .map_err(|_| Error::Refused)?
            .lines()
        {
            let parts = line.split('|').collect::<Vec<_>>();
            require(parts.len() == 3 && parts[1] == "t")?;
            require(
                rows.insert(parts[0].to_owned(), parts[2].to_owned())
                    .is_none(),
            )?;
        }
        require(rows == expected)
    }
    async fn host_backup(&mut self, ctx: &mut Context, attempt: &Path) -> Result<()> {
        let receipt = policy::decode(
            &read(ctx, &attempt.join("database-before.receipt.json"), 65536).await?,
        )?;
        super::super::protocol::exact(
            &receipt,
            &[
                "schemaVersion",
                "dump",
                "bytes",
                "sha256",
                "revision",
                "completedAt",
            ],
        )?;
        let digest = policy::string(&receipt["sha256"])?;
        let count = receipt["bytes"].as_u64().ok_or(Error::Refused)?;
        require(
            receipt["schemaVersion"] == 1
                && receipt["dump"] == "database-before.dump"
                && receipt["revision"] == ctx.request.revision
                && policy::digest(digest)
                && count > 5
                && count <= 16 * 1024 * 1024,
        )?;
        chrono::DateTime::parse_from_rfc3339(policy::string(&receipt["completedAt"])?)
            .map_err(|_| Error::Refused)?;
        let dump = read(ctx, &attempt.join("database-before.dump"), 16 * 1024 * 1024).await?;
        require(
            dump.len() as u64 == count
                && dump.starts_with(b"PGDMP")
                && super::super::files::hex(&Sha256::digest(&dump)) == digest,
        )
    }
    pub(in super::super) async fn host_observe(
        &mut self,
        ctx: &mut Context,
        baseline: &Baseline,
        attempt: &Path,
        observed: &mut Observed,
    ) -> Result<Value> {
        require(ctx.request.is_host())?;
        let success = ctx.request.host_success();
        ctx.control.checkpoint = "host-events";
        // Freeze the complete operation history before independent SQL observers.
        // The same collector remains owned and is joined again idempotently.
        self.collector_finish(ctx).await?;
        {
            let monitors = self.monitors.lock().map_err(|_| Error::Io)?;
            let collector = monitors.collector.as_ref().ok_or(Error::Refused)?;
            self.native_rows = collector.rows[collector.native_start..].to_vec();
        }
        ctx.control.checkpoint = "host-observe";
        let image = self.pg_image.clone();
        let (pg, pg_row) = self.host_service(ctx, "postgres", &image, true).await?;
        require(pg == baseline.pg && pg_row["State"]["Health"]["Status"] == "healthy")?;
        observed.set("databaseContainerUnchanged", true)?;
        let image = self.image.clone();
        let (app, app_row) = self
            .host_service(ctx, "simplestchat", &image, success)
            .await?;
        let image = self.caddy_image.clone();
        let (proxy, proxy_row) = self.host_service(ctx, "caddy", &image, success).await?;
        require(
            (if ctx.request.proxy_upgrade() {
                proxy != baseline.proxy
            } else {
                proxy == baseline.proxy
            }) && if success {
                app != baseline.app
            } else {
                app == baseline.app
            },
        )?;
        if success {
            let env = app_row["Config"]["Env"].as_array().ok_or(Error::Refused)?;
            let expected = if ctx.request.proxy_upgrade() {
                "MAX_ROOMS=175"
            } else {
                "MAX_ROOMS=176"
            };
            require(
                env.iter()
                    .filter(|value| value.as_str() == Some(expected))
                    .count()
                    == 1,
            )?;
        }
        let current = policy::ids(
            &Self::docker(
                ctx,
                "host-fresh-inventory",
                &[
                    "ps".into(),
                    "--all".into(),
                    "--quiet".into(),
                    "--no-trunc".into(),
                ],
                15,
            )
            .await?,
        )?;
        require(current == BTreeSet::from([pg.clone(), app.clone(), proxy.clone()]))?;
        observed.set("freshActorInventoryJoined", true)?;
        ctx.control.checkpoint = "host-none-network";
        let fresh_none = policy::one(
            &Self::docker(
                ctx,
                "host-none-fresh",
                &[
                    "network".into(),
                    "inspect".into(),
                    baseline.none_network.clone(),
                ],
                15,
            )
            .await?,
        )?;
        self.none_network
            .as_ref()
            .ok_or(Error::Refused)?
            .check(&fresh_none)?;
        ctx.control.checkpoint = "host-attempt-delta";
        let names = directory_names(&Path::new(PUBLIC).join("results"))?;
        let added = names
            .difference(&baseline.attempts)
            .cloned()
            .collect::<Vec<_>>();
        let root_name = attempt
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or(Error::Refused)?;
        require(added.len() == 2 && added.iter().any(|v| v == root_name))?;
        let shell = added
            .iter()
            .find(|name| name.as_str() != root_name)
            .ok_or(Error::Refused)?;
        let suffix = shell.strip_prefix("deploy.").ok_or(Error::Refused)?;
        require(suffix.len() == 8 && suffix.bytes().all(|v| v.is_ascii_alphanumeric()))?;
        let shell_path = Path::new(PUBLIC).join("results").join(shell);
        ctx.control.checkpoint = "host-shell-outcome";
        let outcome = policy::decode(&read(ctx, &shell_path.join("outcome.json"), 131072).await?)?;
        super::super::protocol::exact(
            &outcome,
            &[
                "startedAt",
                "finishedAt",
                "phase",
                "exitStatus",
                "passed",
                "serverImage",
            ],
        )?;
        require(
            outcome["passed"] == success
                && outcome["exitStatus"] == if success { 0 } else { 1 }
                && outcome["phase"] == if success { "complete" } else { "configuration" }
                && outcome["serverImage"] == self.image,
        )?;
        ctx.control.checkpoint = "host-shell-validation";
        let validation = if success {
            Some(
                std::str::from_utf8(
                    &read(ctx, &shell_path.join("validation-name.txt"), 4096).await?,
                )
                .map_err(|_| Error::Refused)?
                .trim()
                .to_owned(),
            )
        } else {
            None
        };
        let inputs = Baseline {
            pg: baseline.pg.clone(),
            app: baseline.app.clone(),
            proxy: baseline.proxy.clone(),
            image: baseline.image.clone(),
            caddy_image: baseline.caddy_image.clone(),
            network: baseline.network.clone(),
            none_network: baseline.none_network.clone(),
            health: baseline.health.clone(),
            health_pending: baseline.health_pending.clone(),
            shell_pg_commands: baseline.shell_pg_commands.clone(),
            shell_attempt: validation,
            app_row: baseline.app_row.clone(),
            proxy_row: baseline.proxy_row.clone(),
            attempts: baseline.attempts.clone(),
        };
        ctx.control.checkpoint = "host-event-history";
        if let Err(failed) = events::validate(
            &self.native_rows,
            &inputs,
            &ctx.request,
            &app_row,
            &proxy_row,
        ) {
            ctx.control.checkpoint = failed.checkpoint;
            ctx.host_diagnostic = failed.diagnostic;
            return Err(failed.cause);
        }
        let mut audit = super::super::host::events::Auditor::new(&self.native_rows);
        audit.begin_cold_pg();
        let result = self
            .cold_pg
            .as_mut()
            .ok_or(Error::Refused)?
            .finish(&self.native_rows, &mut audit);
        if result.is_err() && ctx.control.first.is_none() {
            ctx.control.checkpoint = "daemon-pg-history";
        }
        super::observation::retain_pg_failure(
            true,
            None,
            ctx.control.first,
            &mut ctx.host_diagnostic,
            &result,
            audit,
        );
        result?;
        observed.set("finiteDaemonHistoryObserved", true)?;
        observed.set("backupListPassed", true)?;
        // Only an independently bounded global inventory plus closed histories
        // retires obsolete baseline/intermediate IDs from cleanup authority.
        self.actors = current;
        self.pg = Some(pg);
        self.app = Some(app);
        self.proxy = Some(proxy);
        self.unknown_effect = false;
        self.host_ledger(ctx).await?;
        observed.set("exactLedgerObserved", true)?;
        ctx.control.checkpoint = "host-backup";
        self.host_backup(ctx, attempt).await?;
        observed.set("backupHeaderAndDigestMatched", true)?;
        let journal = policy::decode(
            &read(ctx, &Path::new(PUBLIC).join("release-state.json"), 65536).await?,
        )?;
        super::super::protocol::exact(
            &journal,
            &["schemaVersion", "attempt", "finalized", "phase"],
        )?;
        require(
            journal["schemaVersion"] == 1
                && journal["attempt"] == attempt.to_str().ok_or(Error::Refused)?
                && journal["finalized"] == true
                && journal["phase"]
                    == if success {
                        "complete"
                    } else {
                        "maintenance_selected"
                    },
        )?;
        if success {
            self.tls_observe(ctx).await?;
            self.application_observe(ctx).await?;
            observed.set("applicationReadinessObserved", true)?;
        } else {
            observed.set("applicationStoppedObserved", true)?;
        }
        self.source_observe_for(ctx, !success).await?;
        observed.set("sourceCleanObserved", success)?;
        observed.set("sourceDirtyObserved", !success)?;
        ctx.check()?;
        Ok(proxy_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn event(action: &str, id: &str) -> Value {
        json!({
            "Type":"container","Action":action,
            "Actor":{"ID":"a".repeat(64),"Attributes":{"execID":id,"exitCode":"0"}}
        })
    }
    #[test]
    fn only_previously_observed_exact_health_execs_seed_the_new_epoch() {
        let pg = "a".repeat(64);
        let id = "b".repeat(64);
        let create = event("exec_create: fixed-health", &id);
        let start = event("exec_start: fixed-health", &id);
        let end = event("exec_die", &id);
        assert_eq!(
            pending_health(std::slice::from_ref(&create), &pg, "fixed-health").unwrap(),
            BTreeMap::from([(id.clone(), 1)])
        );
        assert_eq!(
            pending_health(&[create.clone(), start.clone()], &pg, "fixed-health").unwrap(),
            BTreeMap::from([(id.clone(), 2)])
        );
        assert!(
            pending_health(
                &[create.clone(), start.clone(), end.clone()],
                &pg,
                "fixed-health"
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            pending_health(
                &[create.clone(), event("exec_start: other", &id)],
                &pg,
                "fixed-health"
            )
            .is_err()
        );
        let mut nonzero = end;
        nonzero["Actor"]["Attributes"]["exitCode"] = "1".into();
        assert!(pending_health(&[create.clone(), start, nonzero], &pg, "fixed-health").is_err());
        assert!(
            pending_health(
                &[create, event("exec_create: fixed-health", &"c".repeat(64))],
                &pg,
                "fixed-health"
            )
            .is_err()
        );
        assert!(
            pending_health(&[event("exec_create: other", &id)], &pg, "fixed-health")
                .unwrap()
                .is_empty()
        );
    }
}
