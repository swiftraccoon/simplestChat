//! One bounded child process: no input, both output streams combined, its
//! own process group, stopped at its deadline or output ceiling. Output is
//! returned to the caller or retained as evidence, never printed.

use super::files::{metadata, write_record};
use super::{Refusal, Result};
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The most combined output a captured command may produce.
const MAX_CAPTURE: usize = 2 * 1024 * 1024;
/// The most combined output a logged command may write to its log.
const MAX_LOG: u64 = 4 * 1024 * 1024;

/// The complete environment of a child; nothing is inherited.
pub(super) type Environment = BTreeMap<String, String>;

/// A command that ran to its own exit.
#[derive(Debug)]
pub(super) struct Finished {
    pub status: i32,
    pub text: String,
}

/// Excludes inherited interpreters, loaders, plugins, agents and shell hooks.
pub(super) fn controller_environment(home: &Path) -> Environment {
    let home = home.to_string_lossy().into_owned();
    [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".to_owned()),
        ("HOME", home),
        ("LC_ALL", "C".to_owned()),
        ("LANG", "C".to_owned()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}

/// Selects the checkout's Ansible and configuration without inherited
/// Ansible overrides. Ansible refuses the plain C locale, so it gets C.UTF-8.
pub(super) fn ansible_environment(root: &Path, home: &Path) -> Environment {
    let mut result = controller_environment(home);
    let tools = root.join("ops/ansible/.venv/bin");
    let path = format!("{}:{}", tools.display(), result["PATH"]);
    let config = root.join("ops/ansible/ansible.cfg");
    for (name, value) in [
        ("PATH", path),
        ("LC_ALL", "C.UTF-8".to_owned()),
        ("LANG", "C.UTF-8".to_owned()),
        ("ANSIBLE_CONFIG", config.display().to_string()),
        ("ANSIBLE_HOST_KEY_CHECKING", "True".to_owned()),
        ("ANSIBLE_RETRY_FILES_ENABLED", "False".to_owned()),
    ] {
        result.insert(name.to_owned(), value);
    }
    result
}

fn command(argv: &[String], cwd: &Path, environment: &Environment) -> Command {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(cwd).env_clear();
    command.envs(environment).stdin(Stdio::null());
    command.process_group(0);
    command
}

/// Stops the child's whole group and reaps the child.
fn stop(child: &mut Child) {
    let _ = kill_process_group(Pid::from_child(child), Signal::KILL);
    let _ = child.wait();
}

/// Waits for the child until `deadline` or until `halt` is raised; `None`
/// means it was stopped.
fn wait(child: &mut Child, deadline: Instant, halt: &AtomicBool) -> Option<i32> {
    loop {
        match child.try_wait() {
            // A signalled child has no code; no caller treats it as success.
            Ok(Some(status)) => return Some(status.code().unwrap_or(-1)),
            Ok(None) if Instant::now() < deadline && !halt.load(Ordering::Relaxed) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                stop(child);
                return None;
            }
        }
    }
}

/// Runs `argv` and returns its status with its combined, trimmed output.
/// A start failure, the deadline, too much output or output that is not
/// UTF-8 are all the same fixed refusal.
pub(super) fn capture(
    argv: &[String],
    cwd: &Path,
    environment: &Environment,
    timeout: Duration,
) -> Result<Finished> {
    let failed = |_: std::io::Error| Refusal("controller_command_failed");
    let (mut reader, writer) = std::io::pipe().map_err(failed)?;
    let mut command = command(argv, cwd, environment);
    command.stdout(writer.try_clone().map_err(failed)?);
    command.stderr(writer);
    let mut child = command.spawn().map_err(failed)?;
    // The reader sees the end of output only once no writer remains here.
    drop(command);
    let collector = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let limit = u64::try_from(MAX_CAPTURE + 1).unwrap();
        let read = (&mut reader).take(limit).read_to_end(&mut bytes);
        read.map(|_| bytes)
    });
    let status = wait(
        &mut child,
        Instant::now() + timeout,
        &AtomicBool::new(false),
    );
    let bytes = collector
        .join()
        .map_err(|_| Refusal("controller_command_failed"))?;
    let bytes = bytes.map_err(failed)?;
    let status = status.ok_or(Refusal("controller_command_failed"))?;
    if bytes.len() > MAX_CAPTURE {
        return Err(Refusal("controller_command_failed"));
    }
    let text = String::from_utf8(bytes);
    let text = text.map_err(|_| Refusal("controller_command_failed"))?;
    let text = text.trim().to_owned();
    Ok(Finished { status, text })
}

