//! Enrollment against shell stand-ins and recorded answers: no test contacts
//! a host. The stand-ins ask for the login password exactly as OpenSSH does,
//! through the askpass helper, and read a forced change from their input.

use super::super::command::{capture, controller_environment};
use super::super::files::tests::Scratch;
use super::super::secrets::tests::Typed;
use super::super::ssh::tests::target;
use super::super::tests::{PUBLIC, Recorded};
use super::*;
use std::cell::RefCell;
use std::os::unix::fs::MetadataExt;

const INITIAL: &str = "initial fixture";
const REPLACEMENT: &str = "replacement fixture";

fn passwords(private: &Scratch, checkout: &Scratch) -> (Password, Password) {
    let read = |name: &str, value: &str| {
        let path = private.file(name, value.as_bytes(), 0o600);
        password_value(Some(&path), "", &checkout.0, &Typed(None)).unwrap()
    };
    (read("initial", INITIAL), read("replacement", REPLACEMENT))
}

/// One session of `script` run by `/bin/sh` in place of the OpenSSH client.
fn session(private: &Scratch, script: &str, timeout: Duration) -> Result<Outcome> {
    let checkout = Scratch::new();
    let (initial, replacement) = passwords(private, &checkout);
    let client = OpenSsh {
        home: private.0.clone(),
        private: private.0.clone(),
        timeout,
    };
    let argv = ["/bin/sh", "-c", script].map(String::from);
    client.password_session(&argv, &initial, &replacement)
}

/// The stand-in's login: ask the helper as OpenSSH does and compare.
const LOGIN: &str = concat!(
    "login=$(\"$SSH_ASKPASS\" \"debian@host's password: \") || exit 9\n",
    "[ \"$login\" = 'initial fixture' ] || exit 8\n",
);

/// From `test_real_pty_forced_change_can_exit_before_enrollment`: Debian
/// PAM's successful rotation followed by the connection closing.
#[test]
fn forced_change_can_exit_before_enrollment() {
    let private = Scratch::new();
    let script = [
        LOGIN,
        "printf 'Changing password for debian.\\r\\nCurrent password: '; read -r a\n",
        "printf '\\r\\nNew password: '; read -r b\n",
        "printf '\\r\\nRetype new password: '; read -r c\n",
        "[ \"$a\" = 'initial fixture' ] || exit 7\n",
        "[ \"$b\" = 'replacement fixture' ] && [ \"$c\" = \"$b\" ] || exit 6\n",
        "echo 'passwd: password updated successfully'\n",
        "exit 1\n",
    ]
    .concat();
    let outcome = session(&private, &script, Duration::from_secs(10)).unwrap();
    let expected = Outcome {
        enrolled: false,
        rotated: true,
        exit_status: 1,
    };
    assert_eq!(outcome, expected);
    let left = std::fs::read_dir(&private.0).unwrap();
    let left = left.map(|entry| entry.unwrap().file_name().into_string().unwrap());
    let left = left.filter(|name| name.starts_with(".simplestchat-askpass-"));
    assert_eq!(left.count(), 0, "the askpass material is removed");
}

/// From `test_real_pty_drains_markers_after_large_final_burst`: a marker
/// written just before exit, after more than one read of output, is seen.
#[test]
fn markers_after_a_large_final_burst_are_drained() {
    let private = Scratch::new();
    let script = [
        LOGIN,
        "head -c 12000 /dev/zero | tr '\\0' x; echo\n",
        "echo SIMPLESTCHAT_BOOTSTRAP_KEY_ENROLLED\n",
    ]
    .concat();
    let outcome = session(&private, &script, Duration::from_secs(10)).unwrap();
    let expected = Outcome {
        enrolled: true,
        rotated: false,
        exit_status: 0,
    };
    assert_eq!(outcome, expected);
}

/// The helper answers only a password prompt, the login password never
/// reaches the stream's dialogue, and a second login prompt on the stream is
/// refused instead of answered.
#[test]
fn the_helper_answers_password_prompts_only_and_a_stream_login_is_refused() {
    let private = Scratch::new();
    let other = concat!(
        "\"$SSH_ASKPASS\" 'Are you sure you want to continue connecting (yes/no)? '",
        " && exit 1\n",
        "case \"$SIMPLESTCHAT_BOOTSTRAP_PASSWORD_FILE$SSH_ASKPASS\" in",
        " *fixture*) exit 2 ;; esac\n",
        "[ \"$SSH_ASKPASS_REQUIRE\" = force ] || exit 3\n",
        "exit 0\n",
    );
    let outcome = session(&private, other, Duration::from_secs(10)).unwrap();
    assert_eq!(outcome.exit_status, 0);
    let again = [LOGIN, "printf \"debian@host's password: \"; read -r a\n"].concat();
    let refused = session(&private, &again, Duration::from_secs(10));
    assert_eq!(refused.unwrap_err(), Refusal("repeated_password_prompt"));
}

