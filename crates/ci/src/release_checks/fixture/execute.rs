//! The fixture's phases on a disposable host: bootstrap the deployment from
//! the production templates, drive the browser rollover, one successful
//! app-only release and one deliberately failed start with its rollback,
//! then remove only what it can prove it owns.

use super::{
    files::{self, present},
    host::{Commands, Fault, Outcome, Paths, Result, Runner, require},
    images::{self, Derived, Kind},
    json::{self, object},
    plan::{self, Action, CADDY, LABEL, POSTGRES},
    policy::{self, Unavailable},
    render::{self, Identity, Sources},
    report::{Phase, Release, Report},
};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

/// What the fixture was given and generated before it touched the host.
pub(super) struct Inputs {
    /// The local production image selector.
    pub image: String,
    pub node: PathBuf,
    pub chromium: PathBuf,
    pub http_policy: PathBuf,
    pub ansible: PathBuf,
    /// The run's ownership token: 32 random hexadecimal digits.
    pub token: String,
    /// The six deployment secrets.
    pub secrets: [String; 6],
}

/// Retained identities of the three running services.
type Snapshot = [[Value; 4]; 3];
/// The configuration files a release selects.
const SELECTION: [&str; 3] = ["compose.public.yml", "app.env", "images.json"];
const MAX_TEMPLATE: u64 = 1024 * 1024;

/// One fixture run: its command owner, paths, inputs and public report.
pub(super) struct Fixture<R> {
    pub commands: Commands<R>,
    pub paths: Paths,
    pub inputs: Inputs,
    pub report: Report,
    private: PathBuf,
    created: bool,
    authority: Option<(PathBuf, Option<String>)>,
    base: Option<(String, String)>,
    derived: BTreeMap<Kind, Derived>,
}

fn io<T>(result: std::io::Result<T>, message: &'static str) -> Result<T> {
    result.map_err(|_| Fault::Check(message))
}

impl<R: Runner> Fixture<R> {
    pub(super) fn new(commands: Commands<R>, paths: Paths, inputs: Inputs, output: &Path) -> Self {
        Self {
            commands,
            paths,
            inputs,
            report: Report::new(),
            private: output.join("private"),
            created: false,
            authority: None,
            base: None,
            derived: BTreeMap::new(),
        }
    }

    /// The runner, once the fixture is finished with it.
    pub(super) fn into_runner(self) -> R {
        self.commands.runner
    }

    /// Record `derived` as the `kind` release without building it.
    #[cfg(test)]
    pub(super) fn insert_derived(&mut self, kind: Kind, derived: Derived) {
        self.derived.insert(kind, derived);
    }

    fn derived(&self, kind: Kind) -> Result<&Derived> {
        self.derived
            .get(&kind)
            .ok_or(Fault::Check("Fixture image was not derived"))
    }

    async fn owned(&mut self, identity: &str) -> Result<Value> {
        require(
            policy::identity(identity),
            "Container identity must be exact",
        )?;
        let text = self.commands.text(plan::inspect(identity)).await?;
        policy::owned(identity, &text, &self.inputs.token)
    }

    /// The verified container running `service` in the fixture's project.
    async fn service(&mut self, service: &str) -> Result<Value> {
        let identity = self
            .commands
            .text(plan::running(&self.paths, service))
            .await?;
        self.owned(&identity).await
    }

    async fn service_id(&mut self, service: &str) -> Result<String> {
        let value = self.service(service).await?;
        Ok(value["id"].as_str().unwrap_or_default().to_owned())
    }

    fn selection(&self) -> Result<Vec<Vec<u8>>> {
        SELECTION
            .iter()
            .map(|name| {
                io(
                    std::fs::read(self.paths.config.join(name)),
                    "Cannot read the selection",
                )
            })
            .collect()
    }

    async fn snapshot(&mut self) -> Result<Snapshot> {
        let mut values = Vec::new();
        for name in ["simplestchat", "postgres", "caddy"] {
            values.push(policy::stable(&self.service(name).await?)?);
        }
        Ok(values.try_into().expect("three services were inspected"))
    }

    async fn sql(&mut self, command: Option<&str>, input: Option<&Path>) -> Result<Outcome> {
        let identity = self.service_id("postgres").await?;
        self.commands
            .run(plan::sql(&identity, command, input))
            .await
    }

