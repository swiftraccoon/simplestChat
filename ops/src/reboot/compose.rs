//! The comparison of a candidate Compose executable with the installed
//! Compose before a runtime upgrade stops anything: the candidate must be
//! the selected version and give the public project the same resolved
//! configuration and the same identity of each of its three services. Only
//! fixed commands are run, each with a 20 second deadline, a fixed
//! environment and at most 8 MiB of output; nothing they print leaves this
//! comparison, because the resolved configuration carries credentials.

use crate::host::command::{Commands, Request};
use crate::json::{self, Json};
use crate::text::python_strip;

/// The installed Compose, ignoring inherited Docker selectors.
const INSTALLED: [&str; 4] = [
    "/usr/bin/docker",
    "--host",
    "unix:///var/run/docker.sock",
    "compose",
];
/// The public project as the installed configuration names it.
const PROJECT: [&str; 8] = [
    "--project-name",
    "simplestchat-public",
    "--project-directory",
    "/etc/simplestchat-public",
    "--env-file",
    "/etc/simplestchat-public/app.env",
    "-f",
    "/etc/simplestchat-public/compose.public.yml",
];
/// The whole environment of each command.
const ENVIRONMENT: [(&str, &str); 3] = [
    ("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"),
    ("LC_ALL", "C"),
    ("DOCKER_HOST", "unix:///var/run/docker.sock"),
];
/// The services whose identities must not change, in the order compared.
const SERVICES: [&str; 3] = ["postgres", "simplestchat", "caddy"];
const SECONDS: f64 = 20.0;
/// The most output one command may give.
const LIMIT: usize = 8 * 1024 * 1024;

/// A command that failed, outlived its deadline, said too much or said
/// something that is not its answer.
pub(super) const FAILED: &str = "Compose compatibility command failed; no services were stopped";
pub(super) const VERSION: &str = "Candidate Compose does not match the selected package";
pub(super) const CONFIGURATION: &str =
    "Candidate Compose changes resolved configuration; separate migration required";
pub(super) const IDENTITY: &str =
    "Candidate Compose changes a service identity; separate migration required";
/// What a comparison that found no difference says.
pub(super) const PRESERVED: &str =
    "Candidate Compose preserves configuration and all three service identities";

/// Compare the Compose at `candidate` with the installed one through
/// `commands`, in this order: the candidate's version must be `version`
/// once Python's white space is stripped; the installed and then the
/// candidate resolved configuration must be equal JSON values as Python
/// compares them; for each service the installed and then the candidate
/// identity line must be the same bytes. The first difference or failure
/// refuses with its fixed text and runs nothing after it. Blocks for at
/// most 20 seconds per command, nine commands in all.
pub(super) fn compare(
    commands: &dyn Commands,
    candidate: &str,
    version: &str,
) -> Result<(), &'static str> {
    let run = |prefix: &[&str], arguments: &[&str]| {
        let argv = prefix
            .iter()
            .chain(arguments)
            .map(|word| (*word).to_owned());
        let request = Request {
            environment: &ENVIRONMENT,
            ..Request::fixed(argv.collect(), SECONDS, LIMIT, FAILED)
        };
        commands.run(&request).map_err(|_| FAILED)
    };
    let said = run(&[candidate], &["version", "--short"])?;
    let said = std::str::from_utf8(&said).map_err(|_| FAILED)?;
    if python_strip(said) != version {
        return Err(VERSION);
    }
    let resolved = [PROJECT.as_slice(), &["config", "--format", "json"]].concat();
    let installed = configuration(&run(&INSTALLED, &resolved)?)?;
    let candidates = configuration(&run(&[candidate], &resolved)?)?;
    if !installed.semantic_eq(&candidates) {
        return Err(CONFIGURATION);
    }
    for service in SERVICES {
        let identity = [PROJECT.as_slice(), &["config", "--hash", service]].concat();
        if run(&INSTALLED, &identity)? != run(&[candidate], &identity)? {
            return Err(IDENTITY);
        }
    }
    Ok(())
}

/// A resolved configuration: one JSON value without duplicate keys or
/// non-finite numbers, in UTF-8 or the UTF-16 and UTF-32 forms Python's
/// JSON reader detects.
fn configuration(bytes: &[u8]) -> Result<Json, &'static str> {
    json::decode_limited(bytes, LIMIT).map_err(|_| FAILED)
}
