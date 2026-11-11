//! Key enrollment: use the key when it already works; otherwise log in once
//! with the password, append the public key, and reconnect exactly once when
//! the host forced a password change first.
//!
//! The login password reaches OpenSSH through its own askpass mechanism and
//! the forced change is answered over the session's pipes, so no secret is
//! ever an argument, an environment value or part of an error.

use super::command::controller_environment;
use super::dialogue::{Dialogue, ENROLLED, Secret};
use super::files::{local_path, metadata, write_new};
use super::secrets::{Password, Terminal, password_value, replacement_password};
use super::ssh::{Authentication, Target, command, quote};
use super::{Refusal, Result, Runner, require};
use rustix::process::{Pid, Signal, kill_process_group};
use std::fs::{DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// The most output one password session may produce.
const MAX_TRANSCRIPT_BYTES: usize = 65536;
/// What the key probe prints when the enrolled key works.
const READY: &str = "SIMPLESTCHAT_KEY_READY";

/// What one password session established; nothing of its transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Outcome {
    pub enrolled: bool,
    pub rotated: bool,
    pub exit_status: i32,
}

/// One authenticated enrollment attempt.
pub(super) trait Session {
    /// Runs `argv`, answering the login with `current` and a forced change
    /// with `current` then `replacement`.
    fn password_session(
        &self,
        argv: &[String],
        current: &Password,
        replacement: &Password,
    ) -> Result<Outcome>;
}

/// The local password files an enrollment may read, as the operator named
/// them. They are resolved only once a password is actually needed.
#[derive(Clone, Copy)]
pub(super) struct Credentials<'a> {
    pub password_file: Option<&'a str>,
    pub replacement_file: Option<&'a str>,
    pub home: &'a Path,
    pub current: &'a Path,
}

/// The remote command that appends `public` to `authorized_keys` once,
/// keeping everything already there, and then prints the marker.
pub(super) fn enrollment_command(public: &str) -> String {
    let script = [
        "set -eu; umask 077; ",
        "test ! -L \"$HOME/.ssh\"; mkdir -p \"$HOME/.ssh\"; chmod 700 \"$HOME/.ssh\"; ",
        "file=\"$HOME/.ssh/authorized_keys\"; test ! -L \"$file\"; ",
        "if test -e \"$file\"; then test -f \"$file\"; else : > \"$file\"; fi; ",
        "chmod 600 \"$file\"; ",
        &format!("key={}; ", quote(public)),
        "if ! grep -qxF \"$key\" \"$file\"; then printf \"\\n%s\\n\" \"$key\" >> \"$file\"; fi; ",
        &format!("printf '%s\\n' {ENROLLED}"),
    ]
    .concat();
    format!("sh -c {}", quote(&script))
}

/// The private directory that holds one session's password file and the
/// helper OpenSSH runs to read it; removed when the session ends.
struct Askpass(PathBuf);

