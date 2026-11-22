//! The fixture's fixed command lines. Every program is an absolute path and
//! every dynamic value (a container, image or revision identity the fixture
//! has already checked) occupies its own argument; no shell is involved.

use super::host::{Paths, Request};
use std::{ffi::OsString, path::Path, time::Duration};

/// The local engine, always through its fixed socket.
pub(super) const DOCKER: [&str; 3] = ["/usr/bin/docker", "--host", "unix:///var/run/docker.sock"];
/// The Ops executable where a signed release installs it on a prepared host.
pub(super) const OPS: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
pub(super) const CURL: &str = "/usr/bin/curl";
pub(super) const OPENSSL: &str = "/usr/bin/openssl";
pub(super) const NSENTER: &str = "/usr/bin/nsenter";
pub(super) const UPDATE_TRUST: &str = "/usr/sbin/update-ca-certificates";
pub(super) const GIT: &str = "/usr/bin/git";
/// The host programs the fixture needs before it touches anything.
pub(super) const TOOLS: [&str; 5] = [DOCKER[0], CURL, OPENSSL, NSENTER, UPDATE_TRUST];
/// The label every fixture resource carries, valued with the run's token.
pub(super) const LABEL: &str = "simplestchat.release-test";
pub(super) const PROJECT: &str = "simplestchat-public";
/// The deployment's PostgreSQL and Caddy images (`scpub_postgres_image` and
/// `scpub_caddy_image` of the benchmark host group).
pub(super) const POSTGRES: &str = concat!(
    "docker.io/library/postgres:18.6-bookworm@sha256:",
    "3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650",
);
pub(super) const CADDY: &str = concat!(
    "ghcr.io/swiftraccoon/simplestchat-caddy:2.11.7-",
    "73c81fdbfe625d3df3989ba807064c81fbe2c071@sha256:",
    "23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0",
);
/// The one line the deliberately failing candidate prints before it exits 42.
pub(super) const FAILURE_MARKER: &str = "simplestchat-disposable-release-startup-failure";
/// What a release command reserves for its own end.
const FINAL: [&str; 2] = ["--finalization-seconds", "60"];
pub(super) const CONTAINER_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},"#,
    r#""restarts":{{.RestartCount}},"labels":{{json .Config.Labels}}}"#,
);
pub(super) const IMAGE_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"os":{{json .Os}},"#,
    r#""architecture":{{json .Architecture}},"user":{{json .Config.User}},"#,
    r#""cmd":{{json .Config.Cmd}},"#,
    r#""entrypoint":{{json (index .Config "Entrypoint")}}}"#,
);
pub(super) const VALIDATOR_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},"#,
    r#""user":{{json .Config.User}},"entrypoint":{{json .Config.Entrypoint}},"#,
    r#""network":{{json .HostConfig.NetworkMode}},"#,
    r#""readOnly":{{json .HostConfig.ReadonlyRootfs}},"state":{{json .State}}}"#,
);
/// The services cleanup stops, in this order; `migrate` is the maintenance one.
pub(super) const SERVICES: [&str; 4] = ["simplestchat", "migrate", "caddy", "postgres"];

fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn words<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

fn path(value: &Path) -> OsString {
    value.as_os_str().to_owned()
}

/// One engine command named `operation` in public evidence.
pub(super) fn docker(operation: &'static str, arguments: &[&str]) -> Request {
    let mut argv = words(DOCKER);
    argv.extend(words(arguments.iter().copied()));
    Request::new(operation, argv)
}

/// One engine command with path arguments.
fn docker_paths(operation: &'static str, arguments: Vec<OsString>) -> Request {
    let mut argv = words(DOCKER);
    argv.extend(arguments);
    Request::new(operation, argv)
}

/// One Compose command of the fixture's fixed project and files.
pub(super) fn compose(paths: &Paths, arguments: &[&str]) -> Request {
    let mut argv = words(DOCKER);
    argv.extend(words([
        "compose",
        "--project-name",
        PROJECT,
        "--project-directory",
    ]));
    argv.push(path(&paths.config));
    argv.push("--env-file".into());
    argv.push(path(&paths.config.join("app.env")));
    argv.push("-f".into());
    argv.push(path(&paths.config.join("compose.public.yml")));
    argv.extend(words(arguments.iter().copied()));
    Request::new("docker compose", argv)
}

pub(super) fn inspect(identity: &str) -> Request {
    docker(
        "docker inspect",
        &["inspect", "--format", CONTAINER_FORMAT, identity],
    )
}

pub(super) fn image_identity(selector: &str) -> Request {
    docker(
        "docker image inspect",
        &["image", "inspect", "--format", "{{.Id}}", selector],
    )
}

