//! Actual Linux owned inert children; these tests never invoke host policy.

use super::*;
use nix::sys::signal::{SigSet, SigmaskHow, pthread_sigmask};

const CHILD: &str = "capacity_vps::process::tests::inert_child_signal_mask";

fn mask() -> SigSet {
    let mut mask = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_SETMASK, None, Some(&mut mask)).unwrap();
    mask
}
fn invocation() -> (std::path::PathBuf, Vec<String>, BTreeMap<String, String>) {
    let program = std::env::current_exe().unwrap();
    let args = vec![
        "--exact".into(),
        CHILD.into(),
        "--ignored".into(),
        "--nocapture".into(),
    ];
    let mut environment = environment();
    environment.insert("SIMPLESTCHAT_OWNED_MASK_CHILD".into(), "1".into());
    (program, args, environment)
}
fn observe_spawn(fail_next: bool) {
    CLOSE_PROBE.with(|probe| {
        *probe.borrow_mut() = CloseProbe {
            fail_next,
            retain_identity: true,
            ..CloseProbe::default()
        }
    });
}
fn assert_original_child_reaped() {
    let identity = CLOSE_PROBE
        .with(|probe| probe.borrow_mut().identity.take())
        .unwrap();
    let result = waitid(
        Id::PIDFd(identity.as_fd()),
        WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT | WaitPidFlag::WNOHANG,
    );
    let closed = nix::unistd::close(identity);
    assert_eq!(result, Err(nix::errno::Errno::ECHILD));
    closed.unwrap();
}