impl Askpass {
    const HELPER: &'static str = concat!(
        "#!/bin/sh\n",
        "# Answers OpenSSH's one password prompt from the controller's private file.\n",
        "case \"$1\" in *assword:*) ;; *) exit 1 ;; esac\n",
        "exec /bin/cat \"$SIMPLESTCHAT_BOOTSTRAP_PASSWORD_FILE\"\n",
    );

    fn create(parent: &Path, password: &Password) -> Result<Self> {
        let mut random = [0u8; 8];
        let mut source = std::fs::File::open("/dev/urandom").map_err(metadata)?;
        source.read_exact(&mut random).map_err(metadata)?;
        let suffix = random.iter().map(|byte| format!("{byte:02x}"));
        let name = format!(".simplestchat-askpass-{}", suffix.collect::<String>());
        let directory = parent.join(name);
        let mut builder = DirBuilder::new();
        builder.mode(0o700).create(&directory).map_err(metadata)?;
        let material = Self(directory);
        let secret = format!("{}\n", password.expose());
        write_new(&material.password(), secret.as_bytes())?;
        let mut options = OpenOptions::new();
        let options = options.write(true).create_new(true).mode(0o700);
        let mut helper = options.open(material.helper()).map_err(metadata)?;
        helper
            .write_all(Self::HELPER.as_bytes())
            .map_err(metadata)?;
        helper.sync_all().map_err(metadata)?;
        Ok(material)
    }

    fn password(&self) -> PathBuf {
        self.0.join("password")
    }

    fn helper(&self) -> PathBuf {
        self.0.join("askpass")
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Password sessions through the real OpenSSH client.
pub(super) struct OpenSsh {
    /// The caller's home directory, the only inherited location.
    pub home: PathBuf,
    /// A private directory outside the checkout for the askpass material.
    pub private: PathBuf,
    pub timeout: Duration,
}

impl OpenSsh {
    /// Reads the child's output and answers its prompts until the output
    /// ends and the child has exited.
    fn converse(
        &self,
        child: &mut Child,
        output: impl Read + Send + 'static,
        current: &Password,
        replacement: &Password,
    ) -> Result<Outcome> {
        let mut input = child
            .stdin
            .take()
            .ok_or(Refusal("password_session_failed"))?;
        let (sender, receiver) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut output = output;
            let mut buffer = [0u8; 4096];
            while let Ok(read) = output.read(&mut buffer) {
                if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut dialogue = Dialogue::after_login();
        let deadline = Instant::now() + self.timeout;
        let (mut pending, mut observed) = (String::new(), 0usize);
        let (mut ended, mut status) = (false, None);
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(chunk) => {
                    observed += chunk.len();
                    require(observed <= MAX_TRANSCRIPT_BYTES, "password_output_limit")?;
                    pending.push_str(&String::from_utf8_lossy(&chunk));
                    if let Some(secret) = dialogue.consume(&pending)? {
                        let secret = match secret {
                            Secret::Current => current,
                            Secret::Replacement => replacement,
                        };
                        let answer = format!("{}\n", secret.expose());
                        input.write_all(answer.as_bytes()).map_err(metadata)?;
                        input.flush().map_err(metadata)?;
                        pending.clear();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    ended = true;
                    // The output is complete; only the exit status is awaited.
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if status.is_none() {
                let exited = child.try_wait().map_err(metadata)?;
                status = exited.map(|status| status.code().unwrap_or(-1));
            }
            if let (true, Some(exit_status)) = (ended, status) {
                return Ok(Outcome {
                    enrolled: dialogue.enrolled,
                    rotated: dialogue.rotated,
                    exit_status,
                });
            }
        }
        Err(Refusal("password_session_timeout"))
    }
}

impl Session for OpenSsh {
    fn password_session(
        &self,
        argv: &[String],
        current: &Password,
        replacement: &Password,
    ) -> Result<Outcome> {
        require(!argv.is_empty(), "invalid_password_session")?;
        let material = Askpass::create(&self.private, current)?;
        let mut environment = controller_environment(&self.home);
        for (name, value) in [
            ("SSH_ASKPASS", material.helper()),
            ("SIMPLESTCHAT_BOOTSTRAP_PASSWORD_FILE", material.password()),
        ] {
            environment.insert(name.to_owned(), value.display().to_string());
        }
        // Use the helper even when the controller itself has a terminal.
        environment.insert("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned());
        let (reader, writer) = std::io::pipe().map_err(metadata)?;
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).env_clear().envs(&environment);
        command.stdin(Stdio::piped());
        command.stdout(writer.try_clone().map_err(metadata)?);
        command.stderr(writer).process_group(0);
        let mut child = command.spawn().map_err(metadata)?;
        // The reader sees the end of output only once no writer remains here.
        drop(command);
        let outcome = self.converse(&mut child, reader, current, replacement);
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = kill_process_group(Pid::from_child(&child), Signal::KILL);
            let _ = child.wait();
        }
        drop(material);
        outcome
    }
}

/// Prefers existing key access; otherwise performs the password enrollment,
/// with exactly one fresh login after a forced password change. Returns how
/// access was obtained.
pub(super) fn enroll(
    runner: &dyn Runner,
    session: &dyn Session,
    terminal: &dyn Terminal,
    target: &Target,
    public: &str,
    credentials: Credentials<'_>,
    root: &Path,
) -> Result<&'static str> {
    let probe = format!("printf '%s\\n' {READY}");
    let probe = command(target, &probe, Authentication::Key, false);
    let ready = || -> Result<bool> {
        let found = runner.run(&probe, Duration::from_secs(30))?;
        Ok(found.status == 0 && found.text.lines().any(|line| line == READY))
    };
    if ready()? {
        return Ok("existing_key");
    }
    let resolve = |named: &str| local_path(named, credentials.home, credentials.current);
    let saved = credentials.replacement_file;
    let saved = saved.ok_or(Refusal("replacement_password_file_required"))?;
    let replacement = replacement_password(&resolve(saved)?, root, terminal)?;
    let prompt = "Initial SSH password: ";
    let supplied = credentials.password_file.map(resolve).transpose()?;
    let initial = password_value(supplied.as_deref(), prompt, root, terminal)?;
    let login = enrollment_command(public);
    let login = command(target, &login, Authentication::Password, true);
    let first = session.password_session(&login, &initial, &replacement)?;
    if !first.enrolled && first.rotated {
        // Debian commonly closes the connection after PAM's forced change,
        // before the requested command starts. One fresh login uses the new
        // secret.
        let second = session.password_session(&login, &replacement, &replacement)?;
        let enrolled = second.enrolled && second.exit_status == 0;
        require(enrolled, "key_enrollment_failed_after_rotation")?;
    } else {
        let enrolled = first.enrolled && first.exit_status == 0;
        require(enrolled, "key_enrollment_failed")?;
    }
    require(ready()?, "enrolled_key_verification_failed")?;
    Ok(if first.rotated {
        "password_rotated_and_key_enrolled"
    } else {
        "key_enrolled"
    })
}

#[cfg(test)]
#[path = "enroll_tests.rs"]
mod tests;