pub(super) fn running(paths: &Paths, service: &str) -> Request {
    compose(paths, &["ps", "--status", "running", "--quiet", service])
}

/// PostgreSQL's own client in the verified database container, as its user,
/// over the shared socket; `command` is the statement, or `None` for input.
pub(super) fn sql(identity: &str, command: Option<&str>, input: Option<&Path>) -> Request {
    let mut arguments = vec![
        "exec",
        "--interactive",
        "--user",
        "999:999",
        identity,
        "timeout",
        "--kill-after=2s",
        "15s",
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
    ];
    if let Some(command) = command {
        arguments.extend(["--command", command]);
    }
    let mut request = docker("docker exec", &arguments);
    request.input = input.map(Path::to_path_buf);
    request
}

/// The backup's table of contents, read by the database container's own tool.
pub(super) fn backup_listing(identity: &str, backup: &Path) -> Request {
    let arguments = [
        "exec",
        "--interactive",
        "--user",
        "999:999",
        identity,
        "timeout",
        "--kill-after=2s",
        "15s",
        "pg_restore",
        "--list",
    ];
    let mut request = docker("docker exec", &arguments);
    request.input = Some(backup.to_path_buf());
    request
}

/// One readiness probe: the proxy's HTTPS origin, or, inside the migration
/// container's network namespace held open as descriptor `namespace`, the
/// application's own port.
pub(super) fn ready(namespace: Option<i32>) -> Request {
    let (protocol, url) = match namespace {
        Some(_) => ("=http", "http://127.0.0.1:3000/ready"),
        None => ("=https", "https://localhost/ready"),
    };
    let probe = [
        CURL,
        "--disable",
        "--noproxy",
        "*",
        "--proto",
        protocol,
        "--fail",
        "--silent",
        "--show-error",
        "--max-time",
        "2",
        url,
    ];
    let mut argv = Vec::new();
    let operation = match namespace {
        Some(descriptor) => {
            argv.push(NSENTER.into());
            argv.push(format!("--net=/proc/self/fd/{descriptor}").into());
            argv.push("--".into());
            "nsenter"
        }
        None => "curl",
    };
    argv.extend(words(probe));
    let mut request = Request::new(operation, argv);
    request.timeout = seconds(5);
    request.check = false;
    request
}

/// The fixed release action of the installed Ops executable. Staging admits a
/// whole budget of at most ten minutes; a deployment needs more than the time
/// it reserves for its one rollback.
pub(super) fn release(action: Action, revision: &str) -> Request {
    let budget = match action {
        Action::Stage => "600",
        Action::Deploy => "900",
    };
    let mut argv = words([OPS, "release", action.name(), "--revision", revision]);
    argv.extend(words(["--timeout-seconds", budget]));
    argv.extend(words(FINAL));
    let mut request = Request::new(action.operation(), argv);
    request.timeout = seconds(240);
    request.check = false;
    request
}

/// The two release actions the fixture drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Stage,
    Deploy,
}
impl Action {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Stage => "stage",
            Self::Deploy => "deploy",
        }
    }
    fn operation(self) -> &'static str {
        match self {
            Self::Stage => "simplestchat-ops release stage",
            Self::Deploy => "simplestchat-ops release deploy",
        }
    }
}

/// Initialize the browser asset store and select `revision`, through the
/// installed executable's own store owner.
pub(super) fn assets(revision: &str) -> Request {
    let argv = words([
        OPS,
        "release",
        "assets",
        "initialize",
        "--revision",
        revision,
    ]);
    Request::new("simplestchat-ops release assets", argv)
}

/// The installed verifier's check of one exported archive against its manifest.
pub(super) fn archive(directory: &Path) -> Request {
    let mut argv = words([OPS, "release", "archive", "--manifest"]);
    argv.push(path(&directory.join("release.json")));
    argv.push("--path".into());
    argv.push(path(&directory.join("image.tar")));
    let mut request = Request::new("simplestchat-ops release archive", argv);
    request.timeout = seconds(90);
    request
}

/// Build one derived fixture image from `directory` with the default local
/// builder, offline, without pulling a base.
pub(super) fn build(tag: &str, directory: &Path) -> Request {
    let mut arguments = words([
        "buildx",
        "build",
        "--builder",
        "default",
        "--load",
        "--platform",
        "linux/amd64",
        "--provenance=false",
        "--sbom=false",
        "--network",
        "none",
        "--pull=false",
        "--tag",
        tag,
    ]);
    arguments.push(path(directory));
    let mut request = docker_paths("docker buildx build", arguments);
    request.timeout = seconds(90);
    request
}

