//! `simplestchat-ci release-fixture`: the app-only release and rollback check
//! on a fresh disposable Linux Docker host, run as root by `release-checks`.
//!
//! It occupies the production service paths only after proving them absent,
//! refuses any existing container, publishes services only on loopback, and
//! drives the installed `simplestchat-ops release stage|deploy` through a
//! real replacement, a deliberately failed start with its rollback, a
//! browser rollover and the passive response policy. Private fixture files
//! and derived images remain for inspection; only verified owned containers,
//! their network and the fixture CA trust entry are removed. `report.json` is
//! the only file meant for upload. No reboot or physical media is exercised.

mod execute;
mod files;
mod host;
mod images;
mod json;
mod native;
mod plan;
mod policy;
mod render;
mod report;
#[cfg(test)]
mod tests;

use host::{Commands, Paths, Runner};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

const USAGE: &str = concat!(
    "usage: simplestchat-ci release-fixture --disposable-host --image SELECTOR ",
    "--output NEW_DIRECTORY --node ABSOLUTE --chromium ABSOLUTE ",
    "--http-policy-binary ABSOLUTE --ansible ABSOLUTE",
);
/// The bound on bootstrap and every case together.
const WORK: Duration = Duration::from_secs(660);
/// The bound on cleanup, which every cleanup command's stop tail fits inside.
const CLEANUP: Duration = Duration::from_secs(100);

/// The command line: explicit disposable-host intent, one local image
/// selector and absolute tool paths, each given exactly once.
#[derive(Debug, PartialEq, Eq)]
struct Options {
    image: String,
    output: PathBuf,
    node: PathBuf,
    chromium: PathBuf,
    http_policy: PathBuf,
    ansible: PathBuf,
}

/// One local image selector: up to 256 characters of a reference or an ID.
fn selector(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 256
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/:@-".contains(byte))
}

impl Options {
    fn parse(arguments: &[String]) -> Option<Self> {
        let mut values: [Option<&String>; 6] = [None; 6];
        let names = [
            "--image",
            "--output",
            "--node",
            "--chromium",
            "--http-policy-binary",
            "--ansible",
        ];
        let mut disposable = false;
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            if word == "--disposable-host" && !disposable {
                disposable = true;
                continue;
            }
            let slot = names.iter().position(|name| name == word)?;
            if values[slot].is_some() {
                return None;
            }
            values[slot] = Some(words.next()?);
        }
        let [
            Some(image),
            Some(output),
            Some(node),
            Some(chromium),
            Some(policy),
            Some(ansible),
        ] = values
        else {
            return None;
        };
        if !disposable || !selector(image) {
            return None;
        }
        Some(Self {
            image: image.clone(),
            output: output.into(),
            node: node.into(),
            chromium: chromium.into(),
            http_policy: policy.into(),
            ansible: ansible.into(),
        })
    }
}

/// `count` random bytes from the kernel, as lowercase hexadecimal digits.
fn random(count: usize) -> std::io::Result<String> {
    let mut bytes = vec![0; count];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(files::hex(&bytes))
}

/// Run setup, the browser rollover and both release cases, then cleanup in
/// its own window whatever happened, and publish the report. The fixture
/// passes only when every case and cleanup passed and nothing interrupted it.
async fn lifecycle<R: Runner>(fixture: &mut execute::Fixture<R>) -> bool {
    let work = async {
        fixture.setup().await?;
        fixture.browser().await?;
        fixture.exercise().await
    };
    match work.await {
        Ok(()) => {
            fixture.report.phase = report::Phase::Complete;
            fixture.report.passed = true;
        }
        Err(fault) => fixture.report.failure = Some(fault.to_string()),
    }
    let end = fixture.commands.now() + CLEANUP;
    fixture.commands.cleanup(end);
    fixture.cleanup().await;
    let interrupted = fixture.commands.interrupted || fixture.commands.runner.latched();
    fixture.report.interrupted = interrupted;
    fixture.report.passed = fixture.report.passed && fixture.report.cleanup_passed && !interrupted;
    fixture.report.last_nonzero_command = fixture.commands.last_failure.clone();
    fixture.report.last_command = fixture.commands.last_started.clone();
    fixture.report.passed
}

/// Write the public report, the one file other users may read.
fn publish(output: &Path, report: &report::Report) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = output.join("report.json");
    files::write_json(&path, report)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o755))
}

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let Some(options) = Options::parse(arguments) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let paths = Paths::fixed(root);
    let output = match files::fresh_output(&options.output, &paths.service()) {
        Ok(output) => output,
        Err(message) => {
            eprintln!("{message}");
            return Ok(ExitCode::FAILURE);
        }
    };
    let browser = [&options.node, &options.chromium];
    if !browser
        .iter()
        .all(|path| path.is_absolute() && path.is_file())
        || !options.ansible.is_absolute()
    {
        eprintln!("Browser and renderer tools must be explicit existing absolute files");
        return Ok(ExitCode::FAILURE);
    }
    let Ok(policy) = simplestchat_toolkit::executable::Executable::open(&options.http_policy)
    else {
        eprintln!("HTTP policy binary is not an ordinary executable");
        return Ok(ExitCode::FAILURE);
    };
    let generated = random(16).and_then(|token| {
        let secrets: Vec<String> = (0..6).map(|_| random(32)).collect::<std::io::Result<_>>()?;
        Ok((token, secrets))
    });
    let Ok((token, secrets)) = generated else {
        eprintln!("Fixture identity unavailable");
        return Ok(ExitCode::FAILURE);
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let passed = runtime.block_on(async {
        let signals = simplestchat_toolkit::owned_process::Signals::new()?;
        let native = native::Native::new(signals, root, policy);
        let started = native.now();
        let commands = Commands::new(native, &output.join("private"), started + WORK);
        let inputs = execute::Inputs {
            image: options.image,
            node: options.node,
            chromium: options.chromium,
            http_policy: options.http_policy,
            ansible: options.ansible,
            token,
            secrets: secrets.try_into().expect("six secrets were generated"),
        };
        let mut fixture = execute::Fixture::new(commands, paths, inputs, &output);
        let passed = lifecycle(&mut fixture).await;
        fixture.report.seconds =
            host::rounded(fixture.commands.now().saturating_duration_since(started));
        let published = publish(&output, &fixture.report);
        let text = serde_json::to_string(&fixture.report).unwrap_or_default();
        println!("{text}");
        println!(
            "Private fixture data and images retained; verified owned containers and fixture \
             trust entry cleaned: {}",
            fixture.report.cleanup_passed
        );
        let latched = fixture.commands.runner.latched();
        let closed = fixture.into_runner().finish();
        anyhow::Ok(passed && published.is_ok() && closed.is_ok() && !latched)
    })?;
    Ok(if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
