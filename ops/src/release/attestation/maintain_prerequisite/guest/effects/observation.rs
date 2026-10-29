//! Independent fixed SQL, daemon, source and known-resource retirement facts.

use super::super::host::events::{Auditor, PgReason};
use super::*;
use sha2::{Digest, Sha256, Sha384};
use std::os::unix::fs::OpenOptionsExt;

pub(super) const EMPTY_SCHEMA: &str = concat!(
    "SELECT to_regclass('public._sqlx_migrations') IS NULL AND ",
    "to_regclass('public.users') IS NULL AND to_regclass('public.rooms') IS NULL",
);
pub(super) const INITIAL_ROLES: &str = concat!(
    "SELECT rolname,NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole ",
    "AND NOT rolreplication AND NOT rolbypassrls FROM pg_roles WHERE rolname IN ",
    "('simplestchat_app','simplestchat_migrate') ORDER BY rolname",
);
const RUNTIME_PRIVILEGES: &str = concat!(
    "SELECT NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole AND ",
    "NOT rolreplication AND NOT rolbypassrls AND NOT ",
    "has_database_privilege('simplestchat_app','simplestchat','CREATE') AND NOT ",
    "has_schema_privilege('simplestchat_app','public','CREATE') AND NOT ",
    "has_table_privilege('simplestchat_app','public._sqlx_migrations',",
    "'SELECT,INSERT,UPDATE,DELETE') FROM pg_roles WHERE rolname='simplestchat_app'",
);

fn close_phase<T>(result: Result<T>, ended: Result<()>) -> Result<T> {
    result.and_then(|value| ended.map(|()| value))
}

#[derive(Clone, Copy)]
pub(super) enum DaemonStage {
    ActorIdentity,
    Inventory,
    ValidationInput,
    EventShape,
    ActorClassification,
    Rename,
    ActionHistory,
    NetworkHistory,
    PgHistory,
    FinalTraces,
}
impl DaemonStage {
    fn checkpoint(self) -> &'static str {
        match self {
            Self::ActorIdentity => "daemon-actor-identity",
            Self::Inventory => "daemon-inventory",
            Self::ValidationInput => "daemon-validation-input",
            Self::EventShape => "daemon-event-shape",
            Self::ActorClassification => "daemon-actor-classification",
            Self::Rename => "daemon-rename",
            Self::ActionHistory => "daemon-action-history",
            Self::NetworkHistory => "daemon-network-history",
            Self::PgHistory => "daemon-pg-history",
            Self::FinalTraces => "daemon-final-traces",
        }
    }
}

pub(super) fn daemon_result(
    control: &mut super::super::Control,
    earlier: Option<Error>,
    stage: DaemonStage,
    result: Result<()>,
) -> Result<()> {
    if let Err(error) = result {
        let first = earlier.is_none() && control.first.is_none_or(|first| first == error);
        // Record a returned policy error before the caller's later phase-pop fence.
        control.record(error);
        if first {
            control.checkpoint = stage.checkpoint();
        }
    }
    result
}

fn private_attempt_name(value: &str) -> bool {
    value
        .strip_prefix("deploy.")
        .is_some_and(|v| v.len() == 8 && v.bytes().all(|b| b.is_ascii_alphanumeric()))
}
fn timestamp(value: &Value) -> bool {
    value.as_str().is_some_and(|v| {
        v.len() == 20
            && v.bytes().enumerate().all(|(i, b)| match i {
                4 | 7 => b == b'-',
                10 => b == b'T',
                13 | 16 => b == b':',
                19 => b == b'Z',
                _ => b.is_ascii_digit(),
            })
    })
}

pub(super) fn retain_pg_failure(
    host: bool,
    earlier: Option<Error>,
    current: Option<Error>,
    diagnostic: &mut Option<Value>,
    result: &Result<()>,
    audit: Auditor<'_>,
) {
    if host && result.is_err() && earlier.is_none() && current.is_none() && diagnostic.is_none() {
        *diagnostic = audit.diagnostic();
    }
}

