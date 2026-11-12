//! One long-running child spoken to in turns: bytes in, bounded pieces out,
//! every wait under the caller's deadline. The child leads its own process
//! group, which is killed and reaped when the conversation ends, however it
//! ends; its standard error goes nowhere.

use super::failure::{Failure, Result, require};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The most one reading returns.
pub(crate) const PIECE: usize = 65536;
/// How long a child may take to leave once its input is closed.
pub(super) const GRACE: Duration = Duration::from_secs(4);
/// The longest single wait, so that a stop or an exit is noticed.
const SLICE: Duration = Duration::from_secs(1);
/// How long a parting word may take.
const FAREWELL: Duration = Duration::from_secs(1);

/// What a wait for the child's output found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// The next bytes it wrote, at most [`PIECE`].
    Bytes(Vec<u8>),
    /// Nothing in time.
    Quiet,
    /// It closed its output.
    Closed,
}

/// A conversation with one child.
pub(crate) trait Dialogue {
    /// Write all of `payload` within `within`: [`Failure::Timeout`] when the
    /// child does not take it in time, [`Failure::Command`] when it has
    /// exited and [`Failure::Os`] when it no longer reads.
    fn send(&mut self, payload: &[u8], within: Duration) -> Result<()>;

    /// Wait for output, at most `within` and at most a second.
    fn hear(&mut self, within: Duration) -> Result<Heard>;

    /// Whether the child is still running.
    fn alive(&mut self) -> bool;

    /// Say `farewell` if the child still runs, close its input, give it four
    /// seconds to leave and let nothing of its group outlive this call.
    fn end(self: Box<Self>, farewell: &[u8]);
}

/// The flag that stops a conversation, and the refusal that says so.
pub(super) type Stop<'a> = Option<(&'a AtomicBool, &'static str)>;

/// A child of this process.
pub(super) struct Running<'a> {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
    stop: Stop<'a>,
    grace: Duration,
}

/// Whether `descriptor` became ready for `interest` within `within`.
fn ready(descriptor: impl AsFd, interest: PollFlags, within: Duration) -> Result<bool> {
    let mut watched = [PollFd::new(&descriptor, interest)];
    let within = Timespec {
        tv_sec: i64::try_from(within.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(within.subsec_nanos()),
    };
    match poll(&mut watched, Some(&within)) {
        Ok(count) => Ok(count > 0),
        Err(rustix::io::Errno::INTR) => Ok(false),
        Err(_) => Err(Failure::Os),
    }
}

/// Start `argv` under exactly `environment`.
pub(super) fn open<'a>(
    argv: &[String],
    environment: &[(&str, &str)],
    stop: Stop<'a>,
    grace: Duration,
) -> Result<Running<'a>> {
    let (program, arguments) = argv.split_first().ok_or(Failure::Command)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .envs(environment.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().map_err(|_| Failure::Command)?;
    let (input, output) = (child.stdin.take(), child.stdout.take());
    let Some(output) = output else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Failure::Command);
    };
    let running = Running {
        child,
        input,
        output,
        stop,
        grace,
    };
    // A full pipe must never hold the caller past its deadline.
    let input = running.input.as_ref().ok_or(Failure::Command)?;
    let flags = fcntl_getfl(input).map_err(|_| Failure::Os)?;
    fcntl_setfl(input, flags | OFlags::NONBLOCK).map_err(|_| Failure::Os)?;
    Ok(running)
}

impl Running<'_> {
    fn stopped(&self) -> Result<()> {
        match self.stop {
            Some((stop, refusal)) => require(!stop.load(Ordering::SeqCst), refusal),
            None => Ok(()),
        }
    }
}

