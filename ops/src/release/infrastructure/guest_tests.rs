//! The guest actions over an owned scratch tree laid out as the provisioned
//! guest is, with scripted commands. Every owner role is this test process's
//! own uid and gid; the commands are answered by fixed fakes, not a daemon.

use super::*;
use crate::host::command::Request;
use crate::host::command::tests::Scripted;
use crate::host::state::tests::Scratch;
use crate::host::tests::{Fixed, paths};
use crate::release::assets::Takeover;
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;

const RUN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTAINER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const APPLICATION: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const MIGRATOR: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const RESTORE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const REPLACED: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const VERIFY_SQL: &[u8] = b"SELECT counts;\n";
const COUNTS: &[u8] = br#"{"users":1,"rooms":1,"sessions":0,"credentials":0,"activeIncidents":1,"resolvedIncidents":1}"#;

fn revision() -> String {
    "b".repeat(40)
}
fn server() -> String {
    format!("sha256:{}", "d".repeat(64))
}
fn database_image() -> String {
    format!("sha256:{}", "7".repeat(64))
}
fn checksum() -> String {
    "4".repeat(96)
}

fn write(path: &Path, bytes: &[u8], mode: u32) {
    let _ = std::fs::remove_file(path);
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn directory(path: &Path, mode: u32) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn layout(base: &Path, socket: &Path) -> Layout {
    Layout {
        fixture: base.join("fixture"),
        marker: base.join("marker"),
        library: base.join("library"),
        executable: base.join("exe"),
        socket: socket.to_owned(),
        vendor: base.join("sys_vendor"),
        os_release: [base.join("os-release"), base.join("usr-os-release")],
        config: base.join("etc/simplestchat-public"),
        root: base.join("srv/simplestchat-public"),
        units: base.join("units"),
        launcher: base.join("launcher"),
    }
}

fn identity() -> Identity {
    let ids = (
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    );
    Identity {
        root: ids,
        postgres: ids,
        caddy: ids,
        euid: ids.0,
        system: "Linux".into(),
        machine: "x86_64".into(),
    }
}

/// A scratch guest as the playbooks leave it before the first snapshot. Its
/// Docker socket lives at a short path of its own: a socket's path is bounded.
struct Tree {
    scratch: Scratch,
    socket: PathBuf,
    _listener: std::os::unix::net::UnixListener,
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn socket_path() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = Path::new("/tmp").canonicalize().unwrap();
    directory.join(format!("scg-{}-{next}.sock", std::process::id()))
}

impl Tree {
    fn new() -> Self {
        let scratch = Scratch::new();
        let base = scratch.path().to_owned();
        let socket = socket_path();
        let layout = layout(&base, &socket);
        for name in ["srv", "etc", "run"] {
            directory(&base.join(name), 0o755);
        }
        directory(&layout.fixture, 0o700);
        write(&layout.marker, format!("{RUN}\n").as_bytes(), 0o600);
        let selection = json!({"schemaVersion":1,"runId":RUN,"revision":revision()});
        let selected = layout.fixture.join("selection.json");
        write(&selected, selection.to_string().as_bytes(), 0o600);
        directory(&layout.library, 0o755);
        write(&layout.library.join("simplestchat-ops"), b"\x7fELF", 0o755);
        let installed = layout.library.join("simplestchat-ops");
        std::os::unix::fs::symlink(installed, &layout.executable).unwrap();
        write(
            &layout.library.join("monitoring-schema.sql"),
            b"SELECT 1;\n",
            0o644,
        );
        write(
            &layout.library.join("restore-verify.sql"),
            VERIFY_SQL,
            0o644,
        );
        write(
            &layout.library.join("backup-nightly.sh"),
            b"#!/bin/sh\n",
            0o700,
        );
        let listener = std::os::unix::net::UnixListener::bind(&layout.socket).unwrap();
        let mode = std::fs::Permissions::from_mode(0o660);
        std::fs::set_permissions(&layout.socket, mode).unwrap();
        write(&layout.vendor, b"QEMU\n", 0o444);
        write(
            &layout.os_release[0],
            b"ID=debian\nVERSION_ID=\"13\"\n",
            0o644,
        );
        directory(&layout.config, 0o700);
        directory(&layout.root, 0o700);
        for name in [
            "results",
            "backups",
            "postgres",
            "caddy-data",
            "caddy-config",
        ] {
            directory(&layout.root.join(name), 0o700);
        }
        directory(&layout.root.join("postgres-socket"), 0o755);
        directory(&layout.root.join("backups/nightly"), 0o700);
        let names = policy::SECRET_NAMES.iter().enumerate();
        let secrets: BTreeMap<&str, String> = names
            .map(|(index, name)| (*name, format!("{:x}", index + 1).repeat(64)))
            .collect();
        let url = |role: &str, password: &str| {
            format!(
                "postgres://simplestchat_{role}:{password}@localhost/simplestchat\
                 ?host=/run/simplestchat-postgres&sslmode=disable"
            )
        };
        let application = format!(
            "ALLOWED_ORIGINS=https://vm-fixture.test\nWEBAUTHN_RP_ID=vm-fixture.test\n\
             WEBAUTHN_ORIGIN=https://vm-fixture.test\nANNOUNCE_IP=127.0.0.1\nANNOUNCE_IPV6=\n\
             RUN_MIGRATIONS=false\nREGISTRATION_ENABLED=false\nALLOW_AD_HOC_ROOMS=false\n\
             MEDIA_WORKERS=1\nBIND_ADDR=0.0.0.0\nPORT=3000\nMETRICS_TOKEN={}\n\
             TRUSTED_PROXY_SECRET={}\nDATABASE_URL={}\n",
            secrets["metrics_token"],
            secrets["proxy_secret"],
            url("app", &secrets["database_password"]),
        );
        let migration = format!(
            "DATABASE_URL={}\nRUN_MIGRATIONS=true\nBIND_ADDR=127.0.0.1\nPORT=3000\n\
             ANNOUNCE_IP=127.0.0.1\nMEDIA_WORKERS=1\nREGISTRATION_ENABLED=false\n\
             ALLOW_AD_HOC_ROOMS=false\nRUST_LOG=simplestchat=info,mediasoup=warn\n",
            url("migrate", &secrets["migration_password"]),
        );
        let proxy = format!(
            "CADDY_DOMAIN=vm-fixture.test\nCADDY_UPSTREAM=simplestchat:3000\n\
             TRUSTED_PROXY_SECRET={}\n",
            secrets["proxy_secret"],
        );
        let postgres = format!("docker.io/library/postgres:18.1@sha256:{}", "8".repeat(64));
        let images = json!({"revision":revision(),"serverImage":server(),
            "postgresImage":postgres,"caddyImage":"caddy"});
        for (name, _, mode) in policy::FILES {
            let content = match name {
                "secrets.json" => json!(secrets).to_string(),
                "app.env" => application.clone(),
                "migration.env" => migration.clone(),
                "proxy.env" => proxy.clone(),
                "postgres-admin-password" => format!("{}\n", secrets["postgres_admin_password"]),
                "images.json" => images.to_string(),
                other => format!("{other}\n"),
            };
            write(&layout.config.join(name), content.as_bytes(), mode);
        }
        directory(&layout.units, 0o755);
        for name in [
            "simplestchat-benchmark.service",
            "simplestchat-image-build.service",
            "simplestchat-backup.service",
            "simplestchat-backup.timer",
        ] {
            write(&layout.units.join(name), b"[Unit]\n", 0o644);
        }
        write(&layout.launcher, b"#!/bin/sh\n", 0o755);
        let release = layout.root.join("releases").join(revision());
        directory(&release, 0o700);
        let manifest = json!({"schemaVersion":1,"revision":revision(),"platform":"linux/amd64",
            "archiveSha256":"9".repeat(64),
            "imageTag":format!("simplestchat-release/production:{}", revision()),
            "migrations":{"1":checksum()},"createdAt":"2026-11-21T01:02:03Z"})
        .to_string();
        write(&release.join("release.json"), manifest.as_bytes(), 0o600);
        let staged = json!({"schemaVersion":1,"revision":revision(),"serverImage":server(),
            "manifestSha256":model::sha256(manifest.as_bytes()),
            "stagedAt":"2026-11-21T01:02:03Z"});
        write(
            &release.join("staged.json"),
            staged.to_string().as_bytes(),
            0o600,
        );
        Self {
            scratch,
            socket: layout.socket,
            _listener: listener,
        }
    }

    fn base(&self) -> &Path {
        self.scratch.path()
    }

    fn layout(&self) -> Layout {
        layout(self.base(), &self.socket)
    }
}

/// The scripted guest: systemd, the launcher, `curl` and the daemon, with
/// just the state the actions observe.
struct Machine {
    base: PathBuf,
    active: RefCell<Vec<&'static str>>,
    changed: RefCell<bool>,
    migrator: RefCell<bool>,
    restore: RefCell<String>,
    /// One injected change of the guest: `replaced` (another migrator by
    /// the time it is removed), `image` (its image changed by then), `left`
    /// (still listed after removal) or `backup-failed`.
    fault: Option<&'static str>,
    listings: RefCell<usize>,
    inspections: RefCell<usize>,
}

fn has(words: &[&str], wanted: &[&str]) -> bool {
    words.windows(wanted.len()).any(|window| window == wanted)
}

fn container(id: &str, service: &str, image: &str, user: &str, network: &str) -> Vec<u8> {
    json!({"id":id,"name":format!("/simplestchat-public-{service}-1"),"image":image,"user":user,
        "labels":{"com.docker.compose.project":"simplestchat-public",
            "com.docker.compose.service":service},
        "state":{"Running":true,"OOMKilled":false},
        "host":{"ReadonlyRootfs":true,"Privileged":false,"CapDrop":["ALL"],"CapAdd":null,
            "SecurityOpt":["no-new-privileges:true"],"Memory":1_073_741_824_u64,
            "PidsLimit":256,"NanoCpus":1_000_000_000_u64,"NetworkMode":network}})
    .to_string()
    .into_bytes()
}

fn compose_model(changed: bool) -> Vec<u8> {
    let service = |user: &str| {
        json!({"user":user,"read_only":true,"cap_drop":["ALL"],
            "security_opt":["no-new-privileges:true"],"mem_limit":"1073741824",
            "pids_limit":256,"cpus":1,"init":true})
    };
    let mut application = service("10001:10001");
    application["ports"] = json!([{"protocol":"tcp","target":3000,"published":"3000",
        "host_ip":"127.0.0.1"}]);
    let mut migrate = service("10001:10001");
    migrate["network_mode"] = json!("none");
    let mut database = service("999:999");
    database["network_mode"] = json!("none");
    database.as_object_mut().unwrap().remove("init");
    let mut proxy = service("10001:10001");
    proxy["cap_add"] = json!(["NET_BIND_SERVICE"]);
    if changed {
        proxy["pids_limit"] = json!(128);
    }
    json!({"name":"simplestchat-public","services":{"simplestchat":application,
        "migrate":migrate,"postgres":database,"caddy":proxy}})
    .to_string()
    .into_bytes()
}

impl Machine {
    fn new(base: &Path) -> Self {
        Self {
            base: base.to_owned(),
            active: RefCell::new(vec!["active", "inactive"]),
            changed: RefCell::new(false),
            migrator: RefCell::new(false),
            restore: RefCell::new(String::new()),
            fault: None,
            listings: RefCell::new(0),
            inspections: RefCell::new(0),
        }
    }

    fn faulty(base: &Path, fault: &'static str) -> Self {
        Self {
            fault: Some(fault),
            ..Self::new(base)
        }
    }

    fn unit(&self, unit: &str, names: &str) -> Vec<u8> {
        if names == "ActiveState" {
            let state = self.active.borrow_mut().remove(0);
            return format!("ActiveState={state}\n").into_bytes();
        }
        let mut value: BTreeMap<String, String> = [
            ("LoadState", "loaded".to_owned()),
            ("FragmentPath", format!("/etc/systemd/system/{unit}")),
            ("User", "root".into()),
            ("DynamicUser", "no".into()),
            ("UMask", "0077".into()),
            ("RuntimeDirectory", "simplestchat-bench".into()),
            ("RuntimeDirectoryMode", "0700".into()),
            ("RuntimeDirectoryPreserve", "yes".into()),
            ("ActiveState", "inactive".into()),
            ("UnitFileState", "static".into()),
            ("Result", "success".into()),
            ("ExecMainStatus", "0".into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
        if unit == "simplestchat-image-build.service" {
            value.insert("RuntimeDirectory".into(), String::new());
        }
        if unit == "simplestchat-backup.service" && self.fault == Some("backup-failed") {
            value.insert("Result".into(), "exit-code".into());
        }
        for (name, setting) in policy::BACKUP_SANDBOX {
            value.insert(name.into(), setting.into());
        }
        if unit == "simplestchat-backup.timer" {
            value = policy::BACKUP_TIMER
                .iter()
                .map(|(name, setting)| ((*name).to_owned(), (*setting).to_owned()))
                .collect();
        }
        let lines = names
            .split(',')
            .map(|name| format!("{name}={}\n", value[name]));
        lines.collect::<String>().into_bytes()
    }

    fn answer(&self, request: &Request<'_>) -> Result<Vec<u8>> {
        let words: Vec<&str> = request.argv.iter().map(String::as_str).collect();
        let output = if words[0] == "/usr/bin/systemctl" {
            match words[1] {
                "show" => self.unit(words[2], words[4].strip_prefix("--property=").unwrap()),
                "start" if words[2] == "simplestchat-backup.service" => {
                    self.backup();
                    Vec::new()
                }
                _ => Vec::new(),
            }
        } else if words[0].ends_with("launcher") {
            if has(&words, &["config", "--format", "json"]) {
                compose_model(*self.changed.borrow())
            } else {
                if has(&words, &["create"]) {
                    *self.migrator.borrow_mut() = true;
                }
                Vec::new()
            }
        } else if words[0] == "/usr/bin/curl" {
            br#"{"status":"ready"}"#.to_vec()
        } else {
            self.docker(&words[3..], request.input)
        };
        if let Some(path) = request.archive {
            std::fs::write(path, &output).unwrap();
        }
        Ok(output)
    }

    fn docker(&self, words: &[&str], input: Option<&[u8]>) -> Vec<u8> {
        let migrator = *self.migrator.borrow();
        match words {
            ["compose", .., "ps", "--all", "--quiet", "postgres"] => CONTAINER.into(),
            ["compose", .., "ps", "--all", "--quiet", "simplestchat"] => APPLICATION.into(),
            ["compose", .., "ps", "--all", "--quiet", "migrate"] if migrator => {
                *self.listings.borrow_mut() += 1;
                let replaced = self.fault == Some("replaced") && *self.listings.borrow() >= 3;
                if replaced { REPLACED } else { MIGRATOR }.into()
            }
            ["rm", id] if *id == MIGRATOR => {
                *self.migrator.borrow_mut() = self.fault == Some("left");
                Vec::new()
            }
            ["inspect", "--format", _, id] if *id == REPLACED => {
                container(REPLACED, "migrate", &server(), "10001:10001", "none")
            }
            ["inspect", "--format", _, id] if *id == CONTAINER => {
                container(CONTAINER, "postgres", &database_image(), "999:999", "none")
            }
            ["inspect", "--format", _, id] if *id == APPLICATION => container(
                APPLICATION,
                "simplestchat",
                &server(),
                "10001:10001",
                "bridge",
            ),
            ["inspect", "--format", _, id] if *id == MIGRATOR => {
                *self.inspections.borrow_mut() += 1;
                let changed = self.fault == Some("image") && *self.inspections.borrow() >= 3;
                let image = if changed { database_image() } else { server() };
                container(MIGRATOR, "migrate", &image, "10001:10001", "none")
            }
            ["inspect", "--format", _, id] if *id == RESTORE => {
                let name = self.restore.borrow().clone();
                json!({"id":RESTORE,"name":format!("/{name}"),"image":database_image(),
                    "owner":name})
                .to_string()
                .into_bytes()
            }
            ["image", "inspect", "--format", "{{.Id}}", _] => {
                format!("{}\n", database_image()).into_bytes()
            }
            ["image", "inspect", "--format", _, _] => json!({"id":server(),"os":"linux",
                "architecture":"amd64","user":"10001:10001",
                "labels":{"org.opencontainers.image.revision":revision()},
                "cmd":["/app/simplestChat"],"entrypoint":null,"workingDir":"/app",
                "hasHealthcheck":false,"envNames":["PATH"]})
            .to_string()
            .into_bytes(),
            ["exec", .., "--command", _] => format!("1 t {}\n", checksum()).into_bytes(),
            ["create", "--name", name, ..] => {
                *self.restore.borrow_mut() = (*name).to_owned();
                format!("{RESTORE}\n").into_bytes()
            }
            ["ps", "--all", "--quiet", "--no-trunc", ..] => {
                if self.restore.borrow().is_empty() {
                    Vec::new()
                } else {
                    RESTORE.into()
                }
            }
            ["rm", "--force", "--volumes", _] => {
                self.restore.borrow_mut().clear();
                Vec::new()
            }
            ["exec", ..] if input == Some(VERIFY_SQL) => COUNTS.to_vec(),
            ["exec", "-i", "--user", "999:999", _, "psql", ..]
                if input.is_some_and(|sql| sql.starts_with(b"SELECT version")) =>
            {
                format!("1|t|{}\n", checksum()).into_bytes()
            }
            _ => Vec::new(),
        }
    }

    /// The installed backup service: one new receipt and its archive.
    fn backup(&self) {
        let target = self.base.join("srv/simplestchat-public/backups/nightly");
        let dump = b"PGDMP fixture archive";
        write(&target.join("20261122T115959Z.dump"), dump, 0o600);
        let receipt = json!({"schemaVersion":1,"dump":"20261122T115959Z.dump",
            "bytes":dump.len(),"sha256":model::sha256(dump),
            "completedAt":"2026-11-22T11:59:59Z","postgresImage":database_image(),
            "backupMigrations":{"1":checksum()}});
        let path = target.join("20261122T115959Z.receipt.json");
        write(&path, receipt.to_string().as_bytes(), 0o600);
    }
}

/// Run `action` on `tree` with `machine`, returning the receipt, the status
/// and the commands in order.
fn act(tree: &Tree, machine: &Machine, action: Action) -> (Value, u8, Vec<Vec<String>>) {
    act_as(tree, machine, action, identity())
}

fn act_as(
    tree: &Tree,
    machine: &Machine,
    action: Action,
    identity: Identity,
) -> (Value, u8, Vec<Vec<String>>) {
    let commands = Scripted::new(|request: &Request<'_>| machine.answer(request));
    let clock = Fixed::at("2026-11-22T12:00:00Z");
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: paths(tree.base()),
        owner: identity.root.0,
    };
    let base = tree.base().to_owned();
    let owner = identity.root.0;
    let lock = move || Workload::fixture_for(&base, owner, Takeover::Nothing);
    let guest = Guest {
        host: &host,
        layout: tree.layout(),
        identity,
        lock: &lock,
    };
    let (report, status) = execute(&guest, action, RUN);
    let asked = commands
        .asked
        .borrow()
        .iter()
        .map(|asked| asked.argv.clone())
        .collect();
    (report, status, asked)
}

fn labels(report: &Value) -> [&str; 4] {
    ["phase", "context", "code", "exceptionClass"].map(|name| report[name].as_str().unwrap())
}

/// Two `public.yml` runs that leave the same configuration pass, the second
/// as idempotent; a changed Compose model between them is refused as changed
/// configuration, never as a pass.
#[test]
fn a_repeated_snapshot_passes_only_when_the_protected_configuration_is_unchanged() {
    let tree = Tree::new();
    let machine = Machine::new(tree.base());
    let (first, status, asked) = act(&tree, &machine, Action::Snapshot);
    assert_eq!(status, 0, "{first}");
    assert_eq!(
        first,
        json!({"schemaVersion":1,"runId":RUN,"action":"snapshot","passed":true,
            "configurationFiles":13,"idempotent":false})
    );
    let programs: Vec<&str> = asked.iter().map(|argv| argv[0].as_str()).collect();
    assert_eq!(programs.len(), 4);
    assert!(asked[0].ends_with(&[
        "--no-pager".into(),
        format!("--property={}", policy::UNIT_PROPERTIES.join(","))
    ]));
    assert!(has(
        &asked[2].iter().map(String::as_str).collect::<Vec<_>>(),
        &["ps", "--all", "--quiet"]
    ));
    let (second, status, _) = act(&tree, &machine, Action::Snapshot);
    assert_eq!(
        (status, &second["idempotent"]),
        (0, &json!(true)),
        "{second}"
    );
    let record = std::fs::read_to_string(tree.layout().fixture.join("snapshot.json")).unwrap();
    assert!(
        !record.contains(&"1".repeat(64)),
        "a secret reached the record"
    );

    let changed = Tree::new();
    let machine = Machine::new(changed.base());
    assert_eq!(act(&changed, &machine, Action::Snapshot).1, 0);
    *machine.changed.borrow_mut() = true;
    let (refused, status, _) = act(&changed, &machine, Action::Snapshot);
    assert_eq!(status, 1);
    assert_eq!(
        labels(&refused),
        [
            "snapshot",
            "unspecified",
            "configuration_changed",
            "release_error"
        ]
    );
    assert_eq!(refused["passed"], json!(false));
    assert_eq!(refused.as_object().unwrap().len(), 8);
}

/// Each guard refuses before any command runs and names its own slot.
#[test]
fn the_guard_refuses_another_host_before_any_command() {
    type Change = fn(&Tree, &mut Identity);
    let cases: [(&str, Change, [&str; 4]); 15] = [
        (
            "root",
            |_, id| id.euid += 1,
            ["guard", "unspecified", "root", "release_error"],
        ),
        (
            "platform",
            |_, id| id.machine = "aarch64".into(),
            ["guard", "unspecified", "platform", "release_error"],
        ),
        (
            "system",
            |_, id| id.system = "Darwin".into(),
            ["guard", "unspecified", "platform", "release_error"],
        ),
        (
            "distribution",
            |tree, _| {
                write(
                    &tree.layout().os_release[0],
                    b"ID=ubuntu\nVERSION_ID=13\n",
                    0o644,
                )
            },
            ["guard", "unspecified", "distribution", "release_error"],
        ),
        (
            "debian",
            |tree, _| {
                write(
                    &tree.layout().os_release[0],
                    b"ID=debian\nVERSION_ID=12\n",
                    0o644,
                )
            },
            ["guard", "unspecified", "distribution", "release_error"],
        ),
        (
            "vendor",
            |tree, _| write(&tree.layout().vendor, b"Other\n", 0o444),
            ["guard", "unspecified", "hypervisor", "release_error"],
        ),
        (
            "marker",
            |tree, _| write(&tree.layout().marker, b"other\n", 0o600),
            ["guard", "marker", "marker", "release_error"],
        ),
        (
            "marker mode",
            |tree, _| write(&tree.layout().marker, format!("{RUN}\n").as_bytes(), 0o644),
            ["guard", "marker", "file_metadata", "release_error"],
        ),
        (
            "selection",
            |tree, _| {
                write(
                    &tree.layout().fixture.join("selection.json"),
                    json!({"schemaVersion":1,"runId":"d".repeat(32),"revision":revision()})
                        .to_string()
                        .as_bytes(),
                    0o600,
                )
            },
            ["guard", "selection", "selection", "release_error"],
        ),
        (
            "executable",
            |tree, _| {
                let layout = tree.layout();
                std::fs::remove_file(&layout.executable).unwrap();
                write(&tree.base().join("copy"), b"\x7fELF", 0o755);
                std::os::unix::fs::symlink(tree.base().join("copy"), &layout.executable).unwrap();
            },
            ["guard", "helper", "helper_origin", "release_error"],
        ),
        (
            "executable mode",
            |tree, _| {
                let installed = tree.layout().library.join("simplestchat-ops");
                let mode = std::fs::Permissions::from_mode(0o775);
                std::fs::set_permissions(installed, mode).unwrap();
            },
            ["guard", "helper", "file_metadata", "release_error"],
        ),
        (
            "executable links",
            |tree, _| {
                let installed = tree.layout().library.join("simplestchat-ops");
                std::fs::hard_link(installed, tree.base().join("second")).unwrap();
            },
            ["guard", "helper", "file_metadata", "release_error"],
        ),
        (
            "executable empty",
            |tree, _| write(&tree.layout().library.join("simplestchat-ops"), b"", 0o755),
            ["guard", "helper", "file_metadata", "release_error"],
        ),
        (
            "socket type",
            |tree, _| write(&tree.layout().socket, b"", 0o660),
            ["guard", "docker_socket", "docker_socket", "release_error"],
        ),
        (
            "socket",
            |tree, _| {
                let mode = std::fs::Permissions::from_mode(0o666);
                std::fs::set_permissions(tree.layout().socket, mode).unwrap();
            },
            ["guard", "docker_socket", "docker_socket", "release_error"],
        ),
    ];
    for (name, change, expected) in cases {
        let tree = Tree::new();
        let machine = Machine::new(tree.base());
        let mut id = identity();
        change(&tree, &mut id);
        let (report, status, asked) = act_as(&tree, &machine, Action::Snapshot, id);
        assert_eq!((status, labels(&report)), (1, expected), "{name}");
        assert!(asked.is_empty(), "{name}");
        assert!(
            !tree.layout().fixture.join("snapshot.json").exists(),
            "{name}"
        );
    }
}

/// The guard refuses a run id that is not 32 lowercase hexadecimal digits
/// itself, whoever calls it, before it reads the guest.
#[test]
fn the_guard_refuses_a_malformed_run_before_reading_the_guest() {
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout().os_release[0]).unwrap();
    for run in ["A".repeat(32), "a".repeat(31), String::new()] {
        let refused = with_guest(&tree, identity(), |guest| guest.guard(&run).err());
        let labels = refused.map(|fault| fault.placed("guard", "unspecified").labels());
        assert_eq!(
            labels,
            Some(["guard", "unspecified", "run_id", "release_error"])
        );
    }
}

/// A protected file's mode and a unit's state are refused in their own slot.
#[test]
fn snapshot_failures_name_the_file_or_unit_they_were_about() {
    let tree = Tree::new();
    let machine = Machine::new(tree.base());
    let app = tree.layout().config.join("app.env");
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (report, status, asked) = act(&tree, &machine, Action::Snapshot);
    assert_eq!(
        (status, labels(&report)),
        (
            1,
            [
                "snapshot_files",
                "app.env",
                "file_metadata",
                "release_error"
            ]
        )
    );
    assert!(asked.is_empty());
    let extra = Tree::new();
    write(&extra.layout().config.join("extra.json"), b"{}", 0o600);
    let (report, _, _) = act(&extra, &Machine::new(extra.base()), Action::Snapshot);
    assert_eq!(
        labels(&report),
        [
            "snapshot_files",
            "unspecified",
            "configuration_files",
            "release_error"
        ]
    );
}

/// The database action requires two snapshot passes, then migrates with the
/// real migrator, removes exactly it, seeds, starts the application and
/// records the containers it created.
#[test]
fn the_database_action_migrates_seeds_and_starts_only_the_application() {
    let tree = Tree::new();
    let machine = Machine::new(tree.base());
    let (early, status, asked) = act(&tree, &machine, Action::Database);
    assert_eq!(status, 1, "{early}");
    assert!(asked.is_empty());
    for _ in 0..2 {
        assert_eq!(act(&tree, &machine, Action::Snapshot).1, 0);
    }
    let (report, status, asked) = act(&tree, &machine, Action::Database);
    assert_eq!(status, 0, "{report}");
    assert_eq!(
        report,
        json!({"schemaVersion":1,"runId":RUN,"action":"database","passed":true,
            "migrations":1,"ready":true,"counts":{"users":1,"rooms":1,"sessions":0,
            "credentials":0,"activeIncidents":1,"resolvedIncidents":1}})
    );
    let text: Vec<String> = asked.iter().map(|argv| argv.join(" ")).collect();
    let position = |needle: &str| {
        text.iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} not asked"))
    };
    assert!(
        position("up --detach --no-deps --no-build --pull never --wait")
            < position("create --no-build")
    );
    assert!(position("create --no-build") < position(&format!("start {MIGRATOR}")));
    assert!(position(&format!("stop --time 10 {MIGRATOR}")) < position(&format!("rm {MIGRATOR}")));
    assert!(position(&format!("rm {MIGRATOR}")) < position("never simplestchat"));
    assert!(position("never simplestchat") < position("/usr/bin/curl"));
    assert!(!*machine.migrator.borrow());
    let state = std::fs::read_to_string(tree.layout().fixture.join("database.json")).unwrap();
    let state: Value = serde_json::from_str(&state).unwrap();
    assert_eq!(
        (state["postgres"].as_str(), state["application"].as_str()),
        (Some(CONTAINER), Some(APPLICATION))
    );
    let (again, status, _) = act(&tree, &machine, Action::Database);
    assert_eq!(status, 1, "{again}");
}

/// The backup action checks the units, pauses the timer, takes exactly one
/// backup, restores it isolated, leaves no restore container or snapshot
/// copy and restarts the timer.
#[test]
fn the_backup_action_restores_one_new_backup_while_the_timer_is_paused() {
    let tree = Tree::new();
    let machine = Machine::new(tree.base());
    for action in [Action::Snapshot, Action::Snapshot, Action::Database] {
        assert_eq!(act(&tree, &machine, action).1, 0);
    }
    let (report, status, asked) = act(&tree, &machine, Action::BackupRestore);
    assert_eq!(status, 0, "{report}");
    assert_eq!(
        report,
        json!({"schemaVersion":1,"runId":RUN,"action":"backup-restore","passed":true,
            "archiveValidated":true,"restoreVerified":true,"cleanupPassed":true,
            "counts":{"users":1,"rooms":1,"sessions":0,"credentials":0,
            "activeIncidents":1,"resolvedIncidents":1}})
    );
    let text: Vec<String> = asked.iter().map(|argv| argv.join(" ")).collect();
    let position = |needle: &str| text.iter().position(|line| line.ends_with(needle)).unwrap();
    let stop = position("stop simplestchat-backup.timer");
    let start = position("start simplestchat-backup.service");
    let restart = position("start simplestchat-backup.timer");
    assert!(stop < start && start < restart);
    assert!(
        text[stop..start]
            .iter()
            .filter(|line| line.ends_with("--property=ActiveState"))
            .count()
            == 2
    );
    assert!(
        text[start..restart]
            .iter()
            .any(|line| line.contains("pg_restore"))
    );
    assert!(machine.restore.borrow().is_empty());
    let leftovers = std::fs::read_dir(tree.layout().fixture)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            path.join("snapshot.dump").exists().then_some(path)
        })
        .count();
    assert_eq!(leftovers, 0);
}