/// Runs `argv` with its output retained in `directory` as `<name>.log`,
/// beside a record of the command and of its outcome, and returns its
/// status. Nothing of the output is read back. The deadline and the log's
/// ceiling stop the command's whole group and refuse.
pub(super) fn logged(
    argv: &[String],
    cwd: &Path,
    environment: &Environment,
    timeout: Duration,
    directory: &Path,
    name: &str,
) -> Result<i32> {
    let mut options = OpenOptions::new();
    let options = options.write(true).create_new(true).mode(0o600);
    let log = directory.join(format!("{name}.log"));
    let mut log = options.open(log).map_err(metadata)?;
    let recorded = json!({"argv": argv, "timeoutSeconds": timeout.as_secs()});
    write_record(&directory.join(format!("{name}.command.json")), &recorded)?;
    let (mut reader, writer) = std::io::pipe().map_err(metadata)?;
    let mut command = command(argv, cwd, environment);
    command.stdout(writer.try_clone().map_err(metadata)?);
    command.stderr(writer);
    let started = Instant::now();
    let mut child = command.spawn().map_err(metadata)?;
    // The reader sees the end of output only once no writer remains here.
    drop(command);
    let limited = Arc::new(AtomicBool::new(false));
    let exceeded = Arc::clone(&limited);
    let copier = std::thread::spawn(move || {
        let (mut buffer, mut written) = ([0u8; 8192], 0u64);
        while let Ok(read @ 1..) = reader.read(&mut buffer) {
            written += u64::try_from(read).unwrap_or(u64::MAX);
            if written > MAX_LOG || log.write_all(&buffer[..read]).is_err() {
                exceeded.store(true, Ordering::Relaxed);
                break;
            }
        }
    });
    let pid = child.id();
    let status = wait(&mut child, started + timeout, &limited);
    let copied = copier.join();
    let limited = limited.load(Ordering::Relaxed) || copied.is_err();
    let outcome = json!({
        "pid": pid,
        "exitStatus": status,
        "timedOut": status.is_none() && !limited,
        "outputLimited": limited,
        "elapsedSeconds": started.elapsed().as_secs_f64(),
    });
    write_record(&directory.join(format!("{name}.outcome.json")), &outcome)?;
    let status = status.filter(|_| !limited);
    status.ok_or(Refusal("provisioning_interrupted_inspect_host"))
}

#[cfg(test)]
mod tests {
    use super::super::files::tests::Scratch;
    use super::*;

    fn shell(script: &str) -> Vec<String> {
        ["/bin/sh", "-c", script].map(String::from).to_vec()
    }

    fn environment() -> Environment {
        controller_environment(Path::new("/nonexistent-home"))
    }

    /// Status and both streams come back; nothing of the caller's environment
    /// reaches the child.
    #[test]
    fn capture_returns_status_and_combined_trimmed_output() {
        let scratch = Scratch::new();
        let script = "printf 'out\\n'; printf 'err\\n' >&2; echo \"$HOME:${CARGO-}\"; exit 3";
        let finished = capture(
            &shell(script),
            &scratch.0,
            &environment(),
            Duration::from_secs(10),
        );
        let finished = finished.unwrap();
        assert_eq!(finished.status, 3);
        let mut lines = finished.text.lines().collect::<Vec<_>>();
        lines.sort_unstable();
        assert_eq!(lines, ["/nonexistent-home:", "err", "out"]);
    }

    /// From `test_timeout_reaps_child_and_does_not_print_transcript` and
    /// `test_output_limit_discards_untrusted_banner`: a stuck or noisy child
    /// is stopped with its group and reported only as a fixed refusal.
    #[test]
    fn deadline_missing_program_and_excess_output_are_one_fixed_refusal() {
        let scratch = Scratch::new();
        let started = Instant::now();
        let stuck = capture(
            &shell("sleep 30 & sleep 30"),
            &scratch.0,
            &environment(),
            Duration::from_millis(200),
        );
        assert_eq!(stuck.unwrap_err(), Refusal("controller_command_failed"));
        assert!(started.elapsed() < Duration::from_secs(10));
        let missing = vec!["/nonexistent/program".to_owned()];
        let missing = capture(&missing, &scratch.0, &environment(), Duration::from_secs(5));
        assert_eq!(missing.unwrap_err(), Refusal("controller_command_failed"));
        let noisy = shell("head -c 3000000 /dev/zero | tr '\\0' x");
        let noisy = capture(&noisy, &scratch.0, &environment(), Duration::from_secs(30));
        assert_eq!(noisy.unwrap_err(), Refusal("controller_command_failed"));
    }