impl Drop for Running<'_> {
    /// No member of the group keeps running, and the child is reaped. The
    /// child is also killed by its own identity, so that a group which could
    /// not be signalled never leaves the wait without an end.
    fn drop(&mut self) {
        self.input = None;
        if let Ok(group) = i32::try_from(self.child.id()).map(Pid::from_raw) {
            let _ = killpg(group, Signal::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Dialogue for Running<'_> {
    fn send(&mut self, payload: &[u8], within: Duration) -> Result<()> {
        let deadline = Instant::now() + within;
        let mut remaining = payload;
        while !remaining.is_empty() {
            self.stopped()?;
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::Timeout);
            }
            if !self.alive() {
                return Err(Failure::Command);
            }
            let input = self.input.as_mut().ok_or(Failure::Os)?;
            if !ready(&*input, PollFlags::OUT, left.min(SLICE))? {
                continue;
            }
            match input.write(remaining) {
                Ok(0) => return Err(Failure::Os),
                Ok(count) => remaining = &remaining[count..],
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(_) => return Err(Failure::Os),
            }
        }
        Ok(())
    }

    fn hear(&mut self, within: Duration) -> Result<Heard> {
        self.stopped()?;
        if !ready(&self.output, PollFlags::IN, within.min(SLICE))? {
            return Ok(Heard::Quiet);
        }
        let mut piece = vec![0; PIECE];
        match self.output.read(&mut piece) {
            Ok(0) => Ok(Heard::Closed),
            Ok(count) => {
                piece.truncate(count);
                Ok(Heard::Bytes(piece))
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => Ok(Heard::Quiet),
            Err(_) => Err(Failure::Os),
        }
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn end(mut self: Box<Self>, farewell: &[u8]) {
        if self.alive() {
            let _ = self.send(farewell, FAREWELL);
        }
        self.input = None;
        let deadline = Instant::now() + self.grace;
        while self.alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Dropping kills whatever is left of the group and reaps the child.
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nix::sys::signal::kill;

    fn shell<'a>(script: &str, stop: Stop<'a>) -> Running<'a> {
        let argv = ["/bin/sh", "-c", script].map(str::to_owned);
        let environment = [("PATH", "/usr/bin:/bin"), ("MARK", "fixed")];
        open(&argv, &environment, stop, Duration::from_millis(300)).unwrap()
    }

    /// Everything the child says before it is quiet or gone.
    fn said(child: &mut Running<'_>, until: &[u8]) -> Vec<u8> {
        let mut heard = Vec::new();
        let started = Instant::now();
        while !heard.ends_with(until) && started.elapsed() < Duration::from_secs(5) {
            match child.hear(Duration::from_millis(200)).unwrap() {
                Heard::Bytes(piece) => heard.extend(piece),
                Heard::Quiet => {}
                Heard::Closed => break,
            }
        }
        heard
    }

    /// Each turn is answered in order, under only the given environment, and
    /// a child that reads to the end of its input leaves on its own.
    #[test]
    fn a_child_answers_each_turn_and_leaves_when_its_input_closes() {
        let script = "printf '%s:%s\\n' \"$MARK\" \"${HOME-unset}\"; \
                      while read line; do echo \"got:$line\"; done; echo bye";
        let mut child = shell(script, None);
        assert_eq!(said(&mut child, b"\n"), b"fixed:unset\n");
        assert_eq!(child.hear(Duration::from_millis(50)), Ok(Heard::Quiet));
        child.send(b"one\n", Duration::from_secs(2)).unwrap();
        assert_eq!(said(&mut child, b"\n"), b"got:one\n");
        child.send(b"two\nthree\n", Duration::from_secs(2)).unwrap();
        assert_eq!(said(&mut child, b"three\n"), b"got:two\ngot:three\n");
        assert!(child.alive());
        let started = Instant::now();
        Box::new(child).end(b"last\n");
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    /// The end of the child's output is told apart from silence, and a child
    /// that has exited is not written to.
    #[test]
    fn a_closed_output_and_an_exited_child_are_named() {
        let mut child = shell("echo bye", None);
        assert_eq!(said(&mut child, b"\n"), b"bye\n");
        assert_eq!(child.hear(Duration::from_secs(2)), Ok(Heard::Closed));
        let started = Instant::now();
        while child.alive() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let refused = child.send(b"late\n", Duration::from_secs(2));
        assert_eq!(refused, Err(Failure::Command));
        assert_eq!(child.send(b"", Duration::ZERO), Ok(()));
        let argv = ["/nonexistent/simplestchat-tool".to_owned()];
        assert!(open(&argv, &[], None, GRACE).is_err());
        assert!(open(&[], &[], None, GRACE).is_err());
    }

    /// A child that never reads cannot hold the caller: more than a pipe
    /// takes is the deadline's failure, and a wait for output ends in time.
    #[test]
    fn a_child_that_neither_reads_nor_writes_cannot_hold_the_caller() {
        let mut child = shell("sleep 30", None);
        let started = Instant::now();
        let flood = vec![b'x'; 4 * 1024 * 1024];
        let refused = child.send(&flood, Duration::from_millis(300));
        assert_eq!(refused, Err(Failure::Timeout));
        assert_eq!(child.hear(Duration::from_millis(100)), Ok(Heard::Quiet));
        // A long wait is cut into pieces of a second.
        assert_eq!(child.hear(Duration::from_secs(600)), Ok(Heard::Quiet));
        let waited = started.elapsed();
        assert!(Duration::from_millis(1300) <= waited && waited < Duration::from_secs(5));
        let started = Instant::now();
        Box::new(child).end(b"ignored\n");
        // It ignored the end of its input for the whole grace, then died.
        let waited = started.elapsed();
        assert!(Duration::from_millis(300) <= waited && waited < Duration::from_secs(3));
    }

    /// Ending kills every member of the child's group, also one the child
    /// left running behind an exit of its own.
    #[test]
    fn nothing_of_the_group_outlives_the_conversation() {
        let mut child = shell("sleep 30 & echo $!; read line", None);
        let text = String::from_utf8(said(&mut child, b"\n")).unwrap();
        let member = Pid::from_raw(text.trim().parse().unwrap());
        assert!(kill(member, None).is_ok());
        Box::new(child).end(b"go\n");
        let started = Instant::now();
        while kill(member, None).is_ok() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(kill(member, None).is_err());
        // The same when the conversation is dropped without a word, and
        // the child itself is reaped.
        let mut child = shell("sleep 30 & echo $!; sleep 30", None);
        let text = String::from_utf8(said(&mut child, b"\n")).unwrap();
        let member = Pid::from_raw(text.trim().parse().unwrap());
        let leader = Pid::from_raw(i32::try_from(child.child.id()).unwrap());
        assert!(kill(leader, None).is_ok());
        drop(child);
        assert!(kill(leader, None).is_err());
        let started = Instant::now();
        while kill(member, None).is_ok() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(kill(member, None).is_err());
    }

    /// What a child says on its standard error goes nowhere and never
    /// blocks it, and the parting word reaches a child that still listens.
    #[test]
    fn complaints_go_nowhere_and_the_parting_word_arrives() {
        let mut child = shell("head -c 1000000 /dev/zero >&2; echo done; cat", None);
        assert_eq!(said(&mut child, b"\n"), b"done\n");
        child.send(b"word\n", Duration::from_secs(2)).unwrap();
        assert_eq!(said(&mut child, b"\n"), b"word\n");
        drop(child);
        let kept = std::env::temp_dir().join(format!("dialogue-farewell-{}", std::process::id()));
        let _ = std::fs::remove_file(&kept);
        let script = format!("cat > {}", kept.to_str().unwrap());
        let child = shell(&script, None);
        Box::new(child).end(b"farewell\n");
        assert_eq!(std::fs::read(&kept).unwrap(), b"farewell\n");
        std::fs::remove_file(&kept).unwrap();
    }

    /// A raised stop refuses every further turn in the caller's words.
    #[test]
    fn a_stop_refuses_further_turns_in_the_callers_words() {
        let stop = AtomicBool::new(false);
        let mut child = shell("cat", Some((&stop, "stopped_by_signal")));
        child.send(b"one\n", Duration::from_secs(2)).unwrap();
        assert_eq!(said(&mut child, b"\n"), b"one\n");
        stop.store(true, Ordering::SeqCst);
        let refused = Err(Failure::Refused("stopped_by_signal"));
        assert_eq!(child.send(b"two\n", Duration::from_secs(2)), refused);
        assert_eq!(
            child.hear(Duration::from_secs(2)),
            refused.map(|()| Heard::Quiet)
        );
        let started = Instant::now();
        Box::new(child).end(b"three\n");
        assert!(started.elapsed() < Duration::from_millis(250));
    }
}