/// The database action requires two matching snapshots of this run, and
/// removes the migrator only while it is still the one it started, then
/// requires it gone.
#[test]
fn the_database_action_refuses_an_unready_or_changed_guest() {
    let tree = Tree::new();
    let machine = Machine::new(tree.base());
    assert_eq!(act(&tree, &machine, Action::Snapshot).1, 0);
    let (report, status, asked) = act(&tree, &machine, Action::Database);
    assert_eq!((status, report["phase"].as_str()), (1, Some("database")));
    assert!(asked.is_empty(), "one snapshot is not idempotence");
    assert_eq!(act(&tree, &machine, Action::Snapshot).1, 0);
    let path = tree.layout().fixture.join("snapshot.json");
    let record = std::fs::read(&path).unwrap();
    let mut foreign: Value = serde_json::from_slice(&record).unwrap();
    foreign["runId"] = json!("d".repeat(32));
    write(&path, foreign.to_string().as_bytes(), 0o600);
    let (_, status, asked) = act(&tree, &machine, Action::Database);
    assert_eq!(status, 1);
    assert!(asked.is_empty(), "another run's snapshots");
    for fault in ["replaced", "image", "left"] {
        let tree = Tree::new();
        let machine = Machine::faulty(tree.base(), fault);
        for _ in 0..2 {
            assert_eq!(act(&tree, &machine, Action::Snapshot).1, 0);
        }
        let (report, status, asked) = act(&tree, &machine, Action::Database);
        assert_eq!(
            (status, report["phase"].as_str()),
            (1, Some("database")),
            "{fault}"
        );
        let stopped = asked
            .iter()
            .any(|argv| argv.iter().any(|word| word == "stop"));
        assert_eq!(stopped, fault == "left", "{fault}");
        assert!(
            !tree.layout().fixture.join("database.json").exists(),
            "{fault}"
        );
    }
}

