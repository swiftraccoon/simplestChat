//! The relay variant of the app-only deployment over real files and flock
//! ownership: the selected release is restarted with exactly the relay's
//! three settings, or nothing is stopped. External daemon completions are
//! finite synthetic observations on a virtual clock; these tests establish
//! no Docker, signature or native pipe execution proof.

use super::maintenance_effect_tests::{
    DUMP, OLD_REVISION, REVISION, asset_tar, digest, id, lock_available, migration_rows, old_image,
    write,
};
use super::*;
use std::{
    fs::{self, File},
    io::{self, Read},
    net::IpAddr,
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
};

const NAME: &str = "fixture.test";
const PUBLIC: &str = "https://fixture.test/ready";
const LOOPBACK: &str = "http://127.0.0.1:3000/ready";
const TOKEN: &str = "fixture-metrics-token-of-forty-one-chars";

/// What the synthetic host does differently from a healthy one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// The candidate environment changes a setting nobody reviewed.
    Unreviewed,
    /// The rendered application lacks one of the three settings.
    Untold,
    /// The restarted application never answers at its public origin.
    Unready,
}

/// One activation: the host it meets and what it is asked.
struct Case {
    fault: Fault,
    /// A line the application's environment already ends with.
    line: &'static str,
    /// How many services of the project name `app.env` as their own file.
    listed: usize,
    /// Whether another release was selected since the caller looked.
    other: bool,
    relay: Option<Relay>,
    /// How many times the application reports rooms before none.
    busy: usize,
}

fn relay(address: Option<&str>) -> Relay {
    Relay {
        domain: NAME.into(),
        secret: "c".repeat(64),
        address: address.map(|address| address.parse::<IpAddr>().unwrap()),
    }
}

impl Default for Case {
    fn default() -> Self {
        Self {
            fault: Fault::None,
            line: "",
            listed: 1,
            other: false,
            relay: Some(relay(None)),
            busy: 0,
        }
    }
}

/// The three lines a restart appends, written out.
fn told() -> String {
    format!(
        concat!(
            "TURN_URLS=turn:{0}:3478?transport=udp,turn:{0}:3478?transport=tcp,",
            "turns:{0}:5349?transport=tcp\nTURN_SECRET={1}\nTURN_TTL=86400\n"
        ),
        NAME,
        "c".repeat(64)
    )
}

struct Prepared {
    data: effect_tests::Data,
    image: String,
    migrations: BTreeMap<String, String>,
    environment: String,
    compose: String,
}

fn prepared(case: &Case) -> Prepared {
    let data = effect_tests::Data::new();
    let release = data.access.root.join("releases").join(REVISION);
    let manifest: Value =
        serde_json::from_slice(&fs::read(release.join("release.json")).unwrap()).unwrap();
    let migrations = manifest["migrations"].as_object().unwrap().iter();
    let migrations = migrations
        .map(|(version, checksum)| (version.clone(), checksum.as_str().unwrap().to_owned()))
        .collect();
    // The image is the one the retained archive holds.
    let mut archive = tar::Archive::new(File::open(release.join("image.tar")).unwrap());
    let mut config = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap() == Path::new("config.json") {
            entry.read_to_end(&mut config).unwrap();
        }
    }
    nix::unistd::close(archive.into_inner()).unwrap();
    let image = format!("sha256:{}", digest(&config));
    let (selected, revision) = if case.other {
        (old_image(), OLD_REVISION)
    } else {
        (image.clone(), REVISION)
    };
    let environment = format!(
        "SIMPLESTCHAT_IMAGE={selected}\nRUN_MIGRATIONS=false\n\
         WEBAUTHN_ORIGIN=https://{NAME}\nMETRICS_TOKEN={TOKEN}\n{}",
        case.line
    );
    let own = "    env_file:\n      - ./app.env\n";
    let [application, migration] = [0, 1].map(|at| if case.listed > at { own } else { "" });
    let compose = format!(
        "services:\n  simplestchat:\n    image: \"{selected}\"\n{application}  migrate:\n    \
         image: \"{selected}\"\n{migration}  postgres: {{}}\n  caddy: {{}}\n"
    );
    let config = &data.access.config;
    write(&config.join("app.env"), environment.as_bytes(), 0o600).unwrap();
    write(
        &config.join("compose.public.yml"),
        compose.as_bytes(),
        0o600,
    )
    .unwrap();
    let images = json!({"revision": revision, "serverImage": selected});
    let images = serde_json::to_vec(&images).unwrap();
    write(&config.join("images.json"), &images, 0o600).unwrap();
    let caddy = b"synthetic protected proxy\n";
    write(&config.join("Caddyfile"), caddy, 0o644).unwrap();
    let receipt = json!({"schemaVersion": 1, "caddyfileSha256": digest(caddy)});
    let receipt = serde_json::to_vec(&receipt).unwrap();
    write(
        &config.join("browser-assets-installed.json"),
        &receipt,
        0o600,
    )
    .unwrap();
    Prepared {
        data,
        image,
        migrations,
        environment,
        compose,
    }
}