    /// Wait up to 45 s for `/ready` through the proxy, or inside the running
    /// migration container's own network namespace.
    async fn ready(&mut self, migration: Option<&str>) -> Result<()> {
        let deadline = self.commands.now() + Duration::from_secs(45);
        loop {
            let mut request = plan::ready(None);
            if let Some(migration) = migration {
                let value = self.owned(migration).await?;
                let state = value.get("state").and_then(object);
                let pid = state.and_then(|state| json::integer(state.get("Pid")));
                let running = state
                    .and_then(|state| state.get("Running"))
                    .is_some_and(|running| running == &Value::Bool(true));
                let pid = pid.filter(|pid| running && *pid > 1);
                let pid =
                    pid.ok_or(Fault::Check("Migration container stopped before readiness"))?;
                let namespace = self.commands.runner.namespace(pid)?;
                let again = self.owned(migration).await?;
                let same = again
                    .get("state")
                    .and_then(object)
                    .and_then(|state| json::integer(state.get("Pid")));
                require(same == Some(pid), "Migration namespace changed")?;
                request = plan::ready(Some(std::os::fd::AsRawFd::as_raw_fd(&namespace)));
                request.inherit = Some(namespace);
            }
            let outcome = self.commands.run(request).await?;
            if outcome.succeeded() {
                let answer = json::decode(outcome.text()?.as_bytes());
                let answer = answer.as_ref().and_then(object);
                let answer = answer.ok_or(Fault::Check("Readiness answer is not a JSON object"))?;
                if answer.get("status") == Some(&json!("ready")) {
                    return Ok(());
                }
            }
            require(
                self.commands.now() < deadline,
                "Fixture readiness deadline exceeded",
            )?;
            self.commands.pause(Duration::from_millis(500)).await?;
        }
    }

    /// The passive response check against the verified proxy and application.
    async fn response_policy(&mut self) -> Result<()> {
        let caddy = self.service_id("caddy").await?;
        let app = self.service_id("simplestchat").await?;
        let digest = match &self.authority {
            Some((_, Some(digest))) => digest.clone(),
            _ => return Err(Fault::Check("Fixture TLS authority is not recorded")),
        };
        let binary = self.inputs.http_policy.clone();
        let ca = self.private.join("ca.crt");
        let request =
            plan::response_policy(&binary, &self.inputs.token, [&caddy, &app], &ca, &digest);
        self.commands.runner.retained(&binary)?;
        let outcome = self.commands.run(request).await;
        self.commands.runner.retained(&binary)?;
        let outcome = outcome?;
        let receipt = json::decode(outcome.text()?.as_bytes());
        require(
            outcome.succeeded() && receipt.as_ref() == Some(&policy::receipt()),
            "Fixture HTTP response policy did not pass completely",
        )?;
        self.report.cases.http_response_policy = Some(policy::receipt());
        Ok(())
    }

    async fn base_unchanged(&mut self, message: &'static str) -> Result<()> {
        let (image, tag) = self.base.clone().ok_or(Fault::Check(message))?;
        let current = self.commands.text(plan::image_identity(&tag)).await?;
        require(current == image, message)
    }

    /// Build the `kind` release from the checked local base tag.
    async fn derive(&mut self, kind: Kind) -> Result<()> {
        let (_, tag) = self
            .base
            .clone()
            .ok_or(Fault::Check("Fixture base tag is not recorded"))?;
        let directory = self.private.join(kind.name());
        io(
            files::directory(&directory, 0o700),
            "Cannot create a fixture build context",
        )?;
        self.base_unchanged("Fixture base tag changed before build")
            .await?;
        for (name, text) in images::context(&tag, &self.inputs.token, kind) {
            io(
                files::write_new(&directory.join(name), text.as_bytes()),
                "Cannot write a build context",
            )?;
        }
        let revision = images::revision(&self.inputs.token, kind);
        let derived_tag = images::tag(&revision);
        self.commands
            .run(plan::build(&derived_tag, &directory))
            .await?;
        self.base_unchanged("Fixture base tag changed during build")
            .await?;
        let image = self
            .commands
            .text(plan::image_identity(&derived_tag))
            .await?;
        require(
            policy::image(&image),
            "Fixture image has no immutable identity",
        )?;
        self.derived.insert(
            kind,
            Derived {
                revision,
                tag: derived_tag,
                image,
            },
        );
        Ok(())
    }

    /// Export the `kind` release as a retained release the Ops executable
    /// accepts, without rebuilding production code.
    async fn artifact(&mut self, kind: Kind) -> Result<()> {
        let derived = self.derived(kind)?.clone();
        let directory = self.paths.root.join("releases").join(&derived.revision);
        io(
            files::directory(&directory, 0o700),
            "Cannot create a fixture release",
        )?;
        let archive = directory.join("image.tar");
        self.commands
            .run(plan::save(&derived.tag, &archive))
            .await?;
        io(
            std::fs::set_permissions(
                &archive,
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            ),
            "Cannot protect a fixture archive",
        )?;
        let migrations = io(
            images::migrations(&self.paths.checkout.join("migrations")),
            "Cannot read the migrations",
        )?;
        let manifest = simplestchat_ops::release::manifest::Fields {
            schema_version: 1,
            revision: derived.revision.clone(),
            platform: "linux/amd64".into(),
            archive_sha256: io(
                files::sha256_file(&archive),
                "Cannot hash a fixture archive",
            )?,
            image_tag: derived.tag.clone(),
            migrations,
            created_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        };
        io(
            files::write_json(&directory.join("release.json"), &manifest),
            "Cannot write a manifest",
        )?;
        self.commands.run(plan::archive(&directory)).await?;
        Ok(())
    }