/// The backup action requires this run's database and the very containers
/// it recorded before it touches the timer, and a completed backup service.
#[test]
fn the_backup_action_refuses_a_foreign_or_changed_database_and_a_failed_backup() {
    let prepared = |fault: Option<&'static str>| {
        let tree = Tree::new();
        let machine = match fault {
            Some(fault) => Machine::faulty(tree.base(), fault),
            None => Machine::new(tree.base()),
        };
        for action in [Action::Snapshot, Action::Snapshot, Action::Database] {
            assert_eq!(act(&tree, &machine, action).1, 0);
        }
        (tree, machine)
    };
    for (field, value) in [
        ("runId", "d".repeat(32)),
        ("postgres", "3".repeat(64)),
        ("application", "3".repeat(64)),
    ] {
        let (tree, machine) = prepared(None);
        let path = tree.layout().fixture.join("database.json");
        let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        state[field] = json!(value);
        write(&path, state.to_string().as_bytes(), 0o600);
        let (report, status, asked) = act(&tree, &machine, Action::BackupRestore);
        assert_eq!(
            (status, report["phase"].as_str()),
            (1, Some("backup-restore")),
            "{field}"
        );
        // The timer is never stopped for a database the action cannot own.
        assert!(
            !asked
                .iter()
                .any(|argv| argv.iter().any(|word| word == "stop")),
            "{field}"
        );
    }
    let (tree, machine) = prepared(Some("backup-failed"));
    let (report, status, asked) = act(&tree, &machine, Action::BackupRestore);
    assert_eq!(
        (status, report["phase"].as_str()),
        (1, Some("backup-restore"))
    );
    let text: Vec<String> = asked.iter().map(|argv| argv.join(" ")).collect();
    assert!(
        text.iter()
            .any(|line| line.ends_with("start simplestchat-backup.timer"))
    );
    assert!(!text.iter().any(|line| line.contains("pg_restore")));
}