struct Daemon {
    root: PathBuf,
    config: PathBuf,
    work: PathBuf,
    docker: PathBuf,
    curl: PathBuf,
    image: String,
    migrations: BTreeMap<String, String>,
    fault: Fault,
    busy: usize,
    commands: Vec<(Role, Vec<String>)>,
    exact: bool,
    polls: usize,
    stops: usize,
    ups: usize,
    dump_inode: Option<(u64, u64)>,
    backup_before_stop: bool,
    validator_live: bool,
    asset_live: bool,
}

impl Daemon {
    /// The project as Compose resolves the file it is given: the image that
    /// file selects, and the environment of the file it names as the
    /// application's own.
    fn rendered(&self, arguments: &[String]) -> io::Result<Vec<u8>> {
        let at = arguments.iter().position(|word| word == "-f");
        let at = at.ok_or_else(|| io::Error::other("missing fixed Compose file"))?;
        let file = Path::new(&arguments[at + 1]);
        let project = fs::read_to_string(file)?;
        let image = project.split("image: \"").nth(1);
        let image = image.and_then(|rest| rest.split('"').next());
        let image = image.ok_or_else(|| io::Error::other("fixture image"))?;
        let candidate = project.contains("candidate.env\"");
        let own = if candidate {
            file.parent().unwrap().join("candidate.env")
        } else {
            self.config.join("app.env")
        };
        let mut environment = BTreeMap::<String, String>::new();
        let text = fs::read_to_string(own)?;
        for line in text.lines().filter(|line| line.contains('=')) {
            let (name, value) = line.split_once('=').unwrap();
            environment.insert(name.into(), value.into());
        }
        if candidate && self.fault == Fault::Unreviewed {
            environment.insert("MAX_CONNECTIONS".into(), "1".into());
        }
        if candidate && self.fault == Fault::Untold {
            environment.remove("TURN_TTL");
        }
        let service = |migrate: bool| {
            let mut environment = if migrate {
                BTreeMap::new()
            } else {
                environment.clone()
            };
            let migrations = if migrate { "true" } else { "false" };
            environment.insert("RUN_MIGRATIONS".into(), migrations.into());
            json!({"image":image,"user":"10001:10001","working_dir":"/app",
                "read_only":true,"init":true,"cap_drop":["ALL"],
                "security_opt":["no-new-privileges:true"],"environment":environment,
                "volumes":[{"type":"bind","source":self.root.join("postgres-socket"),
                    "target":"/run/simplestchat-postgres","read_only":true,
                    "bind":{"create_host_path":true}}],
                "tmpfs":[if migrate {"/tmp:rw,nosuid,nodev,noexec,size=32m,mode=1777"}
                    else {"/tmp:rw,nosuid,nodev,noexec,size=64m,mode=1777"}]})
        };
        let resolved = json!({"services":{"simplestchat":service(false),"migrate":service(true),
            "postgres":{"image":"fixture-postgres"},"caddy":{"volumes":[{
                "type":"bind","source":self.root.join("browser-assets"),
                "target":"/run/simplestchat-browser-assets","read_only":true,
                "bind":{"create_host_path":true}}]}}});
        serde_json::to_vec(&resolved).map_err(io::Error::other)
    }

    /// The image the project on disk selects, which is what runs.
    fn selected(&self) -> String {
        let images = fs::read(self.config.join("images.json")).unwrap();
        let images: Value = serde_json::from_slice(&images).unwrap();
        images["serverImage"].as_str().unwrap().to_owned()
    }

