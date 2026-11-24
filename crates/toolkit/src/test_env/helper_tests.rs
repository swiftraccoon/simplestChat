//! Owned inert CLIs exercise the same setup/readiness helper as real tools.

use super::*;
use crate::owned_process::Signals;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(
            super::super::postgres::create_private_directory(
                &std::env::temp_dir(),
                "helper-owner.",
            )
            .unwrap(),
        )
    }
    /// `sh -c` running `script` after the shell prelude, with the owned
    /// directory as `$1`.
    fn command(&self, script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(format!("{}{script}", crate::fake_shell::PRELUDE))
            .arg("sh")
            .arg(&self.0)
            .stderr(Stdio::null());
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
async fn captures_bounded_output_and_preserves_nonzero_exit() {
    let fixture = Fixture::new();
    let output = run(
        fixture.command("printf 'fixed\\n'\nexit 23"),
        Duration::from_secs(5),
        Some(64),
        None,
    )
    .await
    .unwrap();
    assert_eq!(output.status, 23);
    assert_eq!(output.stdout, b"fixed\n");
    let error = run(
        fixture.command("repeat x 65\nprintf '\\n'"),
        Duration::from_secs(5),
        Some(64),
        None,
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("exceeded its bound"));
}

#[tokio::test]
async fn startup_interruption_retires_helper_descendants_and_preserves_status() {
    let fixture = Fixture::new();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    let command = fixture.command(
        r#"
root=$1
record() {
  printf '%s' "$2" >"$root/$1.pending"
  /bin/mv -f "$root/$1.pending" "$root/$1"
}
(
  trap '' TERM
  record worker "$(exec /bin/sh -c 'printf %s "$PPID"')"
  while :; do /bin/sleep 0.01; done
) &
trap 'record term observed; exit 0' TERM
while [ ! -e "$root/worker" ]; do /bin/sleep 0.01; done
record ready "$$"
while :; do /bin/sleep 0.01; done
"#,
    );
    let interrupt = async {
        ready(&fixture.0.join("ready")).await;
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(
        run(command, Duration::from_secs(10), None, Some(&mut signals)),
        interrupt
    );
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        143
    );
    assert!(fixture.0.join("term").is_file());
    let worker = fs::read_to_string(fixture.0.join("worker")).unwrap();
    let worker = rustix::process::Pid::from_raw(worker.parse().unwrap()).unwrap();
    let gone = rustix::process::test_kill_process(worker).is_err();
    // A grandchild can remain a zombie until the system adopter reaps it.
    if !gone {
        let status = Command::new("/bin/ps")
            .args([
                "-p",
                &worker.as_raw_nonzero().get().to_string(),
                "-o",
                "stat=",
            ])
            .output()
            .unwrap();
        assert!(
            String::from_utf8(status.stdout)
                .unwrap()
                .trim()
                .chars()
                .next()
                .is_none_or(|state| state == 'Z')
        );
    }
}

#[tokio::test]
async fn prior_cancellation_does_not_acquire_a_helper() {
    let fixture = Fixture::new();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(130).unwrap();
    let mut signals = Signals::injected(receiver);
    let result = run(
        fixture.command(": >\"$1/started\""),
        Duration::from_secs(5),
        None,
        Some(&mut signals),
    )
    .await;
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        130
    );
    assert!(!fixture.0.join("started").exists());
}

#[tokio::test]
async fn cancellable_in_memory_readiness_does_not_wait_for_its_deadline() {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    sender.send(130).unwrap();
    let result = wait(&mut signals, std::future::pending::<()>()).await;
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        130
    );
}

#[tokio::test]
async fn zero_exited_leader_retires_stdout_holding_worker_before_final_read() {
    let fixture = Fixture::new();
    let command = fixture.command(
        r#"
root=$1
(
  trap '' TERM
  printf '%s' "$(exec /bin/sh -c 'printf %s "$PPID"')" >"$root/worker.pending"
  /bin/mv -f "$root/worker.pending" "$root/worker"
  while :; do /bin/sleep 0.01; done
) &
while [ ! -e "$root/worker" ]; do /bin/sleep 0.01; done
printf 'closed\n'
exit 0
"#,
    );
    let output = run(command, Duration::from_secs(5), Some(64), None)
        .await
        .unwrap();
    assert_eq!(output.status, 0);
    assert_eq!(output.stdout, b"closed\n");
    assert!(fixture.0.join("worker").is_file());
}