/// From `test_timeout_reaps_child_and_does_not_print_transcript` and
/// `test_output_limit_discards_untrusted_banner`.
#[test]
fn a_stalled_or_noisy_session_is_stopped_with_a_fixed_refusal() {
    let private = Scratch::new();
    let started = Instant::now();
    let stalled = [LOGIN, "echo private-banner; sleep 20 & sleep 20\n"].concat();
    let stalled = session(&private, &stalled, Duration::from_millis(400));
    assert_eq!(stalled.unwrap_err(), Refusal("password_session_timeout"));
    assert!(started.elapsed() < Duration::from_secs(8));
    let noisy = "head -c 70000 /dev/zero | tr '\\0' x\n";
    let noisy = session(&private, noisy, Duration::from_secs(10));
    assert_eq!(noisy.unwrap_err(), Refusal("password_output_limit"));
}

/// From `test_enrollment_preserves_existing_authorized_keys_idempotently`:
/// only the generated POSIX script runs, in a disposable home.
#[test]
fn enrollment_preserves_existing_authorized_keys_idempotently() {
    let home = Scratch::new();
    let ssh = home.0.join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    let authorized = ssh.join("authorized_keys");
    let original = "ssh-ed25519 unrelated-key-without-trailing-newline";
    std::fs::write(&authorized, original).unwrap();
    let argv = ["/bin/sh", "-c", &enrollment_command(PUBLIC)].map(String::from);
    let environment = controller_environment(&home.0);
    for _ in 0..2 {
        let finished = capture(&argv, &home.0, &environment, Duration::from_secs(10));
        let finished = finished.unwrap();
        assert_eq!(finished.status, 0, "{}", finished.text);
        assert_eq!(finished.text, ENROLLED);
    }
    let text = std::fs::read_to_string(&authorized).unwrap();
    assert!(text.starts_with(original));
    assert_eq!(text.matches(PUBLIC).count(), 1);
    assert_eq!(authorized.metadata().unwrap().mode() & 0o7777, 0o600);
    assert_eq!(ssh.metadata().unwrap().mode() & 0o7777, 0o700);
}

/// A session double that returns queued outcomes and keeps what it was given.
struct Sessions {
    outcomes: RefCell<Vec<Outcome>>,
    calls: RefCell<Vec<(Vec<String>, String, String)>>,
}

