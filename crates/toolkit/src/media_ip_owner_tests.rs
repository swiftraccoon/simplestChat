//! Actual fresh process groups run only tiny owned fake route executables.
use super::*;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
struct Fixture {
    root: PathBuf,
    directory: PathBuf,
    environment: Environment,
}
impl Fixture {
    /// Fake `ip`, `route` and `ifconfig` commands running `body` in a root
    /// reached through a link. A shell replaces an inherited `PWD` that does
    /// not name its working directory with the directory's own path, which
    /// differs from the link's, so the fakes' `PWD` check sees only what the
    /// query passed.
    fn new(body: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "media query {} {}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let directory = std::env::temp_dir().join(format!("{name} directory"));
        fs::create_dir(&directory).unwrap();
        let root = std::env::temp_dir().join(name);
        std::os::unix::fs::symlink(&directory, &root).unwrap();
        fs::write(root.join("runner-env"), b"BEFORE=1\n").unwrap();
        fs::set_permissions(root.join("runner-env"), fs::Permissions::from_mode(0o600)).unwrap();
        let script = format!(
            concat!(
                "#!/bin/sh\n{prelude}root=${{FIXTURE_ROOT?}}\n",
                "record() {{\n",
                "  printf '%s' \"$2\" >\"$root/$1.pending\"\n",
                "  /bin/mv -f \"$root/$1.pending\" \"$root/$1\"\n",
                "}}\n",
                "printf '%s\\n' \"$(json_strings \"$0\" \"$@\")\" >>\"$root/calls\"\n",
                "[ \"${{LC_ALL?}}\" = C ] && [ \"${{PWD?}}\" = \"$root\" ] || exit 1\n",
                "[ \"${{CAPTURED_MARKER?}}\" = 'literal inherited value' ] || exit 1\n",
                "{body}\n"
            ),
            prelude = crate::fake_shell::PRELUDE,
            body = body
        );
        for name in ["ip", "route", "ifconfig"] {
            fs::write(root.join(name), &script).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let environment = [
            ("PATH".into(), root.clone().into_os_string()),
            ("FIXTURE_ROOT".into(), root.clone().into_os_string()),
            ("CAPTURED_MARKER".into(), "literal inherited value".into()),
            ("LC_ALL".into(), "inert-locale".into()),
            (
                "GITHUB_ENV".into(),
                root.join("runner-env").into_os_string(),
            ),
        ]
        .into();
        Self {
            root,
            directory,
            environment,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.root);
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}
const SUCCESS: &str = r#"
name=${0##*/}
if [ "$name" = route ]; then
  [ "$#" -eq 3 ] && [ "$1" = -n ] && [ "$2" = get ] && [ "$3" = default ] || exit 1
  printf 'interface: en0\n'
elif [ "$name" = ifconfig ]; then
  [ "$#" -eq 1 ] && [ "$1" = en0 ] || exit 1
  printf 'inet 192.0.2.44 netmask 0xffffff00\n'
elif [ "$#" -eq 5 ] && [ "$1" = -j ] && [ "$2" = -4 ] && [ "$3" = route ] &&
  [ "$4" = show ] && [ "$5" = default ]; then
  printf '[{"dst":"default","dev":"en0"}]\n'
else
  [ "$#" -eq 6 ] && [ "$1" = -j ] && [ "$2" = -4 ] && [ "$3" = addr ] && [ "$4" = show ] &&
    [ "$5" = dev ] && [ "$6" = en0 ] || exit 1
  printf '[{"addr_info":[{"family":"inet","local":"192.0.2.44"}]}]\n'
fi
"#;
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn actual_queries_keep_captured_context_and_publish_only_the_fixed_line() {
    let fixture = Fixture::new(SUCCESS);
    execute(
        &fixture.root,
        &fixture.environment,
        &mut signals(),
        true,
        Instant::now() + INVOCATION,
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read(fixture.root.join("runner-env")).unwrap(),
        b"BEFORE=1\nTEST_ANNOUNCE_IP=192.0.2.44\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        fixture.environment[std::ffi::OsStr::new("LC_ALL")],
        "inert-locale"
    );
}
#[tokio::test]
async fn published_line_is_not_a_late_signal_or_identity_success_receipt() {
    for status in [130, 143] {
        let fixture = Fixture::new(SUCCESS);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let result = execute_with(
            &fixture.root,
            &fixture.environment,
            &mut signals,
            true,
            Instant::now() + INVOCATION,
            || {
                sender.send(status).unwrap();
            },
        )
        .await;
        assert_eq!(result.unwrap_err().code, status);
        assert!(
            fs::read(fixture.root.join("runner-env"))
                .unwrap()
                .ends_with(b"TEST_ANNOUNCE_IP=192.0.2.44\n")
        );
    }
    let fixture = Fixture::new(SUCCESS);
    let result = execute_with(
        &fixture.root,
        &fixture.environment,
        &mut signals(),
        true,
        Instant::now() + INVOCATION,
        || {
            fs::rename(
                fixture.root.join("runner-env"),
                fixture.root.join("retired"),
            )
            .unwrap();
            fs::write(fixture.root.join("runner-env"), b"FOREIGN=1\n").unwrap();
        },
    )
    .await;
    assert_eq!(result.unwrap_err().code, 1);
    assert_eq!(
        fs::read(fixture.root.join("runner-env")).unwrap(),
        b"FOREIGN=1\n"
    );
}
#[tokio::test]
async fn malformed_file_or_unfit_schedule_acquires_no_route_process() {
    let fixture = Fixture::new(SUCCESS);
    fs::write(fixture.root.join("runner-env"), b"not newline framed").unwrap();
    assert_eq!(
        execute(
            &fixture.root,
            &fixture.environment,
            &mut signals(),
            true,
            Instant::now() + INVOCATION
        )
        .await
        .unwrap_err()
        .code,
        1
    );
    assert!(!fixture.root.join("calls").exists());
    for status in [124, 130, 143] {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if status != 124 {
            sender.send(status).unwrap();
        }
        let mut signals = Signals::injected(receiver);
        assert_eq!(
            detect_until(
                &fixture.root,
                &fixture.environment,
                &mut signals,
                Instant::now() + Duration::from_secs(20),
                FINAL_FENCE
            )
            .await
            .unwrap_err()
            .code,
            status
        );
        assert!(!fixture.root.join("calls").exists());
    }
}
#[tokio::test]
async fn bounded_output_nonzero_and_parser_failures_never_echo_private_tool_data() {
    for (body, expected) in [
        ("repeat PRIVATE_SENTINEL 10000\nprintf '\\n'", 1),
        ("printf 'PRIVATE_SENTINEL\\n'\nexit 42", 42),
        ("printf 'PRIVATE_SENTINEL\\n'", 1),
    ] {
        let fixture = Fixture::new(body);
        let result = detect(&fixture.root, &fixture.environment, &mut signals())
            .await
            .unwrap_err();
        assert_eq!(result.code, expected);
        assert!(!result.message.contains("PRIVATE_SENTINEL"));
        assert!(!result.message.contains(fixture.root.to_str().unwrap()));
    }
}
#[tokio::test]
async fn interruption_and_body_timeout_retire_admitted_query_before_return() {
    for status in [124, 130, 143] {
        let fixture = Fixture::new(
            r#"
trap 'record term observed; exit 0' TERM
record ready "$$"
while :; do /bin/sleep 0.01; done
"#,
        );
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            if status != 124 {
                sender.send(status).unwrap();
            }
        };
        let (result, ()) = tokio::join!(
            detect(&fixture.root, &fixture.environment, &mut signals),
            cancel
        );
        assert_eq!(result.unwrap_err().code, status);
        assert_eq!(
            fs::read_to_string(fixture.root.join("term")).unwrap(),
            "observed"
        );
    }
}
#[test]
fn argument_and_schedule_profiles_are_fixed_and_do_not_accept_runner_paths_or_keys() {
    assert!(
        run(Path::new("/"), &["--key".into(), "OTHER".into()])
            .is_ok_and(|code| code == ExitCode::from(2))
    );
    assert_eq!(QUERY_SLOT.as_secs(), 25);
    assert_eq!(DETECTION.as_secs(), 60);
    assert_eq!(INVOCATION.as_secs(), 90);
    assert_eq!(REPLAY_SLOT.as_secs(), 30);
    assert!(admission(Instant::now() + Duration::from_secs(29), REPLAY_SLOT).is_err());
}