pub(super) fn derive_pg_commands(
    source: Option<&[u8]>,
    mut audit: Option<&mut Auditor<'_>>,
) -> Result<BTreeMap<String, usize>> {
    if let Some(audit) = audit.as_deref_mut() {
        audit.post_rows();
    }
    let source = source.ok_or_else(|| {
        policy::pg_note(&mut audit, PgReason::SourceMissing, None, None, None, None);
        Error::Refused
    })?;
    let source = std::str::from_utf8(source).map_err(|_| {
        policy::pg_note(&mut audit, PgReason::SourceUtf8, None, None, None, None);
        Error::Refused
    })?;
    let mut queries = Vec::new();
    for rest in source.split("--command \"").skip(1) {
        let query = rest.split('"').next().ok_or_else(|| {
            policy::pg_note(&mut audit, PgReason::QueryShape, None, None, None, None);
            Error::Refused
        })?;
        require(!query.is_empty() && !query.contains('\n')).inspect_err(|_| {
            policy::pg_note(&mut audit, PgReason::QueryShape, None, None, None, None);
        })?;
        queries.push(query);
    }
    require(queries.len() == 2).inspect_err(|_| {
        policy::pg_note(
            &mut audit,
            PgReason::QueryCount,
            None,
            None,
            Some(2),
            Some(queries.len()),
        );
    })?;
    let base = concat!(
        "psql --no-psqlrc --set ON_ERROR_STOP=on --host /run/simplestchat-postgres ",
        "--username postgres --dbname simplestchat",
    );
    Ok(BTreeMap::from([
        ("id -un".into(), 1),
        (
            format!(
                "{base} --tuples-only --no-align --field-separator   --command {}",
                queries[0],
            ),
            1,
        ),
        (base.into(), 1),
        (
            format!("{base} --tuples-only --no-align --command {}", queries[1]),
            1,
        ),
        (
            concat!(
                "pg_dump --host /run/simplestchat-postgres --username postgres ",
                "--dbname simplestchat --format custom",
            )
            .into(),
            1,
        ),
    ]))
}
async fn retained_data(ctx: &mut Context, path: &Path, maximum: u64) -> Result<(Vec<u8>, String)> {
    let original = Original::open(path, maximum, 0, 0o600, &mut ctx.control)?;
    ctx.originals.push(original);
    let index = ctx.originals.len() - 1;
    let bytes = ctx.originals[index].data(maximum, &mut ctx.control)?;
    Ok((bytes, ctx.originals[index].sha256.clone()))
}

/// Admit only Compose's replacement-app rename before changing its trace.
pub(super) fn app_replacement_rename(
    row: &Value,
    app_id: &str,
    inspected_name: &str,
    run_id: &str,
    image: &str,
    services: &BTreeMap<String, String>,
    traces: &BTreeMap<String, Vec<String>>,
) -> Result<()> {
    const NAME: &str = "simplestchat-public-simplestchat-1";
    require(policy::digest(app_id) && inspected_name == format!("/{NAME}"))?;
    let apps = services
        .iter()
        .filter(|(_, service)| service.as_str() == "simplestchat")
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    require(apps.len() == 2 && apps.contains(&app_id))?;
    let old_id = apps
        .into_iter()
        .find(|id| *id != app_id)
        .ok_or(Error::Refused)?;
    // Digest admission proves the prefix boundary is ASCII and in range.
    require(policy::digest(old_id))?;
    let attributes = &row["Actor"]["Attributes"];
    policy::object(attributes)?;
    require(
        row["Type"] == "container"
            && row["Action"] == "rename"
            && row["Actor"]["ID"] == app_id
            && attributes[LABEL] == run_id
            && attributes["com.docker.compose.project"] == PROJECT
            && attributes["com.docker.compose.service"] == "simplestchat"
            && attributes["com.docker.compose.container-number"] == "1"
            && attributes["com.docker.compose.replace"] == "simplestchat-1"
            && attributes["image"] == image
            && attributes["name"] == NAME
            && attributes["oldName"] == format!("/{}_{NAME}", &old_id[..12])
            && traces.get(app_id).is_some_and(|v| v == &["create"])
            && traces
                .get(old_id)
                .is_some_and(|v| v == &["create", "start", "die", "destroy"]),
    )
}

/// Join admitted actor actions without ordering a successful stop after die.
///
/// The stopped waiter may publish stop before the monitor publishes die. This
/// early form requires the same actor's already admitted TERM; final traces
/// still require die and destroy. Early error-recovery stops without that TERM remain
/// outside this narrow profile.
pub(super) fn container_action(
    row: &Value,
    service: Option<&str>,
    app: Option<&str>,
    validation: Option<&str>,
    traces: &mut BTreeMap<String, Vec<String>>,
    mutations: &mut BTreeSet<(String, String)>,
) -> Result<()> {
    let action = policy::string(&row["Action"])?;
    let id = policy::string(&row["Actor"]["ID"])?;
    let attributes = &row["Actor"]["Attributes"];
    let phases = traces.entry(id.into()).or_default();
    require(["create", "start", "kill", "die", "stop", "destroy"].contains(&action))?;
    if action == "kill" {
        require(
            service.is_some_and(|s| ["migrate", "simplestchat"].contains(&s))
                && phases == &["create", "start"]
                && attributes["signal"] == "15",
        )?;
    } else if action == "stop" {
        let early = phases == &["create", "start"]
            && mutations.contains(&(id.to_owned(), "kill".to_owned()));
        require(
            service.is_some_and(|s| ["migrate", "simplestchat"].contains(&s))
                && (phases == &["create", "start", "die"] || early),
        )?;
    }
    if ["kill", "stop"].contains(&action) {
        require(Some(id) != app && mutations.insert((id.to_owned(), action.to_owned())))?;
    }
    if ["create", "start", "die", "destroy"].contains(&action) {
        phases.push(action.into());
        if action == "die" && (service == Some("migrate") || validation == Some(id)) {
            require(attributes["exitCode"] == "0")?;
        }
    }
    Ok(())
}