    /// Evidence keeps the command, its outcome and its whole output in
    /// private files that are never overwritten.
    #[test]
    fn logged_commands_retain_private_evidence() {
        use std::os::unix::fs::MetadataExt;
        let scratch = Scratch::new();
        let argv = shell("echo provisioning; exit 4");
        let status = logged(
            &argv,
            &scratch.0,
            &environment(),
            Duration::from_secs(10),
            &scratch.0,
            "01-ansible-playbook",
        );
        assert_eq!(status.unwrap(), 4);
        let log = scratch.0.join("01-ansible-playbook.log");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "provisioning\n");
        assert_eq!(log.metadata().unwrap().mode() & 0o7777, 0o600);
        let read = |name: &str| -> serde_json::Value {
            let text = std::fs::read(scratch.0.join(name)).unwrap();
            serde_json::from_slice(&text).unwrap()
        };
        let command = read("01-ansible-playbook.command.json");
        assert_eq!(command["argv"], json!(argv));
        assert_eq!(command["timeoutSeconds"], 10);
        let outcome = read("01-ansible-playbook.outcome.json");
        assert_eq!(outcome["exitStatus"], 4);
        assert_eq!(outcome["timedOut"], false);
        assert_eq!(outcome["outputLimited"], false);
        let noisy = shell("head -c 5000000 /dev/zero | tr '\\0' x; sleep 30");
        let stopped = logged(
            &noisy,
            &scratch.0,
            &environment(),
            Duration::from_secs(30),
            &scratch.0,
            "02-noisy",
        );
        let interrupted = Refusal("provisioning_interrupted_inspect_host");
        assert_eq!(stopped.unwrap_err(), interrupted);
        assert_eq!(read("02-noisy.outcome.json")["outputLimited"], true);
        let kept = scratch.0.join("02-noisy.log").metadata().unwrap().len();
        assert!(kept <= MAX_LOG, "{kept}");
        let slow = logged(
            &shell("sleep 30"),
            &scratch.0,
            &environment(),
            Duration::from_millis(200),
            &scratch.0,
            "03-slow",
        );
        assert_eq!(slow.unwrap_err(), interrupted);
        assert_eq!(read("03-slow.outcome.json")["timedOut"], true);
        let again = logged(
            &argv,
            &scratch.0,
            &environment(),
            Duration::from_secs(10),
            &scratch.0,
            "01-ansible-playbook",
        );
        assert!(again.is_err());
    }

    /// From `test_controller_environment_excludes_inherited_code_and_agent_hooks`.
    #[test]
    fn environments_are_closed_and_ansible_gets_the_checkout_configuration() {
        let home = Path::new("/home/operator");
        let plain = controller_environment(home);
        let names = plain.keys().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(names, ["HOME", "LANG", "LC_ALL", "PATH"]);
        assert_eq!(plain["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin");
        assert_eq!(
            (plain["LC_ALL"].as_str(), plain["LANG"].as_str()),
            ("C", "C")
        );
        let ansible = ansible_environment(Path::new("/checkout"), home);
        assert_eq!(
            ansible["PATH"],
            "/checkout/ops/ansible/.venv/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        );
        assert_eq!(
            ansible["ANSIBLE_CONFIG"],
            "/checkout/ops/ansible/ansible.cfg"
        );
        assert_eq!(ansible["ANSIBLE_HOST_KEY_CHECKING"], "True");
        assert_eq!(ansible["ANSIBLE_RETRY_FILES_ENABLED"], "False");
        assert_eq!(ansible["LC_ALL"], "C.UTF-8");
        assert_eq!(ansible.len(), 7);
        for forbidden in [
            "SSH_AUTH_SOCK",
            "PYTHONPATH",
            "LD_PRELOAD",
            "ANSIBLE_LIBRARY",
        ] {
            assert!(!ansible.contains_key(forbidden));
        }
    }
}
