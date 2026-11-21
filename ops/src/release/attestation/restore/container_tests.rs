//! The real container engine under the restore database tests. Every
//! request the operation makes goes unchanged to the installed Docker
//! client, through the operation's own process owner, except the one that
//! asks which image the production database container runs: no runner has
//! that container, so the pinned production image's own ID answers it.
//! Around the operation the engine observes, with requests of its own, what
//! the container ran and what it left behind, and it removes the
//! operation's container, found by its label and name, only when the
//! operation did not. The unit tests below drive the operation over a fake
//! engine; no container is started by them.

use super::super::control::Owner;
use super::database_tests::{DAEMON, PRINTF, Prepared, answer, operate};
use super::{CommandOutput, Executor, Invocation, Role, policy};
use crate::test_support::sha256_hex;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

/// Selects the real-engine mode of the restore database check.
pub(super) const SELECTOR: &str = "RESTORE_CONTAINER_CHECK";
/// The one request no runner can answer: the live database's image.
const LIVE: [&str; 4] = [
    "inspect",
    "--format",
    "{{.Image}}",
    "simplestchat-public-postgres-1",
];
/// The named volumes a container mounts, separated by spaces.
const VOLUMES: &str = "{{range .Mounts}}{{if eq .Type \"volume\"}}{{.Name}} {{end}}{{end}}";
/// The server's version and what the restore and the structural check left.
const OBSERVED: &str = "SELECT current_setting('server_version_num'), \
                        to_regclass('public.users') IS NOT NULL, \
                        EXISTS (SELECT FROM pg_extension WHERE extname = 'amcheck');";

/// The deployment's pinned PostgreSQL image, as its group variables name it.
pub(super) fn pinned_reference() -> String {
    let variables =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible/group_vars/benchmark_hosts.yml");
    let text = std::fs::read_to_string(variables).unwrap();
    let mut values = text
        .lines()
        .filter_map(|line| line.strip_prefix("scpub_postgres_image: "));
    let value = values.next().expect("the pinned PostgreSQL image");
    assert!(values.next().is_none(), "one pinned PostgreSQL image");
    value.trim().to_owned()
}

/// The `server_version_num` of the release a `…/postgres:MAJOR.MINOR-…`
/// reference names, or nothing for any other reference.
pub(super) fn tagged_version(reference: &str) -> Option<String> {
    let (_, tag) = reference.split_once("/postgres:")?;
    let (version, _) = tag.split_once('-')?;
    let (major, minor) = version.split_once('.')?;
    let major: u32 = major.parse().ok()?;
    let minor: u32 = minor.parse().ok()?;
    Some((major * 10_000 + minor).to_string())
}

/// The engine as the test itself asks it: what it printed, or its
/// complaint.
pub(super) trait Client {
    fn docker(&mut self, arguments: &[&str]) -> Result<String, String>;
}

/// The installed client on the operation's own socket, with the fixed
/// environment the operation gives it. Blocks until the client exits.
pub(super) struct Installed(pub(super) PathBuf);

impl Client for Installed {
    fn docker(&mut self, arguments: &[&str]) -> Result<String, String> {
        let output = Command::new(&self.0)
            .args(DAEMON)
            .args(arguments)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C")
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            String::from_utf8(output.stdout).map_err(|error| error.to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }
}

/// Whether the engine passes a request on unchanged or answers it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Request {
    Pass,
    Answer(String),
}

/// Classify one request of the operation, its daemon prefix removed. Only
/// the live database container's image inspection is answered, with
/// `image`; asking it in any other role, or inspecting anything else in its
/// role, is refused.
pub(super) fn classify(role: Role, words: &[&str], image: &str) -> Result<Request, String> {
    match role {
        Role::Inspect if words == LIVE => Ok(Request::Answer(format!("{image}\n"))),
        Role::Inspect => Err(format!("an unexpected inspection {words:?}")),
        _ if words == LIVE => Err(format!("the live image asked as {role:?}")),
        _ => Ok(Request::Pass),
    }
}