/// Require the complete fixed actor histories, including app replacement.
pub(super) fn expected_daemon_traces(
    validation: &str,
    app: &str,
    proxy: &str,
    services: &BTreeMap<String, String>,
    traces: &BTreeMap<String, Vec<String>>,
) -> Result<()> {
    require(
        traces
            .get(validation)
            .is_some_and(|v| v == &["create", "start", "die", "destroy"]),
    )?;
    let migration = services
        .iter()
        .filter(|(_, s)| s.as_str() == "migrate")
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    let apps = services
        .iter()
        .filter(|(_, s)| s.as_str() == "simplestchat")
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    let proxies = services
        .iter()
        .filter(|(_, s)| s.as_str() == "caddy")
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();
    require(
        migration.len() == 1
            && traces
                .get(migration[0])
                .is_some_and(|v| v == &["create", "start", "die", "destroy"]),
    )?;
    require(
        apps.len() == 2
            && apps.contains(&app)
            && traces
                .get(app)
                .is_some_and(|v| v == &["create", "rename", "start"]),
    )?;
    for id in apps {
        if id != app {
            require(
                traces
                    .get(id)
                    .is_some_and(|v| v == &["create", "start", "die", "destroy"]),
            )?;
        }
    }
    require(proxies == [proxy] && traces.get(proxy).is_some_and(|v| v == &["create", "start"]))
}