/// The process succeeds only when the action passed, its receipt reached
/// standard output and no stop signal arrived.
#[test]
fn only_a_passed_printed_and_unstopped_action_succeeds() {
    assert!(succeeded(0, true, false));
    for (status, printed, stopped) in [(1, true, false), (0, false, false), (0, true, true)] {
        assert!(
            !succeeded(status, printed, stopped),
            "{status} {printed} {stopped}"
        );
    }
}

/// Only `ACTION --run-id RUN` is the command line.
#[test]
fn the_command_line_is_one_action_and_its_run() {
    let words = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(
        parse(&words(&["backup-restore", "--run-id", RUN])),
        Some((Action::BackupRestore, RUN.to_owned()))
    );
    for refused in [
        vec!["snapshot"],
        vec!["snapshot", "--run-id"],
        vec!["restore", "--run-id", RUN],
        vec!["snapshot", "--run", RUN],
        vec!["snapshot", "--run-id", RUN, "extra"],
        vec!["--run-id", RUN, "snapshot"],
    ] {
        assert_eq!(parse(&words(&refused)), None, "{refused:?}");
    }
    assert_eq!(run(&words(&["snapshot"])), ExitCode::from(2));
    assert_eq!(
        run(&words(&["snapshot", "--run-id", "A"])),
        ExitCode::FAILURE
    );
}