    fn container(&self, value: &str) -> Value {
        let (service, image) = if value == id('1') {
            ("simplestchat", self.selected())
        } else if value == id('2') {
            ("postgres", "sha256:fixture-postgres".into())
        } else {
            ("caddy", "sha256:fixture-proxy".into())
        };
        json!({"id":value,"image":image,"configHash":format!("hash-{service}"),
            "state":{"Running":true,"OOMKilled":false,"StartedAt":"fixture-start",
                "Health":{"Status":"healthy"}},"restarts":0})
    }

    /// Whether the first stop finds the listed backup and its receipt
    /// retained under an unfinished replacement.
    fn backed_up(&self) -> bool {
        let journal = fs::read(self.root.join("release-state.json")).ok();
        let journal = journal.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        journal.is_some_and(|journal| {
            journal["attempt"].as_str().is_some_and(|attempt| {
                let attempt = Path::new(attempt);
                fs::read(attempt.join("database-before.dump")).is_ok_and(|bytes| bytes == DUMP)
                    && attempt.join("database-before.receipt.json").exists()
                    && journal["phase"] == "replace_application"
                    && journal["finalized"] == false
            })
        })
    }
}

impl Executor for Daemon {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        let arguments = request.arguments.iter();
        let mut arguments: Vec<String> = arguments
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        self.commands.push((request.role, arguments.clone()));
        let held = lock_available(&self.work.join("workload.lock"));
        self.exact &= request.body_end > owner.now()
            && request.body_end < request.settlement_end
            && held.is_ok_and(|available| !available);
        let (mut stdout, mut error, mut bulk) = (Vec::new(), None, None);
        let curl = matches!(request.role, Role::Ready | Role::Metrics);
        let program = if curl { &self.curl } else { &self.docker };
        self.exact &= request.program == *program;
        if !curl {
            let socket = ["--host", "unix:///var/run/docker.sock"];
            if arguments.len() < 2 || arguments[..2] != socket {
                error = Some(Cause::Input);
            } else {
                drop(arguments.drain(..2));
            }
        }
        let has = |word: &str| arguments.iter().any(|value| value == word);
        let first = arguments.first().map(String::as_str).unwrap_or("");
        let last = arguments.last().map(String::as_str).unwrap_or("");
        let services = ["simplestchat", "postgres", "caddy"];
        match request.role {
            Role::Load if first == "image" && has("load") => {}
            Role::Inspect if first == "image" && has("inspect") => {
                // The retained release by its tag or its identity; the other
                // release only by its identity.
                let (image, revision) = if last == old_image() {
                    (old_image(), OLD_REVISION)
                } else {
                    (self.image.clone(), REVISION)
                };
                stdout = serde_json::to_vec(&json!({"id":image,
                    "os":"linux","architecture":"amd64","user":"10001:10001",
                    "labels":{"org.opencontainers.image.revision":revision},
                    "cmd":["/app/simplestChat"],"entrypoint":null,"workingDir":"/app",
                    "hasHealthcheck":false,"envNames":["PATH"]}))
                .unwrap();
            }
            Role::Inspect
                if first == "inspect"
                    && [id('1'), id('2'), id('3')].iter().any(|id| id == last) =>
            {
                stdout = serde_json::to_vec(&self.container(last)).unwrap();
            }
            Role::Create
                if first == "create"
                    && has("none")
                    && has("--read-only")
                    && last.contains("sha384sum") =>
            {
                self.exact &= !self.validator_live;
                self.validator_live = true;
                stdout = format!("{}\n", id('5')).into_bytes();
            }
            Role::Start if arguments == ["start".to_owned(), id('5')] => {}
            Role::Wait if arguments == ["wait".to_owned(), id('5')] => stdout = b"0\n".to_vec(),
            Role::Logs if arguments == ["logs".to_owned(), id('5')] => {
                stdout = migration_rows(&self.migrations, true);
            }
            Role::Remove if first == "rm" && last == id('5') => {
                self.exact &= self.validator_live;
                self.validator_live = false;
            }
            Role::Compose if has("ps") && last == "migrate" => {}
            Role::Compose if has("ps") && services.contains(&last) => {
                let value = match last {
                    "simplestchat" => id('1'),
                    "postgres" => id('2'),
                    _ => id('3'),
                };
                stdout = format!("{value}\n").into_bytes();
            }
            Role::Compose if has("logs") && last == "simplestchat" => {}
            Role::Compose if has("config") && has("--hash") => {
                stdout = format!("{last} hash-{last}\n").into_bytes();
            }
            Role::Compose if has("config") && has("--format") && last == "json" => {
                match self.rendered(&arguments) {
                    Ok(bytes) => stdout = bytes,
                    Err(_) => error = Some(Cause::Input),
                }
            }
            Role::Query if last == LEDGER => stdout = migration_rows(&self.migrations, false),
            Role::AssetCreate
                if first == "create" && has("none") && has("--read-only") && last == "exit 0" =>
            {
                self.exact &= !self.asset_live;
                self.asset_live = true;
                stdout = format!("{}\n", id('6')).into_bytes();
            }
            Role::AssetCopy
                if arguments
                    == [
                        "cp".to_owned(),
                        format!("{}:/app/web/dist/assets", id('6')),
                        "-".into(),
                    ] =>
            {
                self.exact &= self.asset_live;
                bulk = Some(asset_tar());
            }
            Role::AssetRemove if arguments == ["rm".to_owned(), id('6')] => {
                self.exact &= self.asset_live;
                self.asset_live = false;
            }
            Role::Size if last == "SELECT pg_database_size(current_database())" => {
                stdout = b"1\n".to_vec()
            }
            Role::Dump if last == "custom" && has("pg_dump") => bulk = Some(DUMP.to_vec()),
            Role::List if last == "--list" && has("pg_restore") => {
                let mut bytes = vec![0; DUMP.len()];
                self.exact &= request.input.is_some_and(|file| {
                    file.read_exact_at(&mut bytes, 0).is_ok()
                        && bytes == DUMP
                        && file.metadata().ok().map(|m| (m.dev(), m.ino())) == self.dump_inode
                });
                stdout = b"synthetic completed archive listing\n".to_vec();
            }
            Role::Ready if [PUBLIC, LOOPBACK].contains(&last) && has("--disable") => {
                // Only the first restart, and only at the public origin.
                let restarted = self.ups == 1 && self.stops == 1;
                let unready = self.fault == Fault::Unready && restarted && last == PUBLIC;
                let said = if unready { "starting" } else { "ready" };
                stdout = format!("{{\"status\":\"{said}\"}}\n").into_bytes();
            }
            Role::Metrics if last == "http://127.0.0.1:3000/metrics" && has("--config") => {
                self.polls += 1;
                let rooms = if self.polls <= self.busy { 2 } else { 0 };
                stdout = format!("# TYPE gauge\nsimplestchat_rooms_active {rooms}\n").into_bytes();
            }
            Role::Stop if has("stop") && last == "simplestchat" && has("30") => {
                self.stops += 1;
                if self.stops == 1 {
                    self.backup_before_stop = self.backed_up();
                    self.exact &= self.backup_before_stop;
                }
            }
            Role::Up if has("up") && last == "simplestchat" && has("--no-deps") => self.ups += 1,
            _ => error = Some(Cause::Input),
        }
        let mut bulk_bytes = 0;
        let mut bulk_sha256 = digest(b"");
        if let Some(bytes) = bulk {
            let written = request.output.filter(|output| {
                bytes.len() as u64 <= output.maximum && output.file.write_all_at(&bytes, 0).is_ok()
            });
            if let Some(output) = written {
                bulk_bytes = bytes.len() as u64;
                bulk_sha256 = digest(&bytes);
                if request.role == Role::Dump {
                    self.dump_inode = output.file.metadata().ok().map(|m| (m.dev(), m.ino()));
                }
            } else {
                error = Some(Cause::Io);
            }
        }
        if let Some(cause) = error {
            owner.record(cause);
            self.exact = false;
        }
        CommandOutput {
            stdout,
            stderr: Vec::new(),
            status: Some(0),
            error,
            settled: true,
            stdout_complete: true,
            bulk_bytes,
            bulk_sha256,
            bulk_complete: error.is_none(),
        }
    }
}