    /// The fixture CA and the private localhost certificate, and the CA in the
    /// host's trust store.
    async fn certificates(&mut self) -> Result<()> {
        let [authority, server, signed] =
            plan::certificates(&self.inputs.token, &self.private, &self.paths.config);
        self.commands.run(authority).await?;
        self.commands.run(server).await?;
        let extensions = "subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\n\
                          extendedKeyUsage=serverAuth\n";
        io(
            files::write_new(&self.private.join("server.ext"), extensions.as_bytes()),
            "Cannot write certificate extensions",
        )?;
        self.commands.run(signed).await?;
        for name in ["fixture.key", "fixture.crt"] {
            let path = self.paths.config.join(name);
            self.commands.chown(&path, 10001)?;
            io(
                std::fs::set_permissions(
                    &path,
                    std::os::unix::fs::PermissionsExt::from_mode(0o600),
                ),
                "Cannot protect the fixture certificate",
            )?;
        }
        let trusted = self.paths.trust.join(format!(
            "simplestchat-release-test-{}.crt",
            self.inputs.token
        ));
        self.authority = Some((trusted.clone(), None));
        let certificate = io(
            files::read_bounded(&self.private.join("ca.crt"), 65536),
            "Cannot read the fixture CA",
        )?;
        io(
            files::write_new(&trusted, &certificate),
            "Cannot publish the fixture CA",
        )?;
        let digest = io(files::sha256_file(&trusted), "Cannot hash the fixture CA")?;
        self.authority = Some((trusted, Some(digest)));
        self.commands.run(plan::update_trust()).await?;
        Ok(())
    }