/// Run `check` with a guest of `tree` whose owner roles are `identity`'s.
fn with_guest<T>(tree: &Tree, identity: Identity, check: impl FnOnce(&Guest<'_>) -> T) -> T {
    let machine = Machine::new(tree.base());
    let commands = Scripted::new(|request: &Request<'_>| machine.answer(request));
    let clock = Fixed::at("2026-11-22T12:00:00Z");
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: paths(tree.base()),
        owner: identity.root.0,
    };
    let base = tree.base().to_owned();
    let owner = identity.root.0;
    let lock = move || Workload::fixture_for(&base, owner, Takeover::Nothing);
    let guest = Guest {
        host: &host,
        layout: tree.layout(),
        identity,
        lock: &lock,
    };
    check(&guest)
}

/// A protected file is read only when it is an ordinary single-link file of
/// its owner's uid and gid with exactly its mode and a bounded size, opened
/// without following a link; a protected directory only with its owner and
/// exact mode, not through a link.
#[test]
fn protected_files_and_directories_need_their_exact_owner_mode_link_and_size() {
    let tree = Tree::new();
    let base = tree.base().to_owned();
    let path = base.join("probe");
    write(&path, b"data", 0o600);
    let refused = Err(Failure::Refused(policy::FILE_METADATA));
    with_guest(&tree, identity(), |guest| {
        assert_eq!(
            guest.private_file(&path, Owner::Root, 0o600),
            Ok(b"data".to_vec())
        );
        assert_eq!(guest.private_file(&path, Owner::Root, 0o644), refused);
        let alias = base.join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert_eq!(
            guest.private_file(&alias, Owner::Root, 0o600),
            Err(Failure::Os)
        );
        std::fs::remove_file(&alias).unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        assert_eq!(guest.private_file(&path, Owner::Root, 0o600), refused);
        std::fs::remove_file(&alias).unwrap();
        write(&path, b"", 0o600);
        assert_eq!(guest.private_file(&path, Owner::Root, 0o600), refused);
        let limit = usize::try_from(policy::MAX_FILE).unwrap();
        write(&path, &vec![b'x'; limit], 0o600);
        assert_eq!(
            guest
                .private_file(&path, Owner::Root, 0o600)
                .map(|d| d.len()),
            Ok(limit)
        );
        write(&path, &vec![b'x'; limit + 1], 0o600);
        assert_eq!(guest.private_file(&path, Owner::Root, 0o600), refused);
        let probe = base.join("probe-directory");
        directory(&probe, 0o700);
        let refused = Err(Failure::Refused(policy::DIRECTORY_METADATA));
        assert!(guest.directory(&probe, Owner::Root, 0o700).is_ok());
        assert_eq!(guest.directory(&probe, Owner::Root, 0o755), refused);
        assert_eq!(guest.directory(&path, Owner::Root, 0o600), refused);
        let link = base.join("directory-link");
        std::os::unix::fs::symlink(&probe, &link).unwrap();
        assert_eq!(guest.directory(&link, Owner::Root, 0o700), refused);
    });
    write(&path, b"data", 0o600);
    let probe = base.join("probe-directory");
    for (uid, gid) in [(1, 0), (0, 1)] {
        let mut other = identity();
        other.root = (other.root.0 + uid, other.root.1 + gid);
        with_guest(&tree, other, |guest| {
            let file = guest.private_file(&path, Owner::Root, 0o600);
            assert_eq!(file, Err(Failure::Refused(policy::FILE_METADATA)));
            let directory = guest.directory(&probe, Owner::Root, 0o700);
            assert_eq!(directory, Err(Failure::Refused(policy::DIRECTORY_METADATA)));
            let executable = guest.executable();
            assert_eq!(executable, Err(Failure::Refused(policy::FILE_METADATA)));
            // The socket's group is the daemon's; only its owner is root's.
            let socket = guest.socket();
            let expected = if uid == 1 {
                Err(Failure::Refused(policy::DOCKER_SOCKET))
            } else {
                Ok(())
            };
            assert_eq!(socket, expected);
        });
    }
}

/// Only names of real receipts count, and an archive is copied only from a
/// private bounded file of its owner whose copied bytes hash to the receipt.
#[test]
fn backup_receipts_and_archive_copies_are_exact() {
    let tree = Tree::new();
    let target = tree.base().join("receipts");
    directory(&target, 0o700);
    for name in [
        "a.receipt.json",
        ".receipt.json",
        "a.dump",
        "b.receipt.json.part",
    ] {
        write(&target.join(name), b"{}", 0o600);
    }
    let found: Vec<String> = receipts_in(&target)
        .unwrap()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(found, ["a.receipt.json"]);
    let source = target.join("a.dump");
    let digest = model::sha256(b"{}");
    let owner = identity().root.0;
    let copy = |name: &str, digest: &str, owner: u32| {
        copy_archive(&source, &target.join(name), digest, owner)
    };
    assert_eq!(copy("one", &digest, owner), Ok(()));
    assert_eq!(std::fs::read(target.join("one")).unwrap(), b"{}");
    assert_eq!(copy("one", &digest, owner), Err(Failure::Os));
    let differs = Err(Failure::Refused("Release backup digest differs"));
    assert_eq!(copy("two", &"0".repeat(64), owner), differs);
    let unsafe_archive = Err(Failure::Refused("Unsafe or oversized release backup"));
    assert_eq!(copy("three", &digest, owner + 1), unsafe_archive);
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(copy("four", &digest, owner), unsafe_archive);
    write(&source, b"", 0o600);
    assert_eq!(copy("five", &digest, owner), unsafe_archive);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap();
    file.set_len(MAX_BACKUP + 1).unwrap();
    assert_eq!(copy("six", &digest, owner), unsafe_archive);
}