#[test]
#[ignore = "inert child fixture; invoked only by the owned process parent test"]
fn inert_child_signal_mask() {
    let mode = std::env::var("SIMPLESTCHAT_OWNED_MASK_CHILD").unwrap();
    assert!(matches!(mode.as_str(), "1" | "exit7"));
    let mask = mask();
    assert!(!mask.contains(Signal::SIGINT));
    assert!(!mask.contains(Signal::SIGTERM));
    use std::io::Write;
    std::io::stdout()
        .write_all(b"owned_child_mask_unblocked\n")
        .unwrap();
    std::io::stdout().flush().unwrap();
    if mode == "exit7" {
        std::process::exit(7);
    }
    // The parent owns the group and must explicitly terminate/reap us. No
    // signal handler, Python interpreter, Docker or systemd boundary is used.
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn real_posix_spawn_unblocks_child_signals_and_finishes_group_and_original_thread_mask() {
    let previous = mask();
    let mut owner = Owner::new(now().unwrap(), super::super::protocol::Mode::Invoke).unwrap();
    let blocked = mask();
    assert!(blocked.contains(Signal::SIGINT));
    assert!(blocked.contains(Signal::SIGTERM));
    let (program, args, environment) = invocation();
    observe_spawn(false);
    let mut spawn_status = None;
    let mut group = Group::spawn(
        &program,
        &args,
        &environment,
        &mut owner,
        5,
        &mut spawn_status,
    )
    .unwrap();
    let observed = (|| {
        nonblocking(group.stdout.as_ref().unwrap())?;
        nonblocking(group.stderr.as_ref().unwrap())?;
        let end = now()?.checked_add(5 * SECOND).ok_or(Failure::Bounds)?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            owner.check(false)?;
            if now()? >= end {
                return Err(Failure::Deadline);
            }
            for file in [
                group.stdout.as_ref().unwrap(),
                group.stderr.as_ref().unwrap(),
            ] {
                match rustix::io::read(file, &mut buffer) {
                    Ok(n) => {
                        if n > 65536usize.saturating_sub(bytes.len()) {
                            return Err(Failure::Bounds);
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
                    Err(_) => return Err(Failure::Io),
                }
            }
            if bytes
                .windows(b"owned_child_mask_unblocked\n".len())
                .any(|v| v == b"owned_child_mask_unblocked\n")
            {
                return Ok(());
            }
            if group.exited()? {
                return Err(Failure::Unproven);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    let finished = group.finish(&mut owner, 5);
    let pipes_closed = group.stdout.is_none() && group.stderr.is_none();
    let reaped = !group.reap_pending && !group.reserved;
    let control = owner.finish();
    observed.unwrap();
    assert_eq!(finished.unwrap(), -15);
    assert!(pipes_closed && reaped);
    control.unwrap();
    assert_original_child_reaped();
    let restored = mask();
    assert_eq!(
        restored.contains(Signal::SIGINT),
        previous.contains(Signal::SIGINT)
    );
    assert_eq!(
        restored.contains(Signal::SIGTERM),
        previous.contains(Signal::SIGTERM)
    );
}

#[test]
fn reported_parent_pipe_close_failure_after_real_spawn_still_finishes_all_peers_and_reaps() {
    let mut owner = Owner::new(now().unwrap(), super::super::protocol::Mode::Invoke).unwrap();
    let (program, args, environment) = invocation();
    observe_spawn(true);
    let mut runner = Runner::new();
    let result = runner.run(
        Invocation {
            program: &program,
            arguments: &args,
            environment: &environment,
            seconds: 5,
            maximum: 65536,
            cleanup: false,
        },
        &mut owner,
    );
    let captured = runner.take_capture().unwrap();
    let calls = CLOSE_PROBE.with(|probe| {
        let probe = probe.borrow();
        probe.calls
    });
    assert!(matches!(result, Err(Failure::Close)));
    assert_eq!(captured.status, Some(-15));
    assert_eq!(calls, 5);
    assert_original_child_reaped();
    assert_eq!(owner.finish().err(), Some(Failure::Close));
}

#[test]
fn known_signed_reap_status_survives_a_later_reported_pipe_close_failure() {
    let mut owner = Owner::new(now().unwrap(), super::super::protocol::Mode::Invoke).unwrap();
    let (program, args, mut environment) = invocation();
    environment.insert("SIMPLESTCHAT_OWNED_MASK_CHILD".into(), "exit7".into());
    observe_spawn(false);
    let mut spawn_status = None;
    let mut group = Group::spawn(
        &program,
        &args,
        &environment,
        &mut owner,
        5,
        &mut spawn_status,
    )
    .unwrap();
    let observed = (|| {
        let end = now()? + 5 * SECOND;
        loop {
            owner.check(false)?;
            if group.exited()? {
                return Ok(());
            }
            if now()? >= end {
                return Err(Failure::Deadline);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    CLOSE_PROBE.with(|probe| probe.borrow_mut().fail_next = true);
    let finished = group.finish(&mut owner, 5);
    let status = group.status;
    let closed = group.stdout.is_none() && group.stderr.is_none();
    let control = owner.finish();
    observed.unwrap();
    assert_eq!(finished, Err(Failure::Close));
    assert_eq!(status, Some(7));
    assert!(closed);
    assert_eq!(control.err(), Some(Failure::Close));
    assert_original_child_reaped();
}

#[test]
fn real_pump_bound_failure_keeps_its_primary_and_the_actual_signed_reap_in_partial_capture() {
    let mut owner = Owner::new(now().unwrap(), super::super::protocol::Mode::Invoke).unwrap();
    let (program, args, mut environment) = invocation();
    environment.insert("SIMPLESTCHAT_OWNED_MASK_CHILD".into(), "exit7".into());
    observe_spawn(false);
    let mut runner = Runner::new();
    let result = runner.run(
        Invocation {
            program: &program,
            arguments: &args,
            environment: &environment,
            seconds: 5,
            maximum: 0,
            cleanup: false,
        },
        &mut owner,
    );
    let capture = runner.take_capture().unwrap();
    let control = owner.finish();
    assert_original_child_reaped();
    assert!(matches!(result, Err(Failure::Bounds)));
    // The child may complete its explicit exit7 before cancellation, or be
    // terminated by the owned group finalizer. All are actual signed reaps;
    // neither a made-up1 nor an unobserved status is retained.
    assert!(matches!(capture.status, Some(7 | -15 | -9)));
    assert_eq!(control.err(), Some(Failure::Bounds));
}