#[tokio::test]
async fn signal_overflow_and_timeout_close_the_inline_pipe_before_error_return() {
    for expected in [1, 124, 130, 143] {
        let fixture = Fixture::new();
        let script = if expected == 1 {
            "repeat x 1024\nprintf '\\n'"
        } else {
            ": >\"$1/ready\"\n/bin/sleep 30"
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let closed = std::cell::Cell::new(false);
        let cancel = async {
            if matches!(expected, 130 | 143) {
                ready(&fixture.0.join("ready")).await;
                sender.send(expected).unwrap();
            }
        };
        let timeout = if expected == 124 {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(5)
        };
        let mut dispatched = false;
        let (result, ()) = tokio::join!(
            run_owned(
                fixture.command(script),
                timeout,
                Some(64),
                Some(&mut signals),
                &mut dispatched,
                |reader| {
                    reader.close_with(|file| {
                        nix::unistd::close(file)?;
                        closed.set(true);
                        Ok(())
                    })
                }
            ),
            cancel
        );
        assert_eq!(
            result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
            expected
        );
        assert!(closed.get(), "inline pipe explicitly closed for{expected}");
    }
}

#[tokio::test]
async fn capture_close_failure_refuses_zero_and_does_not_replace_known_command_failure() {
    let fixture = Fixture::new();
    for status in [0, 42] {
        let result = run_owned(
            fixture.command(&format!("printf 'fixed\\n'\nexit {status}")),
            Duration::from_secs(5),
            Some(64),
            None,
            &mut false,
            |reader| {
                reader.close_with(|file| {
                    nix::unistd::close(file)?;
                    Err(stopped(1, "injected pipe close failure"))
                })
            },
        )
        .await;
        let error = result.err().unwrap();
        if status == 0 {
            assert!(error.to_string().contains("close"));
        } else {
            assert_eq!(error.downcast_ref::<Stop>().unwrap().code, 42);
        }
    }
}

#[tokio::test]
async fn pipe_admission_rejects_nonpipes_and_write_only_ends_without_retaining_their_fds() {
    use std::os::fd::AsRawFd;

    let fixture = Fixture::new();
    let file = File::create(fixture.0.join("not-a-pipe")).unwrap();
    let descriptor = file.as_raw_fd();
    assert!(Reader::new(file, 64).is_err());
    assert!(!Path::new(&format!("/dev/fd/{descriptor}")).exists());

    let (read, write) = nix::unistd::pipe().unwrap();
    let descriptor = write.as_raw_fd();
    assert!(Reader::new(File::from(write), 64).is_err());
    // Rejection consumes the admitted end through Tokio's Drop path. This
    // proves observed closure, not a checked close result from that library.
    assert!(!Path::new(&format!("/dev/fd/{descriptor}")).exists());
    assert!(rustix::fs::fcntl_getfl(&read).is_ok());
    nix::unistd::close(read).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_pending_read_resumes_original_pipe_without_false_eof_or_lost_bytes() {
    use std::io::Write;

    let (read, write) = nix::unistd::pipe().unwrap();
    let mut reader = Reader::new(File::from(read), 64).unwrap();
    let mut writer = File::from(write);
    let compact;
    let premature;
    {
        let pending = reader.read_once();
        compact = std::mem::size_of_val(&pending) < 4096;
        tokio::pin!(pending);
        premature = tokio::select! {
            biased;
            result = &mut pending => Some(result),
            _ = tokio::task::yield_now() => None,
        };
        // Cancelling this one pending read returns its mutable borrow. No bytes
        // were accepted and the Reader still owns the same original pipe.
    }
    let stayed_pending = !reader.complete && reader.bytes.is_empty();
    writer.write_all(b"later original pipe bytes\n").unwrap();
    nix::unistd::close(writer).unwrap();
    let finishing = reader.finish();
    let finish_compact = std::mem::size_of_val(&finishing) < 4096;
    let finished = tokio::time::timeout(Duration::from_secs(5), finishing).await;
    let complete = reader.complete;
    let bytes = reader.bytes.clone();
    let closed = reader.close();
    finished.unwrap().unwrap();
    closed.unwrap();
    assert!(compact && finish_compact);
    assert!(premature.is_none());
    assert!(stayed_pending && complete);
    assert_eq!(bytes, b"later original pipe bytes\n");
}
