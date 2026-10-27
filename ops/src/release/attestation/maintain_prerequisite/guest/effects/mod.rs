//! Owned daemon effects and independent observations for the fixed guest.

mod actors;
mod observation;
mod policy;
#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;

use super::{
    Context, Error, Result,
    files::Original,
    materials::{self, Materials},
    protocol::Request,
    tls,
};
use actors::{Collector, Monitors, Shared, Watch};
use policy::{Endpoint, LABEL, PROJECT, PUBLIC, require};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, Metadata},
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

const PRIVATE: &str = "/root/simplestchat-vm-fixture/maintain-prerequisite";
const PAYLOAD: &str = "/root/simplestchat-vm-fixture/maintain-prerequisite/server-input";

pub(super) struct Fixture {
    pub image: String,
    pub pg_image: String,
    pub caddy_image: String,
    bridge: String,
    table: String,
    network: Option<String>,
    pg: Option<String>,
    proxy: Option<String>,
    app: Option<String>,
    actors: BTreeSet<String>,
    unknown_effect: bool,
    create_uncertain: bool,
    guarded: bool,
    monitors: Shared,
    materials: Option<Materials>,
    material_indices: Vec<usize>,
    pg_health: String,
    baseline_counts: Vec<u8>,
    attempts_before: BTreeSet<String>,
    native_rows: Vec<Value>,
    socket: Option<(PathBuf, Metadata)>,
    body_finished: bool,
    local_finished: bool,
}
impl Fixture {
    pub fn new(request: &Request) -> Self {
        Self {
            image: String::new(),
            pg_image: String::new(),
            caddy_image: String::new(),
            bridge: format!("sc{}", &request.run_id[..12]),
            table: format!("sc_maintain_{}", &request.run_id[..12]),
            network: None,
            pg: None,
            proxy: None,
            app: None,
            actors: BTreeSet::new(),
            unknown_effect: false,
            create_uncertain: false,
            guarded: false,
            monitors: Arc::new(Mutex::new(Monitors::default())),
            materials: None,
            material_indices: Vec::new(),
            pg_health: String::new(),
            baseline_counts: Vec::new(),
            attempts_before: BTreeSet::new(),
            native_rows: Vec::new(),
            socket: None,
            body_finished: false,
            local_finished: false,
        }
    }
    fn check_socket(&self) -> Result<()> {
        let (path, before) = self.socket.as_ref().ok_or(Error::Refused)?;
        require(fs::canonicalize("/var/run/docker.sock").map_err(|_| Error::Io)? == *path)?;
        let visible = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
        require(
            visible.file_type().is_socket()
                && visible.dev() == before.dev()
                && visible.ino() == before.ino()
                && visible.mode() == before.mode()
                && visible.uid() == before.uid()
                && visible.gid() == before.gid(),
        )
    }
    async fn call(ctx: &mut Context, role: &str, args: &[String], cap: u64) -> Result<Vec<u8>> {
        ctx.check()?;
        let result = ctx
            .commands
            .call(&mut ctx.control, role, args, Duration::from_secs(cap))
            .await;
        match result {
            Ok(value) => {
                ctx.check()?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
    async fn docker(ctx: &mut Context, role: &str, args: &[String], cap: u64) -> Result<Vec<u8>> {
        let mut full = vec![
            "docker".to_owned(),
            "--host".into(),
            "unix:///var/run/docker.sock".into(),
        ];
        full.extend_from_slice(args);
        Self::call(ctx, role, &full, cap).await
    }
    async fn compose(ctx: &mut Context, role: &str, args: &[String], cap: u64) -> Result<Vec<u8>> {
        let mut full = vec!["public".to_owned()];
        full.extend_from_slice(args);
        Self::call(ctx, role, &full, cap).await
    }
    fn check_monitors(&self, ctx: &mut Context) -> Result<()> {
        self.monitors
            .lock()
            .map_err(|_| Error::Io)?
            .check(&mut ctx.control)
    }
    pub async fn prepare_payload(&mut self, ctx: &mut Context) -> Result<()> {
        ctx.control.checkpoint = "payload";
        ctx.check()?;
        let socket = fs::canonicalize("/var/run/docker.sock").map_err(|_| Error::Io)?;
        let socket_stamp = fs::symlink_metadata(&socket).map_err(|_| Error::Io)?;
        require(socket_stamp.file_type().is_socket() && socket_stamp.uid() == 0)?;
        self.socket = Some((socket, socket_stamp));
        ctx.admit_socket()?;
        let manifest_path = Path::new(PAYLOAD).join("release.json");
        let archive_path = Path::new(PAYLOAD).join("image.tar");
        let manifest = Original::open(&manifest_path, 131072, 0, 0o600, &mut ctx.control)?;
        ctx.originals.push(manifest);
        let manifest_index = ctx.originals.len() - 1;
        require(
            ctx.originals[manifest_index].sha256 == ctx.request.manifest.sha256
                && ctx.originals[manifest_index].bytes == ctx.request.manifest.bytes,
        )?;
        let bytes = ctx.originals[manifest_index].data(131072, &mut ctx.control)?;
        let manifest =
            crate::release::manifest::Manifest::decode(&bytes).map_err(|_| Error::Refused)?;
        require(manifest.fields().revision == ctx.request.revision)?;
        let archive = Original::open(
            &archive_path,
            2 * 1024 * 1024 * 1024,
            0,
            0o600,
            &mut ctx.control,
        )?;
        ctx.originals.push(archive);
        let archive_index = ctx.originals.len() - 1;
        require(
            ctx.originals[archive_index].sha256 == ctx.request.archive.sha256
                && ctx.originals[archive_index].bytes == ctx.request.archive.bytes,
        )?;
        let mut duplicate = ctx.originals[archive_index]
            .file()?
            .try_clone()
            .map_err(|_| Error::Io)?;
        let mut first = None;
        let verified =
            crate::release::archive::verify_retained_archive(&mut duplicate, &manifest, || {
                if let Err(error) = ctx.control.check() {
                    first.get_or_insert(error);
                    false
                } else {
                    true
                }
            })
            .map_err(|_| first.unwrap_or(Error::Refused));
        if let Err(error) = &verified {
            ctx.control.record(*error);
        }
        let closed = nix::unistd::close(duplicate).map_err(|_| Error::Close);
        if let Err(error) = closed {
            ctx.control.cleanup_failed = true;
            ctx.control.record(error);
        }
        let verified = verified.and_then(|value| closed.map(|()| value))?;
        ctx.originals[archive_index].check(&mut ctx.control)?;
        Self::docker(
            ctx,
            "server-load",
            &[
                "image".into(),
                "load".into(),
                "--input".into(),
                archive_path.to_str().ok_or(Error::Input)?.into(),
            ],
            120,
        )
        .await?;
        let observed = Self::docker(
            ctx,
            "server-image",
            &[
                "image".into(),
                "inspect".into(),
                manifest.fields().image_tag.clone(),
            ],
            15,
        )
        .await?;
        self.image =
            policy::server_image(&policy::one(&observed)?, &verified, &ctx.request.revision)?;
        for role in ["postgres", "caddy"] {
            let selected = if role == "postgres" {
                ctx.request.postgres_image.clone()
            } else {
                ctx.request.caddy_image.clone()
            };
            Self::docker(
                ctx,
                &format!("{role}-pull"),
                &["pull".into(), selected.clone()],
                90,
            )
            .await?;
            let observed = Self::docker(
                ctx,
                &format!("{role}-image"),
                &["image".into(), "inspect".into(), selected.clone()],
                15,
            )
            .await?;
            let row = policy::one(&observed)?;
            let expected = selected.split_once('@').ok_or(Error::Input)?.1;
            require(
                row["Os"] == "linux"
                    && row["Architecture"] == "amd64"
                    && row["RepoDigests"]
                        .as_array()
                        .ok_or(Error::Refused)?
                        .iter()
                        .any(|value| {
                            value
                                .as_str()
                                .is_some_and(|v| v.ends_with(&format!("@{expected}")))
                        }),
            )?;
            let id = policy::string(&row["Id"])?;
            require(id.strip_prefix("sha256:").is_some_and(policy::digest))?;
            if role == "postgres" {
                self.pg_image = id.into();
            } else {
                self.caddy_image = id.into();
            }
        }
        self.check_socket()?;
        ctx.check()
    }
    pub async fn setup_network(&mut self, ctx: &mut Context) -> Result<()> {
        let collector = Collector::start(ctx).await?;
        self.monitors.lock().map_err(|_| Error::Io)?.collector = Some(collector);
        actors::install(ctx, &self.monitors);
        self.unknown_effect = true;
        let args = [
            "docker".into(),
            "--host".into(),
            "unix:///var/run/docker.sock".into(),
            "network".into(),
            "create".into(),
            "--internal".into(),
            "--driver".into(),
            "bridge".into(),
            "--opt".into(),
            format!("com.docker.network.bridge.name={}", self.bridge),
            "--label".into(),
            format!("{LABEL}={}", ctx.request.run_id),
            self.bridge.clone(),
        ];
        let network = &mut self.network;
        let uncertain = &mut self.unknown_effect;
        ctx.check()?;
        ctx.commands
            .call_adopt(
                &mut ctx.control,
                "network-create",
                &args,
                Duration::from_secs(30),
                |data| {
                    *network = Some(policy::created_id(data)?);
                    *uncertain = false;
                    Ok(())
                },
            )
            .await?;
        ctx.check()?;
        let id = self.network.clone().ok_or(Error::Refused)?;
        let previous = ctx.control.push_phase(Duration::from_secs(5))?;
        let joined = async {
            loop {
                self.check_monitors(ctx)?;
                let found = self
                    .monitors
                    .lock()
                    .map_err(|_| Error::Io)?
                    .collector
                    .as_ref()
                    .ok_or(Error::Refused)?
                    .rows
                    .iter()
                    .any(|r| {
                        r["Type"] == "network" && r["Action"] == "create" && r["Actor"]["ID"] == id
                    });
                if found {
                    self.monitors
                        .lock()
                        .map_err(|_| Error::Io)?
                        .collector
                        .as_mut()
                        .ok_or(Error::Refused)?
                        .marker = Some(id.clone());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                ctx.control.check()?;
            }
            Ok(())
        }
        .await;
        let ended = ctx.control.pop_phase(previous);
        joined.and(ended)?;
        let network = policy::one(
            &Self::docker(
                ctx,
                "network-profile",
                &["network".into(), "inspect".into(), id.clone()],
                15,
            )
            .await?,
        )?;
        policy::bridge(&network, &id, &self.bridge)?;
        let routes = policy::decode(
            &Self::call(
                ctx,
                "physical-route",
                &[
                    "ip".into(),
                    "-json".into(),
                    "route".into(),
                    "show".into(),
                    "default".into(),
                ],
                30,
            )
            .await?,
        )?;
        let routes = routes.as_array().ok_or(Error::Refused)?;
        require(routes.len() == 1)?;
        let interface = policy::string(&routes[0]["dev"])?;
        let rules = policy::firewall(&self.table, interface, &self.bridge)?;
        let path = Path::new(PRIVATE).join("guard.nft");
        let guard = super::files::write_new(&path, rules.as_bytes(), 0, 0o600, &mut ctx.control)?;
        ctx.originals.push(guard);
        let guarded = &mut self.guarded;
        ctx.check()?;
        ctx.commands
            .call_adopt(
                &mut ctx.control,
                "guard-install",
                &[
                    "nft".into(),
                    "-f".into(),
                    path.to_str().ok_or(Error::Input)?.into(),
                ],
                Duration::from_secs(30),
                |_| {
                    *guarded = true;
                    Ok(())
                },
            )
            .await?;
        ctx.check()?;
        let watch = Watch::start(ctx, interface).await?;
        self.monitors.lock().map_err(|_| Error::Io)?.watch = Some(watch);
        let previous = ctx.control.push_phase(Duration::from_secs(5))?;
        let ready = async {
            loop {
                self.check_monitors(ctx)?;
                if self
                    .monitors
                    .lock()
                    .map_err(|_| Error::Io)?
                    .watch
                    .as_ref()
                    .ok_or(Error::Refused)?
                    .ready()
                {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                ctx.control.check()?;
            }
        }
        .await;
        let ended = ctx.control.pop_phase(previous);
        ready.and(ended)?;
        self.guard_probe(ctx).await?;
        ctx.observed.set("collectorMarkerJoined", true)
    }
    async fn guard_probe(&mut self, ctx: &mut Context) -> Result<()> {
        self.check_monitors(ctx)?;
        let args = [
            "nft".into(),
            "-json".into(),
            "list".into(),
            "table".into(),
            "inet".into(),
            self.table.clone(),
        ];
        let bytes = Self::call(ctx, "guard-before", &args, 30).await?;
        require(bytes.windows(6).any(|v| v == b"\"drop\""))?;
        let before = policy::blocked_counter(&bytes, &self.table)?;
        let socket =
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).map_err(|_| Error::Io)?;
        let sent = (|| {
            ctx.control.check()?;
            match socket.send_to(
                b"INERT-CONTAINMENT-PROBE",
                SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 53),
            ) {
                Ok(size) => require(size == b"INERT-CONTAINMENT-PROBE".len())?,
                Err(error) if error.raw_os_error() == Some(1) => {}
                Err(_) => return Err(Error::Io),
            }
            ctx.control.check()
        })();
        if let Err(error) = sent {
            ctx.control.record(error);
        }
        let closed = nix::unistd::close(socket).map_err(|_| Error::Close);
        if let Err(error) = closed {
            ctx.control.cleanup_failed = true;
            ctx.control.record(error);
        }
        sent.and(closed)?;
        ctx.control.check()?;
        let after = policy::blocked_counter(
            &Self::call(ctx, "guard-after", &args, 30).await?,
            &self.table,
        )?;
        require(before.0.checked_add(1) == Some(after.0) && after.1 > before.1)?;
        self.check_monitors(ctx)
    }
    async fn service(
        &mut self,
        ctx: &mut Context,
        name: &str,
        image: &str,
    ) -> Result<(String, Value)> {
        require(["postgres", "simplestchat", "caddy"].contains(&name))?;
        let raw = Self::compose(
            ctx,
            &format!("service-{name}"),
            &["ps".into(), "--quiet".into(), name.into()],
            30,
        )
        .await?;
        let ids = policy::ids(&raw)?;
        require(ids.len() == 1)?;
        let id = ids.first().ok_or(Error::Refused)?.clone();
        let row = policy::one(
            &Self::docker(
                ctx,
                &format!("inspect-{name}"),
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
                && row["State"]["Running"] == true,
        )?;
        self.actors.insert(id.clone());
        Ok((id, row))
    }
    async fn sql(&mut self, ctx: &mut Context, role: &str, query: &str) -> Result<Vec<u8>> {
        let id = self.pg.clone().ok_or(Error::Refused)?;
        Self::docker(
            ctx,
            role,
            &[
                "exec".into(),
                "--user".into(),
                "999:999".into(),
                id,
                "psql".into(),
                "--no-psqlrc".into(),
                "--set".into(),
                "ON_ERROR_STOP=on".into(),
                "--host".into(),
                "/run/simplestchat-postgres".into(),
                "--username".into(),
                "postgres".into(),
                "--dbname".into(),
                "simplestchat".into(),
                "--tuples-only".into(),
                "--no-align".into(),
                "--command".into(),
                query.into(),
            ],
            15,
        )
        .await
    }
    pub async fn setup_database(&mut self, ctx: &mut Context) -> Result<()> {
        ctx.control.checkpoint = "database-setup";
        self.unknown_effect = true;
        Self::compose(
            ctx,
            "postgres-up",
            &[
                "up".into(),
                "--detach".into(),
                "--no-build".into(),
                "--pull".into(),
                "never".into(),
                "--wait".into(),
                "--wait-timeout".into(),
                "120".into(),
                "postgres".into(),
            ],
            135,
        )
        .await?;
        let pg_image = self.pg_image.clone();
        let (pg, row) = self.service(ctx, "postgres", &pg_image).await?;
        self.pg = Some(pg);
        self.unknown_effect = false;
        require(
            row["HostConfig"]["NetworkMode"] == "none"
                && row["HostConfig"]["ReadonlyRootfs"] == true
                && row["HostConfig"]["CapDrop"] == serde_json::json!(["ALL"])
                && row["Config"]["User"] == "999:999",
        )?;
        let health = row["Config"]["Healthcheck"]["Test"]
            .as_array()
            .ok_or(Error::Refused)?;
        require(health.len() == 2 && health[0] == "CMD-SHELL")?;
        self.pg_health = format!("/bin/sh -c {}", policy::string(&health[1])?);
        require(
            self.sql(ctx, "empty-database", observation::EMPTY_SCHEMA)
                .await?
                .trim_ascii()
                == b"t",
        )?;
        self.baseline_counts = self
            .sql(ctx, "initial-roles", observation::INITIAL_ROLES)
            .await?;
        require(self.baseline_counts == b"simplestchat_app|t\nsimplestchat_migrate|t\n")
    }
    async fn proxy_profile(&mut self, ctx: &mut Context, id: &str) -> Result<Value> {
        let row = policy::one(
            &Self::docker(ctx, "proxy-profile", &["inspect".into(), id.into()], 15).await?,
        )?;
        policy::proxy_profile(
            &row,
            id,
            &self.caddy_image,
            &ctx.request.caddy_image,
            &self.bridge,
        )?;
        Ok(row)
    }
    async fn endpoint(
        &mut self,
        ctx: &mut Context,
        id: &str,
        peers: &BTreeSet<String>,
    ) -> Result<Endpoint> {
        require(peers.contains(id) && peers.is_subset(&self.actors))?;
        let row = self.proxy_profile(ctx, id).await?;
        let network_id = self.network.clone().ok_or(Error::Refused)?;
        let network = policy::one(
            &Self::docker(
                ctx,
                "proxy-network",
                &["network".into(), "inspect".into(), network_id.clone()],
                15,
            )
            .await?,
        )?;
        policy::endpoint(id, peers, &row, &network, &network_id, &self.bridge)
    }
    async fn tls_root(
        &mut self,
        ctx: &mut Context,
        id: &str,
        peers: &BTreeSet<String>,
        root: &[u8],
    ) -> Result<tls::TlsProof> {
        let previous = ctx.control.push_phase(Duration::from_secs(30))?;
        let result = async {
            let target = self.endpoint(ctx, id, peers).await?;
            loop {
                self.check_monitors(ctx)?;
                let result = tls::probe_root(target.address, root, &mut ctx.control);
                match result {
                    Ok(proof) => {
                        require(self.endpoint(ctx, id, peers).await? == target)?;
                        ctx.control.check()?;
                        return Ok(proof);
                    }
                    Err(Error::Io)
                        if !ctx.control.cleanup_failed && ctx.control.first.is_none() =>
                    {
                        ctx.control.check()?;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        ctx.control.check()?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        .await;
        let ended = ctx.control.pop_phase(previous);
        result.and_then(|value| ended.map(|()| value))
    }
    pub async fn prepare_caddy(&mut self, ctx: &mut Context) -> Result<()> {
        ctx.control.checkpoint = "caddy-natural";
        let mut args = vec![
            "docker".into(),
            "--host".into(),
            "unix:///var/run/docker.sock".into(),
            "create".into(),
        ];
        args.extend(policy::normal_arguments(
            &ctx.request.caddy_image,
            &self.bridge,
            &ctx.request.run_id,
        ));
        self.create_uncertain = true;
        let actors = &mut self.actors;
        let uncertain = &mut self.create_uncertain;
        let mut adopted = None;
        ctx.check()?;
        ctx.commands
            .call_adopt(
                &mut ctx.control,
                "natural-caddy-create",
                &args,
                Duration::from_secs(30),
                |data| {
                    let id = policy::created_id(data)?;
                    actors.insert(id.clone());
                    adopted = Some(id);
                    *uncertain = false;
                    Ok(())
                },
            )
            .await?;
        ctx.check()?;
        let id = adopted.ok_or(Error::Refused)?;
        self.proxy_profile(ctx, &id).await?;
        Self::docker(
            ctx,
            "natural-caddy-start",
            &["start".into(), id.clone()],
            15,
        )
        .await?;
        let root = Path::new(PUBLIC)
            .join("caddy-data/caddy")
            .join(materials::PATHS[0]);
        let previous = ctx.control.push_phase(Duration::from_secs(30))?;
        let waited = async {
            loop {
                ctx.control.check()?;
                match fs::symlink_metadata(&root) {
                    Ok(_) => return Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(Error::Io),
                }
                self.check_monitors(ctx)?;
                tokio::time::sleep(Duration::from_millis(50)).await;
                ctx.control.check()?;
            }
        }
        .await;
        let ended = ctx.control.pop_phase(previous);
        waited.and(ended)?;
        let original = Original::open(&root, 65536, 10001, 0o600, &mut ctx.control)?;
        self.material_indices.push(ctx.originals.len());
        ctx.originals.push(original);
        let index = *self.material_indices.first().ok_or(Error::Refused)?;
        ctx.originals[index].require_parent(10001, 0o700, &mut ctx.control)?;
        let root_bytes = ctx.originals[index].data(65536, &mut ctx.control)?;
        let proof = self
            .tls_root(ctx, &id, &BTreeSet::from([id.clone()]), &root_bytes)
            .await?;
        for relative in &materials::PATHS[1..] {
            let path = Path::new(PUBLIC).join("caddy-data/caddy").join(relative);
            let original = Original::open(&path, 65536, 10001, 0o600, &mut ctx.control)?;
            self.material_indices.push(ctx.originals.len());
            ctx.originals.push(original);
            ctx.originals[*self.material_indices.last().ok_or(Error::Refused)?].require_parent(
                10001,
                0o700,
                &mut ctx.control,
            )?;
        }
        let mut bytes = BTreeMap::new();
        for (name, index) in materials::PATHS.iter().zip(&self.material_indices) {
            bytes.insert(
                (*name).to_owned(),
                ctx.originals[*index].data(65536, &mut ctx.control)?,
            );
        }
        let remaining = ctx
            .control
            .end()
            .checked_duration_since(Instant::now())
            .ok_or(Error::Deadline)?;
        let materials = materials::analyze(&bytes, SystemTime::now(), remaining)?;
        proof.bind(&materials)?;
        self.materials = Some(materials);
        Self::docker(
            ctx,
            "natural-caddy-stop",
            &["stop".into(), "--time".into(), "5".into(), id.clone()],
            10,
        )
        .await?;
        let actors = &mut self.actors;
        ctx.check()?;
        ctx.commands
            .call_adopt(
                &mut ctx.control,
                "natural-caddy-remove",
                &[
                    "docker".into(),
                    "--host".into(),
                    "unix:///var/run/docker.sock".into(),
                    "rm".into(),
                    "--force".into(),
                    id.clone(),
                ],
                Duration::from_secs(30),
                |_| {
                    actors.remove(&id);
                    Ok(())
                },
            )
            .await?;
        ctx.observed.set("naturalTlsPrepared", true)?;
        ctx.check()
    }
    pub async fn setup_complete(&mut self, ctx: &mut Context) -> Result<()> {
        Self::compose(
            ctx,
            "final-compose-admission",
            &[
                "--profile".into(),
                "maintenance".into(),
                "config".into(),
                "--quiet".into(),
            ],
            30,
        )
        .await?;
        ctx.check()?;
        self.attempts_before = directory_names(Path::new(PUBLIC).join("results").as_path())?;
        require(
            self.attempts_before.is_empty()
                && absent(Path::new(PUBLIC).join("release-state.json").as_path())?
                && absent(Path::new("/run/simplestchat-bench/current.json"))?,
        )?;
        ctx.observed.set("sourceCommitMatched", true)?;
        ctx.observed.set("sourceShallowBoundaryVerified", true)
    }
    pub fn native_boundary(&mut self, ctx: &mut Context) -> Result<()> {
        self.check_monitors(ctx)?;
        self.monitors
            .lock()
            .map_err(|_| Error::Io)?
            .collector
            .as_mut()
            .ok_or(Error::Refused)?
            .boundary(&mut ctx.control)?;
        self.unknown_effect = true;
        Ok(())
    }
}

fn absent(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(Error::Io),
    }
}
fn directory_names(path: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(|_| Error::Io)? {
        let entry = entry.map_err(|_| Error::Io)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::Refused)?;
        require(names.len() < 4096 && names.insert(name))?;
    }
    Ok(names)
}