/// What one activation left.
struct Observed {
    report: Value,
    passed: bool,
    finished: Result<()>,
    exact: bool,
    commands: Vec<(Role, Vec<String>)>,
    polls: usize,
    /// The application's environment and project before and after.
    environment: [String; 2],
    compose: [String; 2],
    selected: Value,
    journal: Option<Value>,
    /// The retained candidate environment and project, when one was written.
    candidate: Option<[String; 2]>,
    retained: Option<String>,
    lock_available: bool,
    closes: [(usize, usize); 2],
}

impl Observed {
    /// The program arguments of every command of `role`, in order.
    fn of(&self, role: Role) -> Vec<&[String]> {
        let commands = self.commands.iter();
        let commands = commands.filter(|(issued, _)| *issued == role);
        commands
            .map(|(_, arguments)| arguments.as_slice())
            .collect()
    }

    /// Where each readiness probe went and, when it was told one, the
    /// address it was sent to instead of the name's own.
    fn probes(&self) -> Vec<(&str, Option<&str>)> {
        let probes = self.of(Role::Ready).into_iter();
        let probes = probes.map(|arguments| {
            let at = arguments.iter().position(|word| word == "--resolve");
            let destination = at.map(|at| arguments[at + 1].as_str());
            (arguments.last().unwrap().as_str(), destination)
        });
        probes.collect()
    }

