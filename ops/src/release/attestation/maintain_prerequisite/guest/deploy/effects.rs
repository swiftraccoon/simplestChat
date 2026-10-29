//! Deploy adapter over the existing concrete daemon, network, TLS and SQL owners.
//! This file is a child of guest::effects; it adds no second resource framework.

use super::super::CONFIG;
use super::super::deploy::{payload, validation};
use super::*;
use serde_json::json;

pub(in super::super) struct Baseline {
    pub dependencies: Value,
    pub seed: Vec<u8>,
    pub pg: String,
    pub proxy: String,
}
fn stable(row: &Value) -> Result<Value> {
    let id = policy::string(&row["Id"])?;
    let image = policy::string(&row["Image"])?;
    let started = policy::string(&row["State"]["StartedAt"])?;
    let restarts = row["RestartCount"].as_u64().ok_or(Error::Refused)?;
    let config = policy::string(&row["Config"]["Labels"]["com.docker.compose.config-hash"])?;
    require(policy::digest(id) && image.strip_prefix("sha256:").is_some_and(policy::digest))?;
    require(
        !started.is_empty()
            && policy::digest(config)
            && row["State"]["Running"] == true
            && row["State"]["OOMKilled"] == false,
    )?;
    Ok(json!([id, image, started, restarts, config]))
}
fn retire_destroyed(actors: &mut BTreeSet<String>, rows: &[Value], token: &str) -> Result<()> {
    for row in rows {
        if row["Type"] == "container"
            && row["Action"] == "destroy"
            && row["Actor"]["Attributes"][LABEL] == token
        {
            let id = policy::string(&row["Actor"]["ID"])?;
            require(policy::digest(id))?;
            // A real retained destroy can retire only an already-known actor.
            actors.remove(id);
        }
    }
    Ok(())
}
impl Fixture {
    pub(in super::super) async fn deploy_retire_trust(ctx: &mut Context) -> Result<()> {
        let path = Path::new("/usr/local/share/ca-certificates/simplestchat-maintain.crt");
        let Some(index) = ctx
            .originals
            .iter()
            .position(|original| original.path == path)
        else {
            return Ok(());
        };
        let bytes = ctx.originals[index].data(65536, &mut ctx.control);
        let retired =
            bytes.and_then(|bytes| ctx.originals[index].retire_owned(&bytes, &mut ctx.control));
        if ctx.originals[index].closed() {
            ctx.host_retired.insert(index);
        }
        if let Err(error) = retired {
            ctx.control.record(error);
        }
        let absent = matches!(fs::symlink_metadata(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound);
        let refreshed = if absent {
            ctx.commands
                .call(
                    &mut ctx.control,
                    "deploy-trust-retire",
                    &["update-ca-certificates".into()],
                    Duration::from_secs(15),
                )
                .await
                .map(|_| ())
        } else {
            Err(Error::Refused)
        };
        retired.and(refreshed)
    }
    pub(in super::super) fn deploy_app(&self) -> Result<&str> {
        self.app.as_deref().ok_or(Error::Refused)
    }
    pub(in super::super) async fn deploy_running_app(ctx: &mut Context) -> Result<String> {
        let bytes = Self::compose(
            ctx,
            "deploy-inject-running-id",
            &[
                "ps".into(),
                "--status".into(),
                "running".into(),
                "--quiet".into(),
                "simplestchat".into(),
            ],
            15,
        )
        .await?;
        let ids = policy::ids(&bytes)?;
        require(ids.len() == 1)?;
        ids.first().cloned().ok_or(Error::Refused)
    }
    pub(in super::super) async fn deploy_docker(
        ctx: &mut Context,
        role: &str,
        args: &[String],
        seconds: u64,
    ) -> Result<Vec<u8>> {
        Self::docker(ctx, role, args, seconds).await
    }
    pub(in super::super) async fn deploy_inspect(
        &mut self,
        ctx: &mut Context,
        id: &str,
    ) -> Result<Value> {
        require(policy::digest(id))?;
        policy::one(&Self::docker(ctx, "deploy-inspect", &["inspect".into(), id.into()], 15).await?)
    }
    pub(in super::super) async fn deploy_image(
        ctx: &mut Context,
        admitted: &payload::Admitted,
    ) -> Result<String> {
        let row = policy::one(
            &Self::docker(
                ctx,
                "deploy-image-profile",
                &[
                    "image".into(),
                    "inspect".into(),
                    admitted.manifest.fields().image_tag.clone(),
                ],
                15,
            )
            .await?,
        )?;
        policy::server_image(
            &row,
            &admitted.archive,
            &admitted.manifest.fields().revision,
        )
    }
    pub(in super::super) async fn deploy_ledger(
        &mut self,
        ctx: &mut Context,
    ) -> Result<BTreeMap<String, String>> {
        validation::ledger(
            &self
                .sql(
                    ctx,
                    "deploy-ledger",
                    concat!(
                        "SELECT version, success, encode(checksum, 'hex') ",
                        "FROM _sqlx_migrations ORDER BY version"
                    ),
                )
                .await?,
        )
    }
    pub(in super::super) async fn deploy_dependencies(
        &mut self,
        ctx: &mut Context,
    ) -> Result<Value> {
        let pg_image = self.pg_image.clone();
        let proxy_image = self.caddy_image.clone();
        let (pg, pg_row) = self.service(ctx, "postgres", &pg_image).await?;
        let (proxy, proxy_row) = self.service(ctx, "caddy", &proxy_image).await?;
        require(self.pg.as_deref() == Some(pg.as_str()))?;
        self.proxy = Some(proxy);
        Ok(json!({"postgres":stable(&pg_row)?,"caddy":stable(&proxy_row)?}))
    }
    pub(in super::super) fn deploy_bind_app(
        &mut self,
        ctx: &Context,
        row: &Value,
        id: &str,
        image: &str,
    ) -> Result<()> {
        let labels = &row["Config"]["Labels"];
        require(
            policy::digest(id)
                && row["Id"] == id
                && row["Image"] == image
                && row["Name"] == "/simplestchat-public-simplestchat-1"
                && labels[LABEL] == ctx.request.run_id
                && labels["com.docker.compose.project"] == PROJECT
                && labels["com.docker.compose.service"] == "simplestchat"
                && row["State"]["Running"] == true
                && row["State"]["OOMKilled"] == false
                && row["Config"]["User"] == "10001:10001"
                && row["HostConfig"]["ReadonlyRootfs"] == true
                && row["HostConfig"]["Privileged"] == false
                && row["HostConfig"]["CapDrop"] == serde_json::json!(["ALL"]),
        )?;
        self.actors.insert(id.into());
        Ok(())
    }
    pub(super) fn deploy_retired(&mut self, ctx: &Context) -> Result<()> {
        require(ctx.request.is_deploy())?;
        let monitors = self.monitors.lock().map_err(|_| Error::Io)?;
        let collector = monitors.collector.as_ref().ok_or(Error::Refused)?;
        require(collector.closed)?;
        retire_destroyed(&mut self.actors, &collector.rows, &ctx.request.run_id)
    }
    pub(in super::super) async fn deploy_ready(
        &mut self,
        ctx: &mut Context,
        seconds: u64,
    ) -> Result<()> {
        let phase = ctx.control.push_phase(Duration::from_secs(seconds))?;
        let result = async {
            loop {
                self.check_monitors(ctx)?;
                let mut ready = true;
                for public in [false, true] {
                    let mut args: Vec<String> = [
                        "curl",
                        "--disable",
                        "--noproxy",
                        "*",
                        "--fail",
                        "--silent",
                        "--show-error",
                        "--max-time",
                        "2",
                    ]
                    .map(str::to_owned)
                    .into();
                    if public {
                        args.extend([
                            "--proto".into(),
                            "=https".into(),
                            "--resolve".into(),
                            "maintain.localhost:443:127.0.0.1".into(),
                            "https://maintain.localhost/ready".into(),
                        ]);
                    } else {
                        args.extend([
                            "--proto".into(),
                            "=http".into(),
                            "http://127.0.0.1:3000/ready".into(),
                        ]);
                    }
                    let out = ctx
                        .commands
                        .execute(
                            &mut ctx.control,
                            "deploy-ready",
                            &args,
                            Duration::from_secs(5),
                            &[0, 7, 22, 28],
                        )
                        .await?;
                    ready &= out.status == 0
                        && policy::decode(&out.stdout)
                            .is_ok_and(|value| value["status"] == "ready");
                }
                if ready {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        .await;
        let ended = ctx.control.pop_phase(phase);
        result.and(ended)
    }
    pub(in super::super) async fn deploy_baseline(
        &mut self,
        ctx: &mut Context,
        old: &payload::Admitted,
    ) -> Result<Baseline> {
        require(ctx.request.is_deploy())?;
        let image = self.image.clone();
        self.unknown_effect = true;
        Self::compose(
            ctx,
            "deploy-migrate-up",
            &[
                "--profile".into(),
                "maintenance".into(),
                "up".into(),
                "--detach".into(),
                "--no-build".into(),
                "--pull".into(),
                "never".into(),
                "--no-deps".into(),
                "migrate".into(),
            ],
            60,
        )
        .await?;
        let ids = policy::ids(
            &Self::compose(
                ctx,
                "deploy-migrate-id",
                &[
                    "--profile".into(),
                    "maintenance".into(),
                    "ps".into(),
                    "--all".into(),
                    "--quiet".into(),
                    "migrate".into(),
                ],
                15,
            )
            .await?,
        )?;
        require(ids.len() == 1)?;
        let id = ids.first().ok_or(Error::Refused)?.clone();
        let row = self.deploy_inspect(ctx, &id).await?;
        require(
            row["Image"] == image
                && row["Config"]["Labels"][LABEL] == ctx.request.run_id
                && row["Config"]["Labels"]["com.docker.compose.service"] == "migrate",
        )?;
        self.actors.insert(id.clone());
        require(
            Self::docker(
                ctx,
                "deploy-migrate-wait",
                &["wait".into(), id.clone()],
                120,
            )
            .await?
            .trim_ascii()
                == b"0",
        )?;
        Self::compose(
            ctx,
            "deploy-migrate-stop",
            &[
                "--profile".into(),
                "maintenance".into(),
                "stop".into(),
                "--timeout".into(),
                "30".into(),
                "migrate".into(),
            ],
            45,
        )
        .await?;
        let row = self.deploy_inspect(ctx, &id).await?;
        require(row["State"]["Running"] == false && row["State"]["ExitCode"].as_i64() == Some(0))?;
        Self::docker(ctx, "deploy-migrate-remove", &["rm".into(), id.clone()], 15).await?;
        self.actors.remove(&id);
        let path = Path::new(CONFIG).join("runtime-grants.sql");
        let source = ctx
            .originals
            .iter()
            .find(|original| original.path == path)
            .ok_or(Error::Input)?;
        let grants = source.data(1024 * 1024, &mut ctx.control)?;
        let grants = std::str::from_utf8(&grants).map_err(|_| Error::Refused)?;
        self.sql(ctx, "deploy-runtime-grants", grants).await?;
        self.sql(
            ctx,
            "deploy-marker-create",
            "CREATE TABLE public.release_fixture_marker(value text NOT NULL)",
        )
        .await?;
        self.sql(
            ctx,
            "deploy-marker-seed",
            &format!(
                "INSERT INTO public.release_fixture_marker VALUES ('{}')",
                ctx.request.run_id
            ),
        )
        .await?;
        self.sql(
            ctx,
            "deploy-seed-create",
            "CREATE TABLE public.release_fixture_seed(id integer PRIMARY KEY,value text NOT NULL)",
        )
        .await?;
        self.sql(
            ctx,
            "deploy-seed-insert",
            "INSERT INTO public.release_fixture_seed VALUES(1,'alpha'),(2,'beta'),(3,'gamma')",
        )
        .await?;
        Self::compose(
            ctx,
            "deploy-app-and-proxy-up",
            &[
                "up".into(),
                "--detach".into(),
                "--no-build".into(),
                "--pull".into(),
                "never".into(),
                "simplestchat".into(),
                "caddy".into(),
            ],
            60,
        )
        .await?;
        let (app, _) = self.service(ctx, "simplestchat", &image).await?;
        self.app = Some(app.clone());
        self.deploy_ready(ctx, 120).await?;
        let dependencies = self.deploy_dependencies(ctx).await?;
        let seed = self
            .sql(
                ctx,
                "deploy-seed-baseline",
                concat!(
                    "SELECT count(*),sum(id),array_agg(value ORDER BY id)",
                    "FROM release_fixture_seed"
                ),
            )
            .await?;
        require(seed.trim_ascii() == b"3|6|{alpha,beta,gamma}")?;
        require(self.deploy_ledger(ctx).await? == old.manifest.fields().migrations)?;
        require(
            self.sql(
                ctx,
                "deploy-marker-baseline",
                "SELECT value FROM public.release_fixture_marker",
            )
            .await?
            .trim_ascii()
                == ctx.request.run_id.as_bytes(),
        )?;
        self.unknown_effect = false;
        Ok(Baseline {
            dependencies,
            seed,
            pg: self.pg.clone().ok_or(Error::Refused)?,
            proxy: self.proxy.clone().ok_or(Error::Refused)?,
        })
    }
    pub(in super::super) async fn deploy_continuity(
        &mut self,
        ctx: &mut Context,
        baseline: &Baseline,
        revision: &str,
        image: &str,
        migrations: &BTreeMap<String, String>,
    ) -> Result<Value> {
        require(self.deploy_dependencies(ctx).await? == baseline.dependencies)?;
        require(self.deploy_ledger(ctx).await? == *migrations)?;
        require(
            self.sql(
                ctx,
                "deploy-seed-current",
                concat!(
                    "SELECT count(*),sum(id),array_agg(value ORDER BY id)",
                    "FROM release_fixture_seed"
                ),
            )
            .await?
                == baseline.seed,
        )?;
        require(
            self.sql(
                ctx,
                "deploy-marker-current",
                "SELECT value FROM public.release_fixture_marker",
            )
            .await?
            .trim_ascii()
                == ctx.request.run_id.as_bytes(),
        )?;
        self.deploy_ready(ctx, 15).await?;
        let (app, _) = self.service(ctx, "simplestchat", image).await?;
        self.app = Some(app.clone());
        let selected = super::super::deploy::operation::read_record(
            ctx,
            &Path::new(CONFIG).join("images.json"),
            0o600,
            1024 * 1024,
        )?;
        require(
            selected["revision"] == revision
                && selected["serverImage"] == image
                && selected["postgresImage"] == ctx.request.postgres_image
                && selected["caddyImage"] == ctx.request.caddy_image,
        )?;
        let version = super::super::deploy::operation::read_record(
            ctx,
            &Path::new(PUBLIC).join("browser-assets/public/app-version.json"),
            0o644,
            128,
        )?;
        require(version == json!({"revision":revision}))?;
        let store = super::super::deploy::operation::read_record(
            ctx,
            &Path::new(PUBLIC).join("browser-assets/index.json"),
            0o644,
            262144,
        )?;
        require(store["generations"].as_array().is_some_and(|generations| {
            generations
                .iter()
                .any(|generation| generation["revision"] == revision)
        }))?;
        // The normal native store inspector validates all indexed asset bytes.
        let inspected = Self::call(
            ctx,
            "deploy-assets-inspect",
            &[
                "ops".into(),
                "release".into(),
                "assets".into(),
                "inspect".into(),
            ],
            15,
        )
        .await?;
        let inspected = policy::decode(&inspected)?;
        require(
            inspected["generations"]
                .as_u64()
                .is_some_and(|count| (1..=4).contains(&count))
                && inspected["files"].as_u64().is_some()
                && inspected["bytes"].as_u64().is_some(),
        )?;
        self.actors = BTreeSet::from([baseline.pg.clone(), baseline.proxy.clone(), app.clone()]);
        Ok(json!({"revision":revision,"image":image,"appId":app}))
    }
    pub(in super::super) fn deploy_accept_complete(&mut self) {
        self.unknown_effect = false;
    }
    pub(in super::super) fn deploy_uncertain(&self) -> bool {
        self.daemon_effects_uncertain()
    }
    pub(in super::super) async fn deploy_probe(
        &mut self,
        ctx: &mut Context,
        image: &str,
        migrations: &BTreeMap<String, String>,
    ) -> Result<Value> {
        use super::super::deploy::probe;
        let name = format!("scpub-stage-proof-{}", ctx.request.run_id);
        let mut args: Vec<String> = [
            "docker",
            "--host",
            "unix:///var/run/docker.sock",
            "create",
            "--name",
            &name,
            "--label",
            &format!("simplestchat.stage-proof={}", ctx.request.run_id),
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
            probe::SCRIPT,
        ]
        .map(str::to_owned)
        .into();
        let mut id = None;
        let actors = &mut self.actors;
        ctx.commands
            .call_adopt(
                &mut ctx.control,
                "deploy-probe-create",
                &args,
                Duration::from_secs(30),
                |bytes| {
                    let actual = policy::created_id(bytes)?;
                    actors.insert(actual.clone());
                    id = Some(actual);
                    Ok(())
                },
            )
            .await?;
        let id = id.ok_or(Error::Refused)?;
        let body = async {
            let row = self.deploy_inspect(ctx, &id).await?;
            probe::profile(&row, &id, image, &name, &ctx.request.run_id)?;
            Self::docker(ctx, "deploy-probe-start", &["start".into(), id.clone()], 15).await?;
            require(
                Self::docker(ctx, "deploy-probe-wait", &["wait".into(), id.clone()], 20)
                    .await?
                    .trim_ascii()
                    == b"0",
            )?;
            let logs =
                Self::docker(ctx, "deploy-probe-logs", &["logs".into(), id.clone()], 15).await?;
            require(probe::migrations(&logs)? == *migrations)?;
            let row = self.deploy_inspect(ctx, &id).await?;
            probe::profile(&row, &id, image, &name, &ctx.request.run_id)?;
            require(
                row["State"]["Running"] == false
                    && row["State"]["OOMKilled"] == false
                    && row["State"]["ExitCode"].as_i64() == Some(0)
                    && row["State"]["Error"] == "",
            )?;
            Ok(json!({"passed":true,"isolationIndependentlyInspected":true,
                "migrationMapMatched":true,"exitStatus":0,"containerId":id}))
        }
        .await;
        if let Err(error) = body {
            ctx.control.record(error);
        }
        let removed = async {
            let row = self.deploy_inspect(ctx, &id).await?;
            probe::profile(&row, &id, image, &name, &ctx.request.run_id)?;
            args = vec!["rm".into(), "--force".into(), id.clone()];
            Self::docker(ctx, "deploy-probe-remove", &args, 15).await?;
            self.actors.remove(&id);
            require(
                Self::docker(
                    ctx,
                    "deploy-probe-retired",
                    &[
                        "ps".into(),
                        "--all".into(),
                        "--quiet".into(),
                        "--no-trunc".into(),
                        "--filter".into(),
                        format!("id={id}"),
                    ],
                    15,
                )
                .await?
                .trim_ascii()
                .is_empty(),
            )
        }
        .await;
        if let Err(error) = removed {
            ctx.control.record(error);
        }
        body.and_then(|value| removed.map(|()| value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_destroy_rows_retire_only_previously_known_owned_actors() {
        let old = "a".repeat(64);
        let live = "b".repeat(64);
        let unknown = "c".repeat(64);
        let event = |id: &str, action: &str, token: &str| {
            json!({
                "Type":"container","Action":action,
                "Actor":{"ID":id,"Attributes":{(LABEL):token}}
            })
        };
        let mut actors = BTreeSet::from([old.clone(), live.clone()]);
        retire_destroyed(
            &mut actors,
            &[
                event(&old, "destroy", "owned"),
                event(&live, "die", "owned"),
                event(&live, "destroy", "other"),
                event(&unknown, "destroy", "owned"),
            ],
            "owned",
        )
        .unwrap();
        assert_eq!(actors, BTreeSet::from([live.clone()]));
        assert!(
            retire_destroyed(
                &mut actors,
                &[event("partial", "destroy", "owned")],
                "owned"
            )
            .is_err()
        );
        assert_eq!(actors, BTreeSet::from([live]));
    }
}