    /// Render the deployment templates with Ansible and write the fixture's
    /// configuration, refusing to replace any existing file.
    async fn configure(&mut self) -> Result<()> {
        let baseline = self.derived(Kind::Baseline)?.clone();
        let identity = Identity {
            image: baseline.image.clone(),
            revision: baseline.revision.clone(),
            token: self.inputs.token.clone(),
            postgres: POSTGRES.into(),
            caddy: CADDY.into(),
        };
        let staging = self.private.join("rendered");
        let ansible = self.private.join("ansible");
        for directory in [&staging, &ansible] {
            io(
                files::directory(directory, 0o700),
                "Cannot create the render directory",
            )?;
        }
        let variables = render::variables(
            &identity,
            &self.paths.config,
            &self.paths.root,
            &self.inputs.secrets,
        );
        let play = self.private.join("render-play.json");
        io(
            files::write_json(
                &play,
                &render::play(&self.paths.checkout, &staging, &variables),
            ),
            "Cannot write the render play",
        )?;
        let mut request = plan::render(&self.inputs.ansible, &play);
        let playbooks = self.paths.checkout.join("ops/ansible");
        request.directory = Some(playbooks.clone());
        request.environment = vec![
            (
                "ANSIBLE_CONFIG".into(),
                playbooks.join("ansible.cfg").into(),
            ),
            ("ANSIBLE_HOME".into(), ansible.join("home").into()),
            ("ANSIBLE_LOCAL_TEMP".into(), ansible.join("local").into()),
            ("ANSIBLE_REMOTE_TEMP".into(), ansible.join("remote").into()),
            ("ANSIBLE_NOCOLOR".into(), "1".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
        ];
        self.commands.run(request).await?;
        let mut rendered = BTreeMap::new();
        for (name, template) in render::TEMPLATES {
            let bytes = io(
                files::read_bounded(&staging.join(template), MAX_TEMPLATE),
                "Rendered template unreadable",
            )?;
            let text = String::from_utf8(bytes)
                .map_err(|_| Fault::Check("Rendered template is not UTF-8"))?;
            rendered.insert(name.to_owned(), text);
        }
        let read = |name: &str| {
            io(
                files::read_bounded(&self.paths.checkout.join(name), MAX_TEMPLATE),
                "Checkout source unreadable",
            )
            .and_then(|bytes| {
                String::from_utf8(bytes).map_err(|_| Fault::Check("Checkout source is not UTF-8"))
            })
        };
        let sources = Sources {
            rendered,
            base: read("docker-compose.yml")?,
            caddyfile: read("Caddyfile")?,
        };
        let outputs = render::outputs(&identity, &self.paths.config, &self.paths.root, &sources)
            .map_err(Fault::Check)?;
        for name in outputs.keys() {
            require(
                !present(&self.paths.config.join(name)),
                "Fixture output already exists",
            )?;
        }
        for (name, text) in &outputs {
            io(
                files::write_new(&self.paths.config.join(name), text.as_bytes()),
                "Cannot write fixture configuration",
            )?;
        }
        Ok(())
    }

    /// Refuse an unsuitable engine, build the fixture releases and bring up
    /// the baseline deployment with its database marker and checked proxy.
    pub(super) async fn setup(&mut self) -> Result<()> {
        self.commands.runner.preflight(&self.paths)?;
        policy::engine::empty_containers(
            &self
                .commands
                .text(plan::docker("docker ps", &["ps", "--all", "--quiet"]))
                .await?,
        )?;
        let filter = format!("label=com.docker.compose.project={}", plan::PROJECT);
        let networks = plan::docker(
            "docker network ls",
            &["network", "ls", "--quiet", "--filter", &filter],
        );
        policy::engine::empty_networks(&self.commands.text(networks).await?)?;
        let help = self
            .commands
            .text(plan::docker(
                "docker image save",
                &["image", "save", "--help"],
            ))
            .await?;
        policy::engine::exports_platform(&help)?;
        let version = plan::docker(
            "docker version",
            &["version", "--format", "{{.Server.APIVersion}}"],
        );
        policy::engine::api(&self.commands.text(version).await?)?;
        let builder = plan::docker("docker buildx inspect", &["buildx", "inspect", "default"]);
        policy::engine::builder(&self.commands.text(builder).await?)?;
        let inspection = plan::docker(
            "docker image inspect",
            &[
                "image",
                "inspect",
                "--format",
                plan::IMAGE_FORMAT,
                &self.inputs.image,
            ],
        );
        let base_image = policy::engine::production(&self.commands.text(inspection).await?)?;
        self.report.base_image = Some(base_image.clone());
        let revision = self
            .commands
            .text(plan::revision(&self.paths.checkout))
            .await?;
        require(
            revision.len() == 40
                && revision
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "Source revision must be an exact Git commit",
        )?;
        self.report.source_revision = Some(revision);
        let base_tag = format!("simplestchat-release-test/base:{}", self.inputs.token);
        let reference = format!("reference={base_tag}");
        let listed = plan::docker(
            "docker image ls",
            &["image", "ls", "--quiet", "--filter", &reference],
        );
        require(
            self.commands.text(listed).await?.is_empty(),
            "Fixture base tag already exists",
        )?;
        self.commands
            .run(plan::docker(
                "docker image tag",
                &["image", "tag", &base_image, &base_tag],
            ))
            .await?;
        self.base = Some((base_image, base_tag));
        // Prerequisite pulls happen before any service is started.
        for image in [POSTGRES, CADDY] {
            self.commands.run(plan::pull(image)).await?;
        }
        for kind in Kind::ALL {
            self.derive(kind).await?;
        }
        let owner = json!({"token": self.inputs.token});
        for path in [
            self.paths.config.clone(),
            self.paths.root.clone(),
            self.paths.work.clone(),
        ] {
            io(
                files::directory(&path, 0o700),
                "Cannot create a fixture service path",
            )?;
            io(
                files::write_json(&path.join("release-test-owner.json"), &owner),
                "Cannot mark a fixture service path",
            )?;
        }
        self.created = true;
        for name in ["releases", "results"] {
            io(
                files::directory(&self.paths.root.join(name), 0o700),
                "Cannot create a fixture data directory",
            )?;
        }
        for (name, owner, mode) in [
            ("postgres", 999, 0o700),
            ("postgres-socket", 999, 0o755),
            ("caddy-data", 10001, 0o700),
            ("caddy-config", 10001, 0o700),
        ] {
            let path = self.paths.root.join(name);
            io(
                files::directory(&path, mode),
                "Cannot create a fixture data directory",
            )?;
            self.commands.chown(&path, owner)?;
            // The application must still traverse the database socket directory.
            io(
                std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(mode)),
                "Cannot create a fixture data directory",
            )?;
        }
        self.configure().await?;
        let baseline = self.derived(Kind::Baseline)?.revision.clone();
        let assets = self.commands.text(plan::assets(&baseline)).await?;
        let receipt = json::decode(assets.as_bytes());
        let initialized = receipt.as_ref().and_then(object).is_some_and(|receipt| {
            receipt.get("passed") == Some(&Value::Bool(true)) && !receipt.contains_key("busy")
        });
        require(initialized, "Browser asset store was not initialized")?;
        for name in [
            "postgres-admin-password",
            "pg_hba.conf",
            "init-database.sql",
        ] {
            self.commands.chown(&self.paths.config.join(name), 999)?;
        }
        // Root's and readable, as a prepared host keeps the proxy's configuration.
        io(
            std::fs::set_permissions(
                self.paths.config.join("Caddyfile"),
                std::os::unix::fs::PermissionsExt::from_mode(0o644),
            ),
            "Cannot set the proxy configuration mode",
        )?;
        self.certificates().await?;
        self.commands
            .run(plan::compose(&self.paths, &["config", "--quiet"]))
            .await?;
        self.report.phase = Phase::Bootstrap;
        let mut postgres = plan::compose(
            &self.paths,
            &[
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "--wait",
                "--wait-timeout",
                "120",
                "postgres",
            ],
        );
        postgres.timeout = Duration::from_secs(135);
        self.commands.run(postgres).await?;
        let migrate = [
            "--profile",
            "maintenance",
            "up",
            "--detach",
            "--no-build",
            "--pull",
            "never",
            "--no-deps",
            "migrate",
        ];
        self.commands
            .run(plan::compose(&self.paths, &migrate))
            .await?;
        let migration = self.service_id("migrate").await?;
        self.ready(Some(&migration)).await?;
        let mut stop = plan::compose(
            &self.paths,
            &[
                "--profile",
                "maintenance",
                "stop",
                "--timeout",
                "30",
                "migrate",
            ],
        );
        stop.timeout = Duration::from_secs(45);
        self.commands.run(stop).await?;
        policy::healthy_exit(&self.owned(&migration).await?, false)?;
        self.commands
            .run(plan::docker(
                "docker logs",
                &["logs", "--tail", "100", &migration],
            ))
            .await?;
        self.commands
            .run(plan::docker("docker rm", &["rm", &migration]))
            .await?;
        let grants = self.paths.config.join("runtime-grants.sql");
        self.sql(None, Some(&grants)).await?;
        // The marker is the run's own generated token, never supplied SQL.
        let marker = format!(
            "CREATE TABLE public.release_fixture_marker (value text NOT NULL); \
             INSERT INTO public.release_fixture_marker VALUES ('{}');",
            self.inputs.token
        );
        self.sql(Some(&marker), None).await?;
        let mut up = plan::compose(
            &self.paths,
            &[
                "up",
                "--detach",
                "--no-build",
                "--pull",
                "never",
                "simplestchat",
                "caddy",
            ],
        );
        up.timeout = Duration::from_secs(60);
        self.commands.run(up).await?;
        self.ready(None).await?;
        self.response_policy().await?;
        for kind in Kind::ALL {
            self.artifact(kind).await?;
        }
        Ok(())
    }