/// The name of the container a create request makes, when it creates it,
/// with the operation's own label, from exactly `image` and never pulled.
pub(super) fn created_from(words: &[&str], image: &str) -> Result<String, String> {
    if words.first() != Some(&"create") {
        return Err(format!("not a create {words:?}"));
    }
    let value = |option: &str| {
        let pair = words.windows(2).find(|pair| pair[0] == option);
        pair.map(|pair| pair[1])
    };
    let name = value("--name").ok_or("a create without a name")?;
    if value("--label") != Some(format!("{}={name}", policy::LABEL).as_str()) {
        return Err(format!("a create without its own label {words:?}"));
    }
    if value("--pull") != Some("never") {
        return Err(format!("a create that may pull {words:?}"));
    }
    // The image follows the entrypoint, as the operation's recipe writes it.
    if value("docker-entrypoint.sh") != Some(image) {
        return Err(format!("a create from another image {words:?}"));
    }
    Ok(name.to_owned())
}

/// What the engine reported about the operation's container just before
/// the operation removed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Inside {
    /// `server_version_num` of the server in the container.
    pub(super) version: String,
    /// The restored users table.
    pub(super) restored: bool,
    /// The extension `pg_amcheck --install-missing` installs where it checks.
    pub(super) checked: bool,
    /// The image the engine says the container was created from.
    pub(super) image: String,
    /// The named volumes the container mounted.
    pub(super) volumes: Vec<String>,
}

/// What the engine held once the operation had finished.
#[derive(Debug)]
pub(super) struct Seen {
    pub(super) inside: Result<Inside, String>,
    /// Every container still carrying the operation's label, one per line.
    pub(super) labelled: String,
    /// The container's volumes that still exist.
    pub(super) volumes_left: Vec<String>,
}