impl Sessions {
    fn new(outcomes: &[Outcome]) -> Self {
        let mut outcomes = outcomes.to_vec();
        outcomes.reverse();
        Self {
            outcomes: RefCell::new(outcomes),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Session for Sessions {
    fn password_session(
        &self,
        argv: &[String],
        current: &Password,
        replacement: &Password,
    ) -> Result<Outcome> {
        let given = (current.expose().to_owned(), replacement.expose().to_owned());
        self.calls
            .borrow_mut()
            .push((argv.to_vec(), given.0, given.1));
        Ok(self
            .outcomes
            .borrow_mut()
            .pop()
            .expect("an unexpected session"))
    }
}

fn outcome(enrolled: bool, rotated: bool, exit_status: i32) -> Outcome {
    Outcome {
        enrolled,
        rotated,
        exit_status,
    }
}

/// From `test_key_success_never_reads_password_files_or_rotates`: key-first
/// reruns tolerate SSH banners and bypass every password operation.
#[test]
fn key_success_never_reads_password_files_or_rotates() {
    let private = Scratch::new();
    let checkout = Scratch::new();
    // Neither path is resolved: one names a directory that does not exist.
    let unread = private.0.join("absent/must-not-read");
    let uncreated = private.0.join("must-not-create");
    let credentials = Credentials {
        password_file: unread.to_str(),
        replacement_file: uncreated.to_str(),
        home: &private.0,
        current: &checkout.0,
    };
    let runner = Recorded::new(&[(0, "Legal banner\nSIMPLESTCHAT_KEY_READY")]);
    let sessions = Sessions::new(&[]);
    let access = enroll(
        &runner,
        &sessions,
        &Typed(None),
        &target(22),
        PUBLIC,
        credentials,
        &checkout.0,
    );
    assert_eq!(access.unwrap(), "existing_key");
    assert!(sessions.calls.borrow().is_empty());
    assert!(!private.0.join("must-not-create").exists());
    let probe = &runner.calls()[0];
    assert_eq!(probe[0], "/usr/bin/ssh");
    assert!(probe.contains(&"BatchMode=yes".to_owned()));
    assert_eq!(
        probe.last().unwrap(),
        "printf '%s\\n' SIMPLESTCHAT_KEY_READY"
    );
}

/// From `test_forced_change_reconnects_once_with_replacement`: a successful
/// PAM rotation with a nonzero SSH exit still proceeds to enrollment.
#[test]
fn forced_change_reconnects_once_with_replacement() {
    let private = Scratch::new();
    let checkout = Scratch::new();
    let initial = private.file("initial", INITIAL.as_bytes(), 0o600);
    let replacement = private.file("replacement", REPLACEMENT.as_bytes(), 0o600);
    let credentials = Credentials {
        password_file: initial.to_str(),
        replacement_file: replacement.to_str(),
        home: &private.0,
        current: &checkout.0,
    };
    let runner = Recorded::new(&[(255, ""), (0, "SIMPLESTCHAT_KEY_READY")]);
    let sessions = Sessions::new(&[outcome(false, true, 1), outcome(true, false, 0)]);
    let access = enroll(
        &runner,
        &sessions,
        &Typed(None),
        &target(22),
        PUBLIC,
        credentials,
        &checkout.0,
    );
    assert_eq!(access.unwrap(), "password_rotated_and_key_enrolled");
    let calls = sessions.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        (calls[0].1.as_str(), calls[0].2.as_str()),
        (INITIAL, REPLACEMENT)
    );
    assert_eq!(
        (calls[1].1.as_str(), calls[1].2.as_str()),
        (REPLACEMENT, REPLACEMENT)
    );
    for (argv, _, _) in calls.iter() {
        let line = argv.join(" ");
        assert!(!line.contains(INITIAL) && !line.contains(REPLACEMENT));
        assert!(argv.contains(&"PubkeyAuthentication=no".to_owned()));
        assert!(argv.contains(&"-tt".to_owned()));
        assert!(argv.last().unwrap().starts_with("sh -c "));
    }
    assert_eq!(std::fs::read_to_string(&replacement).unwrap(), REPLACEMENT);
}

/// From `test_failed_authentication_does_not_retry_or_provision`: a failed
/// password is not retried, limiting account lockouts. Without a replacement
/// file no password is sent at all.
#[test]
fn failed_authentication_does_not_retry() {
    let private = Scratch::new();
    let checkout = Scratch::new();
    let initial = private.file("initial", INITIAL.as_bytes(), 0o600);
    let replacement = private.file("replacement", REPLACEMENT.as_bytes(), 0o600);
    let credentials = Credentials {
        password_file: initial.to_str(),
        replacement_file: replacement.to_str(),
        home: &private.0,
        current: &checkout.0,
    };
    let runner = Recorded::new(&[(255, "")]);
    let sessions = Sessions::new(&[outcome(false, false, 255)]);
    let refused = enroll(
        &runner,
        &sessions,
        &Typed(None),
        &target(22),
        PUBLIC,
        credentials,
        &checkout.0,
    );
    assert_eq!(refused.unwrap_err(), Refusal("key_enrollment_failed"));
    assert_eq!(sessions.calls.borrow().len(), 1);
    let missing = Credentials {
        replacement_file: None,
        ..credentials
    };
    let silent = Sessions::new(&[]);
    let refused = enroll(
        &Recorded::new(&[(255, "")]),
        &silent,
        &Typed(None),
        &target(22),
        PUBLIC,
        missing,
        &checkout.0,
    );
    let expected = Refusal("replacement_password_file_required");
    assert_eq!(refused.unwrap_err(), expected);
    assert!(silent.calls.borrow().is_empty());
    let unverified = Sessions::new(&[outcome(true, false, 0)]);
    let refused = enroll(
        &Recorded::new(&[(255, ""), (255, "")]),
        &unverified,
        &Typed(None),
        &target(22),
        PUBLIC,
        credentials,
        &checkout.0,
    );
    let expected = Refusal("enrolled_key_verification_failed");
    assert_eq!(refused.unwrap_err(), expected);
}

/// Once a password is needed, a named file is resolved like every other
/// selected path: relative to the caller, `~/` to the home directory, and
/// never through a symbolic link.
#[test]
fn password_files_are_resolved_only_when_a_password_is_needed() {
    let private = Scratch::new();
    let checkout = Scratch::new();
    private.file("initial", INITIAL.as_bytes(), 0o600);
    private.file("replacement", REPLACEMENT.as_bytes(), 0o600);
    let home = Credentials {
        password_file: Some("~/initial"),
        replacement_file: Some("replacement"),
        home: &private.0,
        current: &private.0,
    };
    let runner = Recorded::new(&[(255, ""), (0, "SIMPLESTCHAT_KEY_READY")]);
    let sessions = Sessions::new(&[outcome(true, false, 0)]);
    let access = enroll(
        &runner,
        &sessions,
        &Typed(None),
        &target(22),
        PUBLIC,
        home,
        &checkout.0,
    );
    assert_eq!(access.unwrap(), "key_enrolled");
    let calls = sessions.calls.borrow();
    assert_eq!(
        (calls[0].1.as_str(), calls[0].2.as_str()),
        (INITIAL, REPLACEMENT)
    );
    std::os::unix::fs::symlink(private.0.join("initial"), private.0.join("link")).unwrap();
    let linked = Credentials {
        password_file: Some("link"),
        ..home
    };
    let refused = enroll(
        &Recorded::new(&[(255, "")]),
        &Sessions::new(&[]),
        &Typed(None),
        &target(22),
        PUBLIC,
        linked,
        &checkout.0,
    );
    assert_eq!(refused.unwrap_err(), Refusal("selected_path_is_symlink"));
}