    fn attempts(&self) -> Result<Vec<PathBuf>> {
        io(
            files::attempts(&self.paths.root.join("results")),
            "Cannot list release attempts",
        )
    }

    /// The fixed fields of one attempt's outcome, or that it has none.
    fn sanitized(attempt: &Path) -> Result<Value> {
        let path = attempt.join("outcome.json");
        if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            let name = attempt.file_name().unwrap_or_default().to_string_lossy();
            return Ok(json!({"attempt": name, "outcome": "missing"}));
        }
        let outcome = Self::outcome(attempt)?;
        Ok(policy::sanitized(&outcome))
    }

    fn outcome(attempt: &Path) -> Result<Map<String, Value>> {
        let bytes = io(
            files::read_bounded(&attempt.join("outcome.json"), 1024 * 1024),
            "Release outcome unreadable",
        )?;
        json::decode(&bytes)
            .as_ref()
            .and_then(object)
            .cloned()
            .ok_or(Fault::Check("Release outcome is not a JSON object"))
    }

    /// Two real browser documents across an image replacement and rollback.
    pub(super) async fn browser(&mut self) -> Result<()> {
        self.report.phase = Phase::BrowserAssetRollover;
        let caddy = self.service_id("caddy").await?;
        let validate = [
            "exec",
            "--interactive",
            "--user",
            "10001:10001",
            &caddy,
            "caddy",
            "validate",
            "--config",
            "/dev/stdin",
            "--adapter",
            "caddyfile",
        ];
        let mut request = plan::docker("docker exec", &validate);
        request.input = Some(self.paths.config.join("Caddyfile"));
        request.timeout = Duration::from_secs(20);
        self.commands.run(request).await?;
        let configuration = self.private.join("asset-rollover.json");
        let result = self.private.join("asset-rollover-result.json");
        let editions = json!({
            "original": self.derived(Kind::Baseline)?.revision,
            "candidate": self.derived(Kind::Candidate)?.revision,
        });
        io(
            files::write_json(&configuration, &editions),
            "Cannot write the rollover configuration",
        )?;
        let before = self.snapshot().await?;
        let earlier = self.attempts()?;
        let chromium = self.inputs.chromium.clone();
        let request = plan::rollover(
            &self.inputs.node,
            &self.paths.checkout,
            [&configuration, &chromium, &result],
        );
        let outcome = self.commands.run(request).await?;
        let mut releases = Vec::new();
        for attempt in self
            .attempts()?
            .into_iter()
            .filter(|attempt| !earlier.contains(attempt))
        {
            releases.push(Self::sanitized(&attempt)?);
        }
        self.report.browser_asset_releases = Some(releases);
        if !outcome.succeeded() {
            let stderr = files::read_bounded(&outcome.stderr, policy::MAX_BROWSER_ERROR as u64);
            self.report.browser_asset_failure_stage =
                Some(stderr.map_or("unavailable", |bytes| policy::browser_stage(&bytes)));
            return Err(Fault::Check(
                "Browser rollover command failed; inspect fixed stage and private evidence",
            ));
        }
        let report = files::read_bounded(&result, policy::MAX_BROWSER_REPORT)
            .map_err(|_| Fault::Check("Browser rollover report missing or oversized"))?;
        let report = json::decode(&report);
        let report = report
            .as_ref()
            .and_then(object)
            .ok_or(Fault::Check("Browser rollover report is not a JSON object"))?;
        let report = Value::Object(report.clone());
        require(
            report == policy::rollover(),
            "Browser rollover proof incomplete",
        )?;
        self.continuity(&before).await?;
        self.report.cases.browser_assets = Some(report);
        Ok(())
    }

    /// Describe a failed release's validator only after verifying it is that
    /// release's own fixed validator; diagnostics never replace the failure.
    pub(super) async fn validator(&mut self, attempt: &Path, image: &str) {
        let original = self.commands.last_failure.clone();
        let diagnostic = self.diagnose(attempt, image).await;
        self.report.validator_startup = Some(match diagnostic {
            Ok(value) => value,
            Err(reason) => json!({"diagnosticUnavailable": true, "reason": reason.reason()}),
        });
        self.commands.last_failure = original;
    }

    async fn diagnose(
        &mut self,
        attempt: &Path,
        image: &str,
    ) -> std::result::Result<Value, Unavailable> {
        let bytes = files::read_bounded(&attempt.join("validation-name.txt"), 128)
            .ok()
            .filter(|bytes| !bytes.is_empty())
            .ok_or(Unavailable::Name)?;
        let name = policy::validator_name(&bytes)?;
        let mut request = plan::docker(
            "docker inspect",
            &["inspect", "--format", plan::VALIDATOR_FORMAT, &name],
        );
        request.timeout = Duration::from_secs(10);
        request.check = false;
        let outcome = self
            .commands
            .run(request)
            .await
            .map_err(|_| Unavailable::Inspection)?;
        let text = outcome
            .text()
            .ok()
            .filter(|_| outcome.succeeded())
            .ok_or(Unavailable::Inspection)?;
        let (identity, mut diagnostic) = policy::validator(&name, image, &text)?;
        let mut logs = plan::docker("docker logs", &["logs", "--tail", "20", &identity]);
        logs.timeout = Duration::from_secs(10);
        logs.check = false;
        let read = match self.commands.run(logs).await {
            Ok(outcome) if outcome.succeeded() => {
                files::read_prefix(&outcome.stderr, policy::VALIDATOR_STDERR_READ).ok()
            }
            _ => None,
        };
        diagnostic["runtimeLogs"] = match read {
            Some(stderr) => policy::validator_logs(&stderr),
            None => json!({"diagnosticUnavailable": true}),
        };
        Ok(diagnostic)
    }

    /// One real release attempt, its single new retained attempt, and the
    /// agreement of its exit status with its own outcome.
    pub(super) async fn release(
        &mut self,
        action: Action,
        kind: Kind,
        failure: bool,
    ) -> Result<(PathBuf, Map<String, Value>)> {
        let derived = self.derived(kind)?.clone();
        let earlier = self.attempts()?;
        let outcome = self
            .commands
            .run(plan::release(action, &derived.revision))
            .await?;
        let mut attempts: Vec<PathBuf> = self
            .attempts()?
            .into_iter()
            .filter(|attempt| !earlier.contains(attempt))
            .collect();
        require(
            attempts.len() == 1,
            "Release must retain exactly one new attempt",
        )?;
        let attempt = attempts.remove(0);
        let recorded = Self::outcome(&attempt)?;
        self.report.last_release = Some(policy::sanitized(&recorded));
        if !outcome.succeeded() {
            self.validator(&attempt, &derived.image).await;
        }
        policy::release(
            action.name(),
            &derived.revision,
            outcome.status,
            failure,
            &recorded,
        )?;
        Ok((attempt, recorded))
    }

    /// The retained dependencies are the same containers and the durable
    /// database marker is unchanged.
    async fn continuity(&mut self, before: &Snapshot) -> Result<Snapshot> {
        let after = self.snapshot().await?;
        require(
            after[1] == before[1] && after[2] == before[2],
            "A retained dependency changed",
        )?;
        let marker = self
            .sql(
                Some("SELECT value FROM public.release_fixture_marker"),
                None,
            )
            .await?;
        require(
            marker.text()? == self.inputs.token,
            "Durable database marker changed",
        )?;
        Ok(after)
    }

    /// The release's own backup has its recorded digest and the expected tables.
    async fn backup(&mut self, attempt: &Path, outcome: &Map<String, Value>) -> Result<()> {
        let backup = attempt.join("database-before.dump");
        let size = std::fs::symlink_metadata(&backup)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let digest = files::sha256_file(&backup).ok();
        require(
            size > 0
                && digest.is_some()
                && outcome.get("backupSha256").and_then(Value::as_str) == digest.as_deref(),
            "Backup identity mismatch",
        )?;
        let identity = self.service_id("postgres").await?;
        let listing = self
            .commands
            .text(plan::backup_listing(&identity, &backup))
            .await?;
        require(
            listing.contains("release_fixture_marker") && listing.contains("_sqlx_migrations"),
            "Live backup omitted expected tables",
        )
    }

    fn journal(&self) -> Result<Map<String, Value>> {
        let bytes = io(
            files::read_bounded(&self.paths.root.join("release-state.json"), 1024 * 1024),
            "Release journal unreadable",
        )?;
        json::decode(&bytes)
            .as_ref()
            .and_then(object)
            .cloned()
            .ok_or(Fault::Check("Release journal is not a JSON object"))
    }

    fn stdout_texts(attempt: &Path) -> Result<Vec<String>> {
        let mut texts = Vec::new();
        for entry in io(std::fs::read_dir(attempt), "Cannot read the failed attempt")? {
            let path = io(entry, "Cannot read the failed attempt")?.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "stdout")
            {
                continue;
            }
            if let Ok(bytes) = files::read_bounded(&path, super::host::MAX_INSPECTION) {
                texts.push(
                    String::from_utf8(bytes)
                        .map_err(|_| Fault::Check("Failed startup evidence is not UTF-8"))?,
                );
            }
        }
        Ok(texts)
    }

    /// One successful app-only release and one deliberately failed start
    /// that rolls back, each staged first without touching the deployment.
    pub(super) async fn exercise(&mut self) -> Result<()> {
        for (kind, stage, deploy) in [
            (
                Kind::Candidate,
                Phase::StageCandidate,
                Phase::DeployCandidate,
            ),
            (Kind::Failed, Phase::StageFailed, Phase::DeployFailed),
        ] {
            let image = self.derived(kind)?.image.clone();
            let failure = kind == Kind::Failed;
            let (original, before) = (self.selection()?, self.snapshot().await?);
            self.report.phase = stage;
            self.release(Action::Stage, kind, false).await?;
            require(
                self.selection()? == original && self.snapshot().await? == before,
                "Staging interrupted or changed the running deployment",
            )?;
            self.report.phase = deploy;
            let started = self.commands.now();
            let (attempt, outcome) = self.release(Action::Deploy, kind, failure).await?;
            let after = self.continuity(&before).await?;
            self.ready(None).await?;
            self.backup(&attempt, &outcome).await?;
            let journal = self.journal()?;
            require(
                journal.get("finalized") == Some(&Value::Bool(true)),
                "Release cleanup remained unfinished",
            )?;
            let phase = journal.get("phase").and_then(Value::as_str);
            if failure {
                require(
                    outcome.get("rollbackAttempted") == Some(&Value::Bool(true))
                        && outcome.get("rollbackPassed") == Some(&Value::Bool(true))
                        && phase == Some("rolled_back"),
                    "Failed startup did not complete its single rollback",
                )?;
                require(
                    self.selection()? == original && after[0][1] == before[0][1],
                    "Rollback did not restore the prior selection and image",
                )?;
                policy::failed_start(&Self::stdout_texts(&attempt)?, &image)?;
            } else {
                require(
                    after[0][1] == json!(image)
                        && after[0][0] != before[0][0]
                        && phase == Some("complete"),
                    "Successful upgrade did not replace the application",
                )?;
            }
            let case = Release {
                passed: true,
                release_passed: outcome.get("passed") == Some(&Value::Bool(true)),
                rollback_passed: outcome
                    .get("rollbackPassed")
                    .cloned()
                    .unwrap_or(Value::Null),
                dependencies_unchanged: true,
                database_marker_preserved: true,
                backup_verified: true,
                stage_uninterrupted: true,
                command_seconds: super::host::rounded(
                    self.commands.now().saturating_duration_since(started),
                ),
            };
            if failure {
                self.report.cases.failed = Some(case);
            } else {
                self.report.cases.candidate = Some(case);
            }
        }
        Ok(())
    }

    async fn remove_containers(&mut self) -> Result<()> {
        let filter = format!("label={LABEL}={}", self.inputs.token);
        let listing = plan::docker(
            "docker ps",
            &["ps", "--all", "--quiet", "--no-trunc", "--filter", &filter],
        );
        let identities: Vec<String> = self
            .commands
            .text(listing)
            .await?
            .lines()
            .map(str::to_owned)
            .collect();
        let mut containers = Vec::new();
        for identity in &identities {
            containers.push(self.owned(identity).await?);
        }
        let order = |value: &Value| {
            plan::SERVICES
                .iter()
                .position(|service| policy::service(value) == Some(service))
        };
        containers.sort_by_key(order);
        let failed = self
            .derived
            .get(&Kind::Failed)
            .map(|derived| json!(derived.image));
        for value in containers {
            let identity = value["id"].as_str().unwrap_or_default().to_owned();
            self.commands
                .run(plan::docker(
                    "docker logs",
                    &["logs", "--tail", "150", &identity],
                ))
                .await?;
            let running = value.get("state").and_then(|state| state.get("Running"));
            if running
                .is_some_and(|running| running != &Value::Bool(false) && running != &Value::Null)
            {
                let mut stop = plan::docker("docker stop", &["stop", "--time", "30", &identity]);
                stop.timeout = Duration::from_secs(45);
                self.commands.run(stop).await?;
            }
            let stopped = self.owned(&identity).await?;
            let deliberate = failed.is_some() && stopped.get("image") == failed.as_ref();
            policy::healthy_exit(&stopped, deliberate)?;
            self.commands
                .run(plan::docker("docker rm", &["rm", &identity]))
                .await?;
        }
        let listing = plan::docker(
            "docker network ls",
            &[
                "network",
                "ls",
                "--quiet",
                "--no-trunc",
                "--filter",
                &filter,
            ],
        );
        let networks: Vec<String> = self
            .commands
            .text(listing)
            .await?
            .lines()
            .map(str::to_owned)
            .collect();
        for identity in networks {
            require(
                policy::identity(&identity),
                "Network identity must be exact",
            )?;
            let inspection = plan::docker(
                "docker network inspect",
                &["network", "inspect", "--format", "{{json .}}", &identity],
            );
            let value = json::decode(self.commands.text(inspection).await?.as_bytes());
            let owned = value.as_ref().and_then(object).is_some_and(|value| {
                let labels = value.get("Labels").and_then(object);
                let label = |key: &str| {
                    labels
                        .and_then(|labels| labels.get(key))
                        .and_then(Value::as_str)
                };
                value.get("Id").and_then(Value::as_str) == Some(identity.as_str())
                    && label(LABEL) == Some(self.inputs.token.as_str())
                    && label("com.docker.compose.project") == Some(plan::PROJECT)
                    && value
                        .get("Containers")
                        .is_some_and(|containers| match containers {
                            Value::Null => true,
                            Value::Object(map) => map.is_empty(),
                            Value::Array(list) => list.is_empty(),
                            _ => false,
                        })
            });
            require(owned, "Network ownership or cleanup is uncertain")?;
            self.commands
                .run(plan::docker(
                    "docker network rm",
                    &["network", "rm", &identity],
                ))
                .await?;
        }
        // The real helper removes its own validators; an ambiguous remaining
        // container is left for inspection rather than guessed at.
        let remaining = self
            .commands
            .text(plan::docker("docker ps", &["ps", "--all", "--quiet"]))
            .await?;
        require(
            remaining.is_empty(),
            "Containers remain after fixture cleanup; inspect private evidence",
        )
    }

    /// Remove only verified owned containers, their network and the fixture
    /// CA, and only once every command settled; record each failure.
    pub(super) async fn cleanup(&mut self) {
        let mut failures = Vec::new();
        if self.commands.settlement_unconfirmed {
            self.report.cleanup_passed = false;
            self.report.cleanup_failures = vec!["CommandSettlementUnconfirmed".into()];
            return;
        }
        if self.created
            && let Err(fault) = self.remove_containers().await
        {
            failures.push(fault.to_string());
        }
        match self.authority.clone() {
            // A partial exclusive write is retained, never taken for cleanup.
            Some((_, None)) => failures.push("FixtureCaPublicationIncomplete".into()),
            Some((path, Some(digest))) => {
                let removed = async {
                    let regular =
                        std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file());
                    let same = files::sha256_file(&path).ok() == Some(digest);
                    require(regular && same, "Fixture CA ownership changed")?;
                    io(std::fs::remove_file(&path), "Cannot remove the fixture CA")?;
                    self.commands.run(plan::update_trust()).await.map(|_| ())
                };
                if let Err(fault) = removed.await {
                    failures.push(fault.to_string());
                }
            }
            None => {}
        }
        self.report.cleanup_passed = failures.is_empty();
        self.report.cleanup_failures = failures;
    }
}