    /// Whether anything stopped or started the application.
    fn interrupted(&self) -> bool {
        !self.of(Role::Stop).is_empty() || !self.of(Role::Up).is_empty()
    }
}

fn operation(case: Case) -> Observed {
    let mut fixture = prepared(&case);
    fixture.data.access.virtual_poll = true;
    let access = &fixture.data.access;
    let mut daemon = Daemon {
        root: access.root.clone(),
        config: access.config.clone(),
        work: access.work.clone(),
        docker: access.docker.clone(),
        curl: access.curl.clone(),
        image: fixture.image.clone(),
        migrations: fixture.migrations.clone(),
        fault: case.fault,
        busy: case.busy,
        commands: Vec::new(),
        exact: true,
        polls: 0,
        stops: 0,
        ups: 0,
        dump_inode: None,
        backup_before_stop: false,
        validator_live: false,
        asset_live: false,
    };
    let options = match case.relay {
        Some(relay) => Options::relayed(REVISION, relay),
        None => Options {
            revision: REVISION.into(),
            quiet_seconds: 0,
            whole: Duration::from_secs(1800),
            reserve: Duration::from_secs(60),
            relay: None,
        },
    };
    let started = Instant::now();
    let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
    // Time passes only where the operation waits.
    owner.elapsed = Some(Duration::ZERO);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    super::super::super::files::start_probe(None);
    super::super::files::start_probe();
    let outcome = runtime.block_on(execute(&options, access, &mut owner, &mut daemon, started));
    drop(runtime);
    let report: Value = serde_json::from_slice(&outcome.bytes).unwrap();
    let text = |name: &str| fs::read_to_string(access.config.join(name)).unwrap();
    let journal = fs::read(access.root.join("release-state.json")).ok();
    let journal = journal.map(|bytes| serde_json::from_slice(&bytes).unwrap());
    let attempt = report["evidence"].as_str().map(PathBuf::from);
    let kept = |name: &str| {
        attempt
            .as_ref()
            .map(|attempt| fs::read_to_string(attempt.join(name)))
    };
    let candidate = match (kept("candidate.env"), kept("candidate-compose.yml")) {
        (Some(Ok(environment)), Some(Ok(compose))) => Some([environment, compose]),
        _ => None,
    };
    let retained = kept("before-app.env").and_then(std::result::Result::ok);
    let closes = [
        super::super::super::files::take_probe(),
        super::super::files::take_probe(),
    ];
    Observed {
        passed: outcome.passed,
        finished: owner.finish(),
        exact: daemon.exact,
        polls: daemon.polls,
        environment: [fixture.environment, text("app.env")],
        compose: [fixture.compose, text("compose.public.yml")],
        selected: serde_json::from_str(&text("images.json")).unwrap(),
        journal,
        candidate,
        retained,
        lock_available: lock_available(&access.work.join("workload.lock")).unwrap(),
        closes,
        commands: daemon.commands,
        report,
    }
}

/// Every command was one the host answers, the lock is free again and
/// every file was closed.
fn closed(observed: &Observed) {
    let commands = &observed.commands;
    assert!(observed.exact, "the host refused a command: {commands:?}");
    assert!(observed.lock_available);
    assert!(observed.closes.iter().all(|closes| closes.0 > 0));
    assert_eq!(observed.closes.map(|closes| closes.1), [0, 0]);
}