impl Fixture {
    pub(super) async fn attempt(&mut self, ctx: &mut Context) -> Result<Option<PathBuf>> {
        ctx.control.check()?;
        let names = directory_names(&Path::new(PUBLIC).join("results"))?;
        let added = names.difference(&self.attempts_before).collect::<Vec<_>>();
        if ["cancel-owned-launcher", "timeout-stopped-launcher"]
            .contains(&ctx.request.scenario.as_str())
        {
            require(added.is_empty())?;
            return Ok(None);
        }
        require(added.len() == 1 && private_attempt_name(added[0]))?;
        let path = Path::new(PUBLIC).join("results").join(added[0]);
        let metadata = fs::symlink_metadata(&path).map_err(|_| Error::Io)?;
        require(
            metadata.is_dir()
                && metadata.uid() == 0
                && metadata.gid() == 0
                && metadata.mode() & 0o7777 == 0o700,
        )?;
        let (data, _) = retained_data(ctx, &path.join("outcome.json"), 131072).await?;
        let value = policy::decode(&data)?;
        let object = policy::object(&value)?;
        require(
            object.len() == 6
                && [
                    "startedAt",
                    "finishedAt",
                    "phase",
                    "exitStatus",
                    "passed",
                    "serverImage",
                ]
                .iter()
                .all(|n| object.contains_key(*n)),
        )?;
        let positive = ctx.request.native_scenario() == "positive";
        require(
            value["serverImage"] == self.image
                && value["exitStatus"].as_i64() == Some(if positive { 0 } else { 1 })
                && value["passed"].as_bool() == Some(positive)
                && value["phase"]
                    == if positive {
                        "complete"
                    } else {
                        "configuration"
                    }
                && timestamp(&value["startedAt"])
                && timestamp(&value["finishedAt"]),
        )?;
        let (data, _) = retained_data(ctx, &path.join("deployment-inputs.sha256"), 131072).await?;
        let mut expected = Vec::new();
        for (target, source) in [
            (
                "/usr/local/bin/simplestchat-public-deploy",
                Some("ops/ansible/files/deploy-public.sh"),
            ),
            (
                "/usr/local/bin/simplestchat-public",
                Some("ops/ansible/files/public-compose.sh"),
            ),
            ("/etc/simplestchat-public/compose.base.yml", None),
            ("/etc/simplestchat-public/compose.public.yml", None),
            ("/etc/simplestchat-public/Caddyfile", None),
        ] {
            let checksum = if let Some(source) = source {
                super::super::files::hex(&Sha256::digest(
                    ctx.sources.get(source).ok_or(Error::Refused)?,
                ))
            } else {
                let original = Original::open(
                    Path::new(target),
                    131072,
                    0,
                    if target.ends_with("Caddyfile") {
                        0o644
                    } else {
                        0o600
                    },
                    &mut ctx.control,
                )?;
                ctx.originals.push(original);
                ctx.originals.last().ok_or(Error::Refused)?.sha256.clone()
            };
            expected.push(format!("{checksum}  {target}"));
        }
        let actual = std::str::from_utf8(&data)
            .map_err(|_| Error::Refused)?
            .lines()
            .collect::<Vec<_>>();
        require(actual == expected.iter().map(String::as_str).collect::<Vec<_>>())?;
        Ok(Some(path))
    }
    pub(super) fn expected_pg_commands(&self, ctx: &Context) -> Result<BTreeMap<String, usize>> {
        self.expected_pg_commands_observed(ctx, None)
    }
    fn expected_pg_commands_observed(
        &self,
        ctx: &Context,
        audit: Option<&mut Auditor<'_>>,
    ) -> Result<BTreeMap<String, usize>> {
        if ctx.request.native_scenario() != "positive" {
            return Ok(BTreeMap::new());
        }
        derive_pg_commands(
            ctx.sources
                .get("ops/ansible/files/deploy-public.sh")
                .map(Vec::as_slice),
            audit,
        )
    }
    pub(super) async fn daemon_observe(
        &mut self,
        ctx: &mut Context,
        attempt: Option<&Path>,
    ) -> Result<()> {
        let earlier = ctx.control.first;
        if earlier.is_none() {
            ctx.control.checkpoint = "daemon-observe";
        }
        let mut stage = DaemonStage::ActorIdentity;
        let result = async {
            let positive = ctx.request.native_scenario() == "positive";
            let mut app_name = None;
            if positive {
                let image = self.image.clone();
                let (id, row) = self.service(ctx, "simplestchat", &image).await?;
                self.app = Some(id);
                app_name = Some(policy::string(&row["Name"])?.to_owned());
                let image = self.caddy_image.clone();
                self.proxy = Some(self.service(ctx, "caddy", &image).await?.0);
            }
            stage = DaemonStage::Inventory;
            let current = policy::ids(
                &Self::docker(
                    ctx,
                    "fresh-actor-inventory",
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
            let mut expected = BTreeSet::from([self.pg.clone().ok_or(Error::Refused)?]);
            if positive {
                expected.insert(self.app.clone().ok_or(Error::Refused)?);
                expected.insert(self.proxy.clone().ok_or(Error::Refused)?);
            }
            require(current == expected)?;
            ctx.observed.set("freshActorInventoryJoined", true)?;
            stage = DaemonStage::NetworkHistory;
            let none = self.none_network.as_ref().ok_or(Error::Refused)?;
            let fresh_none = policy::one(
                &Self::docker(
                    ctx,
                    "builtin-none-fresh",
                    &["network".into(), "inspect".into(), none.id().into()],
                    15,
                )
                .await?,
            )?;
            none.check(&fresh_none)?;
            stage = DaemonStage::ValidationInput;
            let validation_name = if positive && let Some(path) = attempt {
                Some(
                    String::from_utf8(
                        retained_data(ctx, &path.join("validation-name.txt"), 131072)
                            .await?
                            .0,
                    )
                    .map_err(|_| Error::Refused)?
                    .trim()
                    .to_owned(),
                )
            } else {
                None
            };
            let mut traces: BTreeMap<String, Vec<String>> = BTreeMap::new();
            let mut services: BTreeMap<String, String> = BTreeMap::new();
            let mut validation = None;
            let mut mutations = BTreeSet::new();
            let mut pg_rows = Vec::new();
            let mut none_history = policy::NoneHistory::default();
            for row in &self.native_rows {
                stage = DaemonStage::EventShape;
                let action = policy::string(&row["Action"])?;
                let actor = &row["Actor"];
                let attributes = &actor["Attributes"];
                policy::object(attributes)?;
                let id = policy::string(&actor["ID"])?;
                match policy::string(&row["Type"])? {
                    "container" => {
                        require(policy::digest(id))?;
                        if Some(id) == self.pg.as_deref() {
                            pg_rows.push(row.clone());
                            continue;
                        }
                        require(positive)?;
                        stage = DaemonStage::ActorClassification;
                        let service = attributes["com.docker.compose.service"].as_str();
                        if service
                            .is_some_and(|s| ["simplestchat", "migrate", "caddy"].contains(&s))
                        {
                            let service = service.ok_or(Error::Refused)?;
                            require(
                                attributes[LABEL] == ctx.request.run_id
                                    && attributes["com.docker.compose.project"] == PROJECT
                                    && if service == "caddy" {
                                        attributes["image"] == self.caddy_image
                                            || attributes["image"] == ctx.request.caddy_image
                                    } else {
                                        attributes["image"] == self.image
                                    },
                            )?;
                            services.insert(id.into(), service.into());
                        } else {
                            policy::validator_role(
                                attributes,
                                validation_name.as_deref().ok_or(Error::Refused)?,
                            )?;
                            require(
                                validation_name
                                    .as_deref()
                                    .is_some_and(|name| attributes["name"] == name)
                                    && (attributes["image"] == ctx.request.caddy_image
                                        || attributes["image"] == self.caddy_image),
                            )?;
                            validation = Some(id.to_owned());
                        }
                        if action == "rename" {
                            stage = DaemonStage::Rename;
                            app_replacement_rename(
                                row,
                                self.app.as_deref().ok_or(Error::Refused)?,
                                app_name.as_deref().ok_or(Error::Refused)?,
                                &ctx.request.run_id,
                                &self.image,
                                &services,
                                &traces,
                            )?;
                            traces
                                .get_mut(id)
                                .ok_or(Error::Refused)?
                                .push(action.into());
                            continue;
                        }
                        stage = DaemonStage::ActionHistory;
                        container_action(
                            row,
                            service,
                            self.app.as_deref(),
                            validation.as_deref(),
                            &mut traces,
                            &mut mutations,
                        )?;
                    }
                    "network" => {
                        stage = DaemonStage::NetworkHistory;
                        require(positive)?;
                        if Some(id) == self.network.as_deref() {
                            require(["connect", "disconnect"].contains(&action))?;
                        } else {
                            none_history.observe(
                                row,
                                none,
                                validation.as_deref(),
                                &services,
                                &traces,
                            )?;
                        }
                    }
                    _ => return Err(Error::Refused),
                }
            }
            stage = DaemonStage::PgHistory;
            if ctx.request.is_host() {
                let mut audit = Auditor::new(&self.native_rows);
                audit.begin_cold_pg();
                let opening = self.cold_opening.take();
                let result = self
                    .expected_pg_commands_observed(ctx, Some(&mut audit))
                    .and_then(|commands| {
                        super::cold_pg::ColdPg::admit(
                            opening.ok_or(Error::Refused)?,
                            &pg_rows,
                            &commands,
                            &mut audit,
                        )
                    });
                let result = result.map(|cold| self.cold_pg = Some(cold));
                retain_pg_failure(
                    ctx.request.is_host(),
                    earlier,
                    ctx.control.first,
                    &mut ctx.host_diagnostic,
                    &result,
                    audit,
                );
                result?;
            } else {
                policy::pg_exec_history(
                    &pg_rows,
                    &self.expected_pg_commands(ctx)?,
                    &self.pg_health,
                )?;
            }
            if positive {
                stage = DaemonStage::FinalTraces;
                expected_daemon_traces(
                    validation.as_deref().ok_or(Error::Refused)?,
                    self.app.as_deref().ok_or(Error::Refused)?,
                    self.proxy.as_deref().ok_or(Error::Refused)?,
                    &services,
                    &traces,
                )?;
                none_history.complete(validation.as_deref().ok_or(Error::Refused)?, &services)?;
            }
            ctx.observed.set("observedExpectedDaemonJoins", true)?;
            ctx.observed
                .set("noObservedUnexpectedMutatingEvents", true)?;
            self.unknown_effect = false;
            Ok(())
        }
        .await;
        daemon_result(&mut ctx.control, earlier, stage, result)
    }
    pub(super) async fn database_observe(
        &mut self,
        ctx: &mut Context,
        attempt: Option<&Path>,
    ) -> Result<()> {
        ctx.control.checkpoint = "database-observe";
        if ctx.request.native_scenario() == "positive" {
            let data = self
                .sql(
                    ctx,
                    "final-ledger",
                    concat!(
                        "SELECT version,success,encode(checksum,'hex') FROM ",
                        "public._sqlx_migrations ORDER BY version",
                    ),
                )
                .await?;
            let mut expected = BTreeMap::new();
            for (name, bytes) in &ctx.sources {
                if name.starts_with("migrations/") {
                    let prefix = Path::new(name)
                        .file_name()
                        .and_then(|v| v.to_str())
                        .and_then(|v| v.split_once('_'))
                        .ok_or(Error::Refused)?
                        .0;
                    let version = prefix
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
            for line in std::str::from_utf8(&data)
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
            require(rows == expected)?;
            require(
                self.sql(
                    ctx,
                    "owner-lobby-counts",
                    concat!(
                        "SELECT (SELECT count(*) FROM users),",
                        "(SELECT count(*) FROM rooms WHERE id='lobby')",
                    ),
                )
                .await?
                .trim_ascii()
                    == b"1|1",
            )?;
            require(
                self.sql(ctx, "runtime-privileges", RUNTIME_PRIVILEGES)
                    .await?
                    .trim_ascii()
                    == b"t",
            )?;
            ctx.observed.set("ownerAndLobbyObserved", true)?;
            let attempt = attempt.ok_or(Error::Refused)?;
            let (receipt, _) = retained_data(ctx, &attempt.join("backup.sha256"), 131072).await?;
            let receipt = std::str::from_utf8(receipt.trim_ascii()).map_err(|_| Error::Refused)?;
            let (hash, path) = receipt.split_once("  ").ok_or(Error::Refused)?;
            require(policy::digest(hash))?;
            let name = path
                .strip_prefix("/srv/simplestchat-public/backups/initial.")
                .and_then(|v| v.strip_suffix(".dump"))
                .ok_or(Error::Refused)?;
            require(name.len() == 8 && name.bytes().all(|b| b.is_ascii_alphanumeric()))?;
            let (backup, actual) = retained_data(ctx, Path::new(path), 1024 * 1024).await?;
            require(actual == hash && backup.starts_with(b"PGDMP"))?;
            ctx.observed.set("backupHeaderAndDigestMatched", true)?;
        } else {
            require(
                self.sql(ctx, "unchanged-empty-schema", EMPTY_SCHEMA)
                    .await?
                    .trim_ascii()
                    == b"t",
            )?;
            require(
                self.sql(ctx, "unchanged-source-roles", INITIAL_ROLES)
                    .await?
                    == self.baseline_counts,
            )?;
        }
        ctx.observed.set("expectedDatabaseStateObserved", true)
    }
    pub(super) async fn tls_observe(&mut self, ctx: &mut Context) -> Result<()> {
        ctx.control.checkpoint = "tls-observe";
        for index in &self.material_indices {
            ctx.originals[*index].check(&mut ctx.control)?;
        }
        ctx.observed.set("naturalMaterialsUnchanged", true)?;
        if ctx.request.native_scenario() == "positive" {
            let id = self.proxy.clone().ok_or(Error::Refused)?;
            let peers = BTreeSet::from([self.app.clone().ok_or(Error::Refused)?, id.clone()]);
            let root = ctx.originals[*self.material_indices.first().ok_or(Error::Refused)?]
                .data(65536, &mut ctx.control)?;
            let proof = self.tls_root(ctx, &id, &peers, &root).await?;
            proof.bind(self.materials.as_ref().ok_or(Error::Refused)?)?;
            ctx.observed.set("freshLauncherTlsObserved", true)?;
        }
        Ok(())
    }
    pub(super) async fn application_observe(&mut self, ctx: &mut Context) -> Result<()> {
        ctx.control.checkpoint = "application-observe";
        if ctx.request.native_scenario() == "positive" {
            let bytes = Self::call(
                ctx,
                "actual-backend-ready",
                &[
                    "curl".into(),
                    "--disable".into(),
                    "--noproxy".into(),
                    "*".into(),
                    "--proto".into(),
                    "=http".into(),
                    "--fail".into(),
                    "--silent".into(),
                    "--show-error".into(),
                    "--max-time".into(),
                    "2".into(),
                    "http://127.0.0.1:3000/ready".into(),
                ],
                5,
            )
            .await?;
            require(policy::decode(&bytes)?["status"] == "ready")?;
            ctx.observed.set("applicationReadyObserved", true)?;
        }
        Ok(())
    }
    pub(super) async fn source_observe(&mut self, ctx: &mut Context) -> Result<()> {
        self.source_observe_for(ctx, ctx.request.scenario == "dirty-source")
            .await
    }
    pub(super) async fn source_observe_for(
        &mut self,
        ctx: &mut Context,
        dirty: bool,
    ) -> Result<()> {
        ctx.control.checkpoint = "source-observe";
        ctx.check()?;
        let status = Self::call(
            ctx,
            "final-source-status",
            &[
                "git".into(),
                "-C".into(),
                ctx.source_root.to_str().ok_or(Error::Input)?.into(),
                "status".into(),
                "--porcelain".into(),
                "--untracked-files=all".into(),
            ],
            10,
        )
        .await?;
        require(
            status
                == if dirty {
                    b" M README.md\n".as_slice()
                } else {
                    b""
                },
        )?;
        ctx.observed.set("sourceTreeClean", status.is_empty())?;
        ctx.observed.set("sourceRolesUnchanged", true)?;
        ctx.control.check()?;
        let path = Path::new("/run/simplestchat-bench/workload.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32,
            )
            .open(path)
            .map_err(|_| Error::Io)?;
        let mut locked = false;
        let result = (|| {
            ctx.control.check()?;
            let before = file.metadata().map_err(|_| Error::Io)?;
            require(
                before.is_file()
                    && before.uid() == 0
                    && before.mode() & 0o7777 == 0o600
                    && before.nlink() == 1,
            )?;
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .map_err(|_| Error::Refused)?;
            locked = true;
            ctx.control.check()?;
            let visible = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
            require(lock_stamp(&visible) == lock_stamp(&before))?;
            ctx.control.check()
        })();
        if let Err(error) = result {
            ctx.control.record(error);
        }
        let unlocked = if locked {
            rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock).map_err(|_| Error::Io)
        } else {
            Ok(())
        };
        if let Err(error) = unlocked {
            ctx.control.record(error);
        }
        let marked = if unlocked.is_ok() && result.is_ok() {
            ctx.observed.set("workloadLockReleased", true)
        } else {
            Ok(())
        };
        if let Err(error) = marked {
            ctx.control.record(error);
        }
        let closed = super::super::files::close(file, &mut ctx.control);
        result.and(unlocked).and(marked).and(closed)
    }
    pub async fn observe(&mut self, ctx: &mut Context) -> Result<()> {
        self.check_monitors(ctx)?;
        {
            let monitors = self.monitors.lock().map_err(|_| Error::Io)?;
            let collector = monitors.collector.as_ref().ok_or(Error::Refused)?;
            self.native_rows = collector.rows[collector.native_start..].to_vec();
        }
        let observation = ctx.control.push_phase(Duration::from_secs(120))?;
        let result = async {
            let attempt = self.attempt(ctx).await?;
            let previous = ctx.control.push_phase(Duration::from_secs(15))?;
            let result = self.daemon_observe(ctx, attempt.as_deref()).await;
            let ended = ctx.control.pop_phase(previous);
            close_phase(result, ended)?;
            let previous = ctx.control.push_phase(Duration::from_secs(15))?;
            let result = self.database_observe(ctx, attempt.as_deref()).await;
            let ended = ctx.control.pop_phase(previous);
            close_phase(result, ended)?;
            let previous = ctx.control.push_phase(Duration::from_secs(10))?;
            let result = self.tls_observe(ctx).await;
            let ended = ctx.control.pop_phase(previous);
            close_phase(result, ended)?;
            let previous = ctx.control.push_phase(Duration::from_secs(5))?;
            let result = self.application_observe(ctx).await;
            let ended = ctx.control.pop_phase(previous);
            close_phase(result, ended)?;
            let previous = ctx.control.push_phase(Duration::from_secs(10))?;
            let result = self.source_observe(ctx).await;
            let ended = ctx.control.pop_phase(previous);
            close_phase(result, ended)?;
            self.collector_finish(ctx).await?;
            self.retire_actors(ctx).await?;
            self.retire_network(ctx).await?;
            self.retained_guard(ctx).await
        }
        .await;
        let ended = ctx.control.pop_phase(observation);
        close_phase(result, ended)
    }
    pub(super) async fn collector_finish(&mut self, ctx: &mut Context) -> Result<()> {
        let Some(mut collector) = self
            .monitors
            .lock()
            .map_err(|_| Error::Io)?
            .collector
            .take()
        else {
            return Ok(());
        };
        let previous = ctx.control.push_phase(Duration::from_secs(15));
        let result = match previous {
            Ok(previous) => {
                let result = collector.finish(&mut ctx.control).await;
                let ended = ctx.control.pop_phase(previous);
                close_phase(result, ended)
            }
            Err(error) => {
                ctx.control.record(error);
                if let Err(later) = collector.finish(&mut ctx.control).await {
                    ctx.control.record(later);
                }
                Err(error)
            }
        };
        self.monitors.lock().map_err(|_| Error::Io)?.collector = Some(collector);
        if result.is_ok() {
            ctx.observed.set("collectorFinalized", true)?;
        }
        result
    }
    async fn retire_actors(&mut self, ctx: &mut Context) -> Result<()> {
        if self.actors.is_empty() {
            return ctx.observed.set("knownFixtureActorsRetired", true);
        }
        let known = self.actors.iter().cloned().collect::<Vec<_>>();
        let previous = ctx.control.push_phase(Duration::from_secs(15))?;
        let result = async {
            self.check_socket()?;
            let mut args = vec![
                "docker".into(),
                "--host".into(),
                "unix:///var/run/docker.sock".into(),
                "rm".into(),
                "--force".into(),
                "--volumes".into(),
            ];
            args.extend(known.clone());
            let actors = &mut self.actors;
            ctx.commands
                .call_adopt(
                    &mut ctx.control,
                    "known-fixture-retire",
                    &args,
                    Duration::from_secs(15),
                    |_| {
                        for id in &known {
                            actors.remove(id);
                        }
                        Ok(())
                    },
                )
                .await?;
            Ok(())
        }
        .await;
        let ended = ctx.control.pop_phase(previous);
        let result = close_phase(result, ended);
        if self.actors.is_empty() {
            ctx.observed.set("knownFixtureActorsRetired", true)?;
        }
        result
    }
    async fn retire_network(&mut self, ctx: &mut Context) -> Result<()> {
        let Some(id) = self.network.clone() else {
            return ctx.observed.set("knownFixtureNetworkRetired", true);
        };
        let previous = ctx.control.push_phase(Duration::from_secs(15))?;
        let result = async {
            self.check_socket()?;
            let observed = ctx
                .commands
                .call(
                    &mut ctx.control,
                    "known-network-check",
                    &[
                        "docker".into(),
                        "--host".into(),
                        "unix:///var/run/docker.sock".into(),
                        "network".into(),
                        "inspect".into(),
                        id.clone(),
                    ],
                    Duration::from_secs(5),
                )
                .await?;
            let row = policy::one(&observed)?;
            require(row["Id"] == id && policy::object(&row["Containers"])?.is_empty())?;
            let network = &mut self.network;
            ctx.commands
                .call_adopt(
                    &mut ctx.control,
                    "known-network-retire",
                    &[
                        "docker".into(),
                        "--host".into(),
                        "unix:///var/run/docker.sock".into(),
                        "network".into(),
                        "rm".into(),
                        id.clone(),
                    ],
                    Duration::from_secs(15),
                    |_| {
                        *network = None;
                        Ok(())
                    },
                )
                .await?;
            Ok(())
        }
        .await;
        let ended = ctx.control.pop_phase(previous);
        let result = close_phase(result, ended);
        if self.network.is_none() {
            ctx.observed.set("knownFixtureNetworkRetired", true)?;
        }
        result
    }
    async fn retained_guard(&mut self, ctx: &mut Context) -> Result<()> {
        require(self.actors.is_empty() && !self.create_uncertain)?;
        let previous = ctx.control.push_phase(Duration::from_secs(10))?;
        let result = async {
            for family in ["inet", "netdev"] {
                let bytes = ctx
                    .commands
                    .call(
                        &mut ctx.control,
                        &format!("guard-retained-{family}"),
                        &[
                            "nft".into(),
                            "-json".into(),
                            "list".into(),
                            "table".into(),
                            family.into(),
                            self.table.clone(),
                        ],
                        Duration::from_secs(30),
                    )
                    .await?;
                require(bytes.windows(6).any(|v| v == b"\"drop\""))?;
            }
            Ok(())
        }
        .await;
        let ended = ctx.control.pop_phase(previous);
        close_phase(result, ended)?;
        ctx.observed.set("externalGuardRetained", true)
    }
    pub(super) fn daemon_effects_uncertain(&self) -> bool {
        self.unknown_effect
            || self.create_uncertain
            || self.cold_pg.as_ref().is_some_and(|cold| !cold.complete())
    }
    pub async fn finish_body(&mut self, ctx: &mut Context) -> Result<()> {
        if self.body_finished {
            return Ok(());
        }
        self.body_finished = true;
        // The collector is removed from the callback before its async join; capture stays live.
        actors::install(ctx, &self.monitors);
        let mut first = None;
        if let Err(error) = self.collector_finish(ctx).await {
            first.get_or_insert(error);
            ctx.control.record(error);
        }
        if let Err(error) = self.retire_actors(ctx).await {
            first.get_or_insert(error);
            ctx.control.record(error);
        }
        if let Err(error) = self.retire_network(ctx).await {
            first.get_or_insert(error);
            ctx.control.record(error);
        }
        if self.guarded
            && !ctx.observed.get("externalGuardRetained")?
            && let Err(error) = self.retained_guard(ctx).await
        {
            first.get_or_insert(error);
            ctx.control.record(error);
        }
        ctx.observed
            .set("daemonEffectsUncertain", self.daemon_effects_uncertain())?;
        first.map_or(Ok(()), Err)
    }
    pub fn primary_capture_checkpoint(&self) -> Option<&'static str> {
        self.monitors
            .lock()
            .ok()?
            .watch
            .as_ref()?
            .primary_checkpoint()
    }
    pub async fn finish_local(&mut self, ctx: &mut Context) -> Result<()> {
        if self.local_finished {
            return Ok(());
        }
        self.local_finished = true;
        ctx.commands.set_monitor(None);
        let mut first = None;
        let watch = self.monitors.lock().map_err(|_| Error::Io)?.watch.take();
        if let Some(mut watch) = watch {
            match watch.finish(&mut ctx.control).await {
                Ok(()) => ctx.observed.set("noObservedExternalPackets", true)?,
                Err(error) => {
                    first.get_or_insert(error);
                    ctx.control.record(error);
                }
            }
            self.monitors.lock().map_err(|_| Error::Io)?.watch = Some(watch);
        }
        // Materials themselves own no file descriptors; all seven originals remain in Context.
        for index in &self.material_indices {
            if let Err(error) = ctx.originals[*index].check(&mut ctx.control) {
                first.get_or_insert(error);
                ctx.control.record(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

fn lock_stamp(v: &Metadata) -> (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64) {
    (
        v.dev(),
        v.ino(),
        v.mode(),
        v.uid(),
        v.gid(),
        v.nlink(),
        v.len(),
        v.mtime(),
        v.mtime_nsec(),
        v.ctime(),
        v.ctime_nsec(),
    )
}