pub(super) fn save(tag: &str, archive: &Path) -> Request {
    let mut arguments = words(["image", "save", "--platform", "linux/amd64", "--output"]);
    arguments.push(path(archive));
    arguments.push(tag.into());
    let mut request = docker_paths("docker image save", arguments);
    request.timeout = seconds(90);
    request
}

pub(super) fn pull(image: &str) -> Request {
    let mut request = docker("docker pull", &["pull", image]);
    request.timeout = seconds(90);
    request
}

pub(super) fn revision(checkout: &Path) -> Request {
    let mut argv = vec![GIT.into(), "-C".into(), path(checkout)];
    argv.extend(words(["rev-parse", "HEAD"]));
    Request::new("git", argv)
}

/// The fixture CA, a private localhost key and request, and the signed
/// server certificate, for `token`'s run.
pub(super) fn certificates(token: &str, private: &Path, config: &Path) -> [Request; 3] {
    let subject = format!("/CN=simplestchat-disposable-{token}");
    let ca = path(&private.join("ca.crt"));
    let key = path(&private.join("ca.key"));
    let mut authority = words([OPENSSL, "req", "-x509", "-newkey", "rsa:2048", "-nodes"]);
    authority.extend(words(["-days", "1", "-subj", &subject, "-addext"]));
    authority.extend(words(["basicConstraints=critical,CA:TRUE", "-keyout"]));
    authority.extend([key.clone(), "-out".into(), ca.clone()]);
    let mut server = words([OPENSSL, "req", "-newkey", "rsa:2048", "-nodes", "-subj"]);
    server.extend(words(["/CN=localhost", "-keyout"]));
    server.extend([path(&config.join("fixture.key")), "-out".into()]);
    server.push(path(&private.join("server.csr")));
    let mut signed = words([OPENSSL, "x509", "-req", "-in"]);
    signed.extend([path(&private.join("server.csr")), "-CA".into(), ca]);
    signed.extend(["-CAkey".into(), key, "-CAcreateserial".into()]);
    signed.extend(words(["-days", "1", "-extfile"]));
    signed.extend([path(&private.join("server.ext")), "-out".into()]);
    signed.push(path(&config.join("fixture.crt")));
    [authority, server, signed].map(|argv| Request::new("openssl", argv))
}

pub(super) fn update_trust() -> Request {
    Request::new("update-ca-certificates", words([UPDATE_TRUST]))
}

/// The browser rollover through the checkout's own helper.
pub(super) fn rollover(node: &Path, checkout: &Path, files: [&Path; 3]) -> Request {
    let [configuration, chromium, result] = files;
    let argv = vec![
        path(node),
        path(&checkout.join("web/e2e/release-assets.cjs")),
        path(configuration),
        path(chromium),
        path(result),
    ];
    let mut request = Request::new("node", argv);
    request.timeout = seconds(330);
    request.check = false;
    request
}

/// The passive response check of the selected feature binary, bound to the
/// verified proxy and application containers and the recorded fixture CA.
pub(super) fn response_policy(
    binary: &Path,
    token: &str,
    ids: [&str; 2],
    ca: &Path,
    digest: &str,
) -> Request {
    let [caddy, app] = ids;
    let mut argv = vec![path(binary)];
    argv.extend(words(["release-http-policy", "--fixture-token", token]));
    argv.extend(words(["--caddy-id", caddy, "--app-id", app, "--ca"]));
    argv.push(path(ca));
    argv.extend(words(["--ca-sha256", digest]));
    let mut request = Request::new("release-http-policy", argv);
    request.timeout = seconds(210);
    request.grace = seconds(60);
    request
}

/// The deployment's own renderer over the generated local play.
pub(super) fn render(ansible: &Path, play: &Path) -> Request {
    let argv = vec![path(ansible), "-i".into(), "localhost,".into(), path(play)];
    let mut request = Request::new("ansible-playbook", argv);
    request.timeout = seconds(120);
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_budgets_and_final_reservation_are_the_reviewed_ones() {
        let stage = release(Action::Stage, &"f".repeat(40));
        let deploy = release(Action::Deploy, &"f".repeat(40));
        assert_eq!(stage.argv[stage.argv.len() - 3], "600");
        assert_eq!(deploy.argv[deploy.argv.len() - 3], "900");
        for request in [stage, deploy] {
            assert_eq!(request.argv[0], OPS);
            assert_eq!(
                request.argv[request.argv.len() - 2..],
                ["--finalization-seconds", "60"]
            );
            assert_eq!(request.timeout, seconds(240));
            assert!(!request.check);
        }
        // The response check gets its own bound and a minute to settle.
        let path = Path::new("/x");
        let check = response_policy(path, "c", ["a", "b"], path, "d");
        assert_eq!((check.timeout, check.grace), (seconds(210), seconds(60)));
        assert!(check.check);
    }
}