/// Nothing was stopped, started or changed, and the report says why.
fn refused(observed: &Observed, failure: &str) {
    closed(observed);
    assert!(!observed.passed, "{}", observed.report);
    assert_eq!(observed.report["failure"], failure);
    assert_eq!(observed.report["passed"], false);
    assert!(!observed.interrupted(), "{:?}", observed.commands);
    assert_eq!(observed.environment[0], observed.environment[1]);
    assert_eq!(observed.compose[0], observed.compose[1]);
    assert!(observed.report.get("rollbackAttempted").is_none());
}

/// The selected release is restarted once, after rooms were given their
/// time and the database was backed up, with exactly three lines more in
/// its environment and nothing else selected differently; every probe of
/// the public origin goes to the address the caller verified the relay at.
#[test]
fn the_selected_release_is_restarted_with_exactly_the_three_relay_settings() {
    let observed = operation(Case {
        relay: Some(relay(Some("192.0.2.7"))),
        busy: 3,
        ..Case::default()
    });
    closed(&observed);
    assert!(observed.passed, "{}", observed.report);
    assert_eq!(observed.finished, Ok(()));
    let report = &observed.report;
    assert_eq!(report["action"], "activate");
    assert_eq!(report["revision"], REVISION);
    assert_eq!(report["phase"], "complete");
    assert_eq!(report["quietWaitRequestedSeconds"], 600);
    // Three reports of rooms, five seconds apart, then none.
    assert_eq!(report["quietWaitSeconds"], 15.0);
    assert_eq!(observed.polls, 5);
    assert!(report.get("rollbackAttempted").is_none());
    let [before, after] = &observed.environment;
    assert_eq!(*after, format!("{before}{}", told()));
    assert_eq!(observed.compose[0], observed.compose[1]);
    assert_eq!(observed.selected["revision"], REVISION);
    assert_eq!(
        Some(observed.selected["serverImage"].as_str().unwrap()),
        before
            .lines()
            .next()
            .unwrap()
            .strip_prefix("SIMPLESTCHAT_IMAGE=")
    );
    // What was previewed is what was installed, read through a project
    // that names the candidate as the application's own file.
    let [candidate, project] = observed.candidate.as_ref().unwrap();
    assert_eq!(candidate, after);
    assert!(project.contains("/candidate.env\"\n") && !project.contains("./app.env"));
    assert_eq!(observed.retained.as_ref(), Some(before));
    assert_eq!(observed.of(Role::Stop).len(), 1);
    assert_eq!(observed.of(Role::Up).len(), 1);
    let order = observed.commands.iter().map(|(role, _)| *role);
    let order: Vec<Role> = order
        .filter(|role| matches!(role, Role::Stop | Role::Up))
        .collect();
    assert_eq!(order, [Role::Stop, Role::Up]);
    let destination = Some("fixture.test:443:192.0.2.7");
    assert_eq!(
        observed.probes(),
        [
            (LOOPBACK, None),
            (PUBLIC, destination),
            (LOOPBACK, None),
            (PUBLIC, destination)
        ]
    );
    let journal = observed.journal.as_ref().unwrap();
    assert_eq!(
        (&journal["phase"], &journal["finalized"]),
        (&json!("complete"), &json!(true))
    );
}

/// An address of the other family is bracketed, and without an override no
/// probe is sent anywhere but where the name resolves.
#[test]
fn only_an_address_override_redirects_the_public_probes() {
    let observed = operation(Case {
        relay: Some(relay(Some("2001:db8::7"))),
        ..Case::default()
    });
    closed(&observed);
    assert!(observed.passed, "{}", observed.report);
    let destinations: Vec<Option<&str>> =
        observed.probes().into_iter().map(|probe| probe.1).collect();
    let destination = Some("fixture.test:443:[2001:db8::7]");
    assert_eq!(destinations, [None, destination, None, destination]);
    let observed = operation(Case::default());
    closed(&observed);
    assert!(observed.passed, "{}", observed.report);
    assert!(observed.probes().iter().all(|probe| probe.1.is_none()));
    // An application that reports no rooms is not waited for.
    assert_eq!(observed.report["quietWaitSeconds"], 0.0);
}

/// Rooms that never empty are waited for ten minutes and no longer; the
/// restart then proceeds.
#[test]
fn rooms_are_given_ten_minutes_to_empty() {
    let observed = operation(Case {
        busy: usize::MAX,
        ..Case::default()
    });
    closed(&observed);
    assert!(observed.passed, "{}", observed.report);
    assert_eq!(observed.report["quietWaitSeconds"], 600.0);
    assert_eq!(observed.of(Role::Stop).len(), 1);
}