/// The request that lists the operation's own container by its label and
/// its name; the only selection the guard ever removes from.
pub(super) fn selection(name: &str) -> Vec<String> {
    [
        "ps",
        "--all",
        "--quiet",
        "--no-trunc",
        "--filter",
        &format!("label={}={name}", policy::LABEL),
        "--filter",
        &format!("name=^/{name}$"),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The removals a listing allows: each listed full container ID, nothing
/// else, and never a prune.
pub(super) fn removals(listed: &str) -> Vec<Vec<String>> {
    let ids = listed.lines().map(str::trim);
    let ids = ids.filter(|id| policy::hex(id, 64));
    ids.map(|id| {
        ["rm", "--force", "--volumes", id]
            .map(str::to_owned)
            .to_vec()
    })
    .collect()
}

/// The operation's engine: `inner` runs what the operation asks, `client`
/// is the test's own. Dropping it removes the operation's container if the
/// operation left it, selected by its label and name only.
pub(super) struct Engine<E, C: Client> {
    inner: E,
    client: C,
    program: PathBuf,
    image: String,
    name: Option<String>,
    container: Option<String>,
    inside: Result<Inside, String>,
    pub(super) roles: Vec<Role>,
    pub(super) statuses: Vec<Option<i32>>,
    /// Each request as the operation made it.
    requests: Vec<(PathBuf, Vec<OsString>)>,
}

impl<E: Executor, C: Client> Engine<E, C> {
    /// An engine for an operation whose client is `program`, answering the
    /// live image inspection with `image`.
    pub(super) fn new(inner: E, client: C, program: &Path, image: &str) -> Self {
        Self {
            inner,
            client,
            program: program.to_owned(),
            image: image.to_owned(),
            name: None,
            container: None,
            inside: Err("the container was never removed".to_owned()),
            roles: Vec::new(),
            statuses: Vec::new(),
            requests: Vec::new(),
        }
    }

    /// Whether the operation asked for its container.
    pub(super) fn created(&self) -> bool {
        self.name.is_some()
    }

    fn observe(&mut self) -> Result<Inside, String> {
        let id = self.container.clone().ok_or("no container identity")?;
        let image = self
            .client
            .docker(&["inspect", "--format", "{{.Image}}", &id])?;
        let volumes = self.client.docker(&["inspect", "--format", VOLUMES, &id])?;
        let user = ["exec", "--user", "999:999", &id, "psql", "-X", "-qAt"];
        let connection = [
            "-h",
            policy::SOCKET,
            "-U",
            "postgres",
            "-d",
            policy::DATABASE,
        ];
        let request = [&user[..], &connection[..], &["-c", OBSERVED]].concat();
        let reported = self.client.docker(&request)?;
        let fields: Vec<&str> = reported.trim().split('|').collect();
        let [version, restored, checked] = fields[..] else {
            return Err(format!("an unexpected report {reported:?}"));
        };
        Ok(Inside {
            version: version.to_owned(),
            restored: restored == "t",
            checked: checked == "t",
            image: image.trim().to_owned(),
            volumes: volumes.split_whitespace().map(str::to_owned).collect(),
        })
    }

    /// What the engine holds now that the operation has finished: its
    /// labelled containers and the container's volumes that remain.
    pub(super) fn finish(&mut self) -> Seen {
        let label = format!("label={}", policy::LABEL);
        let labelled = ["ps", "--all", "--quiet", "--no-trunc", "--filter", &label];
        let labelled = self.client.docker(&labelled);
        let labelled = labelled.unwrap_or_else(|complaint| format!("unlisted: {complaint}"));
        let volumes = self.inside.as_ref().map(|inside| inside.volumes.clone());
        let mut volumes_left = Vec::new();
        for name in volumes.unwrap_or_default() {
            let exists = ["volume", "inspect", "--format", "{{.Name}}", &name];
            if self.client.docker(&exists).is_ok() {
                volumes_left.push(name);
            }
        }
        Seen {
            inside: self.inside.clone(),
            labelled: labelled.trim().to_owned(),
            volumes_left,
        }
    }
}

impl<E: Executor, C: Client> Executor for Engine<E, C> {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        let role = request.role;
        self.roles.push(role);
        self.requests
            .push((request.program.clone(), request.arguments.clone()));
        assert_eq!(request.program, self.program, "the operation's own client");
        let owned: Vec<String> = request
            .arguments
            .iter()
            .map(|word| word.to_str().unwrap().to_owned())
            .collect();
        let words: Vec<&str> = owned.iter().map(String::as_str).collect();
        let words = words
            .strip_prefix(&DAEMON)
            .expect("the operation's own daemon");
        let class = classify(role, words, &self.image).unwrap_or_else(|why| panic!("{why}"));
        if role == Role::Create {
            let name = created_from(words, &self.image).unwrap_or_else(|why| panic!("{why}"));
            self.name = Some(name);
        }
        if role == Role::CleanupRemove {
            self.inside = self.observe();
        }
        let output = match class {
            Request::Answer(text) => {
                let (program, arguments) = answer(&text);
                let local = Invocation {
                    role,
                    program,
                    arguments: arguments.into_iter().map(OsString::from).collect(),
                    input: request.input,
                    stdout: request.stdout,
                    stderr: request.stderr,
                    body_end: request.body_end,
                    settlement_end: request.settlement_end,
                };
                self.inner.run(owner, local).await
            }
            Request::Pass => self.inner.run(owner, request).await,
        };
        if role == Role::Create && output.status == Some(0) && output.stdout_complete {
            let printed = String::from_utf8_lossy(&output.stdout);
            let id = printed.trim();
            if policy::hex(id, 64) {
                self.container = Some(id.to_owned());
            }
        }
        self.statuses.push(output.status);
        output
    }
}

impl<E, C: Client> Drop for Engine<E, C> {
    fn drop(&mut self) {
        let Some(name) = &self.name else { return };
        let selection = selection(name);
        let words: Vec<&str> = selection.iter().map(String::as_str).collect();
        let Ok(listed) = self.client.docker(&words) else {
            return;
        };
        for removal in removals(&listed) {
            let words: Vec<&str> = removal.iter().map(String::as_str).collect();
            let _ = self.client.docker(&words);
        }
    }
}

/// The engine's own requests, shared with the test that reads them.
type Log = Rc<RefCell<Vec<Vec<String>>>>;

/// A fake engine answering the test's own requests.
struct Asked {
    log: Log,
    image: String,
    listed: String,
}

impl Client for Asked {
    fn docker(&mut self, arguments: &[&str]) -> Result<String, String> {
        self.log
            .borrow_mut()
            .push(arguments.iter().map(|&word| word.to_owned()).collect());
        Ok(match arguments {
            ["inspect", "--format", "{{.Image}}", _] => format!("{}\n", self.image),
            ["inspect", "--format", _, _] => "fixture-volume \n".to_owned(),
            ["exec", ..] => "180006|t|t\n".to_owned(),
            ["volume", "inspect", ..] => return Err("no such volume".to_owned()),
            ["ps", .., last] if last.starts_with("name=") => self.listed.clone(),
            _ => String::new(),
        })
    }
}

/// A fake engine answering the operation's requests as Docker would, and
/// recording each request as it arrived.
struct Fake {
    got: Vec<(PathBuf, Vec<OsString>)>,
    id: String,
    image: String,
    name: String,
    removed: bool,
    ledger: String,
}

impl Executor for Fake {
    async fn run(&mut self, _owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        self.got
            .push((request.program.clone(), request.arguments.clone()));
        let words: Vec<String> = request
            .arguments
            .iter()
            .map(|word| word.to_str().unwrap().to_owned())
            .collect();
        let printed = if request.program == Path::new(PRINTF) {
            words[1].clone()
        } else {
            match request.role {
                Role::Create => {
                    let at = words.iter().position(|word| word == "--name").unwrap();
                    self.name = words[at + 1].clone();
                    format!("{}\n", self.id)
                }
                Role::Start => format!("{}\n", self.id),
                Role::CleanupRemove => {
                    self.removed = true;
                    format!("{}\n", self.id)
                }
                Role::CleanupSelect | Role::CleanupConfirm if !self.removed => {
                    format!("{}\n", self.id)
                }
                Role::Ledger => self.ledger.clone(),
                Role::Verify => "{\"users\":1}\n".to_owned(),
                Role::CleanupInspect => json!({"id": self.id,
                    "name": format!("/{}", self.name), "image": self.image,
                    "owner": self.name})
                .to_string(),
                _ => String::new(),
            }
        };
        if let Some(mut input) = request.input {
            let mut consumed = Vec::new();
            let _ = input.read_to_end(&mut consumed);
        }
        let bytes = printed.into_bytes();
        request.stdout.write_at(&bytes, 0).unwrap();
        CommandOutput {
            stdout_bytes: bytes.len() as u64,
            stderr_bytes: 0,
            stdout: bytes,
            status: Some(0),
            settled: true,
            stdout_complete: true,
            error: None,
        }
    }
}

fn image() -> String {
    format!("sha256:{}", sha256_hex(b"pinned fixture image"))
}

fn fake(ledger: String) -> Fake {
    Fake {
        got: Vec::new(),
        id: sha256_hex(b"fixture container"),
        image: image(),
        name: String::new(),
        removed: false,
        ledger,
    }
}

fn asked(log: &Log, listed: &str) -> Asked {
    Asked {
        log: Rc::clone(log),
        image: image(),
        listed: listed.to_owned(),
    }
}

/// The engine over a fake: what the fake received, what the engine saw.
fn driven(listed: &str) -> (bool, Engine<Fake, Asked>, Log) {
    let recorded = BTreeMap::from([("1".to_owned(), "d".repeat(96))]);
    let backup = b"an inert archive the fake engine never restores";
    let prepared = Prepared::new(backup, &sha256_hex(backup), &recorded, &recorded);
    let log = Log::default();
    let fake = fake(policy::ledger(&recorded));
    let client = asked(&log, listed);
    let mut engine = Engine::new(fake, client, &prepared.access.docker, &image());
    let (passed, _, _) = operate(&prepared.access, &mut engine);
    (passed, engine, log)
}

/// Over a whole operation, every request reaches the engine exactly as the
/// operation made it, except the live image inspection, which is the one
/// answered request; the container is created from the answered image and
/// observed before its removal.
#[test]
fn the_engine_passes_every_request_but_the_live_image_inspection() {
    let (passed, engine, _) = driven("");
    assert!(passed);
    let got = &engine.inner.got;
    assert_eq!(got.len(), engine.requests.len());
    assert_eq!(engine.roles[0], Role::Inspect);
    let words: Vec<&str> = engine.requests[0]
        .1
        .iter()
        .map(|w| w.to_str().unwrap())
        .collect();
    assert_eq!(words[..2], DAEMON);
    assert_eq!(words[2..], LIVE);
    let printed = OsString::from(format!("{}\n", image()));
    let answered = (PathBuf::from(PRINTF), vec![OsString::from("%s"), printed]);
    assert_eq!(got[0], answered);
    assert_eq!(got[1..], engine.requests[1..]);
    let printed = got
        .iter()
        .filter(|(program, _)| program == Path::new(PRINTF));
    assert_eq!(printed.count(), 1);
    let create = engine.roles.iter().position(|role| *role == Role::Create);
    let create = &got[create.unwrap()].1;
    assert!(create.contains(&OsString::from(image())));
    let inside = engine.inside.clone().unwrap();
    assert_eq!(
        (inside.version.as_str(), inside.restored, inside.checked),
        ("180006", true, true)
    );
    assert_eq!(inside.image, image());
    assert_eq!(inside.volumes, ["fixture-volume"]);
    assert_eq!(engine.statuses.len(), engine.roles.len());
}

#[test]
fn only_the_live_image_inspection_is_answered() {
    let image = image();
    let answered = classify(Role::Inspect, &LIVE, &image);
    assert_eq!(answered, Ok(Request::Answer(format!("{image}\n"))));
    let other = ["inspect", "--format", "{{.Image}}", "another-container"];
    assert!(classify(Role::Inspect, &other, &image).is_err());
    assert!(classify(Role::Prior, &LIVE, &image).is_err());
    for role in [
        Role::Create,
        Role::CleanupInspect,
        Role::Verify,
        Role::CleanupRemove,
    ] {
        let words = ["inspect", "--format", "{{json .Id}}", "own-container"];
        assert_eq!(
            classify(role, &words, &image),
            Ok(Request::Pass),
            "{role:?}"
        );
    }
}

#[test]
fn a_container_is_admitted_only_from_the_pinned_image_never_pulled() {
    let image = image();
    let name = format!("scpub-restore-{}", "a".repeat(32));
    let words = |image: &str| {
        let words = policy::create(&name, image);
        words
            .iter()
            .map(|word| word.to_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let admitted = words(&image);
    let admitted: Vec<&str> = admitted.iter().map(String::as_str).collect();
    assert_eq!(created_from(&admitted, &image), Ok(name.clone()));
    let other = format!("sha256:{}", sha256_hex(b"another image"));
    let refused = words(&other);
    let refused: Vec<&str> = refused.iter().map(String::as_str).collect();
    assert!(created_from(&refused, &image).is_err());
    for (from, to) in [("never", "missing"), (policy::LABEL, "another.label")] {
        let changed: Vec<String> = admitted.iter().map(|word| word.replace(from, to)).collect();
        let changed: Vec<&str> = changed.iter().map(String::as_str).collect();
        assert!(created_from(&changed, &image).is_err(), "{from}");
    }
}

/// The guard selects by the operation's label and name together, removes
/// only full IDs that selection lists, and never prunes.
#[test]
fn the_guard_removes_only_the_operation_container_by_label_and_name() {
    let id = sha256_hex(b"left behind");
    let (_, engine, log) = driven(&format!("{id}\nnot an identity\n"));
    let name = engine.name.clone().unwrap();
    let before = log.borrow().len();
    drop(engine);
    let issued = log.borrow()[before..].to_vec();
    let removal = ["rm", "--force", "--volumes", &id]
        .map(str::to_owned)
        .to_vec();
    assert_eq!(issued, [selection(&name), removal]);
    assert_eq!(
        selection(&name)[4..],
        [
            "--filter".to_owned(),
            format!("label={}={name}", policy::LABEL),
            "--filter".to_owned(),
            format!("name=^/{name}$"),
        ]
    );
    let (_, engine, log) = driven("");
    let before = log.borrow().len();
    drop(engine);
    assert_eq!(log.borrow().len(), before + 1);
    assert!(
        log.borrow()
            .iter()
            .flatten()
            .all(|word| !word.contains("prune"))
    );
    // An operation that never asked for a container leaves nothing to select.
    let quiet = Log::default();
    let unnamed = Engine::new(
        fake(String::new()),
        asked(&quiet, &id),
        Path::new("/unused"),
        &image(),
    );
    drop(unnamed);
    assert!(quiet.borrow().is_empty());
}

/// The reference names the release the container must report.
#[test]
fn the_pinned_reference_names_the_version_the_container_must_report() {
    let reference = pinned_reference();
    let (name, digest) = reference.split_once("@sha256:").unwrap();
    assert!(name.starts_with("docker.io/library/postgres:"));
    assert!(policy::hex(digest, 64));
    assert!(tagged_version(&reference).is_some());
    let tagged = |tag: &str| tagged_version(&format!("docker.io/library/postgres:{tag}"));
    assert_eq!(tagged("18.6-bookworm").as_deref(), Some("180006"));
    assert_eq!(tagged("17.10-bookworm").as_deref(), Some("170010"));
    for tag in ["18-bookworm", "latest", "18.x-bookworm"] {
        assert_eq!(tagged(tag), None, "{tag}");
    }
}