/// A setting that is already there is never rotated by a restart, whatever
/// it says, and a project that does not name `app.env` exactly once as the
/// application's own file cannot be previewed.
#[test]
fn an_environment_that_has_a_relay_line_or_is_not_the_applications_own_is_refused() {
    for line in ["TURN_TTL=86400\n", "TURN_SECRET=\n", "TURN_URLS\n"] {
        let observed = operation(Case {
            line,
            ..Case::default()
        });
        let failure = "TURN is already configured; rotation requires separate maintenance";
        refused(&observed, failure);
        // The candidate that was retained says nothing of the relay.
        let [candidate, _] = observed.candidate.as_ref().unwrap();
        assert_eq!(*candidate, observed.environment[0]);
    }
    for listed in [0, 2] {
        let observed = operation(Case {
            listed,
            ..Case::default()
        });
        refused(&observed, "Unexpected application env_file selection");
    }
}

/// The preview must say exactly what the running project says and the
/// three settings: one setting more, or one of the three missing, refuses.
#[test]
fn a_preview_that_differs_by_more_or_less_than_the_three_settings_is_refused() {
    for fault in [Fault::Unreviewed, Fault::Untold] {
        let observed = operation(Case {
            fault,
            ..Case::default()
        });
        let failure = "Candidate changes configuration beyond the reviewed selection";
        refused(&observed, failure);
    }
}

/// The restart is of the release that is selected when the operation holds
/// the lock, never of the one the caller saw before; and a hostname or a
/// secret that could carry anything else into the environment is refused.
#[test]
fn another_selected_release_or_an_unsafe_relay_is_refused() {
    let observed = operation(Case {
        other: true,
        ..Case::default()
    });
    refused(
        &observed,
        "Relay activation restarts only the selected release",
    );
    let unsafe_relays = [
        Relay {
            domain: "Fixture.test".into(),
            ..relay(None)
        },
        Relay {
            domain: "fixture.test\nRUN_MIGRATIONS=true".into(),
            ..relay(None)
        },
        Relay {
            secret: "c".repeat(63),
            ..relay(None)
        },
        Relay {
            secret: format!("{}\nX=1", "c".repeat(60)),
            ..relay(None)
        },
    ];
    for relay in unsafe_relays {
        let observed = operation(Case {
            relay: Some(relay),
            ..Case::default()
        });
        refused(&observed, "Invalid managed TURN configuration");
    }
}

/// Without a relay the selected image is still never deployed again.
#[test]
fn an_ordinary_deployment_of_the_selected_image_is_still_refused() {
    let observed = operation(Case {
        relay: None,
        ..Case::default()
    });
    refused(&observed, "This image is already selected");
    assert_eq!(observed.report["action"], "deploy");
}

/// A restarted application that never answers at its public origin is
/// rolled back: the environment is the one from before, without the relay,
/// and the probes of the rollback go to the caller's address too.
#[test]
fn a_restart_that_is_not_ready_is_rolled_back_to_the_environment_without_the_relay() {
    let observed = operation(Case {
        fault: Fault::Unready,
        relay: Some(relay(Some("192.0.2.7"))),
        ..Case::default()
    });
    closed(&observed);
    assert!(!observed.passed, "{}", observed.report);
    let report = &observed.report;
    assert_eq!(report["failure"], "Application readiness deadline exceeded");
    assert_eq!(report["rollbackAttempted"], true);
    assert_eq!(report["rollbackPassed"], true, "{report}");
    assert_eq!(observed.environment[0], observed.environment[1]);
    assert_eq!(observed.compose[0], observed.compose[1]);
    assert_eq!(observed.selected["revision"], REVISION);
    assert_eq!(observed.of(Role::Stop).len(), 2);
    assert_eq!(observed.of(Role::Up).len(), 2);
    let probes = observed.probes();
    let public: Vec<_> = probes.iter().filter(|probe| probe.0 == PUBLIC).collect();
    let destination = Some("fixture.test:443:192.0.2.7");
    assert!(public.len() > 3 && public.iter().all(|probe| probe.1 == destination));
    assert!(
        probes
            .iter()
            .all(|probe| probe.0 == PUBLIC || probe.1.is_none())
    );
    let journal = observed.journal.as_ref().unwrap();
    assert_eq!(
        (&journal["phase"], &journal["finalized"]),
        (&json!("rolled_back"), &json!(true))
    );
}
